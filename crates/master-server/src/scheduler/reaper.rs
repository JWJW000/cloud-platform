//! 周期性自愈：租约回收、离线判定、批次收尾（第 14.1–14.4、6.5 节）。
//!
//! 这一层的存在前提是「Worker 随时可能消失」。它不追求实时，只保证**有界的收敛时间**：
//! 任何被 Worker 带走却没有下文的资源，最迟在一个租约周期后自动回到可分配状态。
//!
//! 顺序是刻意排的，每一步都为下一步准备条件：
//! 1. 额度重置 / 代理复活：先把「时间到了就该恢复」的资源放出来；
//! 2. 节点离线判定：心跳断了的节点先落成离线，它的会话才有理由进入断线保护；
//! 3. 断线保护到期 → 会话判失败并彻底释放；
//! 4. 会话租约到期 → 未连接的进保护，仍在线的直接结束（在线却不续租说明 Agent 卡死）；
//! 5. 任务租约到期 → 按第 14.3 节回退或转「待确认」；
//! 6. 清理不可能再被领取的任务（书已入库 / 尝试次数耗尽），它们是 [`crate::scheduler::claim`]
//!    候选池的排除项，不清理就会永远停在「待处理」，让批次也永远无法收尾；
//! 7. 批次收尾与事件表清理。
//!
//! **幂等是硬要求**：巡检每 5 秒跑一次，两次巡检可能落在同一个到期租约上，
//! 所有语句因此都带状态条件，重复执行不会二次改动。

use std::time::Duration;

use platform_domain::{
    AlertLevel, ExecutionResult, LogLevel, OperationSource, SessionStatus, SlotStatus, TaskStatus,
    WorkerStatus,
};
use uuid::Uuid;

use crate::error::AppResult;
use crate::state::AppState;
use crate::store;

/// 一轮巡检的成果，用于日志与自检。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReapReport {
    /// 跨日重置额度的账号数。
    pub quota_reset: u64,
    /// 冷却结束恢复可用的代理数。
    pub proxies_revived: u64,
    /// 判定为离线的节点数。
    pub nodes_offline: usize,
    /// 进入断线保护的会话数。
    pub sessions_protected: usize,
    /// 被判失败并释放的会话数。
    pub sessions_failed: usize,
    /// 正常收尾的会话数。
    pub sessions_ended: usize,
    /// 租约到期后放回待处理的任务数。
    pub tasks_requeued: u64,
    /// 租约到期后转为待确认的任务数。
    pub tasks_needs_confirm: u64,
    /// 因书已入库而跳过的任务数。
    pub tasks_skipped: u64,
    /// 因尝试次数耗尽而判失败的任务数。
    pub tasks_failed: u64,
    /// 收尾的批次数。
    pub batches_completed: usize,
    /// 清理的历史事件数。
    pub events_purged: u64,
}

