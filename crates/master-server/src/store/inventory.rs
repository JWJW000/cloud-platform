//! 馆藏扫描与位置数据访问层（方案第 5 节、第 10 节）。

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

/// 存储位置数据模型。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct StorageLocationRecord {
    /// 存储位置主键
    pub id: Uuid,
    /// 关联的 Worker 节点（可选）
    pub node_id: Option<Uuid>,
    /// 根目录别名
    pub root_key: String,
    /// 后端类型
    pub backend: String,
    /// 显示名称
    pub display_name: String,
    /// 可用性状态
    pub availability: String,
    /// 最后活跃时间
    pub last_seen_at: Option<DateTime<Utc>>,
    /// 创建时间
    pub created_at: DateTime<Utc>,
    /// 更新时间
    pub updated_at: DateTime<Utc>,
}

/// 扫描任务数据模型。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct InventoryScanJobRecord {
    /// 任务编号
    pub id: Uuid,
    /// 节点编号
    pub node_id: Uuid,
    /// 存储位置编号
    pub storage_location_id: Uuid,
    /// 任务状态
    pub status: String,
    /// 扫描模式（增量 / 全量复核）
    pub scan_mode: String,
    /// 检查点 JSON
    pub checkpoint: serde_json::Value,
    /// 已发现数量
    pub discovered_count: i64,
    /// 已哈希数量
    pub hashed_count: i64,
    /// 唯一匹配数量
    pub matched_count: i64,
    /// 待审核数量
    pub review_count: i64,
    /// 未匹配数量
    pub unmatched_count: i64,
    /// 跳过数量
    pub skipped_count: i64,
    /// 错误数量
    pub error_count: i64,
    /// 开始时间
    pub started_at: Option<DateTime<Utc>>,
    /// 完成时间
    pub finished_at: Option<DateTime<Utc>>,
    /// 最后错误信息
    pub last_error: Option<String>,
    /// 创建人
    pub created_by: Option<Uuid>,
    /// 创建时间
    pub created_at: DateTime<Utc>,
    /// 更新时间
    pub updated_at: DateTime<Utc>,
}

/// 待确认审核条目详情。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryReviewDetail {
    /// 条目主键
    pub id: Uuid,
    /// 扫描任务编号
    pub scan_job_id: Uuid,
    /// 相对路径
    pub object_key: String,
    /// 文件名
    pub file_name: String,
    /// 扩展名
    pub extension: String,
    /// 实际字节大小
    pub actual_size_bytes: i64,
    /// SHA256 哈希
    pub sha256: String,
    /// MD5 哈希
    pub md5: Option<String>,
    /// 错误/待审核原因
    pub error_reason: Option<String>,
    /// 候选列表
    pub candidates: Vec<InventoryCandidateDetail>,
}

/// 候选版本明细。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryCandidateDetail {
    /// 候选记录主键
    pub candidate_id: Uuid,
    /// 候选版本主键
    pub edition_id: Uuid,
    /// 版本书名
    pub edition_title: String,
    /// 出版社
    pub publisher: Option<String>,
    /// 出版年份
    pub publish_year: Option<i32>,
    /// 匹配评分
    pub match_score: i32,
    /// 命中字段
    pub matched_fields: serde_json::Value,
    /// 冲突字段
    pub conflict_fields: serde_json::Value,
}

