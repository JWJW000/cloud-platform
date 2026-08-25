//! 上行消息分发（第 13.1、13.2、3.3 节、第 8 节、第 10 节）。
//!
//! 每条上行消息在这里走同一条流水线：**去重登记 → 应用 → 记账 → 确认**。
//! 三件事值得单独说明，因为它们的顺序都是踩过坑之后定的。
//!
//! **去重先于处理。** 至少一次投递意味着同一个事件必然重复到达，去重责任在 Master。
//! 登记用主键冲突而不是「先查后插」：两条流同时把同一个重放事件送上来时，
//! 先查后插会双双认为自己是第一次，一次完成就被算成两次。
//! 代价是「登记成功但处理失败」这个中间态需要有人收拾——见下一条。
//!
//! **可重试的失败要撤销登记。** 数据库暂时不可写属于「重投一次就好」的失败，
//! 但那条事件已经登记过了，不撤销就会被永久判为「已见过」而再也不被应用，
//! 于是一次抖动变成一本书的永久丢单。因此只有**永久性**失败才留下 `applied = false`
//! 的审计记录，可重试的失败调 [`store::session::forget_event`] 把登记抹掉。
//!
//! **`EventAck.accepted` 回答的是「还要不要重投」，不是「处理成功了吗」。**
//! 参数不合法的事件重投一万次也不会变合法，因此它得到 `accepted = true`
//! 让 Worker 把它从 outbox 里删掉，真正的原因写在 `detail` 里留档。
//!
//! 哪些消息参与去重也是有取舍的：只有**会改变业务状态且来自 outbox**的上报参与
//! （任务接受、任务结果、NAS 核验、代理检测、会话结束）。心跳与进度上报不参与——
//! 它们每几秒一条，登记它们等于把事件表当日志用；两者本身也都有各自的过期判定
//! （租约与阶段版本），重复到达不会造成二次影响。

use platform_domain::{
    adopt_reported_worker_status, ExecutionResult, LogLevel, NasLayout, OperationSource,
    SessionStatus, SlotStatus, StatusAdoption, TaskType, WorkerStatus,
};
use platform_proto::v1 as pb;
use platform_proto::v1::worker_message::Payload;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::grpc::auth::NodeIdentity;
use crate::grpc::convert::{self, clamp_to_i32, clamp_to_i64, parse_optional_uuid, parse_uuid};
use crate::models::WorkerNode;
use crate::scheduler::{
    self, AllocationOutcome, ClaimOutcome, FileEvidence, NasCheckReport, ResultReport,
};
use crate::state::AppState;
use crate::store;

/// 单条日志明细的落库上限（字符）。
///
/// Worker 偶尔会把一整页 HTML 当作错误详情送上来，截断是为了让操作日志表
/// 保持可查询；真正的长文本应该走诊断日志，而不是挤进这张表。
const MAX_LOG_DETAIL: usize = 2000;

/// 分发一条从 Worker 发来的消息。
pub async fn dispatch(
    state: &AppState,
    identity: &NodeIdentity,
    message: pb::WorkerMessage,
    outbound: &mpsc::Sender<pb::MasterMessage>,
) -> AppResult<()> {
    let event_id = message.event_id.trim();
    let replayed = message.replayed;
    let payload = match message.payload {
        Some(p) => p,
        None => {
            tracing::warn!(node_id = %identity.node_id(), "收到空的 WorkerMessage 载荷");
            return Ok(());
        }
    };

    match payload {
        Payload::NodeOnline(online) => {
            handle_node_online(state, identity, online, outbound).await?;
        }
        Payload::Heartbeat(heartbeat) => {
            handle_heartbeat(state, identity, heartbeat, outbound).await?;
        }
        Payload::SlotStatus(slot_report) => {
            handle_slot_status(state, identity, slot_report).await?;
        }
        Payload::SessionRequest(req) => {
            handle_session_request(state, identity, req, outbound).await?;
        }
        Payload::SessionReady(ready) => {
            handle_session_ready(state, identity, ready, outbound).await?;
        }
        Payload::NextTaskRequest(req) => {
            handle_next_task_request(state, identity, req, outbound).await?;
        }
        Payload::WorkRequest(req) => {
            handle_work_request(state, identity, req, outbound).await?;
        }
        Payload::RegistrationTaskAccepted(accepted) => {
            handle_registration_task_accepted(
                state, identity, event_id, replayed, accepted, outbound,
            )
            .await?;
        }
        Payload::RegistrationTaskProgress(progress) => {
            handle_registration_task_progress(state, identity, progress).await?;
        }
        Payload::RegistrationTaskResult(result) => {
            handle_registration_task_result(state, identity, event_id, replayed, result, outbound)
                .await?;
        }
        Payload::ManualActionRequired(req) => {
            handle_manual_action_required(state, identity, event_id, replayed, req, outbound)
                .await?;
        }
        Payload::CommandAccepted(_) | Payload::CommandResult(_) => {
            // 命令确认与结果
        }
        Payload::TaskAccepted(accepted) => {
            handle_task_accepted(state, identity, event_id, replayed, accepted, outbound).await?;
        }
        Payload::TaskProgress(progress) => {
            handle_task_progress(state, identity, progress).await?;
        }
        Payload::TaskResult(result) => {
            handle_task_result(state, identity, event_id, replayed, result, outbound).await?;
        }
        Payload::NasCheckResult(check) => {
            handle_nas_check_result(state, identity, event_id, replayed, check, outbound).await?;
        }
        Payload::ProxyCheckResult(check) => {
            handle_proxy_check_result(state, identity, event_id, replayed, check, outbound).await?;
        }
        Payload::SessionClosed(closed) => {
            handle_session_closed(state, identity, event_id, replayed, closed, outbound).await?;
        }
        Payload::WorkerLog(log) => {
            handle_worker_log(state, identity, log).await?;
        }
        Payload::ReconcileAck(ack) => {
            // 逐执行对账 ACK（V4 第 10.5 节）：收齐且全部成功后才下发 complete=true。
            let node_id = identity.node_id();
            let execution_id = ack.execution_id.trim();
            let action = pb::ReconcileAction::from_i32_safe(ack.action);
            if ack.accepted {
                tracing::info!(
                    node_id = %node_id,
                    execution_id = %execution_id,
                    action = %action.display_name(),
                    detail = %ack.detail,
                    "Worker 已执行对账裁决"
                );
            } else {
                tracing::warn!(
                    node_id = %node_id,
                    execution_id = %execution_id,
                    action = %action.display_name(),
                    detail = %ack.detail,
                    "Worker 无法执行对账裁决，现场保留待人工处理"
                );
                store::admin::raise_alert(
                    &state.pool,
                    platform_domain::AlertLevel::Warn,
                    "对账",
                    "Worker 无法执行对账裁决",
                    &format!(
                        "节点 {node_id} 执行 {execution_id}（{}）：{}",
                        action.display_name(),
                        ack.detail
                    ),
                    Some(node_id),
                    Some(&format!("对账失败:{node_id}:{execution_id}")),
                )
                .await?;
            }

            // 只有 accepted=true 且动作与下发一致才会移除待收记录；
            // 失败/动作不符 → 保留待确认（Pending），绝不下发 complete。
            let outcome =
                state
                    .links
                    .ack_reconcile(node_id, execution_id, ack.action, ack.accepted);
            match outcome {
                crate::state::ReconcileAckOutcome::Completed => {
                    let complete_msg = pb::MasterMessage::new(
                        convert::now_rfc3339(),
                        pb::master_message::Payload::ReconcileExecutions(pb::ReconcileExecutions {
                            decisions: Vec::new(),
                            reconciliation_complete: true,
                        }),
                    );
                    let _ = outbound.send(complete_msg).await;
                    tracing::info!(node_id = %node_id, "节点对账裁决已全部 ACK，对账完成");
                }
                crate::state::ReconcileAckOutcome::Pending => {
                    tracing::debug!(
                        node_id = %node_id,
                        execution_id = %execution_id,
                        "对账 ACK 已记录，仍有待收裁决"
                    );
                }
                crate::state::ReconcileAckOutcome::Unknown => {
                    // 该节点没有待收集合：ACK 属于旧链路或伪造，不触发 complete。
                    tracing::warn!(
                        node_id = %node_id,
                        execution_id = %execution_id,
                        "收到与当前对账无关的 ACK（可能来自旧链路），已忽略"
                    );
                }
            }
        }
    }

    Ok(())
}

