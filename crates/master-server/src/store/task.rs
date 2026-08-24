//! 图书任务的查询与管理员操作（第 16.4 节）。
//!
//! 调度相关的写入（领取、提交结果、回收租约）不在这里，而在 [`crate::scheduler`]：
//! 那些操作必须在一个事务里同时动任务、账号、代理和会话，拆到本层会失去原子性。

use platform_domain::{BatchStatus, TaskStatus};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::BookTask;

/// 任务查询的列清单。书名与序号联表取出，界面无需二次请求。
pub const TASK_COLUMNS: &str = "t.id, t.book_id, b.raw_title AS title, b.seq AS book_seq, \
     t.format, t.status, t.attempts, t.max_attempts, t.next_attempt_at, t.stage, \
     t.stage_version, t.downloaded_bytes, t.total_bytes, t.lease_node_id, \
     t.lease_session_id, t.lease_execution_id, t.lease_expires_at, t.nas_relative_path, t.last_error, \
     t.cancel_requested, t.expected_nas_relative_path, t.expected_file_name, t.expected_format, \
     t.expected_size_bytes, t.expected_sha256, t.evidence_execution_id, t.evidence_node_id, \
     t.evidence_recorded_at, t.bound_proxy_id, t.bound_exit_ip, t.proxy_bound_at, t.proxy_change_count, t.updated_at";

/// 任务列表过滤条件。
#[derive(Debug, Clone, Default)]
pub struct TaskFilter {
    /// 中文任务状态。
    pub status: Option<String>,
    /// 批次。
    pub batch_id: Option<Uuid>,
    /// 节点。
    pub node_id: Option<Uuid>,
    /// 书名关键字。
    pub keyword: Option<String>,
    /// 每页条数。
    pub limit: i64,
    /// 偏移。
    pub offset: i64,
}

/// 任务列表。
pub async fn list_tasks(
    executor: impl PgExecutor<'_>,
    filter: &TaskFilter,
) -> AppResult<Vec<BookTask>> {
    let tasks = sqlx::query_as::<_, BookTask>(&format!(
        "SELECT {TASK_COLUMNS} FROM book_tasks t \
         JOIN books b ON b.id = t.book_id \
         WHERE ($1::text IS NULL OR t.status = $1) \
           AND ($2::uuid IS NULL OR EXISTS ( \
                   SELECT 1 FROM batch_books bb WHERE bb.book_id = t.book_id AND bb.batch_id = $2)) \
           AND ($3::uuid IS NULL OR t.lease_node_id = $3) \
           AND ($4::text IS NULL OR b.raw_title ILIKE '%' || $4 || '%') \
         ORDER BY t.updated_at DESC LIMIT $5 OFFSET $6"
    ))
    .bind(filter.status.as_deref())
    .bind(filter.batch_id)
    .bind(filter.node_id)
    .bind(filter.keyword.as_deref())
    .bind(filter.limit.clamp(1, 500))
    .bind(filter.offset.max(0))
    .fetch_all(executor)
    .await?;
    Ok(tasks)
}

/// 单个任务。
pub async fn get_task(executor: impl PgExecutor<'_>, id: Uuid) -> AppResult<BookTask> {
    sqlx::query_as::<_, BookTask>(&format!(
        "SELECT {TASK_COLUMNS} FROM book_tasks t JOIN books b ON b.id = t.book_id WHERE t.id = $1"
    ))
    .bind(id)
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::missing("任务不存在"))
}

/// 按图书与格式查找任务。
pub async fn get_task_by_book_format(
    executor: impl PgExecutor<'_>,
    book_id: Uuid,
    format: &str,
) -> AppResult<Option<BookTask>> {
    sqlx::query_as::<_, BookTask>(&format!(
        "SELECT {TASK_COLUMNS} FROM book_tasks t JOIN books b ON b.id = t.book_id WHERE t.book_id = $1 AND t.format = $2"
    ))
    .bind(book_id)
    .bind(format)
    .fetch_optional(executor)
    .await
    .map_err(Into::into)
}

/// 按状态统计任务数，用于总览页。
pub async fn count_by_status(executor: impl PgExecutor<'_>) -> AppResult<Vec<(String, i64)>> {
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT status, count(*)::bigint FROM book_tasks GROUP BY status")
            .fetch_all(executor)
            .await?;
    Ok(rows)
}

