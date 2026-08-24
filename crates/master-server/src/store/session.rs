//! 执行会话、执行记录与事件幂等表（第 6.4 / 3.3 / 14 节）。
//!
//! 这三张表合起来回答「某本书当时是谁、用哪个账号、走哪条代理、第几次尝试下载的」，
//! 是事后归因唯一的依据，因此写入路径必须保证：
//! - 每次分配都产生一个新的**执行编号**（`task_executions.id`），迟到的上报靠它被识别；
//! - 每个来自 Worker 的事件编号只被应用一次（`task_events` 主键去重）。

use platform_domain::{ExecutionResult, SessionStatus, TaskType};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::{ExecutionSession, TaskExecution};

const SESSION_COLUMNS: &str = "id, node_id, slot_index, account_id, proxy_id, task_type, \
     status, local_forward_port, completed_count, lease_expires_at, protected_until, \
     started_at, ended_at, end_reason";

const EXECUTION_COLUMNS: &str = "id, task_id, account_registration_task_id, session_id, node_id, slot_index, account_id, \
     proxy_id, task_type, attempt, stage_version, result, error, duration_ms, started_at, finished_at";

// ---------------------------------------------------------------- 会话

/// 新建会话所需的字段。
#[derive(Debug, Clone)]
pub struct NewSession {
    /// 节点编号。
    pub node_id: Uuid,
    /// 槽位序号。
    pub slot_index: i32,
    /// 账号编号（NAS 核验、代理检测不需要账号）。
    pub account_id: Option<Uuid>,
    /// 代理编号。
    pub proxy_id: Option<Uuid>,
    /// 任务类型。
    pub task_type: TaskType,
    /// 本机固定转发端口。
    pub local_forward_port: Option<i32>,
    /// 租约时长（秒）。
    pub lease_secs: i64,
}

/// 插入一条会话记录。
///
/// 会话必须与槽位、账号、代理的占用在**同一个事务**里写入，因此这里只接受
/// 连接（而非连接池）：调用方（[`crate::scheduler`]）负责事务边界。
pub async fn create_session(
    tx: &mut sqlx::PgConnection,
    new: &NewSession,
) -> AppResult<ExecutionSession> {
    let session = sqlx::query_as::<_, ExecutionSession>(&format!(
        "INSERT INTO execution_sessions \
             (id, node_id, slot_index, account_id, proxy_id, task_type, status, \
              local_forward_port, lease_expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now() + ($9 || ' seconds')::interval) \
         RETURNING {SESSION_COLUMNS}"
    ))
    .bind(Uuid::new_v4())
    .bind(new.node_id)
    .bind(new.slot_index)
    .bind(new.account_id)
    .bind(new.proxy_id)
    .bind(new.task_type.as_str())
    .bind(SessionStatus::Creating.as_str())
    .bind(new.local_forward_port)
    .bind(new.lease_secs.clamp(1, 24 * 3600).to_string())
    .fetch_one(&mut *tx)
    .await?;
    Ok(session)
}

