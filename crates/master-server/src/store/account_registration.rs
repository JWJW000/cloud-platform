//! 账号注册批次与任务存储（V6 方案）。

use platform_domain::{AccountRegistrationTaskStatus, BatchStatus};
use sqlx::PgExecutor;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::{
    AccountRegistrationBatch, AccountRegistrationBatchProgress, AccountRegistrationTask,
};

const BATCH_COLUMNS: &str = "id, name, source_file, status, priority, created_by, created_at, updated_at";

const TASK_COLUMNS: &str = "t.id, t.batch_id, t.account_id, a.email, a.nickname, t.status, \
     t.priority, t.attempts, t.max_attempts, t.next_attempt_at, t.lease_node_id, \
     t.lease_session_id, t.lease_execution_id, t.lease_expires_at, t.stage, \
     t.stage_version, t.last_error, t.cancel_requested, t.created_at, t.updated_at";

// ---------------------------------------------------------------- 批次操作

/// 新建账号注册批次。
#[derive(Debug, Clone)]
pub struct NewAccountRegistrationBatch {
    /// 批次名称。
    pub name: String,
    /// 来源文件。
    pub source_file: Option<String>,
    /// 优先级。
    pub priority: i32,
    /// 创建人。
    pub created_by: Option<Uuid>,
}

/// 创建注册批次。
pub async fn create_batch(
    executor: impl PgExecutor<'_>,
    new: &NewAccountRegistrationBatch,
) -> AppResult<AccountRegistrationBatch> {
    let batch = sqlx::query_as::<_, AccountRegistrationBatch>(&format!(
        "INSERT INTO account_registration_batches (id, name, source_file, status, priority, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         RETURNING {BATCH_COLUMNS}"
    ))
    .bind(Uuid::new_v4())
    .bind(&new.name)
    .bind(&new.source_file)
    .bind(BatchStatus::NotStarted.as_str())
    .bind(new.priority)
    .bind(new.created_by)
    .fetch_one(executor)
    .await?;
    Ok(batch)
}

/// 查询单个注册批次。
pub async fn get_batch(
    executor: impl PgExecutor<'_>,
    id: Uuid,
) -> AppResult<AccountRegistrationBatch> {
    let batch = sqlx::query_as::<_, AccountRegistrationBatch>(&format!(
        "SELECT {BATCH_COLUMNS} FROM account_registration_batches WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::missing("账号注册批次不存在"))?;
    Ok(batch)
}

/// 列出注册批次。
pub async fn list_batches(
    executor: impl PgExecutor<'_>,
    limit: i64,
) -> AppResult<Vec<AccountRegistrationBatch>> {
    let batches = sqlx::query_as::<_, AccountRegistrationBatch>(&format!(
        "SELECT {BATCH_COLUMNS} FROM account_registration_batches \
         ORDER BY priority DESC, created_at DESC LIMIT $1"
    ))
    .bind(limit.clamp(1, 200))
    .fetch_all(executor)
    .await?;
    Ok(batches)
}

/// 更新注册批次状态。
pub async fn update_batch_status(
    executor: impl PgExecutor<'_>,
    id: Uuid,
    from: BatchStatus,
    to: BatchStatus,
) -> AppResult<()> {
    let affected = sqlx::query(
        "UPDATE account_registration_batches SET status = $2, updated_at = now() \
         WHERE id = $1 AND status = $3",
    )
    .bind(id)
    .bind(to.as_str())
    .bind(from.as_str())
    .execute(executor)
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(AppError::conflict(format!(
            "批次当前状态无法从「{from}」转换到「{to}」"
        )));
    }
    Ok(())
}

/// 更新注册批次优先级。
pub async fn update_batch_priority(
    executor: impl PgExecutor<'_>,
    id: Uuid,
    priority: i32,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE account_registration_batches SET priority = $2, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(priority)
    .execute(executor)
    .await?;
    Ok(())
}

/// 查询注册批次进度。
pub async fn batch_progress(
    executor: impl PgExecutor<'_>,
    batch_id: Uuid,
) -> AppResult<AccountRegistrationBatchProgress> {
    let row = sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64)>(
        "SELECT \
             count(*) AS total, \
             count(*) FILTER (WHERE status = '已完成') AS completed, \
             count(*) FILTER (WHERE status = '失败') AS failed, \
             count(*) FILTER (WHERE status IN ('已分配', '执行中', '正在重试')) AS running, \
             count(*) FILTER (WHERE status = '等待人工确认') AS awaiting_confirm, \
             count(*) FILTER (WHERE status = '待处理') AS pending \
         FROM account_registration_tasks WHERE batch_id = $1",
    )
    .bind(batch_id)
    .fetch_one(executor)
    .await?;

    Ok(AccountRegistrationBatchProgress {
        batch_id,
        total: row.0,
        completed: row.1,
        failed: row.2,
        running: row.3,
        awaiting_confirm: row.4,
        pending: row.5,
    })
}

// ---------------------------------------------------------------- 任务操作

