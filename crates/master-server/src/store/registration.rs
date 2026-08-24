//! V5 直连注册的存储层（实施方案 v5 第 6.6 节）。
//!
//! 职责边界：
//! - 本层只做「读/写注册会话与节点注册字段」的 SQL；
//! - 跨表审批（锁节点 + 签证书 + 记录证书 + 领用会话 + 改槽位 + 发配置）由
//!   [`crate::api::workers`] 或 [`crate::grpc::registration`] 在单个事务内编排，
//!   本层提供事务版本函数。
//! - 令牌只存哈希；CSR 只存公钥侧（PEM）；不存私钥。

use chrono::{DateTime, Duration, Utc};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::{RegistrationSession, WorkerNode};

/// 注册申请最长保留天数（第 6.4 节）。
pub const REGISTRATION_RETENTION_DAYS: i64 = 7;
/// 默认注册会话有效期（分钟）。
pub const SESSION_VALID_MINUTES: i64 = 15;

/// 按安装标识查找节点（幂等注册用）。
pub async fn find_node_by_installation(
    executor: impl PgExecutor<'_>,
    installation_id: Uuid,
) -> AppResult<Option<WorkerNode>> {
    let node = sqlx::query_as::<_, WorkerNode>(&format!(
        "SELECT {} FROM worker_nodes WHERE installation_id = $1",
        crate::store::node::NODE_COLUMNS
    ))
    .bind(installation_id)
    .fetch_optional(executor)
    .await?;
    Ok(node)
}

/// 按公钥指纹查找节点。
pub async fn find_node_by_fingerprint(
    executor: impl PgExecutor<'_>,
    fingerprint: &str,
) -> AppResult<Option<WorkerNode>> {
    let node = sqlx::query_as::<_, WorkerNode>(&format!(
        "SELECT {} FROM worker_nodes WHERE public_key_fingerprint = $1",
        crate::store::node::NODE_COLUMNS
    ))
    .bind(fingerprint)
    .fetch_optional(executor)
    .await?;
    Ok(node)
}

/// 取节点当前最活跃的一条注册会话（status='待审核' 且未过期）。
pub async fn find_active_session(
    executor: impl PgExecutor<'_>,
    node_id: Uuid,
) -> AppResult<Option<RegistrationSession>> {
    let session = sqlx::query_as::<_, RegistrationSession>(
        "SELECT id, node_id, token_hash, csr_pem, csr_fingerprint, challenge, status, pending_node_token, \
                expires_at, attempt_count, created_at, last_seen_at \
         FROM worker_registration_sessions \
         WHERE node_id = $1 AND status = '待审核' AND expires_at > now() \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(node_id)
    .fetch_optional(executor)
    .await?;
    Ok(session)
}

/// 按令牌哈希取注册会话（登录/查询审批状态用）。
pub async fn find_session_by_token_hash(
    executor: impl PgExecutor<'_>,
    token_hash: &str,
) -> AppResult<Option<RegistrationSession>> {
    let session = sqlx::query_as::<_, RegistrationSession>(
        "SELECT id, node_id, token_hash, csr_pem, csr_fingerprint, challenge, status, pending_node_token, \
                expires_at, attempt_count, created_at, last_seen_at \
         FROM worker_registration_sessions WHERE token_hash = $1",
    )
    .bind(token_hash)
    .fetch_optional(executor)
    .await?;
    Ok(session)
}

/// 创建注册会话（令牌哈希唯一；csr_pem 只含公钥侧 CSR）。
pub async fn create_registration_session(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    node_id: Uuid,
    token_hash: &str,
    csr_pem: &str,
    csr_fingerprint: &str,
    challenge: &str,
) -> AppResult<RegistrationSession> {
    let session = sqlx::query_as::<_, RegistrationSession>(
        "INSERT INTO worker_registration_sessions \
             (id, node_id, token_hash, csr_pem, csr_fingerprint, challenge, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, now() + ($7 || ' minutes')::interval) \
         RETURNING id, node_id, token_hash, csr_pem, csr_fingerprint, challenge, status, pending_node_token, \
                expires_at, attempt_count, created_at, last_seen_at",
    )
    .bind(Uuid::new_v4())
    .bind(node_id)
    .bind(token_hash)
    .bind(csr_pem)
    .bind(csr_fingerprint)
    .bind(challenge)
    .bind(SESSION_VALID_MINUTES)
    .fetch_one(&mut **tx)
    .await?;
    Ok(session)
}

/// 注册查询失败计数 +1，并刷新最近访问时间。
pub async fn bump_session_attempt(
    executor: impl PgExecutor<'_>,
    session_id: Uuid,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE worker_registration_sessions SET attempt_count = attempt_count + 1, \
             last_seen_at = now() WHERE id = $1",
    )
    .bind(session_id)
    .execute(executor)
    .await?;
    Ok(())
}

