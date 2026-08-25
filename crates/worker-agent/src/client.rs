//! 与 Master 的长连接、重连与指令分发（第 5.3 节、第 6 节、第 10 节、V3 方案第 7-9 节）。
//!
//! 这里是 Worker 上行消息的唯一出口。
//! 负责与 Master 建立 TLS / mTLS 连接、重连现场对账、Outbox 补报与实时指令分发。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use platform_domain::WorkerStatus;
use platform_proto::v1 as pb;
use platform_proto::v1::worker_link_client::WorkerLinkClient;
use platform_proto::{
    METADATA_AGENT_VERSION, METADATA_CLIENT_CERT_FINGERPRINT, METADATA_NODE_ID, METADATA_NODE_TOKEN,
};
use prost::Message;
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tonic::metadata::MetadataValue;
use tonic::Request;
use uuid::Uuid;

use crate::bus::{volatile_id, OutboundEventBus};
use crate::config::{SavedIdentity, WorkerConfig};
use crate::dynamic::ConfigState;
use crate::outbox::LocalStore;
use crate::slot::SlotManager;
use crate::storage::{self, NasHealth, NasProbeManager};
use crate::tls;

/// 本机 Agent 版本。
const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 单次 gRPC 流的上行缓冲。
const STREAM_CAPACITY: usize = 128;

/// 启动 Worker Agent 主循环。
pub async fn run_agent_loop(config: WorkerConfig, identity: SavedIdentity) -> Result<()> {
    let node_id = identity.node_id.clone();
    let _node_token = identity.node_token.clone();

    std::fs::create_dir_all(&config.storage.data_dir)?;
    let outbox = LocalStore::open(&config.storage.data_dir.join("worker.db"))?;

    // NAS no-replace 能力探测（第 9.3 节）：启动时执行一次，
    // 挂载点重新挂载后再执行；心跳只读取结论。
    let nas_probe = NasProbeManager::start(config.storage.clone(), node_id.clone()).await;

    let config_state = Arc::new(ConfigState::new());
    let (bus, mut bus_rx) = OutboundEventBus::new(outbox.clone());

    let slot_manager = Arc::new(SlotManager::new(
        config.execution.requested_slots,
        config.clone(),
        config_state.clone(),
        outbox.clone(),
        bus.clone(),
        node_id.clone(),
    ));

    let mut sys = System::new_with_specifics(
        RefreshKind::new()
            .with_cpu(CpuRefreshKind::everything())
            .with_memory(MemoryRefreshKind::everything()),
    );

    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(30);

    loop {
        tracing::info!(endpoint = %config.master.endpoint, "正在连接 Master 服务端");

        let session = Connection {
            config: &config,
            node_id: &node_id,
            identity: &identity,
            outbox: &outbox,
            config_state: &config_state,
            slots: &slot_manager,
            nas_probe: &nas_probe,
            reconciling: Arc::new(AtomicBool::new(true)),
        };

        match session.serve(&mut bus_rx, &mut sys).await {
            Ok(()) => {
                tracing::info!("与 Master 的连接已关闭，准备重连");
                backoff = Duration::from_secs(1);
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    retry_after_secs = backoff.as_secs(),
                    "与 Master 的连接异常"
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(max_backoff);
            }
        }
    }
}

/// 构造长连接会话对象。
pub fn create_connection<'a>(
    config: &'a WorkerConfig,
    node_id: &'a str,
    identity: &'a SavedIdentity,
    outbox: &'a LocalStore,
    config_state: &'a Arc<ConfigState>,
    slots: &'a Arc<SlotManager>,
    nas_probe: &'a NasProbeManager,
) -> Connection<'a> {
    Connection {
        config,
        node_id,
        identity,
        outbox,
        config_state,
        slots,
        nas_probe,
        reconciling: Arc::new(AtomicBool::new(true)),
    }
}

/// 一次连接所需的全部借用。
pub struct Connection<'a> {
    config: &'a WorkerConfig,
    node_id: &'a str,
    identity: &'a SavedIdentity,
    outbox: &'a LocalStore,
    config_state: &'a Arc<ConfigState>,
    slots: &'a Arc<SlotManager>,
    nas_probe: &'a NasProbeManager,
    reconciling: Arc<AtomicBool>,
}