/// 单个会话。
pub async fn get_session(executor: impl PgExecutor<'_>, id: Uuid) -> AppResult<ExecutionSession> {
    sqlx::query_as::<_, ExecutionSession>(&format!(
        "SELECT {SESSION_COLUMNS} FROM execution_sessions WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::missing("会话不存在"))
}

/// 会话列表，可按状态与节点过滤。
pub async fn list_sessions(
    executor: impl PgExecutor<'_>,
    status: Option<&str>,
    node_id: Option<Uuid>,
    limit: i64,
) -> AppResult<Vec<ExecutionSession>> {
    let sessions = sqlx::query_as::<_, ExecutionSession>(&format!(
        "SELECT {SESSION_COLUMNS} FROM execution_sessions \
         WHERE ($1::text IS NULL OR status = $1) \
           AND ($2::uuid IS NULL OR node_id = $2) \
         ORDER BY started_at DESC LIMIT $3"
    ))
    .bind(status)
    .bind(node_id)
    .bind(limit.clamp(1, 500))
    .fetch_all(executor)
    .await?;
    Ok(sessions)
}

/// 会话进入运行中。
pub async fn activate_session(executor: impl PgExecutor<'_>, id: Uuid) -> AppResult<()> {
    let affected = sqlx::query(
        "UPDATE execution_sessions SET status = $2, last_renewed_at = now() \
         WHERE id = $1 AND status = $3",
    )
    .bind(id)
    .bind(SessionStatus::Running.as_str())
    .bind(SessionStatus::Creating.as_str())
    .execute(executor)
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(AppError::conflict("会话不存在或已不处于创建中"));
    }
    Ok(())
}

/// 续租（第 14.2 节：Worker 每 30 秒续一次）。
///
/// 返回 `false` 表示这个会话已经被回收过了——Worker 应当据此丢弃本地状态，
/// 而不是继续把结果往一个已经不属于它的会话上报。
pub async fn renew_session(
    executor: impl PgExecutor<'_>,
    id: Uuid,
    lease_secs: i64,
) -> AppResult<bool> {
    let affected = sqlx::query(
        "UPDATE execution_sessions SET \
             lease_expires_at = now() + ($2 || ' seconds')::interval, \
             last_renewed_at = now(), \
             status = CASE WHEN status = $4 THEN $3 ELSE status END, \
             protected_until = CASE WHEN status = $4 THEN NULL ELSE protected_until END \
         WHERE id = $1 AND status IN ($3, $4, $5)",
    )
    .bind(id)
    .bind(lease_secs.clamp(1, 24 * 3600).to_string())
    .bind(SessionStatus::Running.as_str())
    .bind(SessionStatus::Protected.as_str())
    .bind(SessionStatus::Creating.as_str())
    .execute(executor)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// 会话内完成计数加一，同时把账号的当日用量加一。
///
/// 两处计数放在同一条语句之外但同一个事务里由调用方保证；这里只动会话，
/// 账号用量由 [`consume_account_quota`] 负责，便于 NAS 核验之类不消耗额度的会话复用本函数。
pub async fn bump_completed(executor: impl PgExecutor<'_>, id: Uuid) -> AppResult<()> {
    sqlx::query(
        "UPDATE execution_sessions SET completed_count = completed_count + 1 WHERE id = $1",
    )
    .bind(id)
    .execute(executor)
    .await?;
    Ok(())
}

/// 账号当日用量加一，用满则自动落到 `今日额度耗尽`。
pub async fn consume_account_quota(
    executor: impl PgExecutor<'_>,
    account_id: Uuid,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE accounts SET daily_used = daily_used + 1, \
             status = CASE WHEN daily_used + 1 >= daily_limit THEN $2 ELSE status END, \
             reset_date = current_date, updated_at = now() \
         WHERE id = $1",
    )
    .bind(account_id)
    .bind(platform_domain::AccountStatus::ExhaustedToday.as_str())
    .execute(executor)
    .await?;
    Ok(())
}

/// 让会话进入断线保护（第 14.4 节）。
pub async fn protect_session(
    executor: impl PgExecutor<'_>,
    id: Uuid,
    protect_secs: i64,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE execution_sessions SET status = $2, \
             protected_until = now() + ($3 || ' seconds')::interval \
         WHERE id = $1",
    )
    .bind(id)
    .bind(SessionStatus::Protected.as_str())
    .bind(protect_secs.clamp(1, 24 * 3600).to_string())
    .execute(executor)
    .await?;
    Ok(())
}

/// 结束会话并记录原因。
pub async fn end_session(
    executor: impl PgExecutor<'_>,
    id: Uuid,
    status: SessionStatus,
    reason: &str,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE execution_sessions SET status = $2, ended_at = now(), end_reason = $3 \
         WHERE id = $1 AND ended_at IS NULL",
    )
    .bind(id)
    .bind(status.as_str())
    .bind(reason)
    .execute(executor)
    .await?;
    Ok(())
}

