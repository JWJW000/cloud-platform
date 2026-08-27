//! 总库获取目标到现有 Worker 下载协议的兼容适配。
//!
//! 外部 seam 保持现有 `AssignTask` / `TaskProgress` / `TaskResult` 不变；本模块隐藏
//! acquisition_targets 与旧下载状态机之间的映射、执行审计和馆藏证据闭环。

use platform_domain::ExecutionResult;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::scheduler::FileEvidence;

const PDF_BATCH_ID: Uuid = Uuid::from_u128(0x000000000000000000000000000000ac);
const EPUB_BATCH_ID: Uuid = Uuid::from_u128(0x000000000000000000000000000000ad);

#[derive(Debug, sqlx::FromRow)]
struct CatalogCandidate {
    target_id: Uuid,
    edition_id: Uuid,
    title: String,
    author: Option<String>,
    publisher: Option<String>,
    isbn: Option<String>,
    format: String,
    source_asset_id: Option<Uuid>,
    max_attempts: i32,
}

/// 按优先级物化至多一个总库目标，使现有领取事务可以直接分配给 Worker。
///
/// 目标行使用 `FOR UPDATE SKIP LOCKED`，镜像 task 与 target 共用 UUID；并发 Worker
/// 不会为同一目标生成两份任务。没有待下载目标时是正常空操作。
pub async fn materialize_next_target(tx: &mut Transaction<'_, Postgres>) -> AppResult<bool> {
    let candidate = sqlx::query_as::<_, CatalogCandidate>(
        "SELECT at.id AS target_id, e.id AS edition_id, e.edition_title AS title, \
                (SELECT c.name FROM edition_contributors ec \
                 JOIN contributors c ON c.id = ec.contributor_id \
                 WHERE ec.edition_id = e.id ORDER BY ec.sort_order LIMIT 1) AS author, \
                e.publisher, \
                (SELECT i.normalized_value FROM identifiers i \
                 WHERE i.object_id = e.id AND i.identifier_type IN ('isbn13', 'isbn10') \
                   AND i.is_valid ORDER BY (i.identifier_type = 'isbn13') DESC LIMIT 1) AS isbn, \
                COALESCE(asset.format, 'pdf') AS format, asset.id AS source_asset_id, \
                at.max_attempts \
         FROM acquisition_targets at \
         JOIN editions e ON e.id = at.edition_id \
         LEFT JOIN LATERAL ( \
             SELECT sa.id, lower(sa.format) AS format \
             FROM record_resolutions rr \
             JOIN source_assets sa ON sa.source_record_id = rr.source_record_id \
             WHERE rr.edition_id = at.edition_id AND sa.status = '可用' \
               AND lower(sa.format) IN ('pdf', 'epub') \
             ORDER BY (lower(sa.format) = 'epub') DESC, sa.created_at ASC LIMIT 1 \
         ) asset ON TRUE \
         WHERE at.status IN ('待下载', '排队中', '暂时失败') \
           AND at.next_attempt_at <= now() \
           AND (at.lease_expires_at IS NULL OR at.lease_expires_at < now()) \
           AND NOT EXISTS (SELECT 1 FROM book_tasks bt WHERE bt.id = at.id) \
         ORDER BY at.priority DESC, (asset.id IS NOT NULL) DESC, at.next_attempt_at, at.created_at \
         FOR UPDATE OF at SKIP LOCKED LIMIT 1",
    )
    .fetch_optional(&mut **tx)
    .await?;

    let Some(row) = candidate else {
        return Ok(false);
    };

    let normalized_title = row.title.trim().to_lowercase();
    let normalized_author = row.author.as_deref().map(|v| v.trim().to_lowercase());
    let normalized_publisher = row.publisher.as_deref().map(|v| v.trim().to_lowercase());
    let normalized_isbn = row.isbn.as_deref().map(|v| v.trim().to_string());
    let dedup_key = format!("catalog-edition:{}", row.edition_id);

    let (book_id, book_seq): (Uuid, i64) = sqlx::query_as(
        "INSERT INTO books \
             (id, raw_title, raw_author, raw_publisher, raw_isbn, normalized_title, \
              normalized_author, normalized_publisher, normalized_isbn, dedup_key, verify_status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, '已确认') \
         ON CONFLICT (dedup_key) DO UPDATE SET \
             raw_title = EXCLUDED.raw_title, raw_author = EXCLUDED.raw_author, \
             raw_publisher = EXCLUDED.raw_publisher, raw_isbn = EXCLUDED.raw_isbn, \
             updated_at = now() \
         RETURNING id, seq",
    )
    .bind(row.edition_id)
    .bind(&row.title)
    .bind(&row.author)
    .bind(&row.publisher)
    .bind(&row.isbn)
    .bind(normalized_title)
    .bind(normalized_author)
    .bind(normalized_publisher)
    .bind(normalized_isbn)
    .bind(dedup_key)
    .fetch_one(&mut **tx)
    .await?;

    let (batch_id, batch_name) = if row.format == "epub" {
        (EPUB_BATCH_ID, "总库全局获取池（EPUB）")
    } else {
        (PDF_BATCH_ID, "总库全局获取池（PDF）")
    };
    sqlx::query(
        "INSERT INTO download_batches \
             (id, name, source_file, status, priority, download_format) \
         VALUES ($1, $2, 'catalog://global', '执行中', 0, $3) \
         ON CONFLICT (id) DO UPDATE SET \
             status = CASE \
                 WHEN download_batches.status IN ('已暂停', '已取消') THEN download_batches.status \
                 ELSE '执行中' \
             END, \
             updated_at = now()",
    )
    .bind(batch_id)
    .bind(batch_name)
    .bind(&row.format)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        "INSERT INTO book_tasks (id, book_id, format, status, max_attempts) \
         VALUES ($1, $2, $3, '待处理', $4) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(row.target_id)
    .bind(book_id)
    .bind(&row.format)
    .bind(row.max_attempts.max(1))
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        "INSERT INTO batch_books (id, batch_id, book_id, import_line, display_status) \
         VALUES ($1, $2, $3, $4, '待处理') \
         ON CONFLICT (batch_id, book_id) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(batch_id)
    .bind(book_id)
    .bind(book_seq.min(i32::MAX as i64) as i32)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        "UPDATE acquisition_targets SET active_source_asset_id = $2, updated_at = now() \
         WHERE id = $1",
    )
    .bind(row.target_id)
    .bind(row.source_asset_id)
    .execute(&mut **tx)
    .await?;

    Ok(true)
}