/// 节点上线（重连对账与初次配置下发）。
async fn handle_node_online(
    state: &AppState,
    identity: &NodeIdentity,
    online: pb::NodeOnline,
    outbound: &mpsc::Sender<pb::MasterMessage>,
) -> AppResult<()> {
    let node_id = identity.node_id();
    let node = store::node::get_node(&state.pool, node_id).await?;

    // 先把自报信息落库：Agent 版本、操作系统与「已生效的配置版本」。
    // 这一步以前被跳过，导致节点行里的 applied_config_version 永远是空的，
    // 后台无从判断下发的运行配置到底有没有生效（第 3.3 节）。
    // 操作系统标识与注册入口一样做归一化：Worker 自报的是
    // `std::env::consts::OS`（macos/linux/windows 小写），而数据库 CHECK 约束
    // 只接受 `Windows`/`macOS`/`Linux`（本地联调曾因 "macos" 违反约束导致
    // NodeOnline 处理失败、对账永不完成）。
    store::node::record_node_online(
        &state.pool,
        node_id,
        online.agent_version.trim(),
        crate::grpc::enroll::os_label(&online.os),
        &online.os_version,
        online.applied_config_version.trim(),
    )
    .await?;

    // 连上来即在线，但管理员治理中的状态不动：`待审核` 要等审核，
    // `维护中`/`已禁用` 是管理员的决定，节点重连不构成解除（第 3.7 节）。
    let current_status = node
        .status
        .parse::<WorkerStatus>()
        .unwrap_or(WorkerStatus::Offline);
    if !current_status.is_admin_governed() && current_status != WorkerStatus::Online {
        store::node::set_node_status(&state.pool, node_id, WorkerStatus::Online).await?;
        tracing::info!(
            node_id = %node_id,
            from = %current_status,
            "节点建立链路，状态置为在线"
        );
    }

    // 槽位是派生数据：巡检把离线节点的 available_slots 清成 0 之后，
    // 只有这里和心跳会把它按槽位表重新算回来，否则后台会一直显示 0 可用。
    store::node::refresh_available_slots(&state.pool, node_id).await?;

    // 恢复处于断线保护的会话
    let recovered_sessions = scheduler::resume_protected_sessions(state, node_id).await?;
    if recovered_sessions > 0 {
        tracing::info!(
            node_id = %node_id,
            count = recovered_sessions,
            "节点重连，已将断线保护中的会话恢复运行"
        );
    }

    // 重新读一次再下发：上面几步刚改过状态与可用槽位，用改之前的快照下发
    // 会把 `node_status = 离线` 推给一个刚刚上线的节点。
    let node = store::node::get_node(&state.pool, node_id).await?;
    send_node_config(state, &node, outbound).await;

    // 暂停是云端意图，而 Worker 的本地暂停标记只在内存里：进程重启后它会认为自己
    // 可以正常接活。因此重连时必须把暂停重新压下去，否则一次重启就等于悄悄恢复了节点。
    if node.status == WorkerStatus::Paused.as_str() {
        let _ = outbound
            .send(pb::MasterMessage::new(
                convert::now_rfc3339(),
                pb::master_message::Payload::PauseNode(pb::PauseNode {
                    reason: "节点仍处于云端暂停状态".to_string(),
                    finish_current_task: true,
                }),
            ))
            .await;
        tracing::info!(node_id = %node_id, "节点重连，已重新下发暂停指令");
    }

    // 发布节点变更事件
    state
        .events
        .publish("节点变更", serde_json::json!({ "节点": node_id }));

    // 逐项处理重连现场对账（V4 方案第 10.4 节）
    let mut decisions = Vec::new();
    for active_exec in &online.active_executions {
        let decision = reconcile_single_execution(state, node_id, active_exec).await?;
        decisions.push(decision);
    }

    if decisions.is_empty() {
        // 没有现场需要裁决：直接完成对账
        let reconcile_msg = pb::MasterMessage::new(
            convert::now_rfc3339(),
            pb::master_message::Payload::ReconcileExecutions(pb::ReconcileExecutions {
                decisions: Vec::new(),
                reconciliation_complete: true,
            }),
        );
        let _ = outbound.send(reconcile_msg).await;
    } else {
        // 有裁决需要逐执行 ACK：先登记待收集合（含期望动作），再下发裁决（complete=false）。
        // Worker 收齐后才会收到 complete=true 并解除 reconciling（第 10.5 节）。
        let pending_entries: Vec<(String, i32)> = decisions
            .iter()
            .map(|d| (d.execution_id.clone(), d.action))
            .collect();
        state.links.set_pending_reconciles(node_id, pending_entries);
        let reconcile_msg = pb::MasterMessage::new(
            convert::now_rfc3339(),
            pb::master_message::Payload::ReconcileExecutions(pb::ReconcileExecutions {
                decisions,
                reconciliation_complete: false,
            }),
        );
        let _ = outbound.send(reconcile_msg).await;
    }

    tracing::info!(
        node_id = %node_id,
        agent_version = %online.agent_version,
        os = %online.os,
        max_slots = online.max_slots,
        active_executions = online.active_executions.len(),
        "Worker 节点上线对账完成"
    );

    Ok(())
}

