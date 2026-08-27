//! 执行槽位与会话生命周期（第 6 节、第 8 节、第 10 节）。
//!
//! 一个槽位是一条长期协程 + 一份共享状态。命令从 `mpsc` 通道进来，
//! 但**取消不走通道**：槽位在等真实下载时会长时间停在 `download_book` 里，
//! 那一刻没有人在读通道。V2 第 3.6 节记录的「取消命令无法打断任务」就是
//! 这个结构造成的，所以取消改成写共享状态里的 [`CancelToken`]，
//! 由自动化引擎在每个等待点自己发现。
//!
//! 三条本模块必须守住的规则：
//!
//! 1. **站点地址只来自 Master**（第 3.3 节）。快照里没有可用地址时，
//!    会话直接以「失败」收场并说明原因，绝不退回占位域名。
//! 2. **先切下载目录，再点下载**（第 8.2 节）。目录是任务独占的，
//!    切换失败就不开始——否则文件会落进公共 staging 根目录，
//!    多槽位并发时再也分不清哪个文件属于哪本书。
//! 3. **阶段版本原样回带**（第 3.2 节）。`stage_version` 由 Master 在
//!    领取事务里决定，Worker 写死 1/2 会让每个正常结果都被判为过期。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use automation_core::{
    AccountCredential, AutomationEngine, AutomationEvent, BookTarget, CancelToken, DownloadSpec,
    RealAutomationEngine, SessionHandle, SessionSpec, SimulatedEngine,
};
use platform_domain::{ExecutionResult, SessionStatus, SlotStatus};
use platform_proto::v1 as pb;
use tokio::sync::{mpsc, RwLock};

use crate::bus::{volatile_id, OutboundEventBus};
use crate::dynamic::{ConfigState, DynamicConfig};
use crate::outbox::{ExecutionState, LocalStore};
use crate::proxy_forward::LocalProxyServer;
use crate::storage;
use platform_proto::v1::ExecutionStage;
use uuid::Uuid;

/// 执行阶段技术枚举常量（V4 方案第 10.1 节）。
///
/// 裁决与现场记录一律使用该枚举，禁止自由字符串跨组件比较；
/// 中文展示值由 [`ExecutionStage::display_name`] 唯一映射。
pub mod stage {
    use platform_proto::v1::ExecutionStage;

    /// 已收到任务分配。
    pub const ACCEPTED: ExecutionStage = ExecutionStage::Accepted;
    /// 正在搜索目标图书。
    pub const SEARCHING: ExecutionStage = ExecutionStage::Searching;
    /// 正在下载。
    pub const DOWNLOADING: ExecutionStage = ExecutionStage::Downloading;
    /// 本机文件已完成并通过校验。
    pub const LOCAL_DONE: ExecutionStage = ExecutionStage::LocalFileReady;
    /// 正在写入 NAS。
    pub const UPLOADING: ExecutionStage = ExecutionStage::NasUploading;
    /// 已在 NAS 上原子落盘。
    pub const NAS_COMMITTED: ExecutionStage = ExecutionStage::NasCommitted;
    /// 结果已生成，等待 Master 确认。
    pub const RESULT_PENDING: ExecutionStage = ExecutionStage::ResultPending;

    /// 把自动化引擎报出的中文阶段文本映射为技术枚举。
    ///
    /// 引擎（rust_drission）的输出是外部边界，允许中文字符串进来，
    /// 但**一旦进入现场记录就立即转成枚举**，不再以字符串参与裁决。
    pub fn from_display(text: &str) -> Option<ExecutionStage> {
        match text.trim() {
            "搜索中" | "已接受" => Some(ExecutionStage::Searching),
            "下载中" => Some(ExecutionStage::Downloading),
            "本地文件完成" => Some(ExecutionStage::LocalFileReady),
            "NAS 上传中" | "NAS上传中" => Some(ExecutionStage::NasUploading),
            "NAS 已原子落盘" | "NAS已原子落盘" => Some(ExecutionStage::NasCommitted),
            "结果待上报" => Some(ExecutionStage::ResultPending),
            _ => None,
        }
    }
}

/// 槽位命令。取消与结束会话另有直达通路，见 [`SlotRuntime`]。
#[derive(Debug)]
pub enum SlotCommand {
    /// 建立会话（账号 + 固定代理 + Profile）。
    CreateSession(pb::CreateSession),
    /// 在会话内执行一本书。
    AssignTask(pb::AssignTask),
    /// 在会话内执行一个账号注册。
    AssignRegistrationTask(pb::AssignRegistrationTask),
    /// 人工确认继续。
    ContinueManualAction(pb::ContinueManualAction),
    /// 停用一个空闲槽位。
    Pause,
    /// 恢复一个已停用的槽位。
    Resume,
}

/// 槽位状态快照。
#[derive(Debug, Clone)]
pub struct SlotSnapshot {
    /// 槽位序号。
    pub slot_index: u32,
    /// 中文槽位状态。
    pub status: SlotStatus,
    /// 当前会话编号。
    pub active_session_id: Option<String>,
    /// 当前任务编号。
    pub current_task_id: Option<String>,
    /// 当前执行编号。
    pub current_execution_id: Option<String>,
    /// Master 下发的阶段版本，原样回报。
    pub stage_version: u32,
    /// 技术执行阶段（裁决依据，V4 第 10.1 节）。
    pub stage_enum: ExecutionStage,
    /// 中文执行阶段（展示用，派生自 stage_enum）。
    pub stage: String,
    /// 中文明细，供后台展示。
    pub detail: String,
}

/// 正在执行的任务及其取消入口。
struct ActiveTask {
    session_id: String,
    task_id: String,
    execution_id: String,
    stage_version: u32,
    cancel: CancelToken,
}

/// 单个槽位运行时句柄。
pub struct SlotRuntime {
    /// 槽位序号。
    pub index: u32,
    /// 对外可读的状态快照。
    pub snapshot: Arc<RwLock<SlotSnapshot>>,
    /// 命令通道。
    pub command_tx: mpsc::Sender<SlotCommand>,
    /// 当前任务的取消入口。取消命令绕过命令通道直接写它。
    current: Arc<Mutex<Option<ActiveTask>>>,
    /// 「本任务结束后退出会话」的原因；`None` 表示继续领书。
    end_after_task: Arc<Mutex<Option<String>>>,
    /// 管理员是否已停用该槽位。
    paused: Arc<AtomicBool>,
}

impl SlotRuntime {
    fn shared(
        &self,
        node_id: String,
        mail_provider_state: Arc<RwLock<MailProviderState>>,
    ) -> SlotShared {
        SlotShared {
            index: self.index,
            snapshot: self.snapshot.clone(),
            current: self.current.clone(),
            end_after_task: self.end_after_task.clone(),
            paused: self.paused.clone(),
            node_id,
            mail_provider_state,
        }
    }

    /// 若当前任务匹配则取消它，返回是否真的取消了。
    fn cancel_current(&self, matches: impl Fn(&ActiveTask) -> bool, reason: &str) -> bool {
        let guard = lock(&self.current);
        match guard.as_ref() {
            Some(task) if matches(task) => {
                task.cancel.cancel(reason.to_string());
                true
            }
            _ => false,
        }
    }
}

/// 槽位协程持有的共享状态。
#[derive(Clone)]
struct SlotShared {
    index: u32,
    snapshot: Arc<RwLock<SlotSnapshot>>,
    current: Arc<Mutex<Option<ActiveTask>>>,
    end_after_task: Arc<Mutex<Option<String>>>,
    paused: Arc<AtomicBool>,
    /// 本 Worker 的真实节点编号：NAS 临时文件名必须反映真实 Worker（P2）。
    node_id: String,
    mail_provider_state: Arc<RwLock<MailProviderState>>,
}

#[derive(Debug, Clone, Default)]
pub struct MailProviderState {
    pub version: u64,
    pub name: String,
    pub health: String,
}

impl SlotShared {
    async fn set_stage(&self, stage: ExecutionStage) {
        let mut snap = self.snapshot.write().await;
        snap.stage_enum = stage;
        snap.stage = stage.display_name().to_string();
        snap.detail = format!("{}：{}", stage.display_name(), snap.detail);
    }

    fn take_end_reason(&self) -> Option<String> {
        lock(&self.end_after_task).clone()
    }
}

fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match value.lock() {
        Ok(guard) => guard,
        // 持锁区间内没有 await、也没有 panic 点，毒化后数据仍然一致。
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// 槽位池管理器。
pub struct SlotManager {
    slots: Vec<Arc<SlotRuntime>>,
    /// 云端批准的槽位上限来自这里（第 7.4 节）。
    config_state: Arc<ConfigState>,
    mail_provider_state: Arc<RwLock<MailProviderState>>,
}

impl SlotManager {
    /// 按本地请求的槽位数拉起协程。
    ///
    /// 本地数是**上限**，实际对外可用数还要受 Master 批准的 `max_slots` 限制：
    /// 管理员把某台机器调成 1 个槽位，就不能靠 Worker 本地配置绕过去。
    pub fn new(
        requested_slots: u32,
        config: crate::config::WorkerConfig,
        config_state: Arc<ConfigState>,
        outbox: LocalStore,
        bus: OutboundEventBus,
        node_id: String,
    ) -> Self {
        let mut slots = Vec::new();
        let mail_provider_state = Arc::new(RwLock::new(MailProviderState::default()));
        for index in 0..requested_slots {
            let (tx, rx) = mpsc::channel(32);
            let runtime = Arc::new(SlotRuntime {
                index,
                snapshot: Arc::new(RwLock::new(SlotSnapshot {
                    slot_index: index,
                    status: SlotStatus::Idle,
                    active_session_id: None,
                    current_task_id: None,
                    current_execution_id: None,
                    stage_version: 0,
                    stage_enum: ExecutionStage::Unspecified,
                    stage: String::new(),
                    detail: "空闲就绪".to_string(),
                })),
                command_tx: tx,
                current: Arc::new(Mutex::new(None)),
                end_after_task: Arc::new(Mutex::new(None)),
                paused: Arc::new(AtomicBool::new(false)),
            });

            let shared = runtime.shared(node_id.clone(), mail_provider_state.clone());
            let cfg = config.clone();
            let state = config_state.clone();
            let store = outbox.clone();
            let bus = bus.clone();
            tokio::spawn(async move {
                run_slot_worker(shared, rx, cfg, state, store, bus).await;
            });

            slots.push(runtime);
        }

        Self {
            slots,
            config_state,
            mail_provider_state,
        }
    }

    pub async fn mail_provider_status(&self) -> MailProviderState {
        self.mail_provider_state.read().await.clone()
    }

    /// 云端批准数与本地槽位数的较小值。
    fn effective_limit(&self) -> u32 {
        let approved = self.config_state.snapshot().max_slots;
        approved.min(self.slots.len() as u32)
    }

    /// 当前可领取会话的空闲槽位数。
    pub async fn available_slots(&self) -> u32 {
        let limit = self.effective_limit();
        let mut count = 0;
        for slot in self.slots.iter().take(limit as usize) {
            if slot.snapshot.read().await.status == SlotStatus::Idle
                && !slot.paused.load(Ordering::SeqCst)
            {
                count += 1;
            }
        }
        count
    }

    /// 本机拉起的槽位总数。
    pub fn slot_count(&self) -> u32 {
        self.slots.len() as u32
    }

    /// 整台节点是否处于暂停态。
    ///
    /// 判据是「批准范围内的槽位全部被暂停」而不是「有任意一个被暂停」：
    /// 心跳里报「已暂停」意味着这台机器完全不接活，只要还有一个槽位能领会话，
    /// 报暂停就会让后台以为它已经停了。
    pub fn node_paused(&self) -> bool {
        let limit = self.effective_limit();
        if limit == 0 {
            // 云端还没批准任何槽位，这不是「暂停」，而是「还没配」。
            return false;
        }
        self.slots
            .iter()
            .take(limit as usize)
            .all(|slot| slot.paused.load(Ordering::SeqCst))
    }

    /// 全部槽位状态快照。
    ///
    /// 超出云端批准数的槽位一律报「已停用」：它确实还活着，
    /// 但不会被派活，报成「空闲」会让后台以为还能加任务。
    pub async fn slot_states(&self) -> Vec<pb::SlotState> {
        let limit = self.effective_limit();
        let mut states = Vec::new();
        for slot in &self.slots {
            let snap = slot.snapshot.read().await;
            let over_limit = slot.index >= limit;
            let status = if over_limit && snap.status == SlotStatus::Idle {
                SlotStatus::Deactivated
            } else {
                snap.status
            };
            let detail = if over_limit && snap.status == SlotStatus::Idle {
                "超出云端批准的槽位数，已停用".to_string()
            } else {
                snap.detail.clone()
            };
            states.push(pb::SlotState {
                slot_index: snap.slot_index,
                status: status.as_str().to_string(),
                session_id: snap.active_session_id.clone().unwrap_or_default(),
                task_id: snap.current_task_id.clone().unwrap_or_default(),
                detail,
            });
        }
        states
    }

    /// 当前活跃会话编号。
    pub async fn active_session_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        for slot in &self.slots {
            if let Some(id) = slot.snapshot.read().await.active_session_id.clone() {
                ids.push(id);
            }
        }
        ids
    }

    /// 活跃执行列表，用于心跳续租与重连对账。
    pub async fn active_executions(&self) -> Vec<pb::ActiveExecution> {
        let mut items = Vec::new();
        for slot in &self.slots {
            let snap = slot.snapshot.read().await;
            if let (Some(session_id), Some(task_id), Some(execution_id)) = (
                snap.active_session_id.clone(),
                snap.current_task_id.clone(),
                snap.current_execution_id.clone(),
            ) {
                items.push(pb::ActiveExecution {
                    session_id,
                    execution_id,
                    task_id,
                    task_status: snap.stage.clone(),
                    stage_version: snap.stage_version,
                    stage: snap.stage_enum as i32,
                    format: String::new(),
                    local_file_path: String::new(),
                    nas_relative_path: String::new(),
                    source_size_bytes: 0,
                    source_sha256: String::new(),
                    result_event_id: String::new(),
                });
            }
        }
        items
    }

    /// 下一个可用空闲槽位序号。
    pub async fn find_idle_slot(&self) -> Option<u32> {
        let limit = self.effective_limit();
        for slot in self.slots.iter().take(limit as usize) {
            if slot.snapshot.read().await.status == SlotStatus::Idle
                && !slot.paused.load(Ordering::SeqCst)
            {
                return Some(slot.index);
            }
        }
        None
    }

    /// 下发 `CreateSession`。
    pub async fn dispatch_create_session(&self, session: pb::CreateSession) -> Result<()> {
        let limit = self.effective_limit();
        if session.slot_index >= limit {
            tracing::warn!(
                slot = session.slot_index,
                limit,
                "拒绝在超出云端批准数的槽位上创建会话"
            );
            return Ok(());
        }
        match self.slots.get(session.slot_index as usize) {
            Some(slot) => {
                slot.command_tx
                    .send(SlotCommand::CreateSession(session))
                    .await?;
            }
            None => tracing::warn!(slot = session.slot_index, "本机没有该序号的槽位"),
        }
        Ok(())
    }

    /// 下发 `AssignTask` 给持有该会话的槽位。
    pub async fn dispatch_assign_task(&self, task: pb::AssignTask) -> Result<()> {
        for slot in &self.slots {
            let holds = slot.snapshot.read().await.active_session_id.as_deref()
                == Some(task.session_id.as_str());
            if holds {
                slot.command_tx.send(SlotCommand::AssignTask(task)).await?;
                return Ok(());
            }
        }
        tracing::warn!(
            session_id = %task.session_id,
            task_id = %task.task_id,
            "未找到持有该会话的槽位，任务分配已丢弃（Master 会在租约到期后回收）"
        );
        Ok(())
    }

    /// 下发 `AssignRegistrationTask` 给持有该会话的槽位。
    pub async fn dispatch_assign_registration_task(
        &self,
        task: pb::AssignRegistrationTask,
    ) -> Result<()> {
        for slot in &self.slots {
            let holds = slot.snapshot.read().await.active_session_id.as_deref()
                == Some(task.session_id.as_str());
            if holds {
                slot.command_tx
                    .send(SlotCommand::AssignRegistrationTask(task))
                    .await?;
                return Ok(());
            }
        }
        tracing::warn!(
            session_id = %task.session_id,
            registration_task_id = %task.registration_task_id,
            "未找到持有该注册会话的槽位"
        );
        Ok(())
    }

    /// 下发人工确认继续指令给槽位。
    pub async fn dispatch_continue_manual_action(
        &self,
        cont: pb::ContinueManualAction,
    ) -> Result<()> {
        for slot in &self.slots {
            let _ = slot
                .command_tx
                .try_send(SlotCommand::ContinueManualAction(cont.clone()));
        }
        Ok(())
    }

    /// 取消账号注册任务。
    pub async fn dispatch_cancel_registration_task(
        &self,
        cancel: pb::CancelRegistrationTask,
    ) -> Result<()> {
        let reason = if cancel.reason.trim().is_empty() {
            "云端取消注册任务".to_string()
        } else {
            cancel.reason.clone()
        };
        for slot in &self.slots {
            slot.cancel_current(
                |task| {
                    if task.task_id != cancel.registration_task_id
                        || task.execution_id != cancel.execution_id
                    {
                        return false;
                    }
                    if cancel.stage_version != 0 && task.stage_version != cancel.stage_version {
                        return false;
                    }
                    if !cancel.session_id.is_empty() && task.session_id != cancel.session_id {
                        return false;
                    }
                    true
                },
                &reason,
            );
        }
        Ok(())
    }

    /// 取消任务：直接触发取消令牌，不经过命令通道。
    ///
    /// V4 精确取消（第 11.7 节）：只有 task_id、execution_id、stage_version（非 0 时）、
    /// session_id（非空时）**全部匹配**当前执行才取消，旧消息不得误伤新执行。
    pub async fn dispatch_cancel_task(&self, cancel: pb::CancelTask) -> Result<()> {
        let reason = if cancel.reason.trim().is_empty() {
            "云端取消任务".to_string()
        } else {
            cancel.reason.clone()
        };
        let mut hit = false;
        for slot in &self.slots {
            hit |= slot.cancel_current(
                |task| {
                    if task.task_id != cancel.task_id || task.execution_id != cancel.execution_id {
                        return false;
                    }
                    if cancel.stage_version != 0 && task.stage_version != cancel.stage_version {
                        return false;
                    }
                    if !cancel.session_id.is_empty() && task.session_id != cancel.session_id {
                        return false;
                    }
                    true
                },
                &reason,
            );
        }
        if !hit {
            tracing::info!(
                task_id = %cancel.task_id,
                execution_id = %cancel.execution_id,
                stage_version = cancel.stage_version,
                "收到取消命令但本机没有完全匹配的进行中任务（可能已结束或已被新执行取代）"
            );
        }
        Ok(())
    }

    /// 结束会话。`finish_current_task=false` 时立即取消当前任务。
    pub async fn dispatch_end_session(&self, end: pb::EndSession) -> Result<()> {
        let reason = if end.reason.trim().is_empty() {
            "云端结束会话".to_string()
        } else {
            end.reason.clone()
        };
        for slot in &self.slots {
            let holds = slot.snapshot.read().await.active_session_id.as_deref()
                == Some(end.session_id.as_str());
            if !holds {
                continue;
            }
            *lock(&slot.end_after_task) = Some(reason.clone());
            if !end.finish_current_task {
                slot.cancel_current(|task| task.session_id == end.session_id, &reason);
            }
        }
        Ok(())
    }

    /// 停用节点：空闲槽位立即停用，忙碌槽位按 `finish_current_task` 处理。
    pub async fn dispatch_pause(&self, pause: pb::PauseNode) -> Result<()> {
        let reason = if pause.reason.trim().is_empty() {
            "管理员暂停节点".to_string()
        } else {
            pause.reason.clone()
        };
        for slot in &self.slots {
            slot.paused.store(true, Ordering::SeqCst);
            *lock(&slot.end_after_task) = Some(reason.clone());
            if !pause.finish_current_task {
                slot.cancel_current(|_| true, &reason);
            }
            let _ = slot.command_tx.try_send(SlotCommand::Pause);
        }
        Ok(())
    }

    /// 恢复节点。
    pub async fn dispatch_resume(&self) -> Result<()> {
        for slot in &self.slots {
            slot.paused.store(false, Ordering::SeqCst);
            *lock(&slot.end_after_task) = None;
            let _ = slot.command_tx.try_send(SlotCommand::Resume);
        }
        Ok(())
    }

    /// 取消本机所有进行中的任务（进程优雅退出用）。
    pub fn cancel_all(&self, reason: &str) {
        for slot in &self.slots {
            slot.cancel_current(|_| true, reason);
        }
    }
}