/// 释放会话占用的账号与代理。
///
/// 账号没有「已占用」状态，占用只体现在 `lease_session_id` 上，因此释放就是清空租约；
/// 代理则要从 `已占用` 回到 `可用`，但**不能**把冷却中或异常的代理也一并放回：
/// 那会让刚被判定有问题的代理立刻被下一个会话领走。
pub async fn release_session_resources(
    tx: &mut sqlx::PgConnection,
    session_id: Uuid,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE accounts SET lease_session_id = NULL, lease_expires_at = NULL, updated_at = now() \
         WHERE lease_session_id = $1",
    )
    .bind(session_id)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "UPDATE proxies SET lease_session_id = NULL, lease_expires_at = NULL, \
             status = CASE WHEN retire_after_release THEN '已停用' \
                           WHEN status = $2 THEN $3 \
                           ELSE status END, \
             retire_after_release = FALSE, \
             updated_at = now() \
         WHERE lease_session_id = $1",
    )
    .bind(session_id)
    .bind(platform_domain::ProxyStatus::Occupied.as_str())
    .bind(platform_domain::ProxyStatus::Available.as_str())
    .execute(&mut *tx)
    .await?;

    Ok(())
}

/// 租约已过期、但还没进入断线保护的会话。
///
/// 返回 `(会话编号, 节点编号, 槽位序号)`，回收流程需要这三个值把槽位也一起复位。
pub async fn expired_sessions(
    executor: impl PgExecutor<'_>,
    limit: i64,
) -> AppResult<Vec<(Uuid, Uuid, i32)>> {
    let rows: Vec<(Uuid, Uuid, i32)> = sqlx::query_as(
        "SELECT id, node_id, slot_index FROM execution_sessions \
         WHERE status IN ($1, $2, $3) AND lease_expires_at < now() \
         ORDER BY lease_expires_at LIMIT $4",
    )
    .bind(SessionStatus::Creating.as_str())
    .bind(SessionStatus::Running.as_str())
    .bind(SessionStatus::Draining.as_str())
    .bind(limit.clamp(1, 200))
    .fetch_all(executor)
    .await?;
    Ok(rows)
}

/// 断线保护也已到期的会话，这些会话要被判失败并彻底释放资源。
pub async fn protection_expired_sessions(
    executor: impl PgExecutor<'_>,
    limit: i64,
) -> AppResult<Vec<(Uuid, Uuid, i32)>> {
    let rows: Vec<(Uuid, Uuid, i32)> = sqlx::query_as(
        "SELECT id, node_id, slot_index FROM execution_sessions \
         WHERE status = $1 AND protected_until IS NOT NULL AND protected_until < now() \
         ORDER BY protected_until LIMIT $2",
    )
    .bind(SessionStatus::Protected.as_str())
    .bind(limit.clamp(1, 200))
    .fetch_all(executor)
    .await?;
    Ok(rows)
}

/// 某节点当前未结束的会话，节点掉线时用来逐个进入断线保护。
pub async fn live_sessions_of_node(
    executor: impl PgExecutor<'_>,
    node_id: Uuid,
) -> AppResult<Vec<Uuid>> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM execution_sessions WHERE node_id = $1 AND ended_at IS NULL",
    )
    .bind(node_id)
    .fetch_all(executor)
    .await?;
    Ok(ids)
}

/// 未结束会话总数，总览页用。
pub async fn count_live_sessions(executor: impl PgExecutor<'_>) -> AppResult<i64> {
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM execution_sessions WHERE ended_at IS NULL")
            .fetch_one(executor)
            .await?;
    Ok(count)
}

// ---------------------------------------------------------------- 执行记录

/// 新建执行记录所需的字段。
#[derive(Debug, Clone)]
pub struct NewExecution {
    /// 执行编号。由调用方生成，因为它同时要写进任务的 `lease_execution_id`。
    pub id: Uuid,
    /// 图书任务编号（图书下载时填写）。
    pub task_id: Option<Uuid>,
    /// 账号注册任务编号（账号注册时填写）。
    pub account_registration_task_id: Option<Uuid>,
    /// 会话编号。
    pub session_id: Uuid,
    /// 节点编号。
    pub node_id: Uuid,
    /// 槽位序号。
    pub slot_index: i32,
    /// 账号编号。
    pub account_id: Option<Uuid>,
    /// 代理编号。
    pub proxy_id: Option<Uuid>,
    /// 任务类型。
    pub task_type: TaskType,
    /// 第几次尝试。
    pub attempt: i32,
    /// 分配时任务的阶段版本，用于识别迟到事件。
    pub stage_version: i32,
}