/// 单条执行记录的重连对账规则判定（V4 方案第 10.4 节裁决表）。
///
/// 裁决必须覆盖 Worker 的每个技术阶段（枚举），不允许自由字符串比较（V4-02 / V4-03）：
///
/// | Worker 阶段 | 租约匹配 | Master 任务状态 | 裁决 |
/// | --- | --- | --- | --- |
/// | 已接受/搜索中/下载中 | 是 | 活动 | 停止并重试 |
/// | NAS 上传中 | 是 | 活动 | 清理本执行上传临时文件后重试 |
/// | 本地文件完成 | 是 | 活动 | 继续入库 |
/// | NAS 已原子落盘 | 是 | 活动或待确认 | 核验 NAS |
/// | 结果待上报 | 是或任务已完成 | 任意合法状态 | 重放结果 |
/// | 任意阶段 | 否 | 已被新执行领取 | 停止并清理，禁止提交 |
/// | 任意阶段 | 任意 | 已取消 | 停止并清理；最终文件不得删除 |
/// | 任意阶段 | 任意 | 已完成 | 重放尚未确认结果或清理现场 |
async fn reconcile_single_execution(
    state: &AppState,
    node_id: Uuid,
    active: &pb::ActiveExecution,
) -> AppResult<pb::ExecutionReconcileDecision> {
    use pb::ReconcileAction as Action;

    let task_id = match Uuid::parse_str(&active.task_id) {
        Ok(id) => id,
        Err(_) => {
            return Ok(decision(active, Action::CleanupOnly, "任务编号格式非法"));
        }
    };
    let exec_id = match Uuid::parse_str(&active.execution_id) {
        Ok(id) => id,
        Err(_) => {
            return Ok(decision(active, Action::CleanupOnly, "执行编号格式非法"));
        }
    };

    let task = match store::task::get_task(&state.pool, task_id).await {
        Ok(t) => t,
        Err(_) => {
            return Ok(decision(active, Action::CleanupOnly, "任务不存在"));
        }
    };

    let worker_stage = pb::ExecutionStage::from_i32_safe(active.stage);

    // 1. 任务已在 Master 端完成
    if task.status == platform_domain::TaskStatus::Completed.as_str() {
        // 结果待上报 → 重放结果（可能 ACK 已丢）；其余阶段 → 清理现场
        let action = if worker_stage == pb::ExecutionStage::ResultPending {
            Action::ReplayResult
        } else {
            Action::CleanupOnly
        };
        return Ok(decision(active, action, "任务已在 Master 端记录完成"));
    }

    // 2. 任务已取消
    if task.status == platform_domain::TaskStatus::Cancelled.as_str() || task.cancel_requested {
        return Ok(decision(
            active,
            Action::CleanupOnly,
            "任务已取消（最终文件不得删除）",
        ));
    }

    // 3. 租约与世代匹配：按 Worker 阶段裁决
    if task.lease_execution_id == Some(exec_id)
        && task.lease_node_id == Some(node_id)
        && task.stage_version == active.stage_version as i32
    {
        let (action, reason) = match worker_stage {
            // 浏览器进程重启后不可恢复：搜索中、下载中不得返回「继续执行」
            pb::ExecutionStage::Accepted
            | pb::ExecutionStage::Searching
            | pb::ExecutionStage::Downloading => {
                (Action::StopAndRetry, "租约匹配，浏览器阶段重启后不可恢复")
            }
            // NAS 上传中：清理本执行的上传临时文件后重试
            pb::ExecutionStage::NasUploading => {
                (Action::StopAndRetry, "NAS 上传中，清理本执行临时文件后重试")
            }
            // 本地文件完成：证据可证明，继续入库
            pb::ExecutionStage::LocalFileReady => (Action::ResumeIngest, "租约匹配，允许继续入库"),
            // NAS 已原子落盘：核验 NAS
            pb::ExecutionStage::NasCommitted => (Action::VerifyNas, "租约匹配，要求核验 NAS"),
            // 结果待上报：重放结果
            pb::ExecutionStage::ResultPending => {
                (Action::ReplayResult, "租约匹配，要求重放 Outbox 结果")
            }
            pb::ExecutionStage::Unspecified => (Action::StopAndRetry, "Worker 阶段未知，安全重试"),
        };
        return Ok(decision(active, action, reason));
    }

    // 4. 任务已被新世代领走或已被回收
    Ok(decision(
        active,
        Action::CleanupOnly,
        "任务租约已过期或已被新执行领走，禁止提交",
    ))
}

/// 构造一条对账裁决。
fn decision(
    active: &pb::ActiveExecution,
    action: pb::ReconcileAction,
    reason: &str,
) -> pb::ExecutionReconcileDecision {
    pb::ExecutionReconcileDecision {
        execution_id: active.execution_id.clone(),
        task_id: active.task_id.clone(),
        stage_version: active.stage_version,
        action: action as i32,
        reason: reason.to_string(),
    }
}