/// 单个槽位的长期协程。
async fn run_slot_worker(
    shared: SlotShared,
    mut rx: mpsc::Receiver<SlotCommand>,
    config: crate::config::WorkerConfig,
    config_state: Arc<ConfigState>,
    outbox: LocalStore,
    bus: OutboundEventBus,
) {
    tracing::info!(slot = shared.index, "槽位协程已启动");

    while let Some(cmd) = rx.recv().await {
        match cmd {
            SlotCommand::CreateSession(session) => {
                let session_id = session.session_id.clone();
                *lock(&shared.end_after_task) = None;
                {
                    let mut snap = shared.snapshot.write().await;
                    snap.status = SlotStatus::Starting;
                    snap.active_session_id = Some(session_id.clone());
                    snap.detail = "正在启动本地代理与浏览器会话".to_string();
                }

                let outcome = execute_session_loop(
                    &shared,
                    session,
                    &mut rx,
                    &config,
                    &config_state,
                    &outbox,
                    &bus,
                )
                .await;

                let (status, reason, completed) = match outcome {
                    Ok(done) => (
                        SessionStatus::Ended,
                        lock(&shared.end_after_task)
                            .clone()
                            .unwrap_or_else(|| "会话正常结束".to_string()),
                        done,
                    ),
                    Err(err) => (SessionStatus::Failed, format!("会话异常退出：{err}"), 0),
                };

                // 会话收尾必须可靠上报：Master 靠它释放账号、代理与槽位租约。
                bus.send_reliable(
                    &format!("evt-closed-{session_id}"),
                    pb::worker_message::Payload::SessionClosed(pb::SessionClosed {
                        session_id: session_id.clone(),
                        status: status.as_str().to_string(),
                        reason,
                        completed_count: completed,
                    }),
                )
                .await;

                {
                    let mut snap = shared.snapshot.write().await;
                    snap.status = if shared.paused.load(Ordering::SeqCst) {
                        SlotStatus::Deactivated
                    } else {
                        SlotStatus::Idle
                    };
                    snap.active_session_id = None;
                    snap.current_task_id = None;
                    snap.current_execution_id = None;
                    snap.stage_version = 0;
                    snap.stage_enum = ExecutionStage::Unspecified;
                    snap.stage = String::new();
                    snap.detail = if shared.paused.load(Ordering::SeqCst) {
                        "管理员已停用".to_string()
                    } else {
                        "空闲就绪".to_string()
                    };
                }
                *lock(&shared.current) = None;
            }
            SlotCommand::Pause => {
                let mut snap = shared.snapshot.write().await;
                if snap.status == SlotStatus::Idle {
                    snap.status = SlotStatus::Deactivated;
                    snap.detail = "管理员已停用".to_string();
                }
            }
            SlotCommand::Resume => {
                let mut snap = shared.snapshot.write().await;
                if snap.status == SlotStatus::Deactivated {
                    snap.status = SlotStatus::Idle;
                    snap.detail = "空闲就绪".to_string();
                }
            }
            SlotCommand::AssignTask(assign) => {
                // 没有会话就收到任务：只能是过期指令，明确拒绝而不是静默丢弃。
                tracing::warn!(
                    slot = shared.index,
                    task_id = %assign.task_id,
                    "槽位当前没有会话，拒绝执行任务分配"
                );
                report_result(
                    &bus,
                    &assign,
                    ExecutionResult::RetryableFailure,
                    "失败",
                    "槽位当前没有活跃会话，无法执行（可能是过期的任务分配）",
                    None,
                    None,
                )
                .await;
            }
            SlotCommand::AssignRegistrationTask(assign) => {
                tracing::warn!(
                    slot = shared.index,
                    registration_task_id = %assign.registration_task_id,
                    "槽位当前没有会话，拒绝执行注册任务分配"
                );
            }
            SlotCommand::ContinueManualAction(_) => {}
        }
    }
}

/// 会话生命周期入口：按任务类型严格分流。
async fn execute_session_loop(
    shared: &SlotShared,
    session: pb::CreateSession,
    rx: &mut mpsc::Receiver<SlotCommand>,
    config: &crate::config::WorkerConfig,
    config_state: &Arc<ConfigState>,
    outbox: &LocalStore,
    bus: &OutboundEventBus,
) -> Result<u32> {
    match session.task_type.as_str() {
        "图书下载" => {
            execute_download_session(shared, session, rx, config, config_state, outbox, bus).await
        }
        "账号注册" => {
            execute_registration_session(shared, session, rx, config, config_state, outbox, bus)
                .await
        }
        other => {
            anyhow::bail!("不支持的任务类型：{other}");
        }
    }
}

/// 图书下载会话：建立会话（登录） → 连续领书 → 入库 → 收尾。
async fn execute_download_session(
    shared: &SlotShared,
    session: pb::CreateSession,
    rx: &mut mpsc::Receiver<SlotCommand>,
    config: &crate::config::WorkerConfig,
    config_state: &Arc<ConfigState>,
    outbox: &LocalStore,
    bus: &OutboundEventBus,
) -> Result<u32> {
    let snapshot_cfg = config_state.snapshot();
    let session_id = session.session_id.clone();

    // 第 3.3 节：站点地址只能来自 Master。拿不到就不开浏览器。
    let site_base = snapshot_cfg
        .require_site_base()
        .map_err(|err| anyhow::anyhow!("{err}"))?;

    let local_port = if session.local_forward_port > 0 {
        session.local_forward_port as u16
    } else {
        18000 + shared.index as u16
    };

    // 1. 固定代理转发：整个会话共用一个出口 IP（第 6.1 节）
    let mut proxy_server = match session.proxy {
        Some(proxy) => Some(LocalProxyServer::spawn(local_port, proxy).await?),
        None => None,
    };
    let proxy_endpoint = proxy_server
        .as_ref()
        .map(|server| format!("127.0.0.1:{}", server.port()));

    let account = session
        .account
        .clone()
        .ok_or_else(|| anyhow::anyhow!("会话缺少账号凭据，无法登录站点"))?;

    let spec = SessionSpec {
        session_id: session_id.clone(),
        site_base: site_base.clone(),
        browser_path: None,
        headless: config.execution.headless,
        profile_dir: config
            .storage
            .data_dir
            .join(format!("profiles/session-{session_id}")),
        staging_root: config.storage.data_dir.join("staging"),
        proxy_endpoint,
        account: AccountCredential {
            account_id: account.account_id,
            email: account.email,
            password: account.password,
            nickname: account.nickname,
            daily_used: account.daily_used,
            daily_limit: account.daily_limit,
        },
        download_format: snapshot_cfg.download_format.clone(),
        auto_login: true,
        max_duration: Duration::from_secs(
            session
                .max_duration_secs
                .max(60)
                .min(snapshot_cfg.max_session_duration_secs.max(60)) as u64,
        ),
    };

    // 2. 引擎：模拟引擎只用于验证平台自身，真实验收必须走真实引擎（第 18 节）
    let engine: Arc<dyn AutomationEngine> = if config.execution.simulated {
        tracing::warn!(session_id = %session_id, "本会话使用模拟引擎，结果不可用于生产验收");
        Arc::new(SimulatedEngine::with_defaults())
    } else {
        Arc::new(RealAutomationEngine::new())
    };

    let session_handle = engine
        .open_session(&spec)
        .await
        .map_err(|err| anyhow::anyhow!("{err}"))?;

    // 3. SessionReady：Master 收到才把会话置为运行中
    bus.send_volatile(
        volatile_id("ready"),
        pb::worker_message::Payload::SessionReady(pb::SessionReady {
            session_id: session_id.clone(),
            slot_index: shared.index,
            exit_ip: String::new(),
        }),
    );
    {
        let mut snap = shared.snapshot.write().await;
        snap.status = SlotStatus::Running;
        snap.detail = "会话就绪，正在申请任务".to_string();
    }

    let deadline = Instant::now() + spec.max_duration;
    let mut completed = 0u32;
    let max_downloads = session.max_downloads.max(1);

    // 4. 会话内连续领书
    while completed < max_downloads {
        if let Some(reason) = shared.take_end_reason() {
            tracing::info!(session_id = %session_id, %reason, "按云端要求结束会话");
            break;
        }
        if Instant::now() >= deadline {
            *lock(&shared.end_after_task) = Some("会话已达最大时长".to_string());
            break;
        }

        bus.send_volatile(
            volatile_id("req"),
            pb::worker_message::Payload::NextTaskRequest(pb::NextTaskRequest {
                node_id: String::new(),
                session_id: session_id.clone(),
                completed_in_session: completed,
            }),
        );

        // 等待 Master 派书。收到别的命令（例如新的 CreateSession）说明状态已经乱了，
        // 结束本会话让 Master 重新编排比猜测更安全。
        let assign = tokio::select! {
            cmd = rx.recv() => match cmd {
                Some(SlotCommand::AssignTask(assign)) => Some(assign),
                Some(SlotCommand::Pause) => {
                    shared.paused.store(true, Ordering::SeqCst);
                    None
                }
                Some(other) => {
                    tracing::warn!(slot = shared.index, ?other, "会话进行中收到非任务命令，结束会话");
                    None
                }
                None => None,
            },
            _ = tokio::time::sleep(Duration::from_secs(30)) => continue,
        };

        let Some(assign) = assign else { break };

        let succeeded = execute_assigned_task(
            shared,
            &assign,
            &session_handle,
            engine.clone(),
            config,
            &snapshot_cfg,
            outbox,
            bus,
        )
        .await;

        if succeeded {
            completed += 1;
        }
    }

    // 5. 收尾：先停代理再关浏览器会与「浏览器还在用代理」竞争，顺序反过来
    let _ = engine.close_session(&session_handle).await;
    if let Some(mut server) = proxy_server.take() {
        server.stop();
    }

    Ok(completed)
}

