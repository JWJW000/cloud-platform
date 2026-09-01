//! 图书主数据、已入库文件、批次与导入去重（第 8 节、第 16.3 节）。

use platform_domain::{BatchStatus, BookIdentity, TaskStatus, VerifyStatus};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::{
    BatchProgress, Book, BookFile, DownloadBatch, ImportRow, ImportSummary, InvalidRow,
};

const BOOK_COLUMNS: &str = "id, seq, raw_title, raw_author, raw_publisher, raw_isbn, \
     normalized_isbn, dedup_key, verify_status, merged_into, created_at";

const FILE_COLUMNS: &str = "id, book_id, format, nas_relative_path, size_bytes, sha256, \
     status, ingested_by_node, ingested_at";

const BATCH_COLUMNS: &str =
    "id, name, source_file, status, priority, download_format, created_at, updated_at";

/// 导入参数。字段较多，单独成结构体避免一长串位置参数。
#[derive(Debug, Clone)]
pub struct ImportRequest<'a> {
    /// 批次名称。
    pub batch_name: &'a str,
    /// 来源文件名（粘贴导入时为空）。
    pub source_file: Option<&'a str>,
    /// 下载格式（技术标识 `pdf`/`epub`）。
    pub format: &'a str,
    /// 批次优先级。
    pub priority: i32,
    /// 创建者。
    pub created_by: Option<Uuid>,
    /// 单任务最大尝试次数。
    pub max_attempts: i32,
}