impl Connection<'_> {
    /// 建立一次长连接并服务到断开为止。
    pub async fn serve(
        &self,
        bus_rx: &mut mpsc::Receiver<pb::WorkerMessage>,
        sys: &mut System,
    ) -> Result<()> {
        let channel = tls::worker_link_channel(self.config, self.identity).await?;

        let mut client = WorkerLinkClient::new(channel);
        let (stream_tx, stream_rx) = mpsc::channel::<pb::WorkerMessage>(STREAM_CAPACITY);

        let mut req = Request::new(ReceiverStream::new(stream_rx));
        let meta = req.metadata_mut();
        if !self.node_id.trim().is_empty() {
            if let Ok(val) = MetadataValue::try_from(self.node_id) {
                meta.insert(METADATA_NODE_ID, val);
            }
        }
        if !self.identity.node_token.trim().is_empty() {
            if let Ok(val) = MetadataValue::try_from(self.identity.node_token.as_str()) {
                meta.insert(METADATA_NODE_TOKEN, val);
            }
        }
        meta.insert(
            METADATA_AGENT_VERSION,
            MetadataValue::try_from(AGENT_VERSION)?,
        );
        // 客户端证书指纹：生产环境由入口代理（Caddy `client_auth mode request`）
        // 在 TLS 终止时注入真实指纹头并覆盖本值；本地 insecure 联调（无代理）时
        // 用本机保存的证书指纹自报，Master 仍按「指纹必须属于该节点」校验。
        if let Some(fp) = self
            .identity
            .certificate_fingerprint
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            meta.insert(
                METADATA_CLIENT_CERT_FINGERPRINT,
                MetadataValue::try_from(fp)?,
            );
        }

        let mut in_stream = client.open_link(req).await?.into_inner();
        tracing::info!(node_id = %self.node_id, "gRPC mTLS 长连接已建立");

        // 丢弃断线期间积压的易失消息
        let dropped = OutboundEventBus::drain_stale(bus_rx);
        if dropped > 0 {
            tracing::info!(dropped, "已丢弃断线期间积压的易失消息");
        }

        // 1. NodeOnline 上报合并后的现场（内存 + SQLite）
        self.reconciling.store(true, Ordering::SeqCst);
        stream_tx.send(self.node_online().await).await?;

        // 2. 补报 Outbox 中的可靠事件
        self.replay_outbox(&stream_tx, 64).await;

        let mut heartbeat_secs = self.config_state.snapshot().heartbeat_interval_secs.max(5);
        let mut heartbeat = tokio::time::interval(Duration::from_secs(heartbeat_secs as u64));
        let mut replay = tokio::time::interval(Duration::from_secs(5));
        replay.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                Some(msg) = bus_rx.recv() => {
                    if stream_tx.send(msg).await.is_err() {
                        tracing::warn!("上行流已关闭，结束本次连接");
                        break;
                    }
                }

                _ = heartbeat.tick() => {
                    if !self.beat(&stream_tx, sys).await {
                        break;
                    }
                    let wanted = self.config_state.snapshot().heartbeat_interval_secs.max(5);
                    if wanted != heartbeat_secs {
                        heartbeat_secs = wanted;
                        heartbeat = tokio::time::interval(Duration::from_secs(wanted as u64));
                        tracing::info!(heartbeat_secs, "已应用新的心跳间隔");
                    }
                }

                _ = replay.tick() => {
                    self.replay_outbox(&stream_tx, 10).await;
                }

                down = in_stream.next() => match down {
                    Some(Ok(msg)) => {
                        if let Some(payload) = msg.payload {
                            if let Err(err) = self.handle_master_payload(payload, &stream_tx).await {
                                // 协议错误（如未知对账裁决）：断开重连，保持 reconciling
                                tracing::warn!(error = %err, "处理 Master 指令时发生协议错误，断开重连");
                                break;
                            }
                        }
                    }
                    Some(Err(status)) => {
                        tracing::warn!(status = %status, "Master 下行流报错");
                        break;
                    }
                    None => {
                        tracing::info!("Master 下行流已关闭");
                        break;
                    }
                },
            }
        }

        Ok(())
    }

    /// 上线对账消息：合并内存与 SQLite 现场记录。
    async fn node_online(&self) -> pb::WorkerMessage {
        let active_executions = self.merged_active_executions().await;

        pb::WorkerMessage {
            event_id: format!("evt-online-{}", Uuid::new_v4()),
            sent_at: Utc::now().to_rfc3339(),
            replayed: false,
            payload: Some(pb::worker_message::Payload::NodeOnline(pb::NodeOnline {
                node_id: self.node_id.to_string(),
                agent_version: AGENT_VERSION.to_string(),
                os: std::env::consts::OS.to_string(),
                os_version: System::os_version().unwrap_or_default(),
                max_slots: self.slots.slot_count(),
                available_slots: self.slots.available_slots().await,
                applied_config_version: self.config_state.applied_version(),
                active_executions,
            })),
        }
    }

    /// 合并 SQLite 现场与内存现场（V4-04：心跳与上线必须用同一个合并函数）。
    ///
    /// 规则：按 execution_id 去重；SQLite 记录提供完整恢复字段；
    /// 内存只补充更新的阶段与世代，**不删除** SQLite 记录。
    async fn merged_active_executions(&self) -> Vec<pb::ActiveExecution> {
        let mut active_map: HashMap<String, pb::ActiveExecution> = HashMap::new();

        if let Ok(records) = self.outbox.list_active_executions() {
            for r in records {
                active_map.insert(
                    r.execution_id.clone(),
                    pb::ActiveExecution {
                        session_id: r.session_id,
                        execution_id: r.execution_id,
                        task_id: r.task_id,
                        task_status: r.task_status,
                        stage_version: r.stage_version,
                        stage: r.stage as i32,
                        format: r.format,
                        local_file_path: r.local_file_path,
                        nas_relative_path: r.nas_relative_path,
                        source_size_bytes: r.source_size_bytes.max(0) as u64,
                        source_sha256: r.source_sha256,
                        result_event_id: r.result_event_id,
                    },
                );
            }
        }

        for mem in self.slots.active_executions().await {
            match active_map.get_mut(&mem.execution_id) {
                // 内存只补充更新的阶段字段，SQLite 现场保持不变
                Some(rec) => {
                    rec.task_status = mem.task_status;
                    rec.stage_version = mem.stage_version;
                    rec.stage = mem.stage;
                }
                None => {
                    active_map.insert(mem.execution_id.clone(), mem);
                }
            }
        }

        active_map.into_values().collect()
    }

    /// 补报本地 Outbox 中尚未确认的可靠事件。
    async fn replay_outbox(&self, stream_tx: &mpsc::Sender<pb::WorkerMessage>, limit: usize) {
        let items = match self.outbox.fetch_pending(limit) {
            Ok(items) => items,
            Err(err) => {
                tracing::error!(error = %err, "读取本地 Outbox 失败");
                return;
            }
        };
        for item in items {
            match pb::WorkerMessage::decode(&item.payload_bytes[..]) {
                Ok(mut msg) => {
                    msg.replayed = true;
                    let event_id = msg.event_id.clone();
                    if stream_tx.send(msg).await.is_err() {
                        return;
                    }
                    if let Err(err) = self.outbox.mark_sent(&event_id) {
                        tracing::warn!(event_id = %event_id, error = %err, "更新 Outbox 发送时间失败");
                    }
                }
                Err(err) => {
                    tracing::error!(
                        event_id = %item.event_id,
                        error = %err,
                        "本地 Outbox 记录无法解码，已放弃该事件"
                    );
                    let _ = self.outbox.acknowledge(&item.event_id);
                }
            }
        }
    }

    /// 发送心跳与槽位状态，并在对账完成后申请新会话。
    async fn beat(&self, stream_tx: &mpsc::Sender<pb::WorkerMessage>, sys: &mut System) -> bool {
        sys.refresh_cpu_all();
        sys.refresh_memory();

        let mut health = storage::check_nas_health(&self.config.storage, self.node_id).await;
        // NAS 重新挂载后重新探测 no-replace 能力
        self.nas_probe.maybe_reprobe(&health).await;
        let capability = self.nas_probe.capability();
        if health.healthy() && !capability.usable() {
            // 挂载可写但文件系统不支持 no-replace：整体判定存储异常（第 9.2 节）
            health = NasHealth {
                mount_present: true,
                writable: false,
                free_gb: health.free_gb,
                latency_ms: health.latency_ms,
                detail: format!("{}（能力探测：{}）", health.detail, capability.detail),
            };
        }
        let snapshot = self.config_state.snapshot();
        let mail_provider = self.slots.mail_provider_status().await;
        let available = self.slots.available_slots().await;
        let status = self.self_assessed_status(&health, available).await;

        let heartbeat = pb::Heartbeat {
            node_id: self.node_id.to_string(),
            available_slots: available,
            cpu_percent: sys.global_cpu_usage() as f64,
            memory_used_mb: sys.used_memory() / (1024 * 1024),
            memory_total_mb: sys.total_memory() / (1024 * 1024),
            staging_free_gb: storage::free_space_gb(&self.config.storage.data_dir).unwrap_or(0),
            nas_free_gb: health.reported_free_gb(),
            nas_healthy: health.healthy(),
            node_status: status.as_str().to_string(),
            active_session_ids: self.slots.active_session_ids().await,
            // V4-04：心跳上报合并后的现场（SQLite + 内存），不能只报内存任务
            active_executions: self.merged_active_executions().await,
            applied_config_version: snapshot.config_version.clone(),
            // 邮件 Provider 按注册任务租约固定版本，不作为普通节点配置持久化。
            // 具体版本和健康阶段通过注册任务进度上报；空闲心跳不携带敏感配置。
            applied_mail_provider_version: mail_provider.version,
            mail_provider_name: mail_provider.name,
            mail_provider_health: if mail_provider.health.is_empty() {
                "未执行注册任务".to_string()
            } else {
                mail_provider.health
            },
        };

        if stream_tx
            .send(wrap(
                "hb",
                pb::worker_message::Payload::Heartbeat(heartbeat),
            ))
            .await
            .is_err()
        {
            return false;
        }

        let slot_report = pb::SlotStatusReport {
            node_id: self.node_id.to_string(),
            slots: self.slots.slot_states().await,
        };
        if stream_tx
            .send(wrap(
                "slot",
                pb::worker_message::Payload::SlotStatus(slot_report),
            ))
            .await
            .is_err()
        {
            return false;
        }

        // 对账未完成前，禁止申请新会话
        if self.reconciling.load(Ordering::SeqCst) {
            tracing::debug!("重连对账中，暂停申请新会话");
            return true;
        }

        if status != WorkerStatus::Online {
            return true;
        }
        if health.reported_free_gb() < snapshot.minimum_free_gb {
            tracing::warn!(
                free_gb = health.reported_free_gb(),
                minimum_free_gb = snapshot.minimum_free_gb,
                "NAS 剩余空间低于下限，本轮不申请新会话"
            );
            return true;
        }
        if let Some(slot_index) = self.slots.find_idle_slot().await {
            let work_req = pb::WorkRequest {
                node_id: self.node_id.to_string(),
                slot_index,
                supported_task_types: vec![
                    "图书下载".to_string(),
                    "账号注册".to_string(),
                    "NAS核验".to_string(),
                    "代理检测".to_string(),
                ],
                request_id: uuid::Uuid::new_v4().to_string(),
            };
            if stream_tx
                .send(wrap(
                    "wreq",
                    pb::worker_message::Payload::WorkRequest(work_req),
                ))
                .await
                .is_err()
            {
                return false;
            }
        }
        true
    }

    async fn self_assessed_status(&self, health: &NasHealth, available: u32) -> WorkerStatus {
        if !health.healthy() {
            return WorkerStatus::StorageError;
        }
        if self.slots.node_paused() {
            return WorkerStatus::Paused;
        }
        if available == 0 && self.slots.slot_count() > 0 {
            return WorkerStatus::Busy;
        }
        WorkerStatus::Online
    }

    /// 分发 Master 下行指令。返回错误表示协议错误（调用方应断开重连）。
    async fn handle_master_payload(
        &self,
        payload: pb::master_message::Payload,
        stream_tx: &mpsc::Sender<pb::WorkerMessage>,
    ) -> anyhow::Result<()> {
        use pb::master_message::Payload as P;
        let result = match payload {
            P::EventAck(ack) => {
                if ack.accepted {
                    if let Err(err) = self.outbox.acknowledge_event(&ack.event_id) {
                        tracing::warn!(event_id = %ack.event_id, error = %err, "确认本地事件与清理现场失败");
                    }
                } else {
                    tracing::warn!(
                        event_id = %ack.event_id,
                        detail = %ack.detail,
                        "Master 拒绝了该事件，保留在本地待重放"
                    );
                }
                Ok(())
            }
            P::ReconcileExecutions(reconcile) => {
                tracing::info!(
                    decisions = reconcile.decisions.len(),
                    complete = reconcile.reconciliation_complete,
                    "收到 Master 对账裁决"
                );
                // 未知裁决 = 协议错误：保持 reconciling 并断开重连，绝不忽略后恢复调度。
                if reconcile.decisions.iter().any(|d| {
                    pb::ReconcileAction::from_i32_safe(d.action) == pb::ReconcileAction::Unspecified
                }) {
                    return Err(anyhow::anyhow!(
                        "收到未知对账裁决（action 值非法），协议错误，断开重连"
                    ));
                }
                // 逐条执行并发送逐执行 ACK（V4 第 10.5 节）；
                // 只有 Master 在收齐全部 ACK 后下发的 reconciliation_complete=true
                // 才能解除 reconciling——否则保持对账状态，不恢复新任务调度。
                for d in &reconcile.decisions {
                    let action = pb::ReconcileAction::from_i32_safe(d.action);
                    tracing::info!(
                        execution_id = %d.execution_id,
                        action = %action.display_name(),
                        reason = %d.reason,
                        "应用对账裁决"
                    );
                    let ack = self.apply_reconcile_action(d, action, stream_tx).await;
                    let _ = stream_tx
                        .send(wrap("rack", pb::worker_message::Payload::ReconcileAck(ack)))
                        .await;
                }
                if reconcile.reconciliation_complete {
                    self.reconciling.store(false, Ordering::SeqCst);
                    tracing::info!("重连对账已全部完成，恢复会话调度");
                }
                Ok(())
            }
            P::NodeConfig(cfg) => {
                self.apply_node_config(cfg);
                Ok(())
            }
            P::RefreshConfig(refresh) => {
                tracing::info!(
                    version = %refresh.config_version,
                    "Master 要求刷新配置，将在下一次心跳回报当前已生效版本"
                );
                Ok(())
            }
            P::CreateSession(session) => {
                tracing::info!(
                    session_id = %session.session_id,
                    slot = session.slot_index,
                    "收到创建会话指令"
                );
                self.slots.dispatch_create_session(session).await
            }
            P::AssignTask(assign) => {
                tracing::info!(
                    session_id = %assign.session_id,
                    task_id = %assign.task_id,
                    stage_version = assign.stage_version,
                    "收到任务分配"
                );
                self.slots.dispatch_assign_task(assign).await
            }
            P::AssignRegistrationTask(assign) => {
                tracing::info!(
                    session_id = %assign.session_id,
                    registration_task_id = %assign.registration_task_id,
                    stage_version = assign.stage_version,
                    "收到账号注册任务分配"
                );
                self.slots.dispatch_assign_registration_task(assign).await
            }
            P::ContinueManualAction(cont) => {
                tracing::info!(
                    action_id = %cont.action_id,
                    action_type = %cont.action_type,
                    "收到人工确认继续指令"
                );
                self.slots.dispatch_continue_manual_action(cont).await
            }
            P::CancelRegistrationTask(cancel) => {
                tracing::info!(
                    registration_task_id = %cancel.registration_task_id,
                    reason = %cancel.reason,
                    "收到取消账号注册任务指令"
                );
                self.slots.dispatch_cancel_registration_task(cancel).await
            }
            P::ExecuteCommand(cmd) => {
                tracing::info!(
                    command_id = %cmd.command_id,
                    command_type = %cmd.command_type,
                    "收到节点执行命令"
                );
                Ok(())
            }
            P::CancelTask(cancel) => {
                tracing::info!(task_id = %cancel.task_id, reason = %cancel.reason, "收到取消任务指令");
                self.slots.dispatch_cancel_task(cancel).await
            }
            P::EndSession(end) => {
                tracing::info!(session_id = %end.session_id, reason = %end.reason, "收到结束会话指令");
                self.slots.dispatch_end_session(end).await
            }
            P::PauseNode(pause) => {
                tracing::warn!(reason = %pause.reason, "收到停用节点指令");
                self.slots.dispatch_pause(pause).await
            }
            P::ResumeNode(resume) => {
                tracing::info!(reason = %resume.reason, "收到恢复节点指令");
                self.slots.dispatch_resume().await
            }
            P::VerifyNasFile(verify) => {
                let reply = self.verify_nas_file(verify).await;
                let _ = stream_tx
                    .send(wrap(
                        "naschk",
                        pb::worker_message::Payload::NasCheckResult(reply),
                    ))
                    .await;
                Ok(())
            }
            P::NoTask(no_task) => {
                tracing::debug!(
                    reason = %no_task.reason,
                    retry_after_secs = no_task.retry_after_secs,
                    "Master 暂无可用任务"
                );
                Ok(())
            }
            P::PrepareUpgrade(upgrade) => {
                tracing::warn!(
                    target_version = %upgrade.target_version,
                    "收到升级指令，但本版本尚未实现自动升级，已忽略"
                );
                Ok(())
            }
        };

        result
    }

    /// 执行一条对账裁决（V4 方案第 10.5 节）。
    ///
    /// 每一种 [`pb::ReconcileAction`] 都有显式分支；未知值按协议错误处理：
    /// 记录错误、保持 reconciling、断开重连（由调用方决定），绝不忽略后恢复调度。
    async fn apply_reconcile_action(
        &self,
        d: &pb::ExecutionReconcileDecision,
        action: pb::ReconcileAction,
        stream_tx: &mpsc::Sender<pb::WorkerMessage>,
    ) -> pb::ReconcileAck {
        use pb::ReconcileAction as A;
        let execution_id = d.execution_id.clone();

        let accepted = match action {
            A::StopAndRetry => {
                self.stop_memory_execution(&execution_id, &d.reason).await;
                self.cleanup_execution_files(&execution_id);
                let _ = self.outbox.clear_execution(&execution_id);
                true
            }
            A::ResumeIngest => self.resume_ingest(&execution_id, stream_tx).await,
            A::VerifyNas => {
                self.verify_nas_for_reconcile(&execution_id, stream_tx)
                    .await
            }
            A::ReplayResult => self.replay_single_event(&execution_id, stream_tx).await,
            A::CleanupOnly => {
                self.stop_memory_execution(&execution_id, &d.reason).await;
                self.cleanup_execution_files(&execution_id);
                let _ = self.outbox.clear_execution(&execution_id);
                true
            }
            A::Unspecified => {
                // 未知裁决：协议错误。保持 reconciling 并断开重连。
                tracing::error!(
                    execution_id = %execution_id,
                    action_raw = d.action,
                    "收到未知对账裁决，视为协议错误，将断开重连"
                );
                return pb::ReconcileAck {
                    execution_id,
                    action: action as i32,
                    accepted: false,
                    detail: format!("未知对账裁决（action={}），协议错误", d.action),
                };
            }
        };

        pb::ReconcileAck {
            execution_id: d.execution_id.clone(),
            action: action as i32,
            accepted,
            detail: if accepted {
                format!("{} 已执行", action.display_name())
            } else {
                "无法执行该裁决，现场保留待人工处理".to_string()
            },
        }
    }

    /// 取消内存中匹配该 execution 的进行中任务（若有）。
    async fn stop_memory_execution(&self, execution_id: &str, reason: &str) {
        let _ = self
            .slots
            .dispatch_cancel_task(pb::CancelTask {
                node_id: self.node_id.to_string(),
                session_id: String::new(),
                task_id: String::new(),
                execution_id: execution_id.to_string(),
                stage_version: 0,
                reason: format!("对账裁决：{reason}"),
            })
            .await;
    }

    /// 清理该执行的安全范围内临时文件（仅 data_dir/staging 下）。
    fn cleanup_execution_files(&self, execution_id: &str) {
        let Ok(Some(state)) = self.outbox.get_execution(execution_id) else {
            return;
        };
        let staging = std::path::Path::new(&state.staging_dir);
        let data_root = &self.config.storage.data_dir;
        // 只删除位于本地 staging 根之下的目录，绝不碰 NAS 路径
        if staging.starts_with(data_root) {
            if let Err(err) = std::fs::remove_dir_all(staging) {
                tracing::debug!(
                    path = %staging.display(),
                    error = %err,
                    "清理执行临时目录失败（可能不存在）"
                );
            }
        }
    }

    /// RESUME_INGEST：验证本地文件路径、大小与 SHA，继续 NAS 入库（第 10.5 节）。
    ///
    /// 浏览器进程不可恢复，但「本地文件完成」阶段的证据可证明，因此由连接层
    /// 直接复用 NAS 入库流程，不需要浏览器会话。
    async fn resume_ingest(
        &self,
        execution_id: &str,
        stream_tx: &mpsc::Sender<pb::WorkerMessage>,
    ) -> bool {
        let Ok(Some(state)) = self.outbox.get_execution(execution_id) else {
            tracing::warn!(execution_id, "RESUME_INGEST：现场记录不存在");
            return false;
        };
        if state.local_file_path.is_empty() || state.source_sha256.is_empty() {
            tracing::warn!(
                execution_id,
                "RESUME_INGEST：缺少本地文件证据，无法继续入库"
            );
            return false;
        }
        let local = std::path::Path::new(&state.local_file_path);
        let meta = match tokio::fs::metadata(local).await {
            Ok(m) => m,
            Err(err) => {
                tracing::warn!(path = %state.local_file_path, error = %err, "RESUME_INGEST：本地文件不可读");
                return false;
            }
        };
        if !meta.is_file()
            || (state.source_size_bytes > 0 && meta.len() != state.source_size_bytes as u64)
        {
            tracing::warn!(
                execution_id,
                expected = state.source_size_bytes,
                actual = meta.len(),
                "RESUME_INGEST：本地文件大小与现场不一致"
            );
            return false;
        }
        if let Ok(sha) = storage::calculate_sha256(local).await {
            if sha != state.source_sha256 {
                tracing::warn!(execution_id, "RESUME_INGEST：本地文件哈希与现场不一致");
                return false;
            }
        } else {
            return false;
        }

        let task_uuid = Uuid::parse_str(&state.task_id).unwrap_or_else(|_| Uuid::new_v4());
        let exec_uuid = Uuid::parse_str(execution_id).unwrap_or_else(|_| Uuid::new_v4());
        let node_uuid = Uuid::parse_str(self.node_id).unwrap_or_else(|_| Uuid::new_v4());
        let minimum_bytes = self.config_state.snapshot().minimum_file_bytes;

        let ingest = storage::ingest_file(
            &self.config.storage,
            local,
            &state.nas_relative_path,
            task_uuid,
            exec_uuid,
            node_uuid,
            minimum_bytes,
        )
        .await;

        let (result, task_status, reason, file) = match ingest {
            Ok(storage::IngestOutcome::Success(res))
            | Ok(storage::IngestOutcome::AlreadyExistsSameHash(res)) => (
                platform_domain::ExecutionResult::Success
                    .as_str()
                    .to_string(),
                "已完成".to_string(),
                "恢复现场后 NAS 原子入库成功".to_string(),
                Some(pb::FileEvidence {
                    nas_relative_path: res.nas_relative_path,
                    file_name: res.file_name,
                    size_bytes: res.size_bytes,
                    sha256: res.sha256,
                    format: if state.format.is_empty() {
                        "pdf".to_string()
                    } else {
                        state.format.clone()
                    },
                    ingested_at: chrono::Utc::now().to_rfc3339(),
                }),
            ),
            Ok(storage::IngestOutcome::ConflictDifferentHash { .. }) => (
                platform_domain::ExecutionResult::Uncertain
                    .as_str()
                    .to_string(),
                "待确认".to_string(),
                "恢复现场后 NAS 发现同路径不同哈希，保持待确认".to_string(),
                None,
            ),
            Err(err) => {
                tracing::warn!(execution_id, error = %err, "RESUME_INGEST：NAS 入库失败");
                return false;
            }
        };

        // 可靠投递：先落 Outbox 再发内存通道。若连接在 Master 确认前断开，
        // 事件留在 Outbox，重连后 REPLAY_RESULT 才能定向重放（P0 修复）。
        let event_id = format!("evt-res-{execution_id}");
        let msg = pb::WorkerMessage {
            event_id: event_id.clone(),
            sent_at: chrono::Utc::now().to_rfc3339(),
            replayed: true,
            payload: Some(pb::worker_message::Payload::TaskResult(pb::TaskResult {
                session_id: state.session_id.clone(),
                execution_id: execution_id.to_string(),
                task_id: state.task_id.clone(),
                result,
                task_status,
                reason,
                stage_version: state.stage_version,
                attempt: 0,
                duration_ms: 0,
                quota_used: 0,
                quota_total: 0,
                file,
            })),
        };
        let sent = self.send_reliable_direct(stream_tx, &event_id, msg).await;
        if !sent {
            tracing::warn!(
                execution_id,
                "RESUME_INGEST：结果事件无法可靠投递，对账 ACK 返回失败"
            );
            return false;
        }
        let _ = self
            .outbox
            .set_execution_result_event(execution_id, &event_id);
        let _ = self
            .outbox
            .set_execution_stage(execution_id, pb::ExecutionStage::ResultPending);
        true
    }

    /// VERIFY_NAS：读取最终相对路径并复算 NAS 文件证据，发送可靠核验事件（第 10.5 节）。
    async fn verify_nas_for_reconcile(
        &self,
        execution_id: &str,
        stream_tx: &mpsc::Sender<pb::WorkerMessage>,
    ) -> bool {
        let Ok(Some(state)) = self.outbox.get_execution(execution_id) else {
            return false;
        };
        if state.nas_relative_path.is_empty() {
            tracing::warn!(execution_id, "VERIFY_NAS：现场缺少 NAS 相对路径");
            return false;
        }
        let verify = pb::VerifyNasFile {
            task_id: state.task_id.clone(),
            nas_relative_path: state.nas_relative_path.clone(),
            expected_sha256: state.source_sha256.clone(),
            expected_size_bytes: state.source_size_bytes.max(0) as u64,
            expected_format: state.format.clone(),
            expected_file_name: state
                .nas_relative_path
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_string(),
        };
        let reply = self.verify_nas_file(verify).await;
        // 稳定事件编号：可靠投递，断线后由 Outbox 补报，重复上报由 Master 去重
        let event_id = format!("evt-naschk-{execution_id}");
        let msg = pb::WorkerMessage {
            event_id: event_id.clone(),
            sent_at: chrono::Utc::now().to_rfc3339(),
            replayed: true,
            payload: Some(pb::worker_message::Payload::NasCheckResult(reply)),
        };
        self.send_reliable_direct(stream_tx, &event_id, msg).await
    }

    /// 连接层内的「可靠投递」：先写本地 Outbox（`INSERT OR IGNORE`，稳定事件编号
    /// 幂等），再尽力实时发送；发送成功后标记 `已发送`。
    ///
    /// 返回 `false` 表示事件既无法可靠持久化、也无法实时发出——调用方必须
    /// 把对账 ACK 置为 `accepted=false`，让 Master 保持待确认，绝不谎报成功。
    async fn send_reliable_direct(
        &self,
        stream_tx: &mpsc::Sender<pb::WorkerMessage>,
        event_id: &str,
        msg: pb::WorkerMessage,
    ) -> bool {
        if let Err(err) = self.outbox.enqueue(event_id, &msg) {
            tracing::error!(event_id, error = %err, "可靠事件写入本地 Outbox 失败");
            return false;
        }
        let sent = stream_tx.send(msg).await.is_ok();
        if sent {
            if let Err(err) = self.outbox.mark_sent(event_id) {
                tracing::warn!(event_id, error = %err, "更新 Outbox 发送时间失败");
            }
        }
        sent
    }

    /// REPLAY_RESULT：只重放该 execution 对应的结果事件（第 10.5 节）。
    ///
    /// 优先使用现场记录的结果事件编号；现场缺失时回退到稳定推导值
    /// `evt-res-{execution_id}`。绝不笼统重放前 N 条。
    async fn replay_single_event(
        &self,
        execution_id: &str,
        stream_tx: &mpsc::Sender<pb::WorkerMessage>,
    ) -> bool {
        let event_id = match self.outbox.get_execution(execution_id) {
            Ok(Some(state)) if !state.result_event_id.is_empty() => state.result_event_id,
            _ => format!("evt-res-{execution_id}"),
        };
        let item = match self.outbox.fetch_by_event_id(&event_id) {
            Ok(Some(item)) => item,
            Ok(None) => {
                tracing::warn!(
                    execution_id,
                    event_id,
                    "REPLAY_RESULT：结果事件不在本地 Outbox"
                );
                return false;
            }
            Err(err) => {
                tracing::error!(execution_id, error = %err, "REPLAY_RESULT：读取 Outbox 失败");
                return false;
            }
        };
        match pb::WorkerMessage::decode(&item.payload_bytes[..]) {
            Ok(mut msg) => {
                msg.replayed = true;
                let event_id = msg.event_id.clone();
                let sent = stream_tx.send(msg).await.is_ok();
                if sent {
                    if let Err(err) = self.outbox.mark_sent(&event_id) {
                        tracing::warn!(event_id = %event_id, error = %err, "更新 Outbox 发送时间失败");
                    }
                }
                sent
            }
            Err(err) => {
                tracing::error!(event_id = %item.event_id, error = %err, "REPLAY_RESULT：结果事件无法解码");
                let _ = self.outbox.acknowledge(&item.event_id);
                false
            }
        }
    }

    fn apply_node_config(&self, cfg: pb::NodeConfig) {
        let version = cfg.config_version.clone();
        match self.config_state.apply(&cfg) {
            Ok(true) => {
                let snapshot = self.config_state.snapshot();
                let site = match snapshot.require_site_base() {
                    Ok(site) => site,
                    Err(err) => format!("暂不可用（{err}）"),
                };
                tracing::info!(
                    version = %version,
                    max_slots = snapshot.max_slots,
                    site_base = %site,
                    download_format = %snapshot.download_format,
                    minimum_file_bytes = snapshot.minimum_file_bytes,
                    "已应用 Master 下发的运行配置"
                );
            }
            Ok(false) => {
                tracing::debug!(version = %version, "运行配置与当前一致，未做变更");
            }
            Err(rejection) => {
                tracing::error!(
                    version = %version,
                    rejection = %rejection,
                    "拒绝 Master 下发的运行配置，继续使用上一份"
                );
            }
        }
    }

    /// 核查 NAS 上是否已存在目标文件（严格从实际文件推导证据）。
    async fn verify_nas_file(&self, verify: pb::VerifyNasFile) -> pb::NasCheckResult {
        let health = storage::check_nas_health(&self.config.storage, self.node_id).await;

        if let Err(err) = storage::validate_relative_path(&verify.nas_relative_path) {
            return pb::NasCheckResult {
                node_id: self.node_id.to_string(),
                task_id: verify.task_id,
                mount_present: health.mount_present,
                writable: health.writable,
                free_gb: health.reported_free_gb(),
                latency_ms: health.latency_ms,
                file_exists: false,
                file: None,
                detail: format!("核验路径非法，已拒绝：{err}"),
            };
        }

        let target = self
            .config
            .storage
            .nas_mount
            .join(&verify.nas_relative_path);
        let size_bytes = match tokio::fs::metadata(&target).await {
            Ok(meta) if meta.is_file() => Some(meta.len()),
            _ => None,
        };

        let (file, detail) = match size_bytes {
            None => (None, "NAS 上不存在该目标文件".to_string()),
            Some(size) => match storage::calculate_sha256(&target).await {
                Ok(sha256) => {
                    // 从实际文件推导格式与文件名
                    let file_name = target
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(&verify.expected_file_name)
                        .to_string();

                    let actual_format = detect_format_from_file(&target).await;

                    let mut notes = Vec::new();
                    if verify.expected_size_bytes > 0 && verify.expected_size_bytes != size {
                        notes.push(format!(
                            "大小与预期不一致（预期 {} 字节，实际 {} 字节）",
                            verify.expected_size_bytes, size
                        ));
                    }
                    if !verify.expected_sha256.is_empty()
                        && !verify.expected_sha256.eq_ignore_ascii_case(&sha256)
                    {
                        notes.push("SHA-256 与预期不一致".to_string());
                    }
                    if !verify.expected_format.is_empty() && actual_format != verify.expected_format
                    {
                        notes.push(format!(
                            "格式与预期不一致（预期 {}，实际 {}）",
                            verify.expected_format, actual_format
                        ));
                    }

                    let detail = if notes.is_empty() {
                        "NAS 目标文件已核验存在且证据完整".to_string()
                    } else {
                        format!("NAS 目标文件存在但{}", notes.join("；"))
                    };

                    (
                        Some(pb::FileEvidence {
                            nas_relative_path: verify.nas_relative_path.clone(),
                            file_name,
                            size_bytes: size,
                            sha256,
                            format: actual_format,
                            ingested_at: Utc::now().to_rfc3339(),
                        }),
                        detail,
                    )
                }
                Err(err) => (None, format!("NAS 目标文件存在但无法读取校验：{err}")),
            },
        };

        pb::NasCheckResult {
            node_id: self.node_id.to_string(),
            task_id: verify.task_id,
            mount_present: health.mount_present,
            writable: health.writable,
            free_gb: health.reported_free_gb(),
            latency_ms: health.latency_ms,
            file_exists: file.is_some(),
            file,
            detail: format!("{detail}（挂载点：{}）", health.detail),
        }
    }
}

/// 读取文件前 1KB 识别真实格式（魔数 + 扩展名）。
async fn detect_format_from_file(path: &std::path::Path) -> String {
    use tokio::io::AsyncReadExt;
    if let Ok(mut file) = tokio::fs::File::open(path).await {
        let mut magic = [0u8; 1024];
        if let Ok(n) = file.read(&mut magic).await {
            if n >= 4 && &magic[0..4] == b"%PDF" {
                return "pdf".to_string();
            }
            if n >= 4 && &magic[0..2] == b"PK" {
                // ZIP 容器（EPUB）
                let content_str = String::from_utf8_lossy(&magic[..n]);
                if content_str.contains("mimetype") || content_str.contains("epub") {
                    return "epub".to_string();
                }
            }
        }
    }

    // 回退到扩展名推导
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        ext.to_lowercase()
    } else {
        "pdf".to_string()
    }
}

fn wrap(prefix: &str, payload: pb::worker_message::Payload) -> pb::WorkerMessage {
    pb::WorkerMessage {
        event_id: volatile_id(prefix),
        sent_at: Utc::now().to_rfc3339(),
        replayed: false,
        payload: Some(payload),
    }
}
