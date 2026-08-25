//! V7 幂等直连注册请求存储（实施方案 v7 第 8.2 节）。
//!
//! 记录当前有效的 Worker 注册请求（安装标识、CSR 公钥、槽位申请等），
//! 替代短期一次性注册会话，提供幂等持久化。

use chrono::{DateTime, Utc};
use sqlx::PgExecutor;
use uuid::Uuid;

use crate::error::AppResult;
use crate::models::WorkerRegistrationRequest;

/// 注册请求表全部列。
pub const REGISTRATION_REQUEST_COLUMNS: &str =
    "node_id, installation_id, csr_pem, public_key_fingerprint, source_ip, \
     requested_slots, first_seen_at, last_seen_at, expires_at";

/// 插入或更新注册请求。
pub async fn upsert_registration_request(
    executor: impl PgExecutor<'_>,
    node_id: Uuid,
    installation_id: Uuid,
    csr_pem: &str,
    public_key_fingerprint: &str,
    source_ip: Option<&str>,
    requested_slots: i32,
    expires_at: DateTime<Utc>,
) -> AppResult<WorkerRegistrationRequest> {
    let req = sqlx::query_as::<_, WorkerRegistrationRequest>(&format!(
        "INSERT INTO worker_registration_requests \
             (node_id, installation_id, csr_pem, public_key_fingerprint, source_ip, requested_slots, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (node_id) DO UPDATE SET \
             csr_pem = EXCLUDED.csr_pem, \
             public_key_fingerprint = EXCLUDED.public_key_fingerprint, \
             source_ip = EXCLUDED.source_ip, \
             requested_slots = EXCLUDED.requested_slots, \
             last_seen_at = now(), \
             expires_at = EXCLUDED.expires_at \
         RETURNING {REGISTRATION_REQUEST_COLUMNS}"
    ))
    .bind(node_id)
    .bind(installation_id)
    .bind(csr_pem)
    .bind(public_key_fingerprint)
    .bind(source_ip)
    .bind(requested_slots)
    .bind(expires_at)
    .fetch_one(executor)
    .await?;

    Ok(req)
}

/// 按节点编号查找注册请求。
pub async fn find_request_by_node_id(
    executor: impl PgExecutor<'_>,
    node_id: Uuid,
) -> AppResult<Option<WorkerRegistrationRequest>> {
    let req = sqlx::query_as::<_, WorkerRegistrationRequest>(&format!(
        "SELECT {REGISTRATION_REQUEST_COLUMNS} FROM worker_registration_requests WHERE node_id = $1"
    ))
    .bind(node_id)
    .fetch_optional(executor)
    .await?;

    Ok(req)
}

/// 按安装标识查找注册请求。
pub async fn find_request_by_installation_id(
    executor: impl PgExecutor<'_>,
    installation_id: Uuid,
) -> AppResult<Option<WorkerRegistrationRequest>> {
    let req = sqlx::query_as::<_, WorkerRegistrationRequest>(&format!(
        "SELECT {REGISTRATION_REQUEST_COLUMNS} FROM worker_registration_requests \
         WHERE installation_id = $1 ORDER BY last_seen_at DESC LIMIT 1"
    ))
    .bind(installation_id)
    .fetch_optional(executor)
    .await?;

    Ok(req)
}

/// 按公钥指纹查找注册请求。
pub async fn find_request_by_fingerprint(
    executor: impl PgExecutor<'_>,
    public_key_fingerprint: &str,
) -> AppResult<Option<WorkerRegistrationRequest>> {
    let req = sqlx::query_as::<_, WorkerRegistrationRequest>(&format!(
        "SELECT {REGISTRATION_REQUEST_COLUMNS} FROM worker_registration_requests \
         WHERE public_key_fingerprint = $1 ORDER BY last_seen_at DESC LIMIT 1"
    ))
    .bind(public_key_fingerprint)
    .fetch_optional(executor)
    .await?;

    Ok(req)
}