/// 为镜像任务建立总库执行审计。普通旧任务是空操作。
pub async fn execution_started(
    tx: &mut Transaction<'_, Postgres>,
    target_id: Uuid,
    execution_id: Uuid,
    node_id: Uuid,
    session_id: Uuid,
    slot_index: i32,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO acquisition_executions \
             (id, target_id, source_asset_id, node_id, session_id, slot_index, stage) \
         SELECT $2, at.id, at.active_source_asset_id, $3, $4, $5, '已领取' \
         FROM acquisition_targets at WHERE at.id = $1 \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(target_id)
    .bind(execution_id)
    .bind(node_id)
    .bind(session_id)
    .bind(slot_index)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// 将 Worker 接受事件同步到总库执行时间线。
pub async fn task_accepted(
    executor: impl sqlx::PgExecutor<'_>,
    target_id: Uuid,
    execution_id: Uuid,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE acquisition_executions SET stage = '下载中' \
         WHERE id = $1 AND target_id = $2 AND finished_at IS NULL",
    )
    .bind(execution_id)
    .bind(target_id)
    .execute(executor)
    .await?;
    Ok(())
}

/// 将受租约保护的 Worker 进度同步到总库执行时间线。
pub async fn progress(
    executor: impl sqlx::PgExecutor<'_>,
    target_id: Uuid,
    execution_id: Uuid,
    stage: &str,
) -> AppResult<()> {
    let safe_stage: String = stage.chars().take(200).collect();
    sqlx::query(
        "UPDATE acquisition_executions SET stage = $3 \
         WHERE id = $1 AND target_id = $2 AND finished_at IS NULL",
    )
    .bind(execution_id)
    .bind(target_id)
    .bind(safe_stage)
    .execute(executor)
    .await?;
    Ok(())
}