/// 重试一个任务。
///
/// 只允许从终态或「待确认」重试。正在执行的任务不能被重试：那会造成两个 Worker
/// 同时下载同一本书，也会让先完成的那个提交被当成过期事件丢掉。
pub async fn retry_task(pool: &PgPool, id: Uuid) -> AppResult<BookTask> {
    let updated: Option<Uuid> = sqlx::query_scalar(
        "UPDATE book_tasks SET status = $2, attempts = 0, next_attempt_at = now(), \
             cancel_requested = FALSE, last_error = NULL, stage = '', \
             lease_node_id = NULL, lease_session_id = NULL, lease_execution_id = NULL, \
             lease_expires_at = NULL, stage_version = stage_version + 1, updated_at = now() \
         WHERE id = $1 AND status IN ($3, $4, $5, $6) RETURNING id",
    )
    .bind(id)
    .bind(TaskStatus::Pending.as_str())
    .bind(TaskStatus::Failed.as_str())
    .bind(TaskStatus::Skipped.as_str())
    .bind(TaskStatus::Cancelled.as_str())
    .bind(TaskStatus::NeedsConfirm.as_str())
    .fetch_optional(pool)
    .await?;
    if updated.is_none() {
        return Err(AppError::conflict("任务不存在或当前状态不允许重试"));
    }
    get_task(pool, id).await
}