/// 执行一本书：下载 → 校验 → NAS 原子入库 → 上报。返回是否算作成功完成。
#[allow(clippy::too_many_arguments)]
async fn execute_assigned_task(
    shared: &SlotShared,
    assign: &pb::AssignTask,
    session_handle: &SessionHandle,
    engine: Arc<dyn AutomationEngine>,
    config: &crate::config::WorkerConfig,
    dynamic: &DynamicConfig,
    outbox: &LocalStore,
    bus: &OutboundEventBus,
) -> bool {
    let task_id = assign.task_id.clone();
    let execution_id = assign.execution_id.clone();
    let session_id = assign.session_id.clone();
    let started = Instant::now();

    let Some(book) = assign.book.clone() else {
        report_result(
            bus,
            assign,
            ExecutionResult::FatalFailure,
            "失败",
            "任务分配缺少图书信息，无法执行",
            None,
            None,
        )
        .await;
        return false;
    };

    let staging_dir = config
        .storage
        .data_dir
        .join(format!("staging/task-{task_id}"));

    // 取消令牌先挂上再干活：先干活会留下一个「取消命令找不到目标」的窗口。
    let cancel = CancelToken::new();
    *lock(&shared.current) = Some(ActiveTask {
        session_id: session_id.clone(),
        task_id: task_id.clone(),
        execution_id: execution_id.clone(),
        stage_version: assign.stage_version,
        cancel: cancel.clone(),
    });

    {
        let mut snap = shared.snapshot.write().await;
        snap.current_task_id = Some(task_id.clone());
        snap.current_execution_id = Some(execution_id.clone());
        // 第 3.2 节：阶段版本由 Master 决定，Worker 只回带
        snap.stage_version = assign.stage_version;
        snap.stage_enum = stage::ACCEPTED;
        snap.stage = stage::ACCEPTED.display_name().to_string();
        snap.detail = format!("《{}》", book.title);
    }

    let file_format = if book.format.trim().is_empty() {
        dynamic.download_format.clone()
    } else {
        book.format.trim().to_ascii_lowercase()
    };
    let mut state = ExecutionState {
        slot_index: shared.index,
        session_id: session_id.clone(),
        task_id: task_id.clone(),
        execution_id: execution_id.clone(),
        stage_version: assign.stage_version,
        stage: stage::ACCEPTED,
        task_status: stage::ACCEPTED.display_name().to_string(),
        staging_dir: staging_dir.display().to_string(),
        nas_relative_path: assign.nas_relative_path.clone(),
        source_sha256: String::new(),
        format: file_format.clone(),
        local_file_path: String::new(),
        source_size_bytes: 0,
        result_event_id: String::new(),
        node_id: String::new(),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Err(err) = outbox.upsert_execution(&state) {
        tracing::error!(execution_id = %execution_id, error = %err, "写入本地执行现场失败");
    }

    bus.send_reliable(
        &format!("evt-acc-{execution_id}"),
        pb::worker_message::Payload::TaskAccepted(pb::TaskAccepted {
            session_id: session_id.clone(),
            execution_id: execution_id.clone(),
            task_id: task_id.clone(),
            accepted_at: chrono::Utc::now().to_rfc3339(),
        }),
    )
    .await;

    let download_spec = DownloadSpec {
        execution_id: execution_id.clone(),
        task_id: task_id.clone(),
        book: BookTarget {
            book_id: book.book_id.clone(),
            book_seq: book.book_seq,
            title: book.title.clone(),
            author: optional(&book.author),
            publisher: optional(&book.publisher),
            isbn: optional(&book.isbn),
            format: if book.format.trim().is_empty() {
                dynamic.download_format.clone()
            } else {
                book.format.trim().to_ascii_lowercase()
            },
        },
        staging_dir: staging_dir.clone(),
        stall_timeout: Duration::from_secs(
            assign
                .stall_timeout_secs
                .max(30)
                .min(dynamic.stall_timeout_secs.max(30)) as u64,
        ),
        minimum_size_bytes: dynamic.minimum_file_bytes,
        attempt: assign.attempt,
    };
    let file_format = download_spec.book.format.clone();

    // 进度与阶段转发协程：节流参数由 Master 下发（第 13.3 节）
    let (event_tx, event_rx) = mpsc::channel(32);
    let event_sink = automation_core::EventSink::new(event_tx);
    let forwarder = tokio::spawn(forward_events(
        event_rx,
        bus.clone(),
        shared.clone(),
        outbox.clone(),
        ProgressContext {
            session_id: session_id.clone(),
            execution_id: execution_id.clone(),
            task_id: task_id.clone(),
            stage_version: assign.stage_version,
            min_interval: Duration::from_secs(dynamic.progress_min_interval_secs.max(1) as u64),
            min_bytes: dynamic.progress_min_bytes.max(1),
        },
    ));

    // 第 8.2 节：先把浏览器下载目录切到本任务独占目录，再点下载。
    // 目录由 Worker 建：引擎只负责「切过去」，建目录失败必须在下载前暴露，
    // 否则文件会落回公共 staging 根目录，多槽位并发时归属再也分不清。
    let download_res = match tokio::fs::create_dir_all(&staging_dir).await {
        Ok(()) => match engine
            .set_task_download_dir(session_handle, &staging_dir)
            .await
        {
            Ok(()) => {
                engine
                    .download_book(session_handle, &download_spec, &event_sink, &cancel)
                    .await
            }
            Err(err) => Err(err),
        },
        Err(err) => Err(automation_core::AutomationError::new(
            platform_domain::FailureClass::Fatal,
            format!("创建任务暂存目录失败：{}：{err}", staging_dir.display()),
        )),
    };

    drop(event_sink);
    let _ = forwarder.await;

    let outcome = match download_res {
        Ok(outcome) => outcome,
        Err(err) => {
            // 失败时补读一次站点配额指示器：只有「已用 >= 总额」才允许把责任归到账号上，
            // 读不到就报 0，Master 会因此把限流归给出口 IP 而不是账号（第 10.3 节）。
            let quota = engine
                .read_quota_indicator(session_handle)
                .await
                .ok()
                .flatten();
            let (result, task_status) = classify(&err.class);
            report_result(bus, assign, result, &task_status, &err.reason, quota, None).await;
            finish_task(shared, outbox, &execution_id, state.stage).await;
            return false;
        }
    };

    // 本地文件已通过引擎校验（扩展名/书名/大小/签名 + SHA-256）
    state.stage = stage::LOCAL_DONE;
    state.task_status = stage::LOCAL_DONE.display_name().to_string();
    state.source_sha256 = outcome
        .evidence
        .as_ref()
        .map(|e| e.sha256.clone())
        .unwrap_or_default();
    if let Some(evidence) = &outcome.evidence {
        state.local_file_path = outcome.staged_file.display().to_string();
        state.source_size_bytes = evidence.size_bytes as i64;
    }
    let _ = outbox.upsert_execution(&state);
    shared.set_stage(stage::UPLOADING).await;
    let _ = outbox.set_execution_stage(&execution_id, stage::UPLOADING);

    let task_uuid = Uuid::parse_str(&assign.task_id).unwrap_or_else(|_| Uuid::new_v4());
    let exec_uuid = Uuid::parse_str(&assign.execution_id).unwrap_or_else(|_| Uuid::new_v4());
    // P2：NAS 临时文件名必须反映真实 Worker，而不是每次随机生成的 UUID
    let node_uuid = Uuid::parse_str(&shared.node_id).unwrap_or_else(|_| Uuid::new_v4());

    let ingest = storage::ingest_file(
        &config.storage,
        &outcome.staged_file,
        &assign.nas_relative_path,
        task_uuid,
        exec_uuid,
        node_uuid,
        dynamic.minimum_file_bytes,
    )
    .await;

    let duration_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    match ingest {
        Ok(storage::IngestOutcome::Success(result))
        | Ok(storage::IngestOutcome::AlreadyExistsSameHash(result)) => {
            let _ = outbox.set_execution_stage(&execution_id, stage::NAS_COMMITTED);
            // 引擎与 NAS 两侧各自算过一遍哈希，不一致说明中途被改动过。
            if let Some(evidence) = &outcome.evidence {
                if evidence.sha256 != result.sha256 {
                    let reason = format!(
                        "本机校验哈希与 NAS 落盘哈希不一致（{} vs {}），拒绝记为完成",
                        evidence.sha256, result.sha256
                    );
                    report_result(
                        bus,
                        assign,
                        ExecutionResult::Uncertain,
                        "待确认",
                        &reason,
                        outcome.quota_indicator,
                        None,
                    )
                    .await;
                    finish_task(shared, outbox, &execution_id, stage::NAS_COMMITTED).await;
                    return false;
                }
            }

            let evidence = pb::FileEvidence {
                nas_relative_path: result.nas_relative_path,
                file_name: result.file_name,
                size_bytes: result.size_bytes,
                sha256: result.sha256,
                format: file_format,
                ingested_at: chrono::Utc::now().to_rfc3339(),
            };
            report_result_with_duration(
                bus,
                assign,
                ExecutionResult::Success,
                "已完成",
                "下载并原子入库成功",
                outcome.quota_indicator,
                Some(evidence),
                duration_ms,
            )
            .await;
            // 记录结果事件编号：重启后对账按此编号定向重放，不笼统重放前 N 条
            let _ = outbox
                .set_execution_result_event(&execution_id, &format!("evt-res-{execution_id}"));
            finish_task(shared, outbox, &execution_id, stage::RESULT_PENDING).await;
            true
        }
        Ok(storage::IngestOutcome::ConflictDifferentHash {
            existing_sha256,
            local_sha256,
            final_path,
        }) => {
            let reason = format!(
                "NAS 上已存在同名文件但哈希不一致（已有 {} vs 本次 {}），禁止覆盖：{}",
                existing_sha256, local_sha256, final_path
            );
            report_result_with_duration(
                bus,
                assign,
                ExecutionResult::Uncertain,
                "待确认",
                &reason,
                outcome.quota_indicator,
                // R6：不确定结果携带本地文件证据，Master 在同一事务固化期望（第 12.2 节）
                local_evidence(assign, &outcome),
                duration_ms,
            )
            .await;
            finish_task(shared, outbox, &execution_id, stage::UPLOADING).await;
            false
        }
        Err(err) => {
            // NAS 侧失败一律「待确认」而不是「可重试」：文件可能已经落盘一半，
            // 直接重试会在 NAS 上留下第二个同名候选，而那正是第 11 节要防的。
            let reason = format!("NAS 入库未完成：{err}");
            report_result_with_duration(
                bus,
                assign,
                ExecutionResult::Uncertain,
                "待确认",
                &reason,
                outcome.quota_indicator,
                local_evidence(assign, &outcome),
                duration_ms,
            )
            .await;
            finish_task(shared, outbox, &execution_id, stage::UPLOADING).await;
            false
        }
    }
}

/// 从本地校验证据构造 NAS 文件证据（不确定结果携带，供 Master 固化期望）。
fn local_evidence(
    assign: &pb::AssignTask,
    outcome: &automation_core::DownloadOutcome,
) -> Option<pb::FileEvidence> {
    let evidence = outcome.evidence.as_ref()?;
    Some(pb::FileEvidence {
        nas_relative_path: assign.nas_relative_path.clone(),
        file_name: evidence.file_name.clone(),
        size_bytes: evidence.size_bytes,
        sha256: evidence.sha256.clone(),
        format: evidence.format.clone(),
        ingested_at: chrono::Utc::now().to_rfc3339(),
    })
}

/// 空字符串转 `None`：proto3 没有可选字符串，`Some("")` 会被下游当成真值。
fn optional(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// 账号注册会话生命周期：准备浏览器（不自动登录） → 接收注册任务 → 自动化注册 → 上报结果 → 释放会话。
async fn execute_registration_session(
    shared: &SlotShared,
    session: pb::CreateSession,
    rx: &mut mpsc::Receiver<SlotCommand>,
    config: &crate::config::WorkerConfig,
    config_state: &Arc<ConfigState>,
    _outbox: &LocalStore,
    bus: &OutboundEventBus,
) -> Result<u32> {
    let snapshot_cfg = config_state.snapshot();
    let session_id = session.session_id.clone();

    let site_base = snapshot_cfg
        .require_site_base()
        .map_err(|err| anyhow::anyhow!("{err}"))?;

    let local_port = if session.local_forward_port > 0 {
        session.local_forward_port as u16
    } else {
        18000 + shared.index as u16
    };

    let proxy_server = match session.proxy {
        Some(proxy) => Some(LocalProxyServer::spawn(local_port, proxy).await?),
        None => None,
    };
    let proxy_endpoint = proxy_server
        .as_ref()
        .map(|server| format!("127.0.0.1:{}", server.port()));

    let account = session
        .account
        .clone()
        .ok_or_else(|| anyhow::anyhow!("注册会话缺少待注册账号凭据"))?;

    let spec = SessionSpec {
        session_id: session_id.clone(),
        site_base: site_base.clone(),
        browser_path: None,
        headless: config.execution.headless,
        profile_dir: config
            .storage
            .data_dir
            .join(format!("profiles/session-{session_id}")),
        staging_root: config.storage.data_dir.join("staging"),
        proxy_endpoint,
        account: AccountCredential {
            account_id: account.account_id.clone(),
            email: account.email.clone(),
            password: account.password.clone(),
            nickname: account.nickname.clone(),
            daily_used: account.daily_used,
            daily_limit: account.daily_limit,
        },
        download_format: snapshot_cfg.download_format.clone(),
        auto_login: false, // 账号注册严禁先调用下载登录！
        max_duration: Duration::from_secs(
            session
                .max_duration_secs
                .max(60)
                .min(snapshot_cfg.max_session_duration_secs.max(60)) as u64,
        ),
    };

    let engine: Arc<dyn AutomationEngine> = if config.execution.simulated {
        tracing::warn!(session_id = %session_id, "本账号注册会话使用模拟引擎");
        Arc::new(SimulatedEngine::with_defaults())
    } else {
        Arc::new(RealAutomationEngine::new())
    };

    let session_handle = engine
        .open_session(&spec)
        .await
        .map_err(|err| anyhow::anyhow!("{err}"))?;

    // 上报 SessionReady
    bus.send_volatile(
        volatile_id("ready"),
        pb::worker_message::Payload::SessionReady(pb::SessionReady {
            session_id: session_id.clone(),
            slot_index: shared.index,
            exit_ip: String::new(),
        }),
    );

    {
        let mut snap = shared.snapshot.write().await;
        snap.status = SlotStatus::Running;
        snap.detail = "注册会话就绪，等待下发注册任务".to_string();
    }

    // 等待 Master 下发 AssignRegistrationTask
    let assign = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                SlotCommand::AssignRegistrationTask(assign) => return Some(assign),
                SlotCommand::Pause => {
                    shared.paused.store(true, Ordering::SeqCst);
                }
                SlotCommand::Resume => {
                    shared.paused.store(false, Ordering::SeqCst);
                }
                _ => {}
            }
        }
        None
    })
    .await
    .map_err(|_| anyhow::anyhow!("等待 Master 下发注册任务超时"))?
    .ok_or_else(|| anyhow::anyhow!("未收到注册任务指令"))?;

    let reg_task_id = assign.registration_task_id.clone();
    let exec_id = assign.execution_id.clone();
    let stage_version = assign.stage_version;

    // 可靠确认任务已接受
    let accepted_event_id = format!("evt-reg-acc-{session_id}-{reg_task_id}");
    bus.send_reliable(
        &accepted_event_id,
        pb::worker_message::Payload::RegistrationTaskAccepted(pb::RegistrationTaskAccepted {
            session_id: session_id.clone(),
            execution_id: exec_id.clone(),
            registration_task_id: reg_task_id.clone(),
            stage_version,
            attempt: assign.attempt,
            accepted_at: chrono::Utc::now().to_rfc3339(),
        }),
    )
    .await;

    {
        let mut snap = shared.snapshot.write().await;
        snap.current_task_id = Some(reg_task_id.clone());
        snap.current_execution_id = Some(exec_id.clone());
        snap.stage_version = stage_version;
        snap.stage = "注册中".to_string();
        snap.detail = format!("正在执行账号 {} 注册", account.email);
    }

    let (event_tx, mut event_rx) = mpsc::channel(32);
    let sink = automation_core::EventSink::new(event_tx);
    // 注册取消必须在开始浏览器自动化前挂到共享现场，人工等待与 Outlook 轮询
    // 共用同一个令牌，云端取消不会留下悬挂会话。
    let registration_cancel = CancelToken::new();
    *lock(&shared.current) = Some(ActiveTask {
        session_id: session_id.clone(),
        task_id: reg_task_id.clone(),
        execution_id: exec_id.clone(),
        stage_version,
        cancel: registration_cancel.clone(),
    });

    let (manual_adapter, mut manual_requests, manual_submissions) =
        crate::mail::manual::ManualMailCodeAdapter::channel();
    let manual_provider: Arc<dyn automation_core::mail_code::MailCodeProvider> =
        Arc::new(manual_adapter);
    let mail_provider = if assign.needs_mail_code {
        let lease = assign
            .mail_provider
            .clone()
            .unwrap_or(pb::MailProviderLease {
                version: 0,
                provider_type: "manual".to_string(),
                endpoint: String::new(),
                api_key: String::new(),
                poll_interval_secs: 5,
                timeout_secs: 120,
                allowed_hosts: Vec::new(),
                allowed_senders: Vec::new(),
            });
        let configured_provider_type = lease.provider_type.clone();
        let mut applied_provider_name = configured_provider_type.clone();
        let mut provider_health = "已应用".to_string();
        let primary: Arc<dyn automation_core::mail_code::MailCodeProvider> =
            match configured_provider_type.as_str() {
                "outlook_http" => {
                    let outlook = crate::mail::outlook_http::OutlookHttpMailCodeAdapter::new(
                        crate::mail::outlook_http::OutlookConfig {
                            endpoint: lease.endpoint,
                            api_key: lease.api_key,
                            poll_interval: Duration::from_secs(
                                lease.poll_interval_secs.clamp(1, 60) as u64,
                            ),
                            timeout: Duration::from_secs(lease.timeout_secs.clamp(10, 300) as u64),
                            allowed_hosts: lease.allowed_hosts,
                            allowed_senders: lease.allowed_senders,
                        },
                    );
                    match outlook {
                        Ok(provider) => Arc::new(provider),
                        Err(_) => {
                            applied_provider_name = "manual".to_string();
                            provider_health = "Outlook 配置无效，已人工降级".to_string();
                            manual_provider.clone()
                        }
                    }
                }
                "mock" if config.execution.simulated && cfg!(debug_assertions) => {
                    match crate::mail::mock::MockMailCodeAdapter::new(false, "123456") {
                        Ok(provider) => Arc::new(provider),
                        Err(_) => manual_provider.clone(),
                    }
                }
                "manual" => {
                    provider_health = "人工模式已应用".to_string();
                    manual_provider.clone()
                }
                _ => {
                    applied_provider_name = "manual".to_string();
                    provider_health = "Provider 类型不可用，已人工降级".to_string();
                    manual_provider.clone()
                }
            };
        {
            let mut state = shared.mail_provider_state.write().await;
            state.version = lease.version;
            state.name = applied_provider_name;
            state.health = provider_health;
        }
        Some(Arc::new(crate::mail::MailCodeRouter::new_attempt(
            lease.version,
            primary,
            manual_provider,
        ))
            as Arc<dyn automation_core::mail_code::MailCodeProvider>)
    } else {
        None
    };

    let reg_spec = automation_core::RegistrationSpec {
        execution_id: exec_id.clone(),
        account: spec.account.clone(),
        needs_mail_code: assign.needs_mail_code,
        mail_provider,
        cancel: registration_cancel.clone(),
    };

    let bus_clone = bus.clone();
    let session_id_clone = session_id.clone();
    let exec_id_clone = exec_id.clone();
    let reg_task_id_clone = reg_task_id.clone();

    tokio::spawn(async move {
        while let Some(evt) = event_rx.recv().await {
            if let AutomationEvent::Stage { stage } = evt {
                bus_clone.send_volatile(
                    volatile_id("reg-prog"),
                    pb::worker_message::Payload::RegistrationTaskProgress(
                        pb::RegistrationTaskProgress {
                            session_id: session_id_clone.clone(),
                            execution_id: exec_id_clone.clone(),
                            registration_task_id: reg_task_id_clone.clone(),
                            stage_version,
                            stage,
                            percent: 50,
                            message: String::new(),
                            updated_at: chrono::Utc::now().to_rfc3339(),
                        },
                    ),
                );
            }
        }
    });

    // 一边运行浏览器注册，一边接收人工事项请求与回传验证码。这样人工降级
    // 不会关闭浏览器，也不会另起一次注册尝试。
    let register_future = engine.register_account(&session_handle, &reg_spec, &sink);
    tokio::pin!(register_future);
    let mut active_action_id: Option<String> = None;
    let mut command_channel_open = true;
    let reg_result = loop {
        tokio::select! {
            result = &mut register_future => break result,
            request = manual_requests.recv() => {
                let Some(request) = request else { continue; };
                active_action_id = Some(request.action_id.clone());
                {
                    let mut state = shared.mail_provider_state.write().await;
                    state.name = "manual".to_string();
                    state.health = "自动取码不可用，已人工降级".to_string();
                }
                bus.send_volatile(
                    volatile_id("reg-manual"),
                    pb::worker_message::Payload::RegistrationTaskProgress(
                        pb::RegistrationTaskProgress {
                            session_id: session_id.clone(),
                            execution_id: exec_id.clone(),
                            registration_task_id: reg_task_id.clone(),
                            stage_version,
                            stage: "人工降级处理中".to_string(),
                            percent: 50,
                            message: "等待管理员输入邮箱验证码".to_string(),
                            updated_at: chrono::Utc::now().to_rfc3339(),
                        },
                    ),
                );
                let expires_at = chrono::Utc::now() + chrono::Duration::minutes(10);
                bus.send_reliable(
                    &format!("evt-manual-{}", request.action_id),
                    pb::worker_message::Payload::ManualActionRequired(pb::ManualActionRequired {
                        action_id: request.action_id,
                        task_type: "账号注册".to_string(),
                        registration_task_id: reg_task_id.clone(),
                        execution_id: exec_id.clone(),
                        action_type: "邮箱验证码".to_string(),
                        prompt: "自动取码不可用，请输入本次注册收到的 4–8 位邮箱验证码".to_string(),
                        expires_at: expires_at.to_rfc3339(),
                        optional_artifact_id: String::new(),
                    }),
                ).await;
            }
            command = rx.recv(), if command_channel_open => {
                match command {
                    Some(SlotCommand::ContinueManualAction(cont))
                        if cont.execution_id == exec_id
                            && active_action_id.as_deref() == Some(cont.action_id.as_str()) =>
                    {
                        // 验证码仅移动到当前 Adapter 的内存通道，不记录、不打印。
                        let _ = manual_submissions.send(
                            crate::mail::manual::ManualCodeSubmission {
                                action_id: cont.action_id,
                                code: cont.action_payload,
                            },
                        );
                    }
                    Some(SlotCommand::Pause) => shared.paused.store(true, Ordering::SeqCst),
                    Some(SlotCommand::Resume) => shared.paused.store(false, Ordering::SeqCst),
                    Some(_) => {}
                    None => {
                        command_channel_open = false;
                        registration_cancel.cancel("Worker 命令通道已关闭");
                    }
                }
            }
        }
    };

    let (exec_result, result_reason, already_exists, awaiting_verification) = match reg_result {
        Ok(outcome) => {
            let reason = if outcome.already_exists {
                "站点提示邮箱已存在".to_string()
            } else if outcome.awaiting_verification {
                "等待邮箱验证码".to_string()
            } else {
                "注册成功".to_string()
            };
            (
                ExecutionResult::Success,
                reason,
                outcome.already_exists,
                outcome.awaiting_verification,
            )
        }
        Err(err) => {
            let res = match err.class {
                platform_domain::FailureClass::Fatal => ExecutionResult::FatalFailure,
                _ => ExecutionResult::RetryableFailure,
            };
            (res, err.reason, false, false)
        }
    };

    {
        let mut state = shared.mail_provider_state.write().await;
        state.health = if awaiting_verification {
            "等待人工验证".to_string()
        } else if exec_result == ExecutionResult::Success {
            "健康".to_string()
        } else {
            "执行异常".to_string()
        };
    }

    let res_event_id = format!("evt-reg-res-{session_id}-{reg_task_id}");
    bus.send_reliable(
        &res_event_id,
        pb::worker_message::Payload::RegistrationTaskResult(pb::RegistrationTaskResult {
            session_id: session_id.clone(),
            execution_id: exec_id,
            registration_task_id: reg_task_id,
            stage_version,
            attempt: assign.attempt,
            result: exec_result.as_str().to_string(),
            reason: result_reason,
            already_exists,
            awaiting_verification,
            completed_at: chrono::Utc::now().to_rfc3339(),
        }),
    )
    .await;

    *lock(&shared.current) = None;

    let _ = engine.close_session(&session_handle).await;
    if let Some(mut proxy) = proxy_server {
        proxy.stop();
    }

    let success =
        exec_result == ExecutionResult::Success && !already_exists && !awaiting_verification;
    Ok(if success { 1 } else { 0 })
}