/// 导入图书并建立批次（第 8.2 / 16.3 节）。
///
/// 整个导入在一个事务里完成：要么批次和它的全部关联一起可见，要么什么都没写。
/// 去重靠 `books.dedup_key` 的唯一索引，而不是先查后插——并发导入两份含同一本书的
/// 名单时，「先查后插」会双双通过检查然后插出两行。
pub async fn import_books(
    pool: &PgPool,
    request: &ImportRequest<'_>,
    rows: &[ImportRow],
) -> AppResult<ImportSummary> {
    let mut summary = ImportSummary {
        total_rows: rows.len(),
        ..Default::default()
    };

    let mut tx = pool.begin().await?;
    let batch_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO download_batches \
             (id, name, source_file, status, priority, download_format, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(batch_id)
    .bind(request.batch_name)
    .bind(request.source_file)
    .bind(BatchStatus::NotStarted.as_str())
    .bind(request.priority)
    .bind(request.format)
    .bind(request.created_by)
    .execute(&mut *tx)
    .await?;
    summary.batch_id = Some(batch_id);

    for (index, row) in rows.iter().enumerate() {
        let line = index + 1;
        let Some(identity) = BookIdentity::from_raw(
            &row.title,
            row.author.as_deref(),
            row.publisher.as_deref(),
            row.isbn.as_deref(),
        ) else {
            summary.invalid_rows.push(InvalidRow {
                line,
                raw: row.title.clone(),
                reason: "书名为空，无法参与去重".to_string(),
            });
            continue;
        };

        // 总库就是“已经拥有”。无论 NAS 是否存在文件，只要可靠命中总库版本，
        // 本次下载导入就直接跳过，不能再创建或复活下载任务。
        if crate::catalog_ownership::find_owned_edition(&mut *tx, &identity)
            .await?
            .is_some()
        {
            summary.already_owned += 1;
            continue;
        }

        let (book_id, _book_seq, is_new) = upsert_book(&mut tx, &identity).await?;
        if is_new {
            summary.new_books += 1;
        } else {
            summary.deduplicated += 1;
        }
        if identity.verify_status == VerifyStatus::NeedsConfirm {
            summary.needs_confirm += 1;
        }

        let task_status =
            ensure_task(&mut tx, book_id, request.format, request.max_attempts).await?;
        if task_status == TaskStatus::Completed {
            summary.already_ingested += 1;
        }

        sqlx::query(
            "INSERT INTO batch_books (id, batch_id, book_id, import_line, display_status) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (batch_id, book_id) DO UPDATE SET display_status = $5",
        )
        .bind(Uuid::new_v4())
        .bind(batch_id)
        .bind(book_id)
        .bind(line as i32)
        .bind(task_status.as_str())
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(summary)
}

/// 插入或命中已有图书，返回 `(图书编号, 序号, 是否新建)`。
///
/// 命中已被管理员合并的图书时跟随 `merged_into` 指向正本，
/// 这样后续导入不会再往「已合并」的壳记录上挂任务。
async fn upsert_book(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    identity: &BookIdentity,
) -> AppResult<(Uuid, i64, bool)> {
    let dedup_key = identity.storage_key();
    let inserted: Option<(Uuid, i64)> = sqlx::query_as(
        "INSERT INTO books (id, raw_title, raw_author, raw_publisher, raw_isbn, \
             normalized_title, normalized_author, normalized_publisher, normalized_isbn, \
             dedup_key, verify_status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
         ON CONFLICT (dedup_key) DO NOTHING RETURNING id, seq",
    )
    .bind(Uuid::new_v4())
    .bind(&identity.raw_title)
    .bind(&identity.raw_author)
    .bind(&identity.raw_publisher)
    .bind(&identity.raw_isbn)
    .bind(&identity.normalized_title)
    .bind(&identity.normalized_author)
    .bind(&identity.normalized_publisher)
    .bind(&identity.normalized_isbn)
    .bind(&dedup_key)
    .bind(identity.verify_status.as_str())
    .fetch_optional(&mut **tx)
    .await?;

    if let Some((id, seq)) = inserted {
        return Ok((id, seq, true));
    }

    let (id, seq, merged_into): (Uuid, i64, Option<Uuid>) =
        sqlx::query_as("SELECT id, seq, merged_into FROM books WHERE dedup_key = $1")
            .bind(&dedup_key)
            .fetch_one(&mut **tx)
            .await?;

    if let Some(target) = merged_into {
        let canonical: Option<(Uuid, i64)> =
            sqlx::query_as("SELECT id, seq FROM books WHERE id = $1")
                .bind(target)
                .fetch_optional(&mut **tx)
                .await?;
        if let Some((canonical_id, canonical_seq)) = canonical {
            return Ok((canonical_id, canonical_seq, false));
        }
    }
    Ok((id, seq, false))
}

/// 保证 `(图书, 格式)` 的任务存在，返回它当前的状态。
///
/// 三种情况：
/// 1. NAS 上已有该格式的有效文件 → 直接建成 `已完成`，不再下载（第 8.3 节全局唯一文件）；
/// 2. 任务已存在且处于失败/跳过/取消 → 重新入队，这是「把失败的书放进新批次重试」的语义；
/// 3. 其余情况沿用现状，避免把正在执行的任务打回待处理。
async fn ensure_task(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    book_id: Uuid,
    format: &str,
    max_attempts: i32,
) -> AppResult<TaskStatus> {
    let existing_file: Option<String> = sqlx::query_scalar(
        "SELECT nas_relative_path FROM book_files \
         WHERE book_id = $1 AND format = $2 AND status = '有效'",
    )
    .bind(book_id)
    .bind(format)
    .fetch_optional(&mut **tx)
    .await?;

    let (initial_status, initial_path) = match existing_file {
        Some(path) => (TaskStatus::Completed, Some(path)),
        None => (TaskStatus::Pending, None),
    };

    let inserted: Option<String> = sqlx::query_scalar(
        "INSERT INTO book_tasks (id, book_id, format, status, max_attempts, nas_relative_path) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (book_id, format) DO NOTHING RETURNING status",
    )
    .bind(Uuid::new_v4())
    .bind(book_id)
    .bind(format)
    .bind(initial_status.as_str())
    .bind(max_attempts.max(1))
    .bind(&initial_path)
    .fetch_optional(&mut **tx)
    .await?;

    if let Some(status) = inserted {
        return Ok(status.parse::<TaskStatus>()?);
    }

    let requeued: Option<String> = sqlx::query_scalar(
        "UPDATE book_tasks SET status = $3, attempts = 0, next_attempt_at = now(), \
             cancel_requested = FALSE, last_error = NULL, max_attempts = $4, updated_at = now() \
         WHERE book_id = $1 AND format = $2 AND status IN ($5, $6, $7) RETURNING status",
    )
    .bind(book_id)
    .bind(format)
    .bind(TaskStatus::Pending.as_str())
    .bind(max_attempts.max(1))
    .bind(TaskStatus::Failed.as_str())
    .bind(TaskStatus::Skipped.as_str())
    .bind(TaskStatus::Cancelled.as_str())
    .fetch_optional(&mut **tx)
    .await?;

    if let Some(status) = requeued {
        return Ok(status.parse::<TaskStatus>()?);
    }

    let status: String =
        sqlx::query_scalar("SELECT status FROM book_tasks WHERE book_id = $1 AND format = $2")
            .bind(book_id)
            .bind(format)
            .fetch_one(&mut **tx)
            .await?;
    Ok(status.parse::<TaskStatus>()?)
}

// ---------------------------------------------------------------- 图书查询

/// 图书列表，支持书名/作者/ISBN 关键字与核验状态过滤。
pub async fn list_books(
    executor: impl PgExecutor<'_>,
    keyword: Option<&str>,
    verify_status: Option<&str>,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<Book>> {
    let books = sqlx::query_as::<_, Book>(&format!(
        "SELECT {BOOK_COLUMNS} FROM books \
         WHERE ($1::text IS NULL OR raw_title ILIKE '%' || $1 || '%' \
                OR raw_author ILIKE '%' || $1 || '%' OR raw_isbn ILIKE '%' || $1 || '%') \
           AND ($2::text IS NULL OR verify_status = $2) \
         ORDER BY seq DESC LIMIT $3 OFFSET $4"
    ))
    .bind(keyword)
    .bind(verify_status)
    .bind(limit.clamp(1, 500))
    .bind(offset.max(0))
    .fetch_all(executor)
    .await?;
    Ok(books)
}

/// 单本图书。
pub async fn get_book(executor: impl PgExecutor<'_>, id: Uuid) -> AppResult<Book> {
    sqlx::query_as::<_, Book>(&format!("SELECT {BOOK_COLUMNS} FROM books WHERE id = $1"))
        .bind(id)
        .fetch_optional(executor)
        .await?
        .ok_or_else(|| AppError::missing("图书不存在"))
}

/// 人工确认一本「待确认」的图书。
pub async fn confirm_book(executor: impl PgExecutor<'_>, id: Uuid) -> AppResult<Book> {
    sqlx::query_as::<_, Book>(&format!(
        "UPDATE books SET verify_status = $2, updated_at = now() \
         WHERE id = $1 AND verify_status = $3 RETURNING {BOOK_COLUMNS}"
    ))
    .bind(id)
    .bind(VerifyStatus::Confirmed.as_str())
    .bind(VerifyStatus::NeedsConfirm.as_str())
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::conflict("图书不存在或不处于待确认状态"))
}

/// 把 `source` 合并进 `target`。
///
/// 遵循第 12.1 节规范：
/// 1. 检查两本图书均存在，target 未被合并，防止循环合并；
/// 2. 检查 source 无执行中任务（若有则拒绝合并）；
/// 3. 同一事务中：迁移 batch_books（去重）；
/// 4. 迁移或去重 book_tasks（如果 target 已有同格式任务，source 的未完成任务标为取消或清理）；
/// 5. 迁移 book_files（若 target 已有同格式文件，检查哈希一致性，冲突则拒绝合并）；
/// 6. 标记 source 为 Merged 并记录 merged_into = target。
pub async fn merge_books(pool: &PgPool, source: Uuid, target: Uuid) -> AppResult<()> {
    if source == target {
        return Err(AppError::bad("不能把图书合并到自己"));
    }
    let mut tx = pool.begin().await?;

    // 1. 验证 target 存在且未被合并
    let target_book: Option<(Option<Uuid>, String)> =
        sqlx::query_as("SELECT merged_into, verify_status FROM books WHERE id = $1 FOR UPDATE")
            .bind(target)
            .fetch_optional(&mut *tx)
            .await?;

    let Some((target_merged, _)) = target_book else {
        return Err(AppError::missing("目标图书不存在"));
    };
    if target_merged.is_some() {
        return Err(AppError::conflict("目标图书已被合并，不能作为合并目标"));
    }

    // 2. 验证 source 存在且未被合并
    let source_book: Option<(Option<Uuid>, String)> =
        sqlx::query_as("SELECT merged_into, verify_status FROM books WHERE id = $1 FOR UPDATE")
            .bind(source)
            .fetch_optional(&mut *tx)
            .await?;

    let Some((source_merged, _)) = source_book else {
        return Err(AppError::missing("源图书不存在"));
    };
    if source_merged.is_some() {
        return Err(AppError::conflict("源图书已被合并"));
    }

    // 3. 检查 source 是否有执行中的任务
    let running_tasks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM book_tasks WHERE book_id = $1 AND status IN ($2, $3, $4)",
    )
    .bind(source)
    .bind(TaskStatus::Claimed.as_str())
    .bind(TaskStatus::Running.as_str())
    .bind(TaskStatus::AwaitingIngest.as_str())
    .fetch_one(&mut *tx)
    .await?;

    if running_tasks > 0 {
        return Err(AppError::conflict("源图书有正在执行中的任务，禁止合并"));
    }

    // 4. 迁移/检查 book_files
    let source_files: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT format, nas_relative_path, sha256, size_bytes FROM book_files WHERE book_id = $1",
    )
    .bind(source)
    .fetch_all(&mut *tx)
    .await?;

    for (fmt, _path, sha, _size) in source_files {
        let target_file: Option<(String, String)> = sqlx::query_as(
            "SELECT nas_relative_path, sha256 FROM book_files WHERE book_id = $1 AND format = $2",
        )
        .bind(target)
        .bind(&fmt)
        .fetch_optional(&mut *tx)
        .await?;

        if let Some((_, t_sha)) = target_file {
            if !t_sha.is_empty() && !sha.is_empty() && !t_sha.eq_ignore_ascii_case(&sha) {
                return Err(AppError::conflict(format!(
                    "图书格式 {fmt} 在源图书与目标图书中哈希冲突，拒绝自动合并"
                )));
            }
            // 已存在且哈希一致，删除 source 中的重复文件记录
            sqlx::query("DELETE FROM book_files WHERE book_id = $1 AND format = $2")
                .bind(source)
                .bind(&fmt)
                .execute(&mut *tx)
                .await?;
        } else {
            // target 无此格式文件，转移所有权
            sqlx::query("UPDATE book_files SET book_id = $2 WHERE book_id = $1 AND format = $3")
                .bind(source)
                .bind(target)
                .bind(&fmt)
                .execute(&mut *tx)
                .await?;
        }
    }

    // 5. 迁移 batch_books 关联（先删除重复批次关联，再迁移独立批次关联）
    sqlx::query(
        "DELETE FROM batch_books WHERE book_id = $1 \
         AND EXISTS (SELECT 1 FROM batch_books existing \
                     WHERE existing.batch_id = batch_books.batch_id AND existing.book_id = $2)",
    )
    .bind(source)
    .bind(target)
    .execute(&mut *tx)
    .await?;

    sqlx::query("UPDATE batch_books SET book_id = $2 WHERE book_id = $1")
        .bind(source)
        .bind(target)
        .execute(&mut *tx)
        .await?;

    // 6. 迁移 book_tasks（如果 target 已有同格式任务，把 source 未完成任务设为已取消，否则迁移 book_id）
    let source_tasks: Vec<(Uuid, String, String)> =
        sqlx::query_as("SELECT id, format, status FROM book_tasks WHERE book_id = $1")
            .bind(source)
            .fetch_all(&mut *tx)
            .await?;

    for (t_id, fmt, status) in source_tasks {
        let target_task_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM book_tasks WHERE book_id = $1 AND format = $2)",
        )
        .bind(target)
        .bind(&fmt)
        .fetch_one(&mut *tx)
        .await?;

        if target_task_exists {
            // target 已有该格式任务，source 任务取消
            if status != TaskStatus::Completed.as_str() {
                sqlx::query(
                    "UPDATE book_tasks SET status = $2, last_error = '图书合并取消', updated_at = now() WHERE id = $1",
                )
                .bind(t_id)
                .bind(TaskStatus::Cancelled.as_str())
                .execute(&mut *tx)
                .await?;
            }
        } else {
            // 迁移任务给 target
            sqlx::query("UPDATE book_tasks SET book_id = $2 WHERE id = $1")
                .bind(t_id)
                .bind(target)
                .execute(&mut *tx)
                .await?;
        }
    }

    // 7. 标记 source 图书为 Merged
    sqlx::query(
        "UPDATE books SET merged_into = $2, verify_status = $3, updated_at = now() \
         WHERE id = $1",
    )
    .bind(source)
    .bind(target)
    .bind(VerifyStatus::Merged.as_str())
    .execute(&mut *tx)
    .await?;

    // 同步两本书的展示状态
    crate::store::task::sync_display_status(&mut *tx, target).await?;
    crate::store::task::sync_display_status(&mut *tx, source).await?;

    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------- 已入库文件