/// 心跳上报（含节点状态采纳与活动任务租约续期）。
async fn handle_heartbeat(
    state: &AppState,
    identity: &NodeIdentity,
    heartbeat: pb::Heartbeat,
    outbound: &mpsc::Sender<pb::MasterMessage>,
) -> AppResult<()> {
    let node_id = identity.node_id();

    // 自评状态的合法性在这里判，写库的原子性在 SQL 里保证（见 `apply_heartbeat`）。
    // 判定结果只有三种去处：采纳、忽略、告警——**没有**「解析不出来就当在线」这条路。
    let node = store::node::get_node(&state.pool, node_id).await?;
    let current = node
        .status
        .parse::<WorkerStatus>()
        .unwrap_or(WorkerStatus::Offline);
    let reported_status = match adopt_reported_worker_status(current, &heartbeat.node_status) {
        StatusAdoption::Adopt(status) => Some(status),
        StatusAdoption::Unchanged | StatusAdoption::AdminGoverned(_) => None,
        StatusAdoption::Rejected(reason) => {
            // 非法自评保留原状态。它多半意味着 Agent 版本不匹配，因此值得留一条日志，
            // 但不能让心跳整体失败——那会把一台还在干活的机器判成失联。
            tracing::warn!(
                node_id = %node_id,
                reported = %heartbeat.node_status,
                %reason,
                "忽略节点自评状态，保留原状态"
            );
            None
        }
    };

    let metrics = store::node::HeartbeatMetrics {
        nas_healthy: heartbeat.nas_healthy,
        nas_free_gb: clamp_to_i64(heartbeat.nas_free_gb),
        staging_free_gb: clamp_to_i64(heartbeat.staging_free_gb),
        cpu_percent: heartbeat.cpu_percent,
        memory_used_mb: clamp_to_i64(heartbeat.memory_used_mb),
        memory_total_mb: clamp_to_i64(heartbeat.memory_total_mb),
        agent_version: identity.agent_version.clone(),
        // Worker 自报的已生效配置版本。空串表示这一跳没报（旧版 Agent），
        // 由 SQL 的 `NULLIF` 保留库里已有的值，而不是把它清空。
        applied_config_version: heartbeat.applied_config_version.trim().to_string(),
        applied_mail_provider_version: clamp_to_i64(heartbeat.applied_mail_provider_version),
        mail_provider_name: heartbeat.mail_provider_name.trim().to_string(),
        mail_provider_health: heartbeat
            .mail_provider_health
            .trim()
            .chars()
            .take(128)
            .collect(),
        reported_status,
    };

    let effective_status = store::node::apply_heartbeat(&state.pool, node_id, &metrics).await?;

    // 可用槽位是派生数据。巡检把失联节点的 available_slots 清成 0，如果心跳不重算，
    // 一个恢复过来的节点会在后台永远显示 0 可用（第 3.7 节）。
    store::node::refresh_available_slots(&state.pool, node_id).await?;

    if effective_status != node.status {
        tracing::info!(
            node_id = %node_id,
            from = %node.status,
            to = %effective_status,
            "按节点自评更新状态"
        );
        // 从离线恢复意味着之前那条离线告警已经不成立了，顺手关掉，
        // 否则告警列表里会留下一条永远不会自己消失的记录。
        if current == WorkerStatus::Offline {
            store::admin::resolve_alert_by_key(&state.pool, &format!("节点离线:{node_id}")).await?;
        }
        state.events.publish(
            "节点变更",
            serde_json::json!({ "节点": node_id, "状态": effective_status }),
        );
    }

    // 续租该节点活跃的会话
    let renew_secs = state.config.scheduler.session_renew_secs as i64;
    for session_id_str in &heartbeat.active_session_ids {
        if let Ok(session_id) = parse_uuid(session_id_str, "活跃会话") {
            let active = store::session::renew_session(&state.pool, session_id, renew_secs).await?;
            if !active {
                // 会话已被回收，告知 Worker 停止该会话
                let _ = outbound
                    .send(convert::end_session_message(
                        session_id,
                        "会话已被 Master 回收",
                        false,
                    ))
                    .await;
            }
        }
    }

    // 续租活跃的任务执行（第 10.1 节、V3 方案第 9.6 节）
    let task_lease_secs = state.config.scheduler.task_lease_secs as i64;
    for active_exec in &heartbeat.active_executions {
        if let (Ok(task_id), Ok(session_id), Ok(exec_id)) = (
            parse_uuid(&active_exec.task_id, "任务"),
            parse_uuid(&active_exec.session_id, "会话"),
            parse_uuid(&active_exec.execution_id, "执行"),
        ) {
            match sqlx::query(
                "UPDATE book_tasks SET lease_expires_at = now() + ($1 || ' seconds')::interval, updated_at = now() \
                 WHERE id = $2 AND lease_session_id = $3 AND lease_execution_id = $4 AND stage_version = $5 \
                   AND status IN ('已分配', '执行中', '等待入库')",
            )
            .bind(task_lease_secs.to_string())
            .bind(task_id)
            .bind(session_id)
            .bind(exec_id)
            .bind(active_exec.stage_version as i32)
            .execute(&state.pool)
            .await {
                Ok(res) => {
                    if res.rows_affected() == 0 {
                        // 尝试续租账号注册任务
                        let reg_res = sqlx::query(
                            "UPDATE account_registration_tasks SET lease_expires_at = now() + ($1 || ' seconds')::interval, updated_at = now() \
                             WHERE id = $2 AND lease_session_id = $3 AND lease_execution_id = $4 AND stage_version = $5 \
                               AND status IN ('已分配', '执行中')",
                        )
                        .bind(task_lease_secs.to_string())
                        .bind(task_id)
                        .bind(session_id)
                        .bind(exec_id)
                        .bind(active_exec.stage_version as i32)
                        .execute(&state.pool)
                        .await;

                        if reg_res.as_ref().map(|r| r.rows_affected()).unwrap_or(0) == 0 {
                            tracing::warn!(
                                task_id = %task_id,
                                execution_id = %exec_id,
                                "任务心跳续租未匹配任何进行中租约，下发停止指令"
                            );
                            let _ = outbound
                                .send(pb::MasterMessage::new(
                                    convert::now_rfc3339(),
                                    pb::master_message::Payload::CancelTask(pb::CancelTask {
                                        node_id: node_id.to_string(),
                                        session_id: session_id.to_string(),
                                        task_id: task_id.to_string(),
                                        execution_id: exec_id.to_string(),
                                        stage_version: active_exec.stage_version,
                                        reason: "任务租约已失效或已分配给新执行".to_string(),
                                    }),
                                ))
                                .await;
                        }
                    }
                }
                Err(err) => {
                    tracing::error!(
                        task_id = %task_id,
                        error = %err,
                        "心跳任务续租 SQL 执行失败"
                    );
                }
            }
        }
    }

    Ok(())
}

/// 槽位状态上报。
async fn handle_slot_status(
    state: &AppState,
    identity: &NodeIdentity,
    slot_report: pb::SlotStatusReport,
) -> AppResult<()> {
    let node_id = identity.node_id();
    for slot in slot_report.slots {
        let status = slot
            .status
            .parse::<SlotStatus>()
            .unwrap_or(SlotStatus::Idle);
        let session_id = parse_optional_uuid(&slot.session_id, "槽位会话")
            .ok()
            .flatten();
        scheduler::slot_status_report(
            state,
            node_id,
            slot.slot_index as i32,
            status,
            session_id,
            &slot.detail,
        )
        .await?;
    }
    Ok(())
}

/// 申请创建执行会话。
async fn handle_session_request(
    state: &AppState,
    identity: &NodeIdentity,
    req: pb::SessionRequest,
    outbound: &mpsc::Sender<pb::MasterMessage>,
) -> AppResult<()> {
    let node_id = identity.node_id();
    let slot_index = req.slot_index as i32;
    let task_type = req
        .task_type
        .parse::<TaskType>()
        .unwrap_or(TaskType::BookDownload);

    let outcome = scheduler::allocate_session(state, node_id, task_type, Some(slot_index)).await?;
    match outcome {
        AllocationOutcome::Granted(grant) => {
            let msg = convert::create_session_message(&grant);
            if outbound.send(msg).await.is_err() {
                tracing::warn!(node_id = %node_id, "下发 CreateSession 失败：通道已关闭");
            }
        }
        AllocationOutcome::Unavailable(reason) => {
            tracing::debug!(
                node_id = %node_id,
                slot = slot_index,
                reason = ?reason,
                "暂时无法创建会话"
            );
        }
    }
    Ok(())
}

