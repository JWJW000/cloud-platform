//! “总库即已拥有”的匹配与下载成功入库逻辑。
//!
//! 下载候选在成功前只存在于旧 Worker 任务表；只有成功并取得可信文件证据后，
//! 才在 `works` / `editions` 中创建书目并登记文件资产。

use platform_domain::{BookIdentity, DedupKey, ResolutionStatus, VerifyStatus};
use sqlx::{PgExecutor, Postgres, Transaction};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::scheduler::FileEvidence;

/// 用足够可靠的身份信息查询总库版本。
///
/// ISBN 是强匹配；无 ISBN 时优先使用书名、作者、出版社三项匹配。输入只有书名时
/// 沿用系统原有的标题去重规则：规范书名命中总库即可跳过，避免重复抓取。
pub async fn find_owned_edition<'e, E>(
    executor: E,
    identity: &BookIdentity,
) -> AppResult<Option<Uuid>>
where
    E: PgExecutor<'e>,
{
    match &identity.dedup_key {
        DedupKey::Isbn(isbn) => {
            let id = sqlx::query_scalar(
                "SELECT e.id FROM identifiers i \
                 JOIN editions e ON e.id = i.object_id \
                 JOIN works w ON w.id = e.work_id \
                 WHERE i.object_type = 'edition' AND i.is_valid \
                   AND i.identifier_type IN ('isbn13', 'isbn10') \
                   AND i.normalized_value = $1 \
                   AND e.status NOT IN ('已合并', '已拆分') \
                   AND w.resolution_status NOT IN ('已合并', '已忽略') \
                 ORDER BY e.created_at LIMIT 1",
            )
            .bind(isbn)
            .fetch_optional(executor)
            .await?;
            Ok(id)
        }
        DedupKey::TitleAuthorPublisher(_) => {
            let Some(author) = identity.normalized_author.as_deref() else {
                return Ok(None);
            };
            let Some(publisher) = identity.normalized_publisher.as_deref() else {
                return Ok(None);
            };
            let candidates: Vec<(Uuid, Option<String>)> = sqlx::query_as(
                "SELECT e.id, e.publisher FROM works w \
                 JOIN editions e ON e.work_id = w.id \
                 WHERE w.normalized_title = $1 \
                   AND e.status NOT IN ('已合并', '已拆分') \
                   AND w.resolution_status NOT IN ('已合并', '已忽略') \
                   AND EXISTS ( \
                       SELECT 1 FROM edition_contributors ec \
                       JOIN contributors c ON c.id = ec.contributor_id \
                       WHERE ec.edition_id = e.id AND c.normalized_name = $2 \
                   ) \
                 ORDER BY e.created_at LIMIT 20",
            )
            .bind(&identity.normalized_title)
            .bind(author)
            .fetch_all(executor)
            .await?;

            Ok(candidates.into_iter().find_map(|(id, raw_publisher)| {
                raw_publisher
                    .as_deref()
                    .map(platform_domain::normalize_person)
                    .filter(|normalized| normalized == publisher)
                    .map(|_| id)
            }))
        }
        DedupKey::TitleOnly(_) => {
            let id = sqlx::query_scalar(
                "SELECT e.id FROM works w \
                 JOIN editions e ON e.work_id = w.id \
                 WHERE w.normalized_title = $1 \
                   AND e.status NOT IN ('已合并', '已拆分') \
                   AND w.resolution_status NOT IN ('已合并', '已忽略') \
                 ORDER BY e.created_at LIMIT 1",
            )
            .bind(&identity.normalized_title)
            .fetch_optional(executor)
            .await?;
            Ok(id)
        }
    }
}