/// 获取所有已登记的存储位置。
pub async fn list_storage_locations(pool: &PgPool) -> Result<Vec<StorageLocationRecord>> {
    let rows = sqlx::query_as::<_, StorageLocationRecord>(
        "SELECT * FROM storage_locations ORDER BY created_at DESC",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// 创建或更新节点上报的存储位置。
pub async fn upsert_storage_location(
    pool: &PgPool,
    node_id: Uuid,
    root_key: &str,
    backend: &str,
    display_name: &str,
    availability: &str,
) -> Result<StorageLocationRecord> {
    let row = sqlx::query_as::<_, StorageLocationRecord>(
        "INSERT INTO storage_locations
             (id, node_id, root_key, backend, display_name, availability, last_seen_at)
         VALUES ($1, $2, $3, $4, $5, $6, now())
         ON CONFLICT (node_id, root_key) DO UPDATE SET
             backend = EXCLUDED.backend,
             display_name = EXCLUDED.display_name,
             availability = EXCLUDED.availability,
             last_seen_at = now(),
             updated_at = now()
         RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(node_id)
    .bind(root_key)
    .bind(backend)
    .bind(display_name)
    .bind(availability)
    .fetch_one(pool)
    .await?;

    Ok(row)
}

/// 创建扫描任务。
pub async fn create_scan_job(
    pool: &PgPool,
    node_id: Uuid,
    storage_location_id: Uuid,
    scan_mode: &str,
    created_by: Option<Uuid>,
) -> Result<InventoryScanJobRecord> {
    let row = sqlx::query_as::<_, InventoryScanJobRecord>(
        "INSERT INTO inventory_scan_jobs
             (id, node_id, storage_location_id, status, scan_mode, created_by)
         VALUES ($1, $2, $3, '待下发', $4, $5)
         RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(node_id)
    .bind(storage_location_id)
    .bind(scan_mode)
    .bind(created_by)
    .fetch_one(pool)
    .await?;

    Ok(row)
}

/// 获取扫描任务列表。
pub async fn list_scan_jobs(pool: &PgPool) -> Result<Vec<InventoryScanJobRecord>> {
    let rows = sqlx::query_as::<_, InventoryScanJobRecord>(
        "SELECT * FROM inventory_scan_jobs ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// 获取单个扫描任务。
pub async fn get_scan_job(pool: &PgPool, job_id: Uuid) -> Result<Option<InventoryScanJobRecord>> {
    let row = sqlx::query_as::<_, InventoryScanJobRecord>(
        "SELECT * FROM inventory_scan_jobs WHERE id = $1",
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// 更新扫描任务状态。
pub async fn update_scan_job_status(
    pool: &PgPool,
    job_id: Uuid,
    status: &str,
    last_error: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "UPDATE inventory_scan_jobs SET
             status = $2,
             last_error = COALESCE($3, last_error),
             updated_at = now()
         WHERE id = $1",
    )
    .bind(job_id)
    .bind(status)
    .bind(last_error)
    .execute(pool)
    .await?;
    Ok(())
}

type RawPendingReviewRow = (
    Uuid,
    Uuid,
    String,
    String,
    String,
    i64,
    String,
    Option<String>,
    Option<String>,
);

type RawCandidateRow = (
    Uuid,
    Uuid,
    String,
    Option<String>,
    Option<i32>,
    i32,
    serde_json::Value,
    serde_json::Value,
);

/// 获取待确认审核列表及关联候选。
pub async fn list_pending_reviews(pool: &PgPool, limit: i64) -> Result<Vec<InventoryReviewDetail>> {
    let entries: Vec<RawPendingReviewRow> = sqlx::query_as(
        "SELECT id, scan_job_id, object_key, file_name, extension, actual_size_bytes, sha256, md5, error_reason
         FROM inventory_scan_entries
         WHERE resolution_status = '待确认'
         ORDER BY created_at DESC
         LIMIT $1"
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;

    let mut result = Vec::new();
    for (
        id,
        scan_job_id,
        object_key,
        file_name,
        extension,
        actual_size_bytes,
        sha256,
        md5,
        error_reason,
    ) in entries
    {
        let candidates_rows: Vec<RawCandidateRow> = sqlx::query_as(
            "SELECT c.id, c.edition_id, e.edition_title, e.publisher, e.publish_year, c.match_score, c.matched_fields, c.conflict_fields
             FROM inventory_match_candidates c
             JOIN editions e ON c.edition_id = e.id
             WHERE c.scan_entry_id = $1
             ORDER BY c.match_score DESC"
        )
        .bind(id)
        .fetch_all(pool)
        .await?;

        let candidates = candidates_rows
            .into_iter()
            .map(|(cid, eid, title, publ, yr, score, m_fields, c_fields)| {
                InventoryCandidateDetail {
                    candidate_id: cid,
                    edition_id: eid,
                    edition_title: title,
                    publisher: publ,
                    publish_year: yr,
                    match_score: score,
                    matched_fields: m_fields,
                    conflict_fields: c_fields,
                }
            })
            .collect();

        result.push(InventoryReviewDetail {
            id,
            scan_job_id,
            object_key,
            file_name,
            extension,
            actual_size_bytes,
            sha256,
            md5,
            error_reason,
            candidates,
        });
    }

    Ok(result)
}