/// 会话领用（批准后证书/令牌已发，会话立即失效）。
pub async fn mark_session_claimed(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    session_id: Uuid,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE worker_registration_sessions SET status = '已领取', last_seen_at = now() \
         WHERE id = $1 AND status = '待审核'",
    )
    .bind(session_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// 把节点标记为已拒绝（管理员拒绝；后续重复申请关联原记录并拒绝）。
pub async fn set_node_rejected(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    node_id: Uuid,
    admin_id: Uuid,
    reason: &str,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE worker_nodes SET registration_status = '已拒绝', status = '已禁用', \
             rejected_at = now(), rejected_by = $2, reject_reason = $3, updated_at = now() \
         WHERE id = $1",
    )
    .bind(node_id)
    .bind(admin_id)
    .bind(reason)
    .execute(&mut **tx)
    .await?;
    // 未领取的注册会话一并失效
    sqlx::query(
        "UPDATE worker_registration_sessions SET status = '已拒绝', last_seen_at = now() \
         WHERE node_id = $1 AND status = '待审核'",
    )
    .bind(node_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// 清理过期注册：会话到期 → 已过期；节点注册申请超期 → 已过期。
/// 返回清理的会话数（保留行做审计）。
pub async fn expire_stale_registrations(pool: &PgPool) -> AppResult<u64> {
    let mut tx = pool.begin().await?;
    let sessions = sqlx::query(
        "UPDATE worker_registration_sessions SET status = '已过期' \
         WHERE status = '待审核' AND expires_at <= now()",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sqlx::query(
        "UPDATE worker_nodes SET registration_status = '已过期', updated_at = now() \
         WHERE registration_status = '待审核' AND registration_expires_at IS NOT NULL \
           AND registration_expires_at <= now()",
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(sessions)
}

/// 定期清理过期注册会话与超期申请（第 6.4 节：注册会话 15 分钟、申请保留 7 天）。
///
/// 每 10 分钟跑一次；失败只记日志，不影响主流程。清理是软标记（保留审计行），
/// 不会删除数据。
pub fn spawn_registration_cleanup(pool: PgPool) {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(600));
        timer.tick().await; // 首次立即不跑，先给 10 分钟窗口
        loop {
            timer.tick().await;
            match expire_stale_registrations(&pool).await {
                Ok(expired) => {
                    if expired > 0 {
                        tracing::info!(expired_sessions = expired, "已清理过期注册会话");
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err, "清理过期注册失败（下轮重试）");
                }
            }
        }
    });
}

/// 更新节点的注册申请信息（每次 RegisterNode 调用刷新）。
#[allow(clippy::too_many_arguments)]
pub async fn refresh_registration(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    node_id: Uuid,
    installation_id: Uuid,
    fingerprint: &str,
    requested_slots: i32,
    first_seen_ip: Option<&str>,
    expires_in_days: i64,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE worker_nodes SET \
             installation_id = $2, public_key_fingerprint = $3, \
             requested_slots = $4, configured_slots = COALESCE(configured_slots, $4), \
             registration_expires_at = now() + ($5 || ' days')::interval, \
             registration_status = CASE WHEN registration_status = '已过期' THEN '待审核' \
                                        ELSE registration_status END, \
             first_seen_ip = COALESCE(first_seen_ip, $6), \
             last_registration_at = now(), updated_at = now() \
         WHERE id = $1",
    )
    .bind(node_id)
    .bind(installation_id)
    .bind(fingerprint)
    .bind(requested_slots.clamp(1, 50))
    .bind(expires_in_days.max(1))
    .bind(first_seen_ip)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// 把注册申请的到期时间换算为 UTC 时间。
pub fn registration_expires_at(expires_in_days: i64) -> DateTime<Utc> {
    Utc::now() + Duration::days(expires_in_days.max(1))
}

/// 批准一次直连注册（V5 第 6.7 节：批准后才签发正式证书与节点令牌）。
///
/// 单事务内完成：行锁节点 → 校验注册状态 → 用会话 CSR 签证书 → 生成节点令牌 →
/// 写证书记录 → 领用会话 → 更新节点（已批准/槽位/令牌哈希/审批人）→ 发布配置。
/// `sign` 闭包由调用方注入（需要 CA），避免本层依赖 AppState。
#[allow(clippy::too_many_arguments)]
pub async fn approve_registration(
    pool: &PgPool,
    node_id: Uuid,
    admin_id: Uuid,
    configured_slots: i32,
    remark: Option<&str>,
    sign: impl FnOnce(&str) -> crate::error::AppResult<crate::security::ca::IssuedCertificate>,
) -> AppResult<(WorkerNode, RegistrationSession, String)> {
    let mut tx = pool.begin().await?;

    // 1. 行锁节点：防止重复批准并重复签证书（第 6.6/6.7 节）
    let node = sqlx::query_as::<_, WorkerNode>(&format!(
        "SELECT {} FROM worker_nodes WHERE id = $1 FOR UPDATE",
        crate::store::node::NODE_COLUMNS
    ))
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::missing("节点不存在"))?;

    if node.registration_status != "待审核" {
        return Err(AppError::conflict(format!(
            "节点当前注册状态为「{}」，只有待审核节点可以被批准",
            node.registration_status
        )));
    }

    // 2. 取待审核注册会话（必须有 CSR，才能签发证书）
    let session = sqlx::query_as::<_, RegistrationSession>(
        "SELECT id, node_id, token_hash, csr_pem, csr_fingerprint, challenge, status, pending_node_token, \
                expires_at, attempt_count, created_at, last_seen_at \
         FROM worker_registration_sessions \
         WHERE node_id = $1 AND status = '待审核' AND expires_at > now() \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::conflict("节点没有有效的注册会话（可能已过期），无法签发证书"))?;

    // 3. 批准后才签发正式客户端证书（V5 第 6.8 节：待审核阶段不签发）
    let issued = sign(&session.csr_pem)?;

    // 4. 生成节点令牌（明文只在本函数返回值中出现一次）
    let node_token = crate::security::new_node_token();
    let token_hash = crate::security::hash_node_token(&node_token);

    // 5. 写证书记录
    sqlx::query(
        "INSERT INTO node_certificates (id, node_id, fingerprint, certificate_pem, not_after) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Uuid::new_v4())
    .bind(node_id)
    .bind(&issued.fingerprint)
    .bind(&issued.certificate_pem)
    .bind(issued.not_after)
    .execute(&mut *tx)
    .await?;

    // 6. 领用会话，并放入待一次性下发的节点令牌明文（WatchRegistration 领取后清空）
    sqlx::query(
        "UPDATE worker_registration_sessions SET status = '已领取', pending_node_token = $2, \
             last_seen_at = now() WHERE id = $1 AND status = '待审核'",
    )
    .bind(session.id)
    .bind(&node_token)
    .execute(&mut *tx)
    .await?;

    // 7. 更新节点：已批准 + 槽位 + 令牌哈希 + 审批人
    let slots = configured_slots.clamp(1, 50);
    let node = sqlx::query_as::<_, WorkerNode>(&format!(
        "UPDATE worker_nodes SET \
             registration_status = '已批准', status = '离线', \
             configured_slots = $2, max_slots = $2, \
             node_token_hash = $3, approved_at = now(), approved_by = $4, \
             registration_expires_at = NULL, updated_at = now() \
         WHERE id = $1 RETURNING {}",
        crate::store::node::NODE_COLUMNS
    ))
    .bind(node_id)
    .bind(slots)
    .bind(token_hash)
    .bind(admin_id)
    .fetch_one(&mut *tx)
    .await?;

    // 8. 确保槽位行存在；发布配置版本
    crate::store::node::ensure_slots(&mut tx, node_id, slots).await?;
    crate::store::node::publish_config(
        &mut tx,
        node_id,
        &serde_json::json!({
            "槽位上限": slots,
            "备注": remark.unwrap_or(""),
            "配置类型": "v5直连注册批准",
        }),
    )
    .await?;

    tx.commit().await?;

    Ok((node, session, node_token))
}
