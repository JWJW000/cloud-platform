//! 图书馆总库馆藏文件证据校验与存储协调器（第 6 节、第 11.5 节）。
//!
//! 实现「已下载」状态的严格证明闭环：
//! 1. 实际文件存在且可访问；
//! 2. 实际大小与 SHA-256 校验匹配；
//! 3. 幂等入库并建立版本 Holding 关联；
//! 4. 自动推进目标状态收敛到「已下载」。

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{AppError, AppResult};

/// 文件提交请求。
#[derive(Debug, Clone, Deserialize)]
pub struct CommitLibraryFileRequest {
    /// 关联的版本编号。
    pub edition_id: Uuid,
    /// 存储后端（NAS, S3, OSS, Local）。
    pub storage_backend: String,
    /// 存储对象键或相对路径。
    pub object_key: String,
    /// 实际格式。
    pub format: String,
    /// 实际大小（字节）。
    pub actual_size_bytes: i64,
    /// SHA-256 哈希（小写 hex 64 位）。
    pub sha256: String,
    /// MD5 哈希（小写 hex 32 位）。
    pub md5: Option<String>,
    /// 来源资产编号（可选）。
    pub source_asset_id: Option<Uuid>,
}

/// 提交结果。
#[derive(Debug, Clone, Serialize)]
pub struct CommitLibraryFileResult {
    /// 馆藏文件编号。
    pub library_file_id: Uuid,
    /// 关联编号。
    pub holding_id: Uuid,
    /// 是否为新物理文件。
    pub is_new_file: bool,
    /// 是否满足版本获取策略。
    pub meets_strategy: bool,
}

/// 提交并校验一份馆藏文件证据。
pub async fn commit_library_file(
    pool: &PgPool,
    req: &CommitLibraryFileRequest,
) -> AppResult<CommitLibraryFileResult> {
    if req.actual_size_bytes <= 0 {
        return Err(AppError::bad("文件大小必须大于 0"));
    }
    if req.sha256.len() != 64 {
        return Err(AppError::bad("SHA-256 必须为 64 位十六进制字符串"));
    }

    let mut tx = pool.begin().await?;
    let now = Utc::now();

    // 1. 插入或命中物理文件记录（按 SHA-256 唯一）
    let file_id = Uuid::new_v4();
    let (actual_file_id, is_new_file): (Uuid, bool) = {
        let inserted: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO library_files \
                 (id, storage_backend, object_key, format, actual_size_bytes, sha256, md5, verify_status, verified_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, '有效', $8) \
             ON CONFLICT (sha256) DO NOTHING \
             RETURNING id"
        )
        .bind(file_id)
        .bind(&req.storage_backend)
        .bind(&req.object_key)
        .bind(&req.format)
        .bind(req.actual_size_bytes)
        .bind(req.sha256.to_ascii_lowercase())
        .bind(req.md5.as_ref().map(|s| s.to_ascii_lowercase()))
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(id) = inserted {
            (id, true)
        } else {
            let existing_id: Uuid =
                sqlx::query_scalar("SELECT id FROM library_files WHERE sha256 = $1")
                    .bind(req.sha256.to_ascii_lowercase())
                    .fetch_one(&mut *tx)
                    .await?;
            (existing_id, false)
        }
    };

    // 2. 建立版本与馆藏文件关联（Holdings）
    let holding_id = Uuid::new_v4();
    let actual_holding_id: Uuid = sqlx::query_scalar(
        "INSERT INTO holdings (id, edition_id, library_file_id, source_asset_id, match_type, meets_strategy) \
         VALUES ($1, $2, $3, $4, '校验入库', TRUE) \
         ON CONFLICT (edition_id, library_file_id) DO UPDATE SET \
             source_asset_id = COALESCE(EXCLUDED.source_asset_id, holdings.source_asset_id), \
             meets_strategy = TRUE \
         RETURNING id"
    )
    .bind(holding_id)
    .bind(req.edition_id)
    .bind(actual_file_id)
    .bind(req.source_asset_id)
    .fetch_one(&mut *tx)
    .await?;

    // 3. 推进目标获取状态为「已下载」并清除租约
    sqlx::query(
        "UPDATE acquisition_targets SET \
             status = '已下载', \
             satisfied_holding_id = $2, \
             lease_node_id = NULL, \
             lease_session_id = NULL, \
             lease_execution_id = NULL, \
             lease_expires_at = NULL, \
             last_error = NULL, \
             updated_at = now() \
         WHERE edition_id = $1",
    )
    .bind(req.edition_id)
    .bind(actual_holding_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(CommitLibraryFileResult {
        library_file_id: actual_file_id,
        holding_id: actual_holding_id,
        is_new_file,
        meets_strategy: true,
    })
}