/// 会话已完成准备（SessionReady）转入 Running 状态。
async fn handle_session_ready(
    state: &AppState,
    identity: &NodeIdentity,
    ready: pb::SessionReady,
    outbound: &mpsc::Sender<pb::MasterMessage>,
) -> AppResult<()> {
    let session_id = parse_uuid(&ready.session_id, "会话编号")?;
    store::session::activate_session(&state.pool, session_id).await?;
    let session = store::session::get_session(&state.pool, session_id).await?;

    if !ready.exit_ip.trim().is_empty() {
        if let Some(proxy_id) = session.proxy_id {
            let _ =
                sqlx::query("UPDATE proxies SET exit_ip = $2, updated_at = now() WHERE id = $1")
                    .bind(proxy_id)
                    .bind(ready.exit_ip.trim())
                    .execute(&state.pool)
                    .await;
        }
    }

    if session.task_type == "账号注册" {
        if let Some(account_id) = session.account_id {
            let mut tx = state.pool.begin().await?;
            let task_row: Option<(Uuid, i32, i32)> = sqlx::query_as(
                "SELECT t.id, t.attempts, t.stage_version \
                 FROM account_registration_tasks t \
                 JOIN account_registration_batches b ON b.id = t.batch_id \
                 WHERE t.account_id = $1 AND b.status = '执行中' AND t.status = '待处理' \
                 ORDER BY b.priority DESC, t.created_at \
                 FOR UPDATE OF t SKIP LOCKED LIMIT 1",
            )
            .bind(account_id)
            .fetch_optional(&mut *tx)
            .await?;

            if let Some((reg_task_id, _attempts, _stage_ver)) = task_row {
                let execution_id = Uuid::new_v4();
                let lease_secs = state.config.scheduler.task_lease_secs as i64;
                let leased: (i32, i32, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
                    "UPDATE account_registration_tasks SET status = '已分配', stage = '已分配', \
                         attempts = attempts + 1, stage_version = stage_version + 1, \
                         lease_node_id = $2, lease_session_id = $3, lease_execution_id = $4, \
                         lease_expires_at = now() + ($5 || ' seconds')::interval, updated_at = now() \
                     WHERE id = $1 \
                     RETURNING attempts, stage_version, lease_expires_at",
                )
                .bind(reg_task_id)
                .bind(identity.node_id())
                .bind(session_id)
                .bind(execution_id)
                .bind(lease_secs.clamp(1, 24 * 3600).to_string())
                .fetch_one(&mut *tx)
                .await?;

                store::session::start_execution(
                    &mut tx,
                    &store::session::NewExecution {
                        id: execution_id,
                        task_id: None,
                        account_registration_task_id: Some(reg_task_id),
                        session_id,
                        node_id: identity.node_id(),
                        slot_index: session.slot_index,
                        account_id: session.account_id,
                        proxy_id: session.proxy_id,
                        task_type: platform_domain::TaskType::AccountRegister,
                        attempt: leased.0,
                        stage_version: leased.1,
                    },
                )
                .await?;

                tx.commit().await?;

                // Provider 配置按注册尝试生成任务级快照。API Key 只从独立密钥表
                // 解密到本次 mTLS 下行消息的内存中，不进入 NodeConfig、日志或数据库明文。
                let provider_record = store::mail_provider::get_active_config(&state.pool).await?;
                let mail_provider = if let Some(record) = provider_record {
                    // Outlook 密钥只能通过强制校验客户端证书的 mTLS 连接下发。
                    // 本地若显式关闭 mTLS，则自动降级为人工模式，绝不在 bearer-only
                    // 的 Worker 连接中传送第三方密钥。
                    if record.provider_type == "outlook_http"
                        && !state.config.security.require_client_cert
                    {
                        tracing::warn!(
                            node_id = %identity.node_id(),
                            provider_version = record.version,
                            "当前未强制 Worker 客户端证书，Outlook Provider 已安全降级为人工模式"
                        );
                        Some(pb::MailProviderLease {
                            version: record.version.max(0) as u64,
                            provider_type: "manual".to_string(),
                            endpoint: String::new(),
                            api_key: String::new(),
                            poll_interval_secs: 5,
                            timeout_secs: 120,
                            allowed_hosts: Vec::new(),
                            allowed_senders: Vec::new(),
                        })
                    } else {
                        let api_key = if record.provider_type == "outlook_http" {
                            match record.api_key_secret_ref.as_deref() {
                                Some(secret_ref) => {
                                    let cipher_text = store::mail_provider::get_secret_ciphertext(
                                        &state.pool,
                                        secret_ref,
                                    )
                                    .await?;
                                    match cipher_text {
                                        Some(cipher_text) => state
                                            .cipher
                                            .decrypt(&cipher_text)
                                            .map_err(AppError::Internal)?,
                                        None => String::new(),
                                    }
                                }
                                None => String::new(),
                            }
                        } else {
                            String::new()
                        };
                        Some(pb::MailProviderLease {
                            version: record.version.max(0) as u64,
                            provider_type: record.provider_type,
                            endpoint: record.endpoint,
                            api_key,
                            poll_interval_secs: record.poll_interval_secs.max(1) as u32,
                            timeout_secs: record.timeout_secs.max(10) as u32,
                            allowed_hosts: record.allowed_hosts,
                            allowed_senders: record.allowed_senders,
                        })
                    }
                } else {
                    Some(pb::MailProviderLease {
                        version: 0,
                        provider_type: "manual".to_string(),
                        endpoint: String::new(),
                        api_key: String::new(),
                        poll_interval_secs: 5,
                        timeout_secs: 120,
                        allowed_hosts: Vec::new(),
                        allowed_senders: Vec::new(),
                    })
                };

                let msg = convert::assign_registration_task_message(
                    convert::RegistrationTaskAssignment {
                        session_id,
                        execution_id,
                        registration_task_id: reg_task_id,
                        attempt: leased.0,
                        stage_version: leased.1,
                        lease_expires_at: leased.2,
                        needs_mail_code: true,
                        mail_provider,
                    },
                );
                let _ = outbound.send(msg).await;
            } else {
                tx.rollback().await?;
            }
        }
    }

    tracing::info!(node_id = %identity.node_id(), session_id = %session_id, slot = ready.slot_index, "会话已激活就绪");
    Ok(())
}

/// 统一调度：处理 WorkRequest。
async fn handle_work_request(
    state: &AppState,
    identity: &NodeIdentity,
    req: pb::WorkRequest,
    outbound: &mpsc::Sender<pb::MasterMessage>,
) -> AppResult<()> {
    scheduler::handle_work_request(
        state,
        identity.node_id(),
        req.slot_index as i32,
        &req.supported_task_types,
        outbound,
    )
    .await
}