/// 创建注册任务。
pub async fn create_task(
    executor: impl PgExecutor<'_>,
    batch_id: Uuid,
    account_id: Uuid,
    priority: i32,
) -> AppResult<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO account_registration_tasks (id, batch_id, account_id, status, priority) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (batch_id, account_id) DO NOTHING",
    )
    .bind(id)
    .bind(batch_id)
    .bind(account_id)
    .bind(AccountRegistrationTaskStatus::Pending.as_str())
    .bind(priority)
    .execute(executor)
    .await?;
    Ok(id)
}

/// 查询单个注册任务。
pub async fn get_task(
    executor: impl PgExecutor<'_>,
    id: Uuid,
) -> AppResult<AccountRegistrationTask> {
    let task = sqlx::query_as::<_, AccountRegistrationTask>(&format!(
        "SELECT {TASK_COLUMNS} FROM account_registration_tasks t \
         JOIN accounts a ON a.id = t.account_id \
         WHERE t.id = $1"
    ))
    .bind(id)
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::missing("账号注册任务不存在"))?;
    Ok(task)
}

/// 按批次列出注册任务。
pub async fn list_tasks_by_batch(
    executor: impl PgExecutor<'_>,
    batch_id: Uuid,
    status: Option<&str>,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<AccountRegistrationTask>> {
    let tasks = sqlx::query_as::<_, AccountRegistrationTask>(&format!(
        "SELECT {TASK_COLUMNS} FROM account_registration_tasks t \
         JOIN accounts a ON a.id = t.account_id \
         WHERE t.batch_id = $1 AND ($2::text IS NULL OR t.status = $2) \
         ORDER BY t.created_at ASC LIMIT $3 OFFSET $4"
    ))
    .bind(batch_id)
    .bind(status)
    .bind(limit.clamp(1, 200))
    .bind(offset.max(0))
    .fetch_all(executor)
    .await?;
    Ok(tasks)
}

/// 列出所有注册任务。
pub async fn list_all_tasks(
    executor: impl PgExecutor<'_>,
    status: Option<&str>,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<AccountRegistrationTask>> {
    let tasks = sqlx::query_as::<_, AccountRegistrationTask>(&format!(
        "SELECT {TASK_COLUMNS} FROM account_registration_tasks t \
         JOIN accounts a ON a.id = t.account_id \
         WHERE ($1::text IS NULL OR t.status = $1) \
         ORDER BY t.updated_at DESC LIMIT $2 OFFSET $3"
    ))
    .bind(status)
    .bind(limit.clamp(1, 200))
    .bind(offset.max(0))
    .fetch_all(executor)
    .await?;
    Ok(tasks)
}

/// 重试注册任务。
pub async fn retry_task(executor: impl PgExecutor<'_>, id: Uuid) -> AppResult<()> {
    let affected = sqlx::query(
        "UPDATE account_registration_tasks SET \
             status = $2, attempts = 0, next_attempt_at = now(), stage = '', \
             stage_version = stage_version + 1, lease_node_id = NULL, lease_session_id = NULL, \
             lease_execution_id = NULL, lease_expires_at = NULL, last_error = NULL, \
             cancel_requested = FALSE, updated_at = now() \
         WHERE id = $1 AND status IN ($3, $4, $5)",
    )
    .bind(id)
    .bind(AccountRegistrationTaskStatus::Pending.as_str())
    .bind(AccountRegistrationTaskStatus::Failed.as_str())
    .bind(AccountRegistrationTaskStatus::Cancelled.as_str())
    .bind(AccountRegistrationTaskStatus::AwaitingManualConfirm.as_str())
    .execute(executor)
    .await?
    .rows_affected();

    if affected == 0 {
        return Err(AppError::conflict("任务当前状态不允许手动重试"));
    }
    Ok(())
}

/// 取消注册任务。
pub async fn cancel_task(executor: impl PgExecutor<'_>, id: Uuid) -> AppResult<()> {
    let affected = sqlx::query(
        "UPDATE account_registration_tasks SET \
             status = CASE WHEN status = $2 THEN $3 ELSE status END, \
             cancel_requested = TRUE, \
             updated_at = now() \
         WHERE id = $1 AND status NOT IN ($4, $5, $3)",
    )
    .bind(id)
    .bind(AccountRegistrationTaskStatus::Pending.as_str())
    .bind(AccountRegistrationTaskStatus::Cancelled.as_str())
    .bind(AccountRegistrationTaskStatus::Completed.as_str())
    .bind(AccountRegistrationTaskStatus::Failed.as_str())
    .execute(executor)
    .await?
    .rows_affected();

    if affected == 0 {
        return Err(AppError::conflict("任务已完成或无法取消"));
    }
    Ok(())
}

/// 检查批次是否所有任务均已结束，若是则自动将批次更新为已完成。
pub async fn check_batch_completion(conn: &mut sqlx::PgConnection, batch_id: Uuid) -> AppResult<bool> {
    let non_terminal: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM account_registration_tasks \
         WHERE batch_id = $1 AND status NOT IN ('已完成', '失败', '已取消')",
    )
    .bind(batch_id)
    .fetch_one(&mut *conn)
    .await?;

    if non_terminal == 0 {
        sqlx::query(
            "UPDATE account_registration_batches SET status = '已完成', updated_at = now() \
             WHERE id = $1 AND status = '执行中'",
        )
        .bind(batch_id)
        .execute(&mut *conn)
        .await?;
        return Ok(true);
    }
    Ok(false)
}