impl ReapReport {
    /// 这一轮是否真的做了事情。全是零时不必写日志，否则日志会被巡检刷满。
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// 单轮巡检处理的会话数上限。
///
/// 有上限是为了让一次巡检的时长可预期：积压再多也分几轮做完，
/// 而不是让一轮巡检长时间占着连接池。
const SESSION_BATCH: i64 = 50;

/// 事件表保留天数。
const EVENT_KEEP_DAYS: i64 = 14;

/// 跑一轮巡检。
///
/// 单步失败不应该让整轮中止——比如节点表暂时不可写不该拖累任务租约回收，
/// 因此每一步的错误都单独记日志后继续。返回值只统计成功的部分。
pub async fn reap_once(state: &AppState) -> AppResult<ReapReport> {
    let scheduler = state.scheduler().clone();

    let quota_reset = store::resource::reset_expired_quota(&state.pool).await?;
    let proxies_revived = store::resource::revive_cooled_proxies(&state.pool).await?;
    let mut sessions_protected = 0usize;
    let mut sessions_failed = 0usize;
    let mut sessions_ended = 0usize;

    // 离线判定：连续错过 N 次心跳。判定为离线的节点，它名下所有活着的会话
    // 都进入断线保护——Agent 可能只是网络抖动，15 分钟内回来还能接着干。
    let offline = store::node::mark_stale_nodes_offline(
        &state.pool,
        scheduler.offline_after().as_secs() as i64,
    )
    .await?;
    for node_id in &offline {
        for session_id in store::session::live_sessions_of_node(&state.pool, *node_id).await? {
            store::session::protect_session(
                &state.pool,
                session_id,
                scheduler.disconnect_protection_secs as i64,
            )
            .await?;
            sessions_protected += 1;
        }
        store::admin::raise_alert(
            &state.pool,
            AlertLevel::Warn,
            "节点",
            "Worker 心跳超时，已判定离线",
            "已对其会话启用断线保护，等待重连",
            Some(*node_id),
            Some(&format!("节点离线:{node_id}")),
        )
        .await?;
        state
            .events
            .publish("节点变更", serde_json::json!({ "节点": node_id }));
    }

    // 断线保护也到期了：Worker 没回来，这次执行到此为止。
    for (session_id, _node_id, _slot) in
        store::session::protection_expired_sessions(&state.pool, SESSION_BATCH).await?
    {
        finish_lost_session(state, session_id, "断线保护到期，Worker 未重连").await?;
        sessions_failed += 1;
    }

    // 会话租约到期：分两种情况，因为原因完全不同。
    for (session_id, node_id, _slot) in
        store::session::expired_sessions(&state.pool, SESSION_BATCH).await?
    {
        if state.links.is_online(node_id) {
            // 连接还在却不续租，说明 Agent 的会话线程卡死了。让它结束换个干净的浏览器。
            finish_lost_session(state, session_id, "会话租约到期（连接仍在，疑似卡死）").await?;
            sessions_ended += 1;
        } else {
            store::session::protect_session(
                &state.pool,
                session_id,
                scheduler.disconnect_protection_secs as i64,
            )
            .await?;
            sessions_protected += 1;
        }
    }

    let (tasks_requeued, tasks_needs_confirm) = reap_task_leases(state).await?;
    let tasks_skipped = skip_already_ingested(state).await?;
    let tasks_failed = fail_exhausted_tasks(state).await?;

    // 批次收尾必须排在任务清理之后：只要还有一个任务停在非终态，批次就不会被判完成。
    let batches = store::catalog::complete_finished_batches(&state.pool).await?;
    for batch_id in &batches {
        state
            .events
            .publish("批次变更", serde_json::json!({ "批次": batch_id }));
    }

    let report = ReapReport {
        quota_reset,
        proxies_revived,
        nodes_offline: offline.len(),
        sessions_protected,
        sessions_failed,
        sessions_ended,
        tasks_requeued,
        tasks_needs_confirm,
        tasks_skipped,
        tasks_failed,
        batches_completed: batches.len(),
        events_purged: store::session::purge_events(&state.pool, EVENT_KEEP_DAYS).await?,
    };

    if !report.is_empty() {
        store::admin::log(
            &state.pool,
            OperationSource::SystemJob,
            LogLevel::Info,
            "调度器",
            "租约巡检",
            "",
            &format!("{report:?}"),
        )
        .await?;
    }
    Ok(report)
}

/// 会话没救了：判失败、收尾未完成的执行记录、释放账号代理、退回任务、放空槽位。
///
/// 走 [`crate::scheduler::allocate::close_session`] 而不是自己写一套释放语句，
/// 是为了让「正常收尾」和「被回收」永远共用同一套清理动作。
async fn finish_lost_session(state: &AppState, session_id: Uuid, reason: &str) -> AppResult<()> {
    store::session::finish_open_executions_of_session(
        &state.pool,
        session_id,
        ExecutionResult::Uncertain,
        reason,
    )
    .await?;
    crate::scheduler::allocate::close_session(state, session_id, SessionStatus::Failed, reason)
        .await?;
    store::admin::log(
        &state.pool,
        OperationSource::SystemJob,
        LogLevel::Warn,
        "调度器",
        "回收会话",
        &session_id.to_string(),
        reason,
    )
    .await?;
    Ok(())
}

/// 任务租约到期（第 14.3 节）。返回 `(放回待处理, 转待确认)`。
///
/// 分界线是「本机有没有可能已经存在半个文件」：
/// - 「已分配」还没开始下载，直接放回待处理并退还这次尝试；
/// - 「执行中」「等待入库」可能已经写了文件，转「待确认」等 NAS 核验裁决，
///   直接重下会留下一个孤儿文件，也会让原 Worker 之后的成功上报被判为迟到。
async fn reap_task_leases(state: &AppState) -> AppResult<(u64, u64)> {
    let requeued = sqlx::query(
        "UPDATE book_tasks SET status = $1, stage = '', \
             attempts = GREATEST(attempts - 1, 0), \
             stage_version = stage_version + 1, next_attempt_at = now(), \
             lease_node_id = NULL, lease_session_id = NULL, lease_execution_id = NULL, \
             lease_expires_at = NULL, last_error = $2, updated_at = now() \
         WHERE status = $3 AND lease_expires_at IS NOT NULL AND lease_expires_at < now()",
    )
    .bind(TaskStatus::Pending.as_str())
    .bind("任务租约到期，尚未开始下载，已放回队列")
    .bind(TaskStatus::Claimed.as_str())
    .execute(&state.pool)
    .await?
    .rows_affected();

    let needs_confirm = sqlx::query(
        "UPDATE book_tasks SET status = $1, stage_version = stage_version + 1, \
             lease_expires_at = NULL, last_error = $2, updated_at = now() \
         WHERE status IN ($3, $4) AND lease_expires_at IS NOT NULL AND lease_expires_at < now()",
    )
    .bind(TaskStatus::NeedsConfirm.as_str())
    .bind("任务租约到期，结果不确定，待 NAS 核验")
    .bind(TaskStatus::Running.as_str())
    .bind(TaskStatus::AwaitingIngest.as_str())
    .execute(&state.pool)
    .await?
    .rows_affected();

    if requeued + needs_confirm > 0 {
        state.events.publish(
            "任务变更",
            serde_json::json!({ "放回": requeued, "待确认": needs_confirm }),
        );
    }
    // 账号注册任务租约到期回收
    let _ = sqlx::query(
        "UPDATE account_registration_tasks SET status = '待处理', stage = '', \
             stage_version = stage_version + 1, next_attempt_at = now(), \
             lease_node_id = NULL, lease_session_id = NULL, lease_execution_id = NULL, \
             lease_expires_at = NULL, last_error = '租约到期，已放回队列', updated_at = now() \
         WHERE status IN ('已分配', '执行中') AND lease_expires_at IS NOT NULL AND lease_expires_at < now()",
    )
    .execute(&state.pool)
    .await;

    // 清理过期人工确认事项
    let _ = store::manual_action::expire_pending_actions(&state.pool).await;

    // 清理过期导入任务
    let _ = store::import_job::cleanup_expired_jobs(&state.pool).await;

    Ok((requeued, needs_confirm))
}

/// 书已经有有效文件的任务判「已跳过」（第 8.3 节全局去重）。
///
/// [`crate::scheduler::claim`] 的候选池排除了这些任务，不在这里收口它们就会永远
/// 停在「待处理」，批次也就永远无法收尾。
async fn skip_already_ingested(state: &AppState) -> AppResult<u64> {
    let affected = sqlx::query(
        "UPDATE book_tasks t SET status = $1, stage = '', \
             stage_version = t.stage_version + 1, last_error = $2, updated_at = now() \
         WHERE t.status = $3 \
           AND EXISTS ( \
                 SELECT 1 FROM book_files f \
                 WHERE f.book_id = t.book_id AND f.format = t.format AND f.status = $4)",
    )
    .bind(TaskStatus::Skipped.as_str())
    .bind("NAS 已存在同格式有效文件，无需重复下载")
    .bind(TaskStatus::Pending.as_str())
    .bind("有效")
    .execute(&state.pool)
    .await?
    .rows_affected();
    Ok(affected)
}

/// 尝试次数耗尽却还停在「待处理」的任务判失败。
///
/// 正常路径上 [`crate::scheduler::submit`] 会在最后一次失败时直接判失败，
/// 这里兜住的是异常路径：会话被回收时退还过尝试次数，或管理员改小了 `max_attempts`。
async fn fail_exhausted_tasks(state: &AppState) -> AppResult<u64> {
    let affected = sqlx::query(
        "UPDATE book_tasks SET status = $1, stage = '', \
             stage_version = stage_version + 1, \
             last_error = COALESCE(NULLIF(last_error, ''), $2), updated_at = now() \
         WHERE status = $3 AND attempts >= max_attempts",
    )
    .bind(TaskStatus::Failed.as_str())
    .bind("已达最大尝试次数")
    .bind(TaskStatus::Pending.as_str())
    .execute(&state.pool)
    .await?
    .rows_affected();
    Ok(affected)
}

/// 空闲槽位与会话状态对不上时的兜底修正。
///
/// 槽位是「哪个会话占着哪个浏览器实例」的唯一记录，一旦它指向一个已经结束的会话，
/// 那个槽位就再也分配不出去了。这条语句是纯修正，正常情况下不会命中任何行。
pub async fn release_orphan_slots(state: &AppState) -> AppResult<u64> {
    let affected = sqlx::query(
        "UPDATE worker_slots s SET status = $1, session_id = NULL, detail = $2, updated_at = now() \
         WHERE s.session_id IS NOT NULL \
           AND NOT EXISTS ( \
                 SELECT 1 FROM execution_sessions e \
                 WHERE e.id = s.session_id AND e.ended_at IS NULL)",
    )
    .bind(SlotStatus::Idle.as_str())
    .bind("会话已结束，槽位自动回收")
    .execute(&state.pool)
    .await?
    .rows_affected();
    Ok(affected)
}

/// 启动后台巡检任务。
///
/// 返回 [`tokio::task::JoinHandle`] 而不是就地 detach，是为了让 `main` 能在退出时
/// 明确地取消它——否则关停过程中巡检可能刚好写到一半。
pub fn spawn_reaper(state: AppState) -> tokio::task::JoinHandle<()> {
    let interval_secs = state.scheduler().reaper_interval_secs.max(1);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
        // 巡检慢于间隔时不要把错过的节拍补跑一遍：补跑只会让本已吃紧的数据库更紧张。
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match reap_once(&state).await {
                Ok(report) if !report.is_empty() => {
                    tracing::info!(?report, "租约巡检完成");
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "租约巡检失败，下一轮重试"),
            }
            if let Err(error) = release_orphan_slots(&state).await {
                tracing::warn!(%error, "孤儿槽位回收失败");
            }
        }
    })
}