/// 把一次普通下载任务的书目提升为“已拥有”的总库版本。
pub async fn promote_downloaded_book(
    tx: &mut Transaction<'_, Postgres>,
    book_id: Uuid,
    format: &str,
) -> AppResult<Uuid> {
    let row: (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<Uuid>,
    ) = sqlx::query_as(
        "SELECT raw_title, raw_author, raw_publisher, raw_isbn, catalog_edition_id \
             FROM books WHERE id = $1 FOR UPDATE",
    )
    .bind(book_id)
    .fetch_one(&mut **tx)
    .await?;

    if let Some(edition_id) = row.4 {
        return Ok(edition_id);
    }

    // catalog_bridge 的兼容镜像历史上让 book_id 与 edition_id 相同。
    let same_id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM editions WHERE id = $1")
        .bind(book_id)
        .fetch_optional(&mut **tx)
        .await?;
    if let Some(edition_id) = same_id {
        link_book_to_edition(tx, book_id, edition_id).await?;
        return Ok(edition_id);
    }

    let identity =
        BookIdentity::from_raw(&row.0, row.1.as_deref(), row.2.as_deref(), row.3.as_deref())
            .ok_or_else(|| AppError::bad("下载成功但书名为空，无法加入总库"))?;

    if let Some(edition_id) = find_owned_edition(&mut **tx, &identity).await? {
        link_book_to_edition(tx, book_id, edition_id).await?;
        return Ok(edition_id);
    }

    let resolution = if identity.verify_status == VerifyStatus::Confirmed {
        ResolutionStatus::Confirmed
    } else {
        ResolutionStatus::Ambiguous
    };
    let work_id = Uuid::new_v4();
    let edition_id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO works \
             (id, work_type, preferred_title, normalized_title, primary_language, resolution_status) \
         VALUES ($1, '整书', $2, $3, 'und', $4)",
    )
    .bind(work_id)
    .bind(&identity.raw_title)
    .bind(&identity.normalized_title)
    .bind(resolution.as_str())
    .execute(&mut **tx)
    .await?;

    let publisher_id: Option<Uuid> = if let Some(raw_publisher) = identity.raw_publisher.as_deref()
    {
        let normalized = crate::store::publishers::normalize_publisher_name(raw_publisher);
        sqlx::query_scalar(
            "SELECT publisher_id FROM publisher_aliases WHERE normalized_alias = $1 \
             UNION ALL SELECT id FROM publishers WHERE normalized_name = $1 LIMIT 1",
        )
        .bind(normalized)
        .fetch_optional(&mut **tx)
        .await?
    } else {
        None
    };

    sqlx::query(
        "INSERT INTO editions \
             (id, work_id, edition_title, language, publisher, publisher_id, format_summary, status) \
         VALUES ($1, $2, $3, 'und', $4, $5, $6, $7)",
    )
    .bind(edition_id)
    .bind(work_id)
    .bind(&identity.raw_title)
    .bind(&identity.raw_publisher)
    .bind(publisher_id)
    .bind(format.trim().to_ascii_lowercase())
    .bind(resolution.as_str())
    .execute(&mut **tx)
    .await?;

    if let Some(isbn) = identity.normalized_isbn.as_deref() {
        sqlx::query(
            "INSERT INTO identifiers \
                 (id, object_type, object_id, identifier_type, raw_value, normalized_value, is_valid) \
             VALUES ($1, 'edition', $2, 'isbn13', $3, $4, TRUE)",
        )
        .bind(Uuid::new_v4())
        .bind(edition_id)
        .bind(identity.raw_isbn.as_deref().unwrap_or(isbn))
        .bind(isbn)
        .execute(&mut **tx)
        .await?;
    }

    if let (Some(raw_author), Some(normalized_author)) = (
        identity.raw_author.as_deref(),
        identity.normalized_author.as_deref(),
    ) {
        let contributor_id: Uuid = sqlx::query_scalar(
            "INSERT INTO contributors (id, name, normalized_name) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (normalized_name) DO UPDATE SET name = EXCLUDED.name \
             RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(raw_author)
        .bind(normalized_author)
        .fetch_one(&mut **tx)
        .await?;
        sqlx::query(
            "INSERT INTO edition_contributors \
                 (id, edition_id, contributor_id, role, sort_order) \
             VALUES ($1, $2, $3, '作者', 0) \
             ON CONFLICT (edition_id, contributor_id, role) DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(edition_id)
        .bind(contributor_id)
        .execute(&mut **tx)
        .await?;
    }

    link_book_to_edition(tx, book_id, edition_id).await?;
    Ok(edition_id)
}

async fn link_book_to_edition(
    tx: &mut Transaction<'_, Postgres>,
    book_id: Uuid,
    edition_id: Uuid,
) -> AppResult<()> {
    sqlx::query("UPDATE books SET catalog_edition_id = $2, updated_at = now() WHERE id = $1")
        .bind(book_id)
        .bind(edition_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// 为已拥有版本登记当前可用的文件资产；拥有关系不依赖 NAS，文件只是附属资产。
pub async fn record_owned_file(
    tx: &mut Transaction<'_, Postgres>,
    edition_id: Uuid,
    source_asset_id: Option<Uuid>,
    node_id: Option<Uuid>,
    file: &FileEvidence,
    match_type: &str,
) -> AppResult<Uuid> {
    if file.sha256.len() != 64 || !file.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(AppError::bad("SHA-256 必须为 64 位十六进制字符串"));
    }
    let sha256 = file.sha256.to_ascii_lowercase();
    let library_file_id: Uuid = sqlx::query_scalar(
        "INSERT INTO library_files \
             (id, storage_backend, object_key, format, actual_size_bytes, sha256, verify_status, verified_at) \
         VALUES ($1, 'NAS', $2, $3, $4, $5, '有效', now()) \
         ON CONFLICT (sha256) DO UPDATE SET \
             actual_size_bytes = EXCLUDED.actual_size_bytes, verify_status = '有效', \
             verified_at = now(), updated_at = now() \
         RETURNING id",
    )
    .bind(Uuid::new_v4())
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
         VALUES ($1, $2, $3, $4, $5, TRUE) \
         ON CONFLICT (edition_id, library_file_id) DO UPDATE SET \
             source_asset_id = COALESCE(EXCLUDED.source_asset_id, holdings.source_asset_id), \
             meets_strategy = TRUE \
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(edition_id)
    .bind(library_file_id)
    .bind(source_asset_id)
    .bind(match_type)
    .fetch_one(&mut **tx)
    .await?;

    // 出版社列表使用物化统计，下载成功后在同一事务内刷新对应出版社，避免总库
    // 已经增加但出版社页面数字仍停留在旧值。
    sqlx::query(
        "WITH target AS (SELECT publisher_id FROM editions WHERE id = $1), \
              stats AS ( \
                  SELECT count(DISTINCT e.work_id) AS works_c, \
                         count(DISTINCT e.id) AS editions_c, \
                         count(DISTINCT CASE WHEN lf.verify_status = '有效' THEN h.id END) AS files_c, \
                         count(DISTINCT CASE WHEN lf.verify_status = '有效' AND h.meets_strategy \
                                             THEN e.id END) AS editions_with_files_c \
                  FROM editions e \
                  LEFT JOIN holdings h ON h.edition_id = e.id \
                  LEFT JOIN library_files lf ON lf.id = h.library_file_id \
                  WHERE e.publisher_id = (SELECT publisher_id FROM target) \
              ) \
         UPDATE publishers p SET works_count = stats.works_c, editions_count = stats.editions_c, \
             holdings_count = stats.files_c, acquired_count = stats.editions_with_files_c, \
             updated_at = now() \
         FROM stats WHERE p.id = (SELECT publisher_id FROM target)",
    )
    .bind(edition_id)
    .execute(&mut **tx)
    .await?;
    Ok(holding_id)
}
