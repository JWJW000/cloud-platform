//! 导入任务存储（V6 方案两阶段文件导入）。

use chrono::{DateTime, Utc};
use platform_domain::{ImportStatus, ImportType};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::ImportJob;

const IMPORT_JOB_COLUMNS: &str = "id, import_type, status, original_file_name, file_sha256, \
     temp_path, token_hash, created_by, expires_at, committed_at, committed_resource_id, \
     summary, payload_encrypted, created_at, updated_at";

/// 新建导入任务。
#[derive(Debug, Clone)]
pub struct NewImportJob {
    /// 导入类型。
    pub import_type: ImportType,
    /// 原始文件名。
    pub original_file_name: String,
    /// 文件 SHA-256。
    pub file_sha256: String,
    /// 暂存路径。
    pub temp_path: Option<String>,
    /// 令牌哈希。
    pub token_hash: String,
    /// 创建人。
    pub created_by: Option<Uuid>,
    /// 过期时间。
    pub expires_at: DateTime<Utc>,
    /// 统计摘要。
    pub summary: serde_json::Value,
    /// 加密载荷。
    pub payload_encrypted: Option<String>,
}

/// 创建导入任务记录。
pub async fn create_import_job(
    executor: impl PgExecutor<'_>,
    new: &NewImportJob,
) -> AppResult<ImportJob> {
    let job = sqlx::query_as::<_, ImportJob>(&format!(
        "INSERT INTO import_jobs \
             (id, import_type, status, original_file_name, file_sha256, temp_path, \
              token_hash, created_by, expires_at, summary, payload_encrypted) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
         RETURNING {IMPORT_JOB_COLUMNS}"
    ))
    .bind(Uuid::new_v4())
    .bind(new.import_type.as_str())
    .bind(ImportStatus::PendingConfirm.as_str())
    .bind(&new.original_file_name)
    .bind(&new.file_sha256)
    .bind(&new.temp_path)
    .bind(&new.token_hash)
    .bind(new.created_by)
    .bind(new.expires_at)
    .bind(&new.summary)
    .bind(&new.payload_encrypted)
    .fetch_one(executor)
    .await?;
    Ok(job)
}

/// 按 token_hash 查询有效导入任务（未过期）。
pub async fn get_valid_job_by_token_hash(
    executor: impl PgExecutor<'_>,
    token_hash: &str,
) -> AppResult<ImportJob> {
    let job = sqlx::query_as::<_, ImportJob>(&format!(
        "SELECT {IMPORT_JOB_COLUMNS} FROM import_jobs \
         WHERE token_hash = $1 AND expires_at > now() AND status = $2"
    ))
    .bind(token_hash)
    .bind(ImportStatus::PendingConfirm.as_str())
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::missing("导入令牌无效或已过期，请重新上传文件"))?;
    Ok(job)
}

/// 按 ID 查询导入任务。
pub async fn get_import_job(executor: impl PgExecutor<'_>, id: Uuid) -> AppResult<ImportJob> {
    let job = sqlx::query_as::<_, ImportJob>(&format!(
        "SELECT {IMPORT_JOB_COLUMNS} FROM import_jobs WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::missing("导入任务不存在"))?;
    Ok(job)
}

/// 标记导入任务已提交（幂等）。
pub async fn mark_import_job_committed(
    executor: impl PgExecutor<'_>,
    id: Uuid,
    committed_resource_id: Uuid,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE import_jobs SET status = $2, committed_at = now(), \
             committed_resource_id = $3, temp_path = NULL, payload_encrypted = NULL, \
             updated_at = now() \
         WHERE id = $1",
    )
    .bind(id)
    .bind(ImportStatus::Committed.as_str())
    .bind(committed_resource_id)
    .execute(executor)
    .await?;
    Ok(())
}

/// 标记导入任务失败。
pub async fn mark_import_job_failed(
    executor: impl PgExecutor<'_>,
    id: Uuid,
    reason: &str,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE import_jobs SET status = $2, \
             summary = summary || jsonb_build_object('error', $3::text), \
             temp_path = NULL, payload_encrypted = NULL, \
             updated_at = now() \
         WHERE id = $1",
    )
    .bind(id)
    .bind(ImportStatus::Failed.as_str())
    .bind(reason)
    .execute(executor)
    .await?;
    Ok(())
}

/// 删除导入任务。
pub async fn delete_import_job(executor: impl PgExecutor<'_>, id: Uuid) -> AppResult<bool> {
    let affected = sqlx::query("DELETE FROM import_jobs WHERE id = $1")
        .bind(id)
        .execute(executor)
        .await?
        .rows_affected();
    Ok(affected > 0)
}

/// 清理过期导入任务。
pub async fn cleanup_expired_jobs(pool: &PgPool) -> AppResult<u64> {
    let rows = sqlx::query(
        "UPDATE import_jobs SET status = $1, temp_path = NULL, payload_encrypted = NULL, \
             updated_at = now() \
         WHERE status = $2 AND expires_at <= now()",
    )
    .bind(ImportStatus::Expired.as_str())
    .bind(ImportStatus::PendingConfirm.as_str())
    .execute(pool)
    .await?
    .rows_affected();
    Ok(rows)
}