/// 账号注册接受确认（outbox 去重）。
async fn handle_registration_task_accepted(
    state: &AppState,
    identity: &NodeIdentity,
    event_id: &str,
    replayed: bool,
    accepted: pb::RegistrationTaskAccepted,
    outbound: &mpsc::Sender<pb::MasterMessage>,
) -> AppResult<()> {
    let execution_id = parse_uuid(&accepted.execution_id, "执行编号")?;
    let reg_task_id = parse_uuid(&accepted.registration_task_id, "注册任务编号")?;
    let session_id = parse_optional_uuid(&accepted.session_id, "会话编号")?;

    let payload = serde_json::json!({
        "execution_id": accepted.execution_id,
        "registration_task_id": accepted.registration_task_id,
        "session_id": accepted.session_id,
        "accepted_at": accepted.accepted_at,
    });

    handle_deduped_event(
        DedupContext {
            state,
            identity,
            event_id,
            event_type: "账号注册任务接受",
            session_id,
            task_id: Some(reg_task_id),
            replayed,
            payload,
            outbound,
        },
        || async {
            let ok = scheduler::accept_registration_task(state, execution_id, reg_task_id).await?;
            if ok {
                Ok("账号注册任务已转为执行中".to_string())
            } else {
                Ok("任务状态已不处于待分配，已留档".to_string())
            }
        },
    )
    .await
}

/// 账号注册进度上报。
async fn handle_registration_task_progress(
    state: &AppState,
    _identity: &NodeIdentity,
    progress: pb::RegistrationTaskProgress,
) -> AppResult<()> {
    let execution_id = parse_uuid(&progress.execution_id, "执行编号")?;
    let reg_task_id = parse_uuid(&progress.registration_task_id, "注册任务编号")?;
    let stage_version = clamp_to_i32(progress.stage_version);

    scheduler::record_registration_progress(
        state,
        execution_id,
        reg_task_id,
        &progress.stage,
        stage_version,
    )
    .await?;

    Ok(())
}

/// 账号注册结果上报（outbox 去重 + 事务裁决）。
async fn handle_registration_task_result(
    state: &AppState,
    identity: &NodeIdentity,
    event_id: &str,
    replayed: bool,
    res: pb::RegistrationTaskResult,
    outbound: &mpsc::Sender<pb::MasterMessage>,
) -> AppResult<()> {
    let execution_id = parse_uuid(&res.execution_id, "执行编号")?;
    let reg_task_id = parse_uuid(&res.registration_task_id, "注册任务编号")?;
    let session_id = parse_uuid(&res.session_id, "会话编号")?;
    let result = res
        .result
        .parse::<ExecutionResult>()
        .unwrap_or(ExecutionResult::RetryableFailure);

    let report = scheduler::RegistrationResultReport {
        session_id,
        execution_id,
        registration_task_id: reg_task_id,
        node_id: Some(identity.node_id()),
        result,
        reason: res.reason.clone(),
        stage_version: clamp_to_i32(res.stage_version),
        attempt: clamp_to_i32(res.attempt),
        already_exists: res.already_exists,
        awaiting_verification: res.awaiting_verification,
        completed_at: if res.completed_at.is_empty() {
            None
        } else {
            Some(res.completed_at)
        },
    };

    let payload = serde_json::json!({
        "execution_id": res.execution_id,
        "registration_task_id": res.registration_task_id,
        "session_id": res.session_id,
        "result": res.result,
        "reason": res.reason,
        "already_exists": res.already_exists,
        "awaiting_verification": res.awaiting_verification,
    });

    handle_deduped_event(
        DedupContext {
            state,
            identity,
            event_id,
            event_type: "账号注册结果",
            session_id: Some(session_id),
            task_id: Some(reg_task_id),
            replayed,
            payload,
            outbound,
        },
        || async {
            let outcome = scheduler::submit_registration_result(state, &report).await?;
            if outcome.end_session {
                let _ = outbound
                    .send(convert::end_session_message(
                        session_id,
                        &format!("注册任务结束会话：{}", outcome.detail),
                        false,
                    ))
                    .await;
            }
            Ok(outcome.detail)
        },
    )
    .await
}

/// 待确认事项上报（验证码、风控等）。
async fn handle_manual_action_required(
    state: &AppState,
    identity: &NodeIdentity,
    event_id: &str,
    replayed: bool,
    req: pb::ManualActionRequired,
    outbound: &mpsc::Sender<pb::MasterMessage>,
) -> AppResult<()> {
    let task_type = req
        .task_type
        .parse::<TaskType>()
        .unwrap_or(TaskType::AccountRegister);
    let reg_task_id = parse_optional_uuid(&req.registration_task_id, "注册任务")?;
    let exec_id = parse_optional_uuid(&req.execution_id, "执行编号")?;
    let action_id = parse_uuid(&req.action_id, "人工事项编号")?;
    let action_type = req
        .action_type
        .parse::<platform_domain::ManualActionType>()
        .unwrap_or(platform_domain::ManualActionType::MailCode);

    let expires_at = if req.expires_at.is_empty() {
        chrono::Utc::now() + chrono::Duration::minutes(10)
    } else {
        chrono::DateTime::parse_from_rfc3339(&req.expires_at)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now() + chrono::Duration::minutes(10))
    };

    let payload = serde_json::json!({
        "action_id": req.action_id,
        "task_type": req.task_type,
        "registration_task_id": req.registration_task_id,
        "action_type": req.action_type,
        "prompt": req.prompt,
    });

    handle_deduped_event(
        DedupContext {
            state,
            identity,
            event_id,
            event_type: "人工确认请求",
            session_id: None,
            task_id: reg_task_id,
            replayed,
            payload,
            outbound,
        },
        || async {
            let new_action = store::manual_action::NewManualAction {
                id: action_id,
                task_type,
                registration_task_id: reg_task_id,
                book_task_id: None,
                execution_id: exec_id,
                node_id: Some(identity.node_id()),
                session_id: None,
                action_type,
                prompt: req.prompt.clone(),
                artifact_url: if req.optional_artifact_id.is_empty() {
                    None
                } else {
                    Some(req.optional_artifact_id.clone())
                },
                expires_at,
            };
            store::manual_action::create_action(&state.pool, &new_action).await?;

            state.events.publish(
                "人工确认变更",
                serde_json::json!({
                    "任务类型": task_type.as_str(),
                    "类型": action_type.as_str(),
                    "节点": identity.node_id(),
                }),
            );

            Ok("人工确认事项已创建".to_string())
        },
    )
    .await
}