/// 某本图书的已入库文件。
pub async fn list_book_files(
    executor: impl PgExecutor<'_>,
    book_id: Uuid,
) -> AppResult<Vec<BookFile>> {
    let files = sqlx::query_as::<_, BookFile>(&format!(
        "SELECT {FILE_COLUMNS} FROM book_files WHERE book_id = $1 ORDER BY format"
    ))
    .bind(book_id)
    .fetch_all(executor)
    .await?;
    Ok(files)
}

/// 登记一份已入库文件（幂等）。
///
/// 同一本书同一格式重复提交时更新记录而不是报错：Worker 的重试与补报都可能
/// 让同一份文件被提交两次，这里必须容忍。
pub async fn record_book_file(
    executor: impl PgExecutor<'_>,
    book_id: Uuid,
    format: &str,
    nas_relative_path: &str,
    size_bytes: i64,
    sha256: &str,
    node_id: Option<Uuid>,
) -> AppResult<BookFile> {
    let file = sqlx::query_as::<_, BookFile>(&format!(
        "INSERT INTO book_files \
             (id, book_id, format, nas_relative_path, size_bytes, sha256, status, ingested_by_node) \
         VALUES ($1, $2, $3, $4, $5, $6, '有效', $7) \
         ON CONFLICT (book_id, format) DO UPDATE SET \
             nas_relative_path = EXCLUDED.nas_relative_path, \
             size_bytes = EXCLUDED.size_bytes, sha256 = EXCLUDED.sha256, \
             status = '有效', ingested_by_node = EXCLUDED.ingested_by_node, \
             ingested_at = now() \
         RETURNING {FILE_COLUMNS}"
    ))
    .bind(Uuid::new_v4())
    .bind(book_id)
    .bind(format)
    .bind(nas_relative_path)
    .bind(size_bytes)
    .bind(sha256)
    .bind(node_id)
    .fetch_one(executor)
    .await?;
    Ok(file)
}