/// 记录一次分配。必须与任务的租约写入同一个事务。
pub async fn start_execution(tx: &mut sqlx::PgConnection, new: &NewExecution) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO task_executions \
             (id, task_id, account_registration_task_id, session_id, node_id, slot_index, account_id, proxy_id, \
              task_type, attempt, stage_version) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(new.id)
    .bind(new.task_id)
    .bind(new.account_registration_task_id)
    .bind(new.session_id)
    .bind(new.node_id)
    .bind(new.slot_index)
    .bind(new.account_id)
    .bind(new.proxy_id)
    .bind(new.task_type.as_str())
    .bind(new.attempt.max(1))
    .bind(new.stage_version)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// 收尾一条执行记录。
///
/// `WHERE finished_at IS NULL` 让重复上报只落一次结果：Worker 的重试与
/// outbox 重放都可能把同一个结果送来两次，先到的那次才是真相。
pub async fn finish_execution(
    executor: impl PgExecutor<'_>,
    id: Uuid,
    result: ExecutionResult,
    error: Option<&str>,
    duration_ms: Option<i64>,
) -> AppResult<bool> {
    let affected = sqlx::query(
        "UPDATE task_executions SET result = $2, error = $3, duration_ms = $4, \
             finished_at = now() \
         WHERE id = $1 AND finished_at IS NULL",
    )
    .bind(id)
    .bind(result.as_str())
    .bind(error)
    .bind(duration_ms)
    .execute(executor)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// 把会话里所有未收尾的执行记录一并判定，回收租约时用。
pub async fn finish_open_executions_of_session(
    executor: impl PgExecutor<'_>,
    session_id: Uuid,
    result: ExecutionResult,
    error: &str,
) -> AppResult<u64> {
    let affected = sqlx::query(
        "UPDATE task_executions SET result = $2, error = $3, finished_at = now() \
         WHERE session_id = $1 AND finished_at IS NULL",
    )
    .bind(session_id)
    .bind(result.as_str())
    .bind(error)
    .execute(executor)
    .await?
    .rows_affected();
    Ok(affected)
}

/// 某任务的执行历史。
pub async fn list_executions(
    executor: impl PgExecutor<'_>,
    task_id: Uuid,
    limit: i64,
) -> AppResult<Vec<TaskExecution>> {
    let rows = sqlx::query_as::<_, TaskExecution>(&format!(
        "SELECT {EXECUTION_COLUMNS} FROM task_executions WHERE task_id = $1 \
         ORDER BY started_at DESC LIMIT $2"
    ))
    .bind(task_id)
    .bind(limit.clamp(1, 200))
    .fetch_all(executor)
    .await?;
    Ok(rows)
}

/// 一条执行记录的归因要素。
///
/// 结果上报时要回答三个问题：这次执行属于哪个任务、该由谁背这次失败（账号还是代理）、
/// 以及它是否已经被新的分配作废（`stage_version`）。这些字段就是判断的全部输入。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ExecutionContext {
    /// 图书任务编号。
    pub task_id: Option<Uuid>,
    /// 账号注册任务编号。
    pub account_registration_task_id: Option<Uuid>,
    /// 会话编号。
    pub session_id: Option<Uuid>,
    /// 账号编号。
    pub account_id: Option<Uuid>,
    /// 代理编号。
    pub proxy_id: Option<Uuid>,
    /// 分配时任务的阶段版本。
    pub stage_version: i32,
}