/// 会话内申请下一本图书任务。
async fn handle_next_task_request(
    state: &AppState,
    identity: &NodeIdentity,
    req: pb::NextTaskRequest,
    outbound: &mpsc::Sender<pb::MasterMessage>,
) -> AppResult<()> {
    let node_id = identity.node_id();
    let session_id = parse_uuid(&req.session_id, "会话编号")?;

    let outcome = scheduler::claim_next_task(state, node_id, session_id).await?;
    match outcome {
        ClaimOutcome::Assigned(assignment) => {
            let msg = convert::assign_task_message(&assignment);
            if outbound.send(msg).await.is_err() {
                tracing::warn!(
                    node_id = %node_id,
                    session_id = %session_id,
                    "下发 AssignTask 失败：通道已关闭"
                );
            }
        }
        ClaimOutcome::Unavailable(unavail) => {
            let msg = convert::no_task_message(
                Some(session_id),
                &unavail.reason,
                unavail.retry_after_secs,
            );
            let _ = outbound.send(msg).await;
        }
        ClaimOutcome::SessionShouldEnd { reason } => {
            let msg = convert::end_session_message(session_id, &reason, false);
            let _ = outbound.send(msg).await;
        }
    }
    Ok(())
}

/// 任务接受确认（outbox 去重）。
async fn handle_task_accepted(
    state: &AppState,
    identity: &NodeIdentity,
    event_id: &str,
    replayed: bool,
    accepted: pb::TaskAccepted,
    outbound: &mpsc::Sender<pb::MasterMessage>,
) -> AppResult<()> {
    let execution_id = parse_uuid(&accepted.execution_id, "执行编号")?;
    let task_id = parse_uuid(&accepted.task_id, "任务编号")?;
    let session_id = parse_optional_uuid(&accepted.session_id, "会话编号")?;

    let payload = serde_json::json!({
        "execution_id": accepted.execution_id,
        "task_id": accepted.task_id,
        "session_id": accepted.session_id,
        "accepted_at": accepted.accepted_at,
    });

    handle_deduped_event(
        DedupContext {
            state,
            identity,
            event_id,
            event_type: "任务接受",
            session_id,
            task_id: Some(task_id),
            replayed,
            payload,
            outbound,
        },
        || async {
            let ok = scheduler::accept_task(state, execution_id, task_id).await?;
            if ok {
                Ok("任务已转为执行中".to_string())
            } else {
                Ok("任务状态已不处于待分配，已留档".to_string())
            }
        },
    )
    .await
}

/// 任务进度上报（直接应用，不参与 outbox 去重）。
async fn handle_task_progress(
    state: &AppState,
    _identity: &NodeIdentity,
    progress: pb::TaskProgress,
) -> AppResult<()> {
    let execution_id = parse_uuid(&progress.execution_id, "执行编号")?;
    let task_id = parse_uuid(&progress.task_id, "任务编号")?;
    let downloaded_bytes = clamp_to_i64(progress.downloaded_bytes);
    let total_bytes = clamp_to_i64(progress.total_bytes);
    let stage_version = clamp_to_i32(progress.stage_version);

    scheduler::record_progress(
        state,
        execution_id,
        task_id,
        downloaded_bytes,
        total_bytes,
        &progress.stage,
        stage_version,
    )
    .await?;

    Ok(())
}

/// 任务结果上报（outbox 去重 + 归因）。
async fn handle_task_result(
    state: &AppState,
    identity: &NodeIdentity,
    event_id: &str,
    replayed: bool,
    res: pb::TaskResult,
    outbound: &mpsc::Sender<pb::MasterMessage>,
) -> AppResult<()> {
    let execution_id = parse_uuid(&res.execution_id, "执行编号")?;
    let task_id = parse_uuid(&res.task_id, "任务编号")?;
    let session_id = parse_uuid(&res.session_id, "会话编号")?;
    let result = res
        .result
        .parse::<ExecutionResult>()
        .unwrap_or(ExecutionResult::RetryableFailure);

    let quota = if res.quota_total > 0 {
        Some((res.quota_used, res.quota_total))
    } else {
        None
    };

    let file = res.file.map(|f| FileEvidence {
        nas_relative_path: f.nas_relative_path,
        file_name: f.file_name,
        size_bytes: clamp_to_i64(f.size_bytes),
        sha256: f.sha256,
        format: f.format,
    });

    let report = ResultReport {
        session_id,
        execution_id,
        task_id,
        node_id: Some(identity.node_id()),
        result,
        reason: res.reason.clone(),
        stage_version: clamp_to_i32(res.stage_version),
        duration_ms: if res.duration_ms > 0 {
            Some(clamp_to_i64(res.duration_ms))
        } else {
            None
        },
        quota,
        file,
    };

    let payload = serde_json::json!({
        "execution_id": res.execution_id,
        "task_id": res.task_id,
        "session_id": res.session_id,
        "result": res.result,
        "reason": res.reason,
    });

    handle_deduped_event(
        DedupContext {
            state,
            identity,
            event_id,
            event_type: "任务结果",
            session_id: Some(session_id),
            task_id: Some(task_id),
            replayed,
            payload,
            outbound,
        },
        || async {
            let outcome = scheduler::submit_result(state, &report).await?;
            if outcome.end_session {
                let _ = outbound
                    .send(convert::end_session_message(
                        session_id,
                        &format!("任务结果要求结束会话：{}", outcome.detail),
                        false,
                    ))
                    .await;
            }
            Ok(outcome.detail)
        },
    )
    .await
}

/// NAS 检测/核验上报（outbox 去重）。
async fn handle_nas_check_result(
    state: &AppState,
    identity: &NodeIdentity,
    event_id: &str,
    replayed: bool,
    check: pb::NasCheckResult,
    outbound: &mpsc::Sender<pb::MasterMessage>,
) -> AppResult<()> {
    let node_id = identity.node_id();
    let task_id = parse_optional_uuid(&check.task_id, "核验任务")
        .ok()
        .flatten();

    let file_evidence = check.file.map(|f| FileEvidence {
        nas_relative_path: f.nas_relative_path,
        file_name: f.file_name,
        size_bytes: clamp_to_i64(f.size_bytes),
        sha256: f.sha256,
        format: f.format,
    });

    let payload = serde_json::json!({
        "node_id": node_id,
        "task_id": task_id,
        "mount_present": check.mount_present,
        "writable": check.writable,
        "free_gb": check.free_gb,
    });

    handle_deduped_event(
        DedupContext {
            state,
            identity,
            event_id,
            event_type: "NAS核验",
            session_id: None,
            task_id,
            replayed,
            payload,
            outbound,
        },
        || async {
            let report = NasCheckReport {
                node_id,
                task_id,
                mount_present: check.mount_present,
                writable: check.writable,
                free_gb: clamp_to_i64(check.free_gb),
                file: file_evidence.as_ref(),
                detail: &check.detail,
            };
            scheduler::nas_check_result(state, &report).await?;
            Ok("NAS 核验结果已处理".to_string())
        },
    )
    .await
}