/// 把文件标记为已失效（核验发现文件消失时用）。
pub async fn invalidate_book_file(
    executor: impl PgExecutor<'_>,
    book_id: Uuid,
    format: &str,
) -> AppResult<()> {
    sqlx::query("UPDATE book_files SET status = '已失效' WHERE book_id = $1 AND format = $2")
        .bind(book_id)
        .bind(format)
        .execute(executor)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------- 批次

/// 批次列表。
pub async fn list_batches(executor: impl PgExecutor<'_>) -> AppResult<Vec<DownloadBatch>> {
    let batches = sqlx::query_as::<_, DownloadBatch>(&format!(
        "SELECT {BATCH_COLUMNS} FROM download_batches \
         ORDER BY (status = '执行中') DESC, priority DESC, created_at DESC LIMIT 200"
    ))
    .fetch_all(executor)
    .await?;
    Ok(batches)
}

/// 单个批次。
pub async fn get_batch(executor: impl PgExecutor<'_>, id: Uuid) -> AppResult<DownloadBatch> {
    sqlx::query_as::<_, DownloadBatch>(&format!(
        "SELECT {BATCH_COLUMNS} FROM download_batches WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::missing("批次不存在"))
}

/// 批次进度。
///
/// 进度按批次内图书对应的**全局任务**统计，因此同一本书出现在多个批次时，
/// 一次下载会同时推进所有相关批次的进度——这正是全局去重想要的效果。
pub async fn batch_progress(
    executor: impl PgExecutor<'_>,
    batch_id: Uuid,
) -> AppResult<BatchProgress> {
    let progress = sqlx::query_as::<_, BatchProgress>(
        "SELECT $1::uuid AS batch_id, \
             count(*)::bigint AS total, \
             count(*) FILTER (WHERE t.status = '已完成')::bigint AS completed, \
             count(*) FILTER (WHERE t.status = '失败')::bigint AS failed, \
             count(*) FILTER (WHERE t.status IN ('已跳过', '已取消'))::bigint AS skipped, \
             count(*) FILTER (WHERE t.status IN ('已分配', '执行中', '等待入库', '待确认'))::bigint AS running, \
             count(*) FILTER (WHERE t.status = '待处理')::bigint AS pending \
         FROM batch_books bb \
         JOIN download_batches b ON b.id = bb.batch_id \
         LEFT JOIN book_tasks t ON t.book_id = bb.book_id AND t.format = b.download_format \
         WHERE bb.batch_id = $1",
    )
    .bind(batch_id)
    .fetch_one(executor)
    .await?;
    Ok(progress)
}

/// 改批次状态（开始、暂停、恢复、取消）。
///
/// V4 第 11.6 节：所有批次状态迁移都走领域校验——
/// - 待开始 / 执行中 / 已暂停：允许取消；
/// - 已取消：幂等返回当前结果；
/// - 已完成：拒绝一切迁移；
/// - 不允许从已取消恢复到执行中。
///
/// 相同状态幂等返回（例如重复「启动」不报错也不重复发指令）。
pub async fn set_batch_status(
    pool: &PgPool,
    batch_id: Uuid,
    status: BatchStatus,
) -> AppResult<DownloadBatch> {
    let mut tx = pool.begin().await?;
    let current: String =
        sqlx::query_scalar("SELECT status FROM download_batches WHERE id = $1 FOR UPDATE")
            .bind(batch_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| AppError::missing("批次不存在"))?;
    let current_status = current.parse::<BatchStatus>()?;

    if current_status != status {
        current_status
            .ensure_transition(status)
            .map_err(AppError::from)?;
    }

    let batch = sqlx::query_as::<_, DownloadBatch>(&format!(
        "UPDATE download_batches SET status = $2, updated_at = now() WHERE id = $1 \
         RETURNING {BATCH_COLUMNS}"
    ))
    .bind(batch_id)
    .bind(status.as_str())
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(batch)
}

/// 改批次优先级。
pub async fn set_batch_priority(
    executor: impl PgExecutor<'_>,
    batch_id: Uuid,
    priority: i32,
) -> AppResult<DownloadBatch> {
    sqlx::query_as::<_, DownloadBatch>(&format!(
        "UPDATE download_batches SET priority = $2, updated_at = now() WHERE id = $1 \
         RETURNING {BATCH_COLUMNS}"
    ))
    .bind(batch_id)
    .bind(priority.clamp(-1000, 1000))
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::missing("批次不存在"))
}

/// 把批次内任务全部终态的批次自动置为已完成，返回受影响的批次编号。
pub async fn complete_finished_batches(executor: impl PgExecutor<'_>) -> AppResult<Vec<Uuid>> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE download_batches b SET status = $1, updated_at = now() \
         WHERE b.status = $2 \
           AND NOT EXISTS ( \
               SELECT 1 FROM batch_books bb \
               LEFT JOIN book_tasks t ON t.book_id = bb.book_id AND t.format = b.download_format \
               WHERE bb.batch_id = b.id \
                 AND (t.id IS NULL OR t.status NOT IN ('已完成', '失败', '已跳过', '已取消')) \
           ) \
         RETURNING b.id",
    )
    .bind(BatchStatus::Completed.as_str())
    .bind(BatchStatus::Running.as_str())
    .fetch_all(executor)
    .await?;
    Ok(ids)
}