/// 取一条执行记录的归因要素。
pub async fn execution_context(
    executor: impl PgExecutor<'_>,
    id: Uuid,
) -> AppResult<Option<ExecutionContext>> {
    let row = sqlx::query_as::<_, ExecutionContext>(
        "SELECT task_id, account_registration_task_id, session_id, account_id, proxy_id, stage_version \
         FROM task_executions WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(executor)
    .await?;
    Ok(row)
}

/// 最近的执行记录，界面上「刚刚发生了什么」用。
pub async fn recent_executions(pool: &PgPool, limit: i64) -> AppResult<Vec<TaskExecution>> {
    let rows = sqlx::query_as::<_, TaskExecution>(&format!(
        "SELECT {EXECUTION_COLUMNS} FROM task_executions \
         ORDER BY started_at DESC LIMIT $1"
    ))
    .bind(limit.clamp(1, 200))
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

// ---------------------------------------------------------------- 事件幂等

/// 一条待记账的事件。
#[derive(Debug, Clone)]
pub struct IncomingEvent<'a> {
    /// 事件编号，由 Worker 生成，全局唯一。
    pub event_id: &'a str,
    /// 节点编号。
    pub node_id: Option<Uuid>,
    /// 会话编号。
    pub session_id: Option<Uuid>,
    /// 任务编号。
    pub task_id: Option<Uuid>,
    /// 事件类型（技术标识）。
    pub event_type: &'a str,
    /// 来源。
    pub source: platform_domain::OperationSource,
    /// 原始载荷。
    pub payload: serde_json::Value,
    /// 是否为 outbox 重放。
    pub replayed: bool,
}

/// 登记一个事件，返回 `true` 表示第一次见到它。
///
/// 至少一次投递（第 3.3 节）意味着同一个事件一定会重复到达，去重责任在 Master。
/// 这里用主键冲突而不是「先查后插」：两条 gRPC 流同时把同一个重放事件送上来时，
/// 先查后插会双双认为自己是第一次，从而把一次完成算成两次。
pub async fn remember_event(
    executor: impl PgExecutor<'_>,
    event: &IncomingEvent<'_>,
) -> AppResult<bool> {
    let inserted: Option<String> = sqlx::query_scalar(
        "INSERT INTO task_events \
             (event_id, node_id, session_id, task_id, event_type, source, payload, replayed) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         ON CONFLICT (event_id) DO NOTHING RETURNING event_id",
    )
    .bind(event.event_id)
    .bind(event.node_id)
    .bind(event.session_id)
    .bind(event.task_id)
    .bind(event.event_type)
    .bind(event.source.as_str())
    .bind(&event.payload)
    .bind(event.replayed)
    .fetch_optional(executor)
    .await?;
    Ok(inserted.is_some())
}

/// 标记事件是否真的改变了状态，并记录原因。
///
/// `applied = false` 的事件不是错误，而是「被识别为迟到/过期，已按审计留档」，
/// 排障时这条区分比日志里一句「忽略」有用得多。
pub async fn mark_event_applied(
    executor: impl PgExecutor<'_>,
    event_id: &str,
    applied: bool,
    detail: &str,
) -> AppResult<()> {
    sqlx::query("UPDATE task_events SET applied = $2, detail = $3 WHERE event_id = $1")
        .bind(event_id)
        .bind(applied)
        .bind(detail)
        .execute(executor)
        .await?;
    Ok(())
}

/// 撤销一条事件登记，让 Worker 的下一次重投能被重新处理。
///
/// 去重登记发生在处理之前（否则并发的两条重放会同时认为自己是第一次），
/// 于是「登记成功但处理时数据库暂时不可写」这个中间态必须有人收拾：
/// 不撤销的话，那条事件会永远被当成「已见过」而从此不再被应用，
/// 一次数据库抖动就变成一本书的永久丢单。只在**可重试**的失败上调用。
pub async fn forget_event(executor: impl PgExecutor<'_>, event_id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM task_events WHERE event_id = $1")
        .bind(event_id)
        .execute(executor)
        .await?;
    Ok(())
}

/// 清理过期的事件记账（默认保留 14 天），返回删除了几条。
pub async fn purge_events(executor: impl PgExecutor<'_>, keep_days: i64) -> AppResult<u64> {
    let affected = sqlx::query(
        "DELETE FROM task_events WHERE received_at < now() - ($1 || ' days')::interval",
    )
    .bind(keep_days.clamp(1, 365).to_string())
    .execute(executor)
    .await?
    .rows_affected();
    Ok(affected)
}