/// 取消一个任务。
///
/// 返回是否需要通知正在执行的 Worker：待处理任务直接落到已取消；
/// 正在执行的任务先置 `cancel_requested`，由 Worker 在下一个检查点自行收尾，
/// 强杀会留下半个文件在暂存目录。
///
/// V4 精确取消（第 11.7 节）：返回 node/session/execution/stage_version 全量字段，
/// 取消消息只有全部匹配才会命中当前执行，旧消息不得误伤新执行。
pub async fn cancel_task(pool: &PgPool, id: Uuid) -> AppResult<Option<(RunningCancelTarget, i32)>> {
    let mut tx = pool.begin().await?;
    type CancelRow = (String, Option<Uuid>, Option<Uuid>, Option<Uuid>, i32);
    let row: Option<CancelRow> = sqlx::query_as(
        "SELECT status, lease_node_id, lease_session_id, lease_execution_id, stage_version \
         FROM book_tasks WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let (status, node_id, session_id, execution_id, stage_version) =
        row.ok_or_else(|| AppError::missing("任务不存在"))?;
    let status = status.parse::<TaskStatus>()?;

    if status.is_terminal() {
        return Err(AppError::conflict(format!("任务已处于终态：{status}")));
    }

    if status == TaskStatus::Pending {
        sqlx::query(
            "UPDATE book_tasks SET status = $2, cancel_requested = TRUE, \
                 stage_version = stage_version + 1, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(TaskStatus::Cancelled.as_str())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(None);
    }

    sqlx::query("UPDATE book_tasks SET cancel_requested = TRUE, updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok(match (node_id, session_id, execution_id) {
        (Some(node), Some(session), Some(execution)) => Some((
            RunningCancelTarget {
                task_id: id,
                node_id: node,
                session_id: session,
                execution_id: execution,
                stage_version,
            },
            stage_version,
        )),
        _ => None,
    })
}

/// 重试某批次内全部失败任务，返回重试了几个。
pub async fn retry_failed_in_batch(
    executor: impl PgExecutor<'_>,
    batch_id: Uuid,
) -> AppResult<u64> {
    let affected = sqlx::query(
        "UPDATE book_tasks t SET status = $2, attempts = 0, next_attempt_at = now(), \
             last_error = NULL, cancel_requested = FALSE, \
             stage_version = t.stage_version + 1, updated_at = now() \
         FROM batch_books bb, download_batches b \
         WHERE bb.batch_id = $1 AND b.id = bb.batch_id \
           AND t.book_id = bb.book_id AND t.format = b.download_format \
           AND t.status IN ($3, $4)",
    )
    .bind(batch_id)
    .bind(TaskStatus::Pending.as_str())
    .bind(TaskStatus::Failed.as_str())
    .bind(TaskStatus::Skipped.as_str())
    .execute(executor)
    .await?
    .rows_affected();
    Ok(affected)
}

/// 批次取消裁决明细。
#[derive(Debug, Clone)]
pub struct BatchCancelOutcome {
    /// 批次编号。
    pub batch_id: Uuid,
    /// 直接取消的待处理任务编号列表。
    pub directly_cancelled_task_ids: Vec<Uuid>,
    /// 正在运行、需下发 CancelTask 的目标列表。
    pub running_targets: Vec<RunningCancelTarget>,
    /// 被其他待开始/执行中/已暂停批次共享而保留的任务编号列表。
    pub shared_task_ids: Vec<Uuid>,
}

/// 正在运行的任务取消下发目标。
#[derive(Debug, Clone)]
pub struct RunningCancelTarget {
    /// 任务编号。
    pub task_id: Uuid,
    /// 节点编号。
    pub node_id: Uuid,
    /// 会话编号。
    pub session_id: Uuid,
    /// 执行编号。
    pub execution_id: Uuid,
    /// 执行世代（V4 精确取消：旧消息不得误伤新执行）。
    pub stage_version: i32,
}

/// 取消批次并裁决关联任务（V3 方案第 10 节）。
///
/// 严格事务内判定：
/// - 仅当全局任务不再被任何处于「待开始」「执行中」「已暂停」的其他批次需要时，才允许取消该任务；
/// - 待处理任务直接转为「已取消」；
/// - 进行中任务置 `cancel_requested = TRUE` 并记录下发目标；
/// - 共享任务保留原状态。
pub async fn cancel_batch(
    pool: &PgPool,
    batch_id: Uuid,
) -> AppResult<(crate::models::DownloadBatch, BatchCancelOutcome)> {
    let mut tx = pool.begin().await?;

    let batch = sqlx::query_as::<_, crate::models::DownloadBatch>(
        "SELECT id, name, source_file, status, priority, download_format, created_at, updated_at \
         FROM download_batches WHERE id = $1 FOR UPDATE",
    )
    .bind(batch_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::missing("批次不存在"))?;

    // V4 第 11.6 节：批次取消使用领域迁移校验。
    // - 待开始 / 执行中 / 已暂停：允许取消；
    // - 已取消：幂等返回当前结果，不重复发送取消；
    // - 已完成：默认拒绝取消。
    let current = batch.status.parse::<BatchStatus>()?;
    match current {
        BatchStatus::Cancelled => {
            let outcome = BatchCancelOutcome {
                batch_id,
                directly_cancelled_task_ids: Vec::new(),
                running_targets: Vec::new(),
                shared_task_ids: Vec::new(),
            };
            return Ok((batch, outcome));
        }
        BatchStatus::Completed => {
            return Err(AppError::conflict(
                "批次已完成，默认不允许取消（如确需归档请另行处理）",
            ));
        }
        BatchStatus::NotStarted | BatchStatus::Running | BatchStatus::Paused => {}
    }
    current
        .ensure_transition(BatchStatus::Cancelled)
        .map_err(AppError::from)?;

    let batch = sqlx::query_as::<_, crate::models::DownloadBatch>(
        "UPDATE download_batches SET status = $2, updated_at = now() WHERE id = $1 \
         RETURNING id, name, source_file, status, priority, download_format, created_at, updated_at",
    )
    .bind(batch_id)
    .bind(BatchStatus::Cancelled.as_str())
    .fetch_one(&mut *tx)
    .await?;

    #[derive(sqlx::FromRow)]
    struct TaskRow {
        task_id: Uuid,
        book_id: Uuid,
        status: String,
        lease_node_id: Option<Uuid>,
        lease_session_id: Option<Uuid>,
        lease_execution_id: Option<Uuid>,
        stage_version: i32,
    }

    let task_rows = sqlx::query_as::<_, TaskRow>(
        "SELECT t.id AS task_id, t.book_id, t.status, t.lease_node_id, t.lease_session_id, t.lease_execution_id, t.stage_version \
         FROM batch_books bb \
         JOIN book_tasks t ON t.book_id = bb.book_id AND t.format = $2 \
         WHERE bb.batch_id = $1 \
         FOR UPDATE OF t",
    )
    .bind(batch_id)
    .bind(&batch.download_format)
    .fetch_all(&mut *tx)
    .await?;

    let mut directly_cancelled_task_ids = Vec::new();
    let mut running_targets = Vec::new();
    let mut shared_task_ids = Vec::new();

    for row in task_rows {
        let has_other_active: bool = sqlx::query_scalar(
            "SELECT EXISTS ( \
                 SELECT 1 FROM batch_books other \
                 JOIN download_batches ob ON ob.id = other.batch_id \
                 WHERE other.book_id = $1 AND other.batch_id <> $2 \
                   AND ob.status IN ('待开始', '执行中', '已暂停') \
                   AND ob.download_format = $3 \
             )",
        )
        .bind(row.book_id)
        .bind(batch_id)
        .bind(&batch.download_format)
        .fetch_one(&mut *tx)
        .await?;

        if has_other_active {
            shared_task_ids.push(row.task_id);
            sqlx::query(
                "UPDATE batch_books SET display_status = $3 WHERE batch_id = $1 AND book_id = $2",
            )
            .bind(batch_id)
            .bind(row.book_id)
            .bind(TaskStatus::Cancelled.as_str())
            .execute(&mut *tx)
            .await?;
        } else if row.status == TaskStatus::Pending.as_str() {
            sqlx::query("UPDATE book_tasks SET status = $2, updated_at = now() WHERE id = $1")
                .bind(row.task_id)
                .bind(TaskStatus::Cancelled.as_str())
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "UPDATE batch_books SET display_status = $3 WHERE batch_id = $1 AND book_id = $2",
            )
            .bind(batch_id)
            .bind(row.book_id)
            .bind(TaskStatus::Cancelled.as_str())
            .execute(&mut *tx)
            .await?;
            directly_cancelled_task_ids.push(row.task_id);
        } else if row.status == TaskStatus::Running.as_str()
            || row.status == TaskStatus::Claimed.as_str()
            || row.status == TaskStatus::AwaitingIngest.as_str()
        {
            sqlx::query(
                "UPDATE book_tasks SET cancel_requested = TRUE, updated_at = now() WHERE id = $1",
            )
            .bind(row.task_id)
            .execute(&mut *tx)
            .await?;
            if let (Some(node_id), Some(session_id), Some(execution_id)) = (
                row.lease_node_id,
                row.lease_session_id,
                row.lease_execution_id,
            ) {
                running_targets.push(RunningCancelTarget {
                    task_id: row.task_id,
                    node_id,
                    session_id,
                    execution_id,
                    stage_version: row.stage_version,
                });
            }
        }
    }

    tx.commit().await?;

    let outcome = BatchCancelOutcome {
        batch_id,
        directly_cancelled_task_ids,
        running_targets,
        shared_task_ids,
    };

    Ok((batch, outcome))
}