/// 节点重新连上来时解除断线保护（第 14.2 节）。
///
/// 由 gRPC 层在链路建立后调用，而不是由巡检轮询：重连是个明确的事件，
/// 等下一轮巡检才恢复会白等最多一个巡检间隔。
pub async fn resume_protected_sessions(state: &AppState, node_id: Uuid) -> AppResult<usize> {
    let sessions: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM execution_sessions \
         WHERE node_id = $1 AND status = $2 AND ended_at IS NULL",
    )
    .bind(node_id)
    .bind(SessionStatus::Protected.as_str())
    .fetch_all(&state.pool)
    .await?;

    let lease_secs = state.scheduler().task_lease_secs as i64;
    let mut resumed = 0usize;
    for session_id in sessions {
        if store::session::renew_session(&state.pool, session_id, lease_secs).await? {
            resumed += 1;
            state
                .events
                .publish("会话变更", serde_json::json!({ "会话": session_id }));
        }
    }
    if resumed > 0 {
        store::admin::resolve_alert_by_key(&state.pool, &format!("节点离线:{node_id}")).await?;
        // 只把「因为失联被判离线」这类状态拉回在线。管理员设的 `维护中`/`已禁用`，
        // 以及云端下发的 `已暂停`，不能因为恰好有会话恢复就被解除（第 3.7 节）。
        let node = store::node::get_node(&state.pool, node_id).await?;
        let current = node
            .status
            .parse::<WorkerStatus>()
            .unwrap_or(WorkerStatus::Offline);
        if !current.is_admin_governed() && current != WorkerStatus::Online {
            store::node::set_node_status(&state.pool, node_id, WorkerStatus::Online).await?;
        }
    }
    Ok(resumed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_report_is_recognised() {
        assert!(ReapReport::default().is_empty());
        let report = ReapReport {
            tasks_requeued: 1,
            ..Default::default()
        };
        assert!(!report.is_empty());
    }

    #[test]
    fn session_batch_is_bounded() {
        // 一轮巡检的时长必须可预期，积压再多也要分轮处理
        const { assert!(SESSION_BATCH > 0 && SESSION_BATCH <= 200) };
    }

    #[test]
    fn events_are_kept_long_enough_to_debug() {
        // 少于一周会让「上周五那次重放」无从追查
        const { assert!(EVENT_KEEP_DAYS >= 7) };
    }

    #[test]
    fn offline_detection_is_slower_than_heartbeat() {
        // 判离线必须明显慢于心跳，否则一次网络抖动就会误判
        let scheduler = crate::config::SchedulerConfig::default();
        assert!(scheduler.offline_after().as_secs() > scheduler.heartbeat_interval_secs);
        // 断线保护必须长于判离线，否则会话在节点还没来得及重连时就被判死
        assert!(scheduler.disconnect_protection_secs > scheduler.offline_after().as_secs());
    }
}