/// 代理检测上报（outbox 去重）。
async fn handle_proxy_check_result(
    state: &AppState,
    identity: &NodeIdentity,
    event_id: &str,
    replayed: bool,
    check: pb::ProxyCheckResult,
    outbound: &mpsc::Sender<pb::MasterMessage>,
) -> AppResult<()> {
    let proxy_id = parse_uuid(&check.proxy_id, "代理编号")?;
    let exit_ip = if check.exit_ip.trim().is_empty() {
        None
    } else {
        Some(check.exit_ip.trim())
    };
    let latency_ms = if check.latency_ms > 0 {
        Some(clamp_to_i32(check.latency_ms as u32))
    } else {
        None
    };

    let payload = serde_json::json!({
        "proxy_id": check.proxy_id,
        "reachable": check.reachable,
        "exit_ip": check.exit_ip,
        "latency_ms": check.latency_ms,
    });

    handle_deduped_event(
        DedupContext {
            state,
            identity,
            event_id,
            event_type: "代理检测",
            session_id: None,
            task_id: None,
            replayed,
            payload,
            outbound,
        },
        || async {
            scheduler::proxy_check_result(
                state,
                proxy_id,
                check.reachable,
                exit_ip,
                latency_ms,
                &check.detail,
            )
            .await?;
            Ok("代理检测结果已记录".to_string())
        },
    )
    .await
}

/// 会话结束上报（outbox 去重）。
async fn handle_session_closed(
    state: &AppState,
    identity: &NodeIdentity,
    event_id: &str,
    replayed: bool,
    closed: pb::SessionClosed,
    outbound: &mpsc::Sender<pb::MasterMessage>,
) -> AppResult<()> {
    let session_id = parse_uuid(&closed.session_id, "会话编号")?;
    let status = closed
        .status
        .parse::<SessionStatus>()
        .unwrap_or(SessionStatus::Ended);

    let payload = serde_json::json!({
        "session_id": closed.session_id,
        "status": closed.status,
        "reason": closed.reason,
        "completed_count": closed.completed_count,
    });

    handle_deduped_event(
        DedupContext {
            state,
            identity,
            event_id,
            event_type: "会话结束",
            session_id: Some(session_id),
            task_id: None,
            replayed,
            payload,
            outbound,
        },
        || async {
            scheduler::session_closed(state, session_id, status, &closed.reason).await?;
            Ok("会话已结束".to_string())
        },
    )
    .await
}

/// Worker 日志上报。
async fn handle_worker_log(
    state: &AppState,
    identity: &NodeIdentity,
    log: pb::WorkerLog,
) -> AppResult<()> {
    let level = log.level.parse::<LogLevel>().unwrap_or(LogLevel::Info);
    let detail = if log.message.chars().count() > MAX_LOG_DETAIL {
        let truncated: String = log.message.chars().take(MAX_LOG_DETAIL).collect();
        format!("{truncated}…[已截断]")
    } else {
        log.message
    };

    let session_id = log.session_id.trim();
    let target = if session_id.is_empty() {
        identity.node_id().to_string()
    } else {
        format!("{}/{}", identity.node_id(), session_id)
    };

    store::admin::log(
        &state.pool,
        OperationSource::Worker,
        level,
        &identity.node.name,
        "Agent日志",
        &target,
        &detail,
    )
    .await?;

    Ok(())
}

/// 发送最新节点配置。
pub async fn send_node_config(
    state: &AppState,
    node: &WorkerNode,
    outbound: &mpsc::Sender<pb::MasterMessage>,
) {
    let layout_root = NasLayout::default().files_dir;
    let max_duration = state
        .config
        .scheduler
        .session_max_duration_secs
        .min(u32::MAX as u64) as u32;
    let msg = convert::node_config_message(node, &state.config, &layout_root, max_duration);
    let _ = outbound.send(msg).await;
}

struct DedupContext<'a> {
    pub state: &'a AppState,
    pub identity: &'a NodeIdentity,
    pub event_id: &'a str,
    pub event_type: &'static str,
    pub session_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub replayed: bool,
    pub payload: serde_json::Value,
    pub outbound: &'a mpsc::Sender<pb::MasterMessage>,
}

/// 统一流水线：**去重登记 → 应用 → 记账 → 确认**。
async fn handle_deduped_event<F, Fut>(ctx: DedupContext<'_>, process: F) -> AppResult<()>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = AppResult<String>>,
{
    if ctx.event_id.is_empty() {
        return Err(AppError::bad("事件编号不能为空"));
    }

    let incoming = store::session::IncomingEvent {
        event_id: ctx.event_id,
        node_id: Some(ctx.identity.node_id()),
        session_id: ctx.session_id,
        task_id: ctx.task_id,
        event_type: ctx.event_type,
        source: OperationSource::Worker,
        payload: ctx.payload,
        replayed: ctx.replayed,
    };

    // 1. 去重登记
    let is_new = store::session::remember_event(&ctx.state.pool, &incoming).await?;
    if !is_new {
        // 重复事件：直接确认让 Worker 从 outbox 删掉
        let _ = ctx
            .outbound
            .send(convert::ack(ctx.event_id, true, "重复事件，已忽略"))
            .await;
        return Ok(());
    }

    // 2. 应用
    match process().await {
        Ok(detail) => {
            // 3. 记账
            store::session::mark_event_applied(&ctx.state.pool, ctx.event_id, true, &detail)
                .await?;
            // 4. 确认
            let _ = ctx
                .outbound
                .send(convert::ack(ctx.event_id, true, &detail))
                .await;
        }
        Err(err) => {
            if is_retryable(&err) {
                tracing::warn!(
                    event_id = ctx.event_id,
                    error = %err,
                    "处理事件时发生可重试失败，撤销登记以允许重放"
                );
                // 撤销登记：让 Worker 的下一次重投能重新走入处理
                let _ = store::session::forget_event(&ctx.state.pool, ctx.event_id).await;
                let _ = ctx
                    .outbound
                    .send(convert::ack(
                        ctx.event_id,
                        false,
                        "服务器暂时繁忙，请稍后重试",
                    ))
                    .await;
            } else {
                let detail = err.to_string();
                tracing::error!(
                    event_id = ctx.event_id,
                    error = %err,
                    "处理事件时发生不可重试失败，记录留档"
                );
                store::session::mark_event_applied(&ctx.state.pool, ctx.event_id, false, &detail)
                    .await?;
                let _ = ctx
                    .outbound
                    .send(convert::ack(ctx.event_id, true, &detail))
                    .await;
            }
        }
    }

    Ok(())
}

/// 判断错误是否可由 Worker 重新重试。
fn is_retryable(error: &AppError) -> bool {
    matches!(error, AppError::Database(_) | AppError::Internal(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_error_classification() {
        assert!(is_retryable(&AppError::Database(sqlx::Error::PoolTimedOut)));
        assert!(is_retryable(&AppError::Internal(anyhow::anyhow!(
            "网络抖动"
        ))));
        assert!(!is_retryable(&AppError::bad("参数不合法")));
        assert!(!is_retryable(&AppError::missing("任务不存在")));
        assert!(!is_retryable(&AppError::conflict("冲突")));
    }
}