/// 导出批次结果的一行。
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct BatchExportRow {
    /// 导入行号。
    pub import_line: i32,
    /// 书名。
    pub title: String,
    /// 作者。
    pub author: Option<String>,
    /// ISBN。
    pub isbn: Option<String>,
    /// 中文状态。
    pub status: Option<String>,
    /// NAS 相对路径。
    pub nas_relative_path: Option<String>,
    /// 最近错误。
    pub last_error: Option<String>,
}

/// 导出批次结果（第 16.3 节：导出 CSV）。
pub async fn export_batch(pool: &PgPool, batch_id: Uuid) -> AppResult<Vec<BatchExportRow>> {
    let rows = sqlx::query_as::<_, BatchExportRow>(
        "SELECT bb.import_line, b.raw_title AS title, b.raw_author AS author, \
             b.raw_isbn AS isbn, t.status, t.nas_relative_path, t.last_error \
         FROM batch_books bb \
         JOIN books b ON b.id = bb.book_id \
         JOIN download_batches batch ON batch.id = bb.batch_id \
         LEFT JOIN book_tasks t ON t.book_id = bb.book_id AND t.format = batch.download_format \
         WHERE bb.batch_id = $1 ORDER BY bb.import_line",
    )
    .bind(batch_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// 需要 NAS 核验的任务（第 14.4 节：断线保护到期后的「待确认」）。
pub async fn list_needs_confirm(
    executor: impl PgExecutor<'_>,
    limit: i64,
) -> AppResult<Vec<BookTask>> {
    let tasks = sqlx::query_as::<_, BookTask>(&format!(
        "SELECT {TASK_COLUMNS} FROM book_tasks t JOIN books b ON b.id = t.book_id \
         WHERE t.status = $1 ORDER BY t.updated_at LIMIT $2"
    ))
    .bind(TaskStatus::NeedsConfirm.as_str())
    .bind(limit.clamp(1, 100))
    .fetch_all(executor)
    .await?;
    Ok(tasks)
}

/// 把批次关联行的展示状态同步成任务的真实状态。
///
/// 展示状态是冗余字段，存在的意义是让「某批次的清单」可以只查 `batch_books`
/// 一张表。真相仍在 `book_tasks`，因此这里以任务状态为准覆盖过去。
pub async fn sync_display_status(executor: impl PgExecutor<'_>, book_id: Uuid) -> AppResult<()> {
    sqlx::query(
        "UPDATE batch_books bb SET display_status = t.status \
         FROM book_tasks t, download_batches b \
         WHERE bb.book_id = $1 AND t.book_id = bb.book_id \
           AND b.id = bb.batch_id AND t.format = b.download_format \
           AND bb.display_status <> t.status",
    )
    .bind(book_id)
    .execute(executor)
    .await?;
    Ok(())
}