/// 失败分类 → （中文执行结果，中文任务状态建议值）。
///
/// 两者都直接取自 `platform-domain` 的第 10.3 节归因表，而不是在 Worker 里
/// 另写一份 match：归因规则只应该有一份，Worker 复述一遍就等于埋下分叉。
/// Master 对任务状态有最终决定权，这里给出的只是建议值。
fn classify(class: &platform_domain::FailureClass) -> (ExecutionResult, String) {
    let attribution = class.attribution();
    let task_status = attribution
        .task_status
        .map(|status| status.as_str().to_string())
        .unwrap_or_else(|| platform_domain::TaskStatus::Pending.as_str().to_string());
    (attribution.result, task_status)
}

async fn report_result(
    bus: &OutboundEventBus,
    assign: &pb::AssignTask,
    result: ExecutionResult,
    task_status: &str,
    reason: &str,
    quota: Option<(u32, u32)>,
    file: Option<pb::FileEvidence>,
) {
    report_result_with_duration(bus, assign, result, task_status, reason, quota, file, 0).await;
}

#[allow(clippy::too_many_arguments)]
async fn report_result_with_duration(
    bus: &OutboundEventBus,
    assign: &pb::AssignTask,
    result: ExecutionResult,
    task_status: &str,
    reason: &str,
    quota: Option<(u32, u32)>,
    file: Option<pb::FileEvidence>,
    duration_ms: u64,
) {
    bus.send_reliable(
        &format!("evt-res-{}", assign.execution_id),
        pb::worker_message::Payload::TaskResult(pb::TaskResult {
            session_id: assign.session_id.clone(),
            execution_id: assign.execution_id.clone(),
            task_id: assign.task_id.clone(),
            result: result.as_str().to_string(),
            task_status: task_status.to_string(),
            reason: reason.to_string(),
            // 原样回带 Master 下发的世代号
            stage_version: assign.stage_version,
            attempt: assign.attempt,
            duration_ms,
            quota_used: quota.map(|q| q.0).unwrap_or(0),
            quota_total: quota.map(|q| q.1).unwrap_or(0),
            file,
        }),
    )
    .await;
}