/// 在旧任务成功事务内建立总库文件、物理位置、holding 和执行结果。
pub async fn success_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    target_id: Uuid,
    execution_id: Uuid,
    node_id: Option<Uuid>,
    file: &FileEvidence,
) -> AppResult<()> {
    if file.sha256.len() != 64 || !file.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(AppError::bad("SHA-256 必须为 64 位十六进制字符串"));
    }
    let target: Option<(Uuid, Option<Uuid>)> = sqlx::query_as(
        "SELECT edition_id, active_source_asset_id FROM acquisition_targets WHERE id = $1",
    )
    .bind(target_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((edition_id, source_asset_id)) = target else {
        return Ok(());
    };

    let sha256 = file.sha256.to_ascii_lowercase();
    let inserted_file_id = Uuid::new_v4();
    let library_file_id: Uuid = sqlx::query_scalar(
        "INSERT INTO library_files \
             (id, storage_backend, object_key, format, actual_size_bytes, sha256, verify_status, verified_at) \
         VALUES ($1, 'NAS', $2, $3, $4, $5, '有效', now()) \
         ON CONFLICT (sha256) DO UPDATE SET \
             actual_size_bytes = EXCLUDED.actual_size_bytes, verify_status = '有效', \
             verified_at = now(), updated_at = now() \
         RETURNING id",
    )
    .bind(inserted_file_id)
    .bind(&file.nas_relative_path)
    .bind(file.format.to_ascii_lowercase())
    .bind(file.size_bytes)
    .bind(&sha256)
    .fetch_one(&mut **tx)
    .await?;

    if let Some(node_id) = node_id {
        let location_id: Uuid = sqlx::query_scalar(
            "INSERT INTO storage_locations \
                 (id, node_id, root_key, backend, display_name, availability, last_seen_at) \
             VALUES ($1, $2, 'worker_downloads', 'NAS', 'Worker 下载目录', '在线', now()) \
             ON CONFLICT (node_id, root_key) DO UPDATE SET \
                 availability = '在线', last_seen_at = now(), updated_at = now() \
             RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(node_id)
        .fetch_one(&mut **tx)
        .await?;
        sqlx::query(
            "INSERT INTO library_file_locations \
                 (id, library_file_id, storage_location_id, object_key, actual_size_bytes, \
                  verify_status, verified_at, last_seen_at) \
             VALUES ($1, $2, $3, $4, $5, '有效', now(), now()) \
             ON CONFLICT (storage_location_id, object_key) DO UPDATE SET \
                 library_file_id = EXCLUDED.library_file_id, \
                 actual_size_bytes = EXCLUDED.actual_size_bytes, verify_status = '有效', \
                 verified_at = now(), last_seen_at = now(), updated_at = now()",
        )
        .bind(Uuid::new_v4())
        .bind(library_file_id)
        .bind(location_id)
        .bind(&file.nas_relative_path)
        .bind(file.size_bytes)
        .execute(&mut **tx)
        .await?;
    }

    let holding_id: Uuid = sqlx::query_scalar(
        "INSERT INTO holdings \
             (id, edition_id, library_file_id, source_asset_id, match_type, meets_strategy) \
         VALUES ($1, $2, $3, $4, 'Worker校验入库', TRUE) \
         ON CONFLICT (edition_id, library_file_id) DO UPDATE SET \
             source_asset_id = COALESCE(EXCLUDED.source_asset_id, holdings.source_asset_id), \
             meets_strategy = TRUE \
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(edition_id)
    .bind(library_file_id)
    .bind(source_asset_id)
    .fetch_one(&mut **tx)
    .await?;

    sqlx::query(
        "UPDATE acquisition_targets SET status = '已下载', satisfied_holding_id = $2, \
             lease_node_id = NULL, lease_session_id = NULL, lease_execution_id = NULL, \
             lease_expires_at = NULL, last_error = NULL, updated_at = now() WHERE id = $1",
    )
    .bind(target_id)
    .bind(holding_id)
    .execute(&mut **tx)
    .await?;
    finish_execution(tx, execution_id, "成功", "已完成", None).await
}

/// 在旧任务失败事务内收尾对应的总库执行审计。
pub async fn failure_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    execution_id: Uuid,
    result: ExecutionResult,
    reason: &str,
) -> AppResult<()> {
    let catalog_result = match result {
        ExecutionResult::Cancelled => "取消",
        ExecutionResult::Uncertain => "超时",
        _ => "失败",
    };
    let safe_reason: String = reason.chars().take(2000).collect();
    finish_execution(
        tx,
        execution_id,
        catalog_result,
        "已结束",
        Some(&safe_reason),
    )
    .await?;
    // target 状态、租约与重试时间由 book_tasks 同步触发器原子推进。
    Ok(())
}

async fn finish_execution(
    tx: &mut Transaction<'_, Postgres>,
    execution_id: Uuid,
    result: &str,
    stage: &str,
    error: Option<&str>,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE acquisition_executions SET result = $2, stage = $3, error_message = $4, \
             finished_at = COALESCE(finished_at, now()) WHERE id = $1",
    )
    .bind(execution_id)
    .bind(result)
    .bind(stage)
    .bind(error)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