/// 清理槽位现场。执行现场记录**不删除**：要等 Master 的 `EventAck` 才算落定。
async fn finish_task(
    shared: &SlotShared,
    outbox: &LocalStore,
    execution_id: &str,
    reached_stage: ExecutionStage,
) {
    let _ = outbox.set_execution_stage(execution_id, reached_stage);
    *lock(&shared.current) = None;
    let mut snap = shared.snapshot.write().await;
    snap.current_task_id = None;
    snap.current_execution_id = None;
    snap.stage_enum = stage::RESULT_PENDING;
    snap.stage = stage::RESULT_PENDING.display_name().to_string();
    snap.detail = "任务收尾，准备申请下一本".to_string();
}

/// 进度上报上下文。
struct ProgressContext {
    session_id: String,
    execution_id: String,
    task_id: String,
    stage_version: u32,
    min_interval: Duration,
    min_bytes: u64,
}

/// 把引擎事件翻译成上行消息，并按第 13.3 节节流。
///
/// 阶段变化**不节流**：它是后台唯一能看到「卡在哪一步」的信号，
/// 而且一次任务里只有几条。字节进度才是需要压制的那一类。
async fn forward_events(
    mut events: mpsc::Receiver<AutomationEvent>,
    bus: OutboundEventBus,
    shared: SlotShared,
    outbox: LocalStore,
    ctx: ProgressContext,
) {
    let mut last_sent = Instant::now() - ctx.min_interval;
    let mut last_bytes = 0u64;
    let mut current_stage = stage::ACCEPTED.display_name().to_string();
    // 当前技术阶段枚举：引擎中文文本只在进入现场记录时转换一次（V4 第 10.1 节）
    let mut current_stage_enum = stage::ACCEPTED;

    while let Some(event) = events.recv().await {
        match event {
            AutomationEvent::Stage { stage } => {
                current_stage = stage.clone();
                if let Some(parsed) = stage::from_display(&stage) {
                    current_stage_enum = parsed;
                }
                {
                    let mut snap = shared.snapshot.write().await;
                    snap.stage = stage.clone();
                    snap.stage_enum = current_stage_enum;
                }
                let _ = outbox.set_execution_stage(&ctx.execution_id, current_stage_enum);
                bus.send_volatile(
                    volatile_id("stage"),
                    pb::worker_message::Payload::TaskProgress(pb::TaskProgress {
                        session_id: ctx.session_id.clone(),
                        execution_id: ctx.execution_id.clone(),
                        task_id: ctx.task_id.clone(),
                        downloaded_bytes: last_bytes,
                        total_bytes: 0,
                        stage,
                        stage_version: ctx.stage_version,
                    }),
                );
            }
            AutomationEvent::Progress {
                downloaded_bytes,
                total_bytes,
            } => {
                let enough_time = last_sent.elapsed() >= ctx.min_interval;
                let enough_bytes = downloaded_bytes.saturating_sub(last_bytes) >= ctx.min_bytes;
                let finished = total_bytes > 0 && downloaded_bytes >= total_bytes;
                if !(enough_time || enough_bytes || finished) {
                    continue;
                }
                last_sent = Instant::now();
                last_bytes = downloaded_bytes;
                bus.send_volatile(
                    volatile_id("p"),
                    pb::worker_message::Payload::TaskProgress(pb::TaskProgress {
                        session_id: ctx.session_id.clone(),
                        execution_id: ctx.execution_id.clone(),
                        task_id: ctx.task_id.clone(),
                        downloaded_bytes,
                        total_bytes,
                        stage: current_stage.clone(),
                        stage_version: ctx.stage_version,
                    }),
                );
            }
            AutomationEvent::Quota { used, total } => {
                tracing::debug!(
                    task_id = %ctx.task_id,
                    used,
                    total,
                    "读取到站点配额指示器"
                );
            }
            AutomationEvent::Log { level, message } => {
                bus.send_volatile(
                    volatile_id("log"),
                    pb::worker_message::Payload::WorkerLog(pb::WorkerLog {
                        node_id: String::new(),
                        session_id: ctx.session_id.clone(),
                        level,
                        message,
                        repeat_count: 1,
                    }),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Master 下发的一份最小可用配置。`max_slots` 是被测的核心（第 3.5 节）。
    fn node_config(max_slots: u32) -> pb::NodeConfig {
        pb::NodeConfig {
            node_id: "节点".to_string(),
            node_name: "书房台式机".to_string(),
            node_status: "在线".to_string(),
            max_slots,
            upload_concurrency: 1,
            heartbeat_interval_secs: 15,
            session_renew_secs: 60,
            progress_min_interval_secs: 2,
            progress_min_bytes: 1024,
            max_session_duration_secs: 3600,
            stall_timeout_secs: 300,
            nas_relative_root: "文件".to_string(),
            minimum_free_gb: 10,
            minimum_file_bytes: 32 * 1024,
            site_base: "https://books.internal.lan".to_string(),
            download_format: "pdf".to_string(),
            config_version: "v1".to_string(),
            min_agent_version: String::new(),
            diagnostics_enabled: false,
        }
    }

    fn worker_config() -> crate::config::WorkerConfig {
        crate::config::WorkerConfig {
            master: crate::config::MasterLinkConfig {
                endpoint: "http://127.0.0.1:9443".to_string(),
                enroll_endpoint: None,
                identity_file: std::path::PathBuf::from("data/identity.json"),
                client_cert_file: std::path::PathBuf::from("data/client.crt"),
                client_key_file: std::path::PathBuf::from("data/client.key"),
                node_ca_file: std::path::PathBuf::from("data/node_ca.crt"),
                server_ca_file: None,
                tls_domain: None,
                insecure: true,
            },
            storage: crate::config::StorageConfig::default(),
            execution: crate::config::ExecutionConfig {
                requested_slots: 3,
                simulated: true,
            },
            inventory: crate::inventory::InventoryConfig::default(),
        }
    }

    /// 拉起一个 3 槽位管理器，云端批准 `approved` 个。
    ///
    /// 总线接收端一并返回：丢掉它会让通道关闭，槽位随后的每次上报都会失败，
    /// 从而掩盖被测逻辑本身的问题。
    fn manager(approved: u32) -> (SlotManager, mpsc::Receiver<pb::WorkerMessage>) {
        let store = LocalStore::memory().unwrap();
        let state = Arc::new(ConfigState::new());
        state.apply(&node_config(approved)).unwrap();
        let (bus, rx) = OutboundEventBus::new(store.clone());
        (
            SlotManager::new(
                3,
                worker_config(),
                state,
                store,
                bus,
                "节点-测试".to_string(),
            ),
            rx,
        )
    }

    /// 给某个槽位伪造一个进行中的任务，用于验证取消路径。
    async fn pretend_running(
        slots: &SlotManager,
        index: usize,
        session_id: &str,
        task_id: &str,
    ) -> CancelToken {
        let cancel = CancelToken::new();
        let slot = &slots.slots[index];
        *lock(&slot.current) = Some(ActiveTask {
            session_id: session_id.to_string(),
            task_id: task_id.to_string(),
            execution_id: format!("exec-{task_id}"),
            stage_version: 7,
            cancel: cancel.clone(),
        });
        let mut snap = slot.snapshot.write().await;
        snap.status = SlotStatus::Running;
        snap.active_session_id = Some(session_id.to_string());
        snap.current_task_id = Some(task_id.to_string());
        snap.current_execution_id = Some(format!("exec-{task_id}"));
        snap.stage_version = 7;
        snap.stage_enum = stage::DOWNLOADING;
        snap.stage = stage::DOWNLOADING.display_name().to_string();
        cancel
    }

    #[tokio::test]
    async fn slots_beyond_the_cloud_approved_count_are_reported_deactivated() {
        // 第 3.5 节：管理员把这台机器调成 1 个槽位，本地配置的 3 个不能绕过去
        let (slots, _bus_rx) = manager(1);
        assert_eq!(slots.slot_count(), 3, "本地槽位仍然全部拉起");
        assert_eq!(slots.available_slots().await, 1, "对外只暴露批准的那一个");

        let states = slots.slot_states().await;
        assert_eq!(states[0].status, SlotStatus::Idle.as_str());
        for state in &states[1..] {
            // 报「空闲」会让后台以为还有容量，必须报「已停用」
            assert_eq!(state.status, SlotStatus::Deactivated.as_str());
            assert!(state.detail.contains("超出云端批准的槽位数"));
        }
    }

    #[tokio::test]
    async fn an_unapproved_config_leaves_no_usable_slot() {
        // 尚未收到 NodeConfig（max_slots = 0）时不能自作主张开工
        let store = LocalStore::memory().unwrap();
        let (bus, _bus_rx) = OutboundEventBus::new(store.clone());
        let slots = SlotManager::new(
            3,
            worker_config(),
            Arc::new(ConfigState::new()),
            store,
            bus,
            "节点-测试".to_string(),
        );
        assert_eq!(slots.available_slots().await, 0);
        assert_eq!(slots.find_idle_slot().await, None);
        // 「还没批准」不等于「已暂停」：两者在后台是完全不同的处置
        assert!(!slots.node_paused());
    }

    #[tokio::test]
    async fn create_session_on_an_unapproved_slot_is_refused() {
        let (slots, _bus_rx) = manager(1);
        slots
            .dispatch_create_session(pb::CreateSession {
                session_id: "会话1".to_string(),
                slot_index: 2,
                ..Default::default()
            })
            .await
            .unwrap();
        // 命令根本没有下发，槽位 2 不会进入启动中
        tokio::time::sleep(Duration::from_millis(20)).await;
        let states = slots.slot_states().await;
        assert!(states[2].session_id.is_empty());
        assert_eq!(states[2].status, SlotStatus::Deactivated.as_str());
    }

    #[tokio::test]
    async fn find_idle_slot_only_returns_approved_slots() {
        let (slots, _bus_rx) = manager(2);
        assert_eq!(slots.find_idle_slot().await, Some(0));
        pretend_running(&slots, 0, "会话1", "任务1").await;
        assert_eq!(slots.find_idle_slot().await, Some(1));
        pretend_running(&slots, 1, "会话2", "任务2").await;
        // 槽位 2 存在但没被批准，不能顶上来
        assert_eq!(slots.find_idle_slot().await, None);
    }

    #[tokio::test]
    async fn cancel_task_fires_the_token_of_the_matching_task() {
        // 第 3.6 节：取消不走命令通道——槽位此刻正卡在下载里，没人读通道
        let (slots, _bus_rx) = manager(3);
        let running = pretend_running(&slots, 0, "会话1", "任务1").await;
        let bystander = pretend_running(&slots, 1, "会话2", "任务2").await;

        slots
            .dispatch_cancel_task(pb::CancelTask {
                node_id: String::new(),
                session_id: "会话1".to_string(),
                task_id: "任务1".to_string(),
                execution_id: "exec-任务1".to_string(),
                stage_version: 7,
                reason: "管理员取消".to_string(),
            })
            .await
            .unwrap();

        assert!(running.is_cancelled());
        assert_eq!(running.reason().as_deref(), Some("管理员取消"));
        assert!(!bystander.is_cancelled(), "不能牵连其他槽位的任务");
    }

    #[tokio::test]
    async fn cancel_requires_all_fields_to_match() {
        // V4 精确取消：task/execution/stage_version 任一不匹配都不取消
        let (slots, _bus_rx) = manager(3);
        let running = pretend_running(&slots, 0, "会话1", "任务1").await;

        // 世代不匹配（旧消息）：不得取消新执行
        slots
            .dispatch_cancel_task(pb::CancelTask {
                node_id: String::new(),
                session_id: "会话1".to_string(),
                task_id: "任务1".to_string(),
                execution_id: "exec-任务1".to_string(),
                stage_version: 3,
                reason: "旧取消消息".to_string(),
            })
            .await
            .unwrap();
        assert!(!running.is_cancelled(), "世代不匹配的旧消息不得误伤新执行");

        // 执行编号不匹配：不得取消
        slots
            .dispatch_cancel_task(pb::CancelTask {
                node_id: String::new(),
                session_id: "会话1".to_string(),
                task_id: "任务1".to_string(),
                execution_id: "exec-其他".to_string(),
                stage_version: 7,
                reason: String::new(),
            })
            .await
            .unwrap();
        assert!(!running.is_cancelled(), "执行编号不匹配不得取消");

        // 全部匹配才取消
        slots
            .dispatch_cancel_task(pb::CancelTask {
                node_id: String::new(),
                session_id: "会话1".to_string(),
                task_id: "任务1".to_string(),
                execution_id: "exec-任务1".to_string(),
                stage_version: 7,
                reason: String::new(),
            })
            .await
            .unwrap();
        assert!(running.is_cancelled());
        assert_eq!(running.reason().as_deref(), Some("云端取消任务"));
    }

    #[tokio::test]
    async fn cancel_matches_by_execution_id_without_stage_constraint() {
        // stage_version = 0 表示「不约束世代」的兼容形态
        let (slots, _bus_rx) = manager(3);
        let running = pretend_running(&slots, 0, "会话1", "任务1").await;
        slots
            .dispatch_cancel_task(pb::CancelTask {
                node_id: String::new(),
                session_id: String::new(),
                task_id: "任务1".to_string(),
                execution_id: "exec-任务1".to_string(),
                stage_version: 0,
                reason: String::new(),
            })
            .await
            .unwrap();
        assert!(running.is_cancelled());
        assert_eq!(running.reason().as_deref(), Some("云端取消任务"));
    }

    #[tokio::test]
    async fn end_session_can_either_interrupt_now_or_after_this_book() {
        let (slots, _bus_rx) = manager(3);

        // finish_current_task = false：立刻打断
        let interrupted = pretend_running(&slots, 0, "会话1", "任务1").await;
        slots
            .dispatch_end_session(pb::EndSession {
                session_id: "会话1".to_string(),
                reason: "账号额度用尽".to_string(),
                finish_current_task: false,
            })
            .await
            .unwrap();
        assert!(interrupted.is_cancelled());
        assert_eq!(interrupted.reason().as_deref(), Some("账号额度用尽"));

        // finish_current_task = true：只登记收尾原因，当前这本书继续下完
        let finishing = pretend_running(&slots, 1, "会话2", "任务2").await;
        slots
            .dispatch_end_session(pb::EndSession {
                session_id: "会话2".to_string(),
                reason: "达到单会话上限".to_string(),
                finish_current_task: true,
            })
            .await
            .unwrap();
        assert!(!finishing.is_cancelled(), "这本书应当被允许下完");
        assert_eq!(
            slots.slots[1]
                .shared(
                    "节点-测试".to_string(),
                    Arc::new(RwLock::new(MailProviderState::default()))
                )
                .take_end_reason()
                .as_deref(),
            Some("达到单会话上限"),
            "收尾原因必须留下，否则会话会继续领下一本"
        );
    }

    #[tokio::test]
    async fn end_session_ignores_slots_holding_another_session() {
        let (slots, _bus_rx) = manager(3);
        let other = pretend_running(&slots, 0, "会话1", "任务1").await;
        slots
            .dispatch_end_session(pb::EndSession {
                session_id: "不存在的会话".to_string(),
                reason: "无关".to_string(),
                finish_current_task: false,
            })
            .await
            .unwrap();
        assert!(!other.is_cancelled());
        assert_eq!(
            slots.slots[0]
                .shared(
                    "节点-测试".to_string(),
                    Arc::new(RwLock::new(MailProviderState::default()))
                )
                .take_end_reason(),
            None
        );
    }

    #[tokio::test]
    async fn pause_stops_dispatch_and_resume_restores_it() {
        let (slots, _bus_rx) = manager(3);
        let running = pretend_running(&slots, 0, "会话1", "任务1").await;

        slots
            .dispatch_pause(pb::PauseNode {
                reason: "机器要重启".to_string(),
                finish_current_task: false,
            })
            .await
            .unwrap();

        assert!(slots.node_paused());
        assert!(running.is_cancelled());
        assert_eq!(slots.available_slots().await, 0, "暂停期间不得再领会话");
        assert_eq!(slots.find_idle_slot().await, None);

        slots.dispatch_resume().await.unwrap();
        assert!(!slots.node_paused());
        assert_eq!(
            slots.slots[0]
                .shared(
                    "节点-测试".to_string(),
                    Arc::new(RwLock::new(MailProviderState::default()))
                )
                .take_end_reason(),
            None,
            "恢复时必须清掉收尾原因，否则新会话一开始就想结束"
        );
        // 槽位 0 还挂着伪造的进行中状态，能领会话的是另外两个
        assert_eq!(slots.available_slots().await, 2);
    }

    #[tokio::test]
    async fn pause_with_finish_current_task_lets_the_book_complete() {
        let (slots, _bus_rx) = manager(3);
        let running = pretend_running(&slots, 0, "会话1", "任务1").await;
        slots
            .dispatch_pause(pb::PauseNode {
                reason: "计划维护".to_string(),
                finish_current_task: true,
            })
            .await
            .unwrap();
        assert!(slots.node_paused(), "节点已经不再接新活");
        assert!(!running.is_cancelled(), "手上这本书应当下完");
    }

    #[tokio::test]
    async fn cancel_all_interrupts_every_running_task_for_a_graceful_exit() {
        let (slots, _bus_rx) = manager(3);
        let a = pretend_running(&slots, 0, "会话1", "任务1").await;
        let b = pretend_running(&slots, 2, "会话3", "任务3").await;
        slots.cancel_all("Worker 正在退出");
        assert!(a.is_cancelled());
        assert!(b.is_cancelled());
        assert_eq!(a.reason().as_deref(), Some("Worker 正在退出"));
    }

    #[tokio::test]
    async fn active_executions_echo_the_master_owned_stage_version() {
        // 第 3.2 节：世代号由 Master 拥有，Worker 只回带，绝不自增
        let (slots, _bus_rx) = manager(3);
        pretend_running(&slots, 0, "会话1", "任务1").await;
        let items = slots.active_executions().await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].stage_version, 7);
        assert_eq!(items[0].task_status, stage::DOWNLOADING.display_name());
        assert_eq!(
            ExecutionStage::from_i32_safe(items[0].stage),
            stage::DOWNLOADING
        );
        assert_eq!(items[0].execution_id, "exec-任务1");
        assert_eq!(slots.active_session_ids().await, vec!["会话1".to_string()]);
    }

    #[tokio::test]
    async fn assign_task_to_an_unknown_session_is_dropped_not_misrouted() {
        // 错投给一个空闲槽位会让它在没有浏览器的情况下开始「下载」
        let (slots, _bus_rx) = manager(3);
        slots
            .dispatch_assign_task(pb::AssignTask {
                session_id: "不存在的会话".to_string(),
                task_id: "任务9".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        for state in slots.slot_states().await {
            assert!(state.task_id.is_empty(), "任务不应落到任何槽位上");
        }
    }

    #[test]
    fn failure_classification_comes_from_the_shared_attribution_table() {
        use platform_domain::FailureClass;

        // 「书不存在」不是失败而是跳过：重试一万次也不会把它变出来
        let (result, status) = classify(&FailureClass::BookNotFound);
        assert_eq!(result, ExecutionResult::Skipped);
        assert_eq!(status, platform_domain::TaskStatus::Skipped.as_str());

        // 逐项与 platform-domain 的第 10.3 节归因表对齐：Worker 不另写一份
        for class in [
            FailureClass::AccountQuotaExhausted,
            FailureClass::SiteRateLimited,
            FailureClass::ProxyFailure,
            FailureClass::BookNotFound,
            FailureClass::AuthFailed,
            FailureClass::SiteUnavailable,
            FailureClass::Uncertain,
            FailureClass::Retryable,
            FailureClass::Fatal,
        ] {
            let attribution: platform_domain::failure::Attribution = class.attribution();
            let (result, status) = classify(&class);
            assert_eq!(
                result, attribution.result,
                "{class:?} 的执行结果必须取自归因表"
            );
            match attribution.task_status {
                Some(expected) => assert_eq!(status, expected.as_str(), "{class:?}"),
                // 归因表不指定任务状态时回到「待处理」，让 Master 自己决定
                None => assert_eq!(status, platform_domain::TaskStatus::Pending.as_str()),
            }
        }
    }
}
