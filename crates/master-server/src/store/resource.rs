//! 站点账号与代理池（第 6.2 / 6.3 节）。
//!
//! 密码与代理密码以 AES-256-GCM 密文列（`password_cipher`）保存，本层只搬运密文，
//! 解密发生在真正要下发给 Worker 的那一刻（[`crate::scheduler`]），
//! 这样任何列表接口都不可能顺手把明文序列化出去。

use chrono::{DateTime, Duration, Utc};
use platform_domain::{AccountStatus, ProxyStatus};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::{Account, Proxy};

const ACCOUNT_COLUMNS: &str = "id, email, nickname, status, daily_used, daily_limit, \
     reset_date, lease_session_id, last_error, registered_at, last_login_at, created_at";

const PROXY_COLUMNS: &str = "id, provider, external_id, label, scheme, host, port, status, \
     exit_ip, latency_ms, success_count, failure_count, throttle_count, cooldown_until, \
     lease_session_id, last_checked_at";

// ---------------------------------------------------------------- 账号

/// 新增账号。已存在同邮箱时返回 409。
pub async fn create_account(
    executor: impl PgExecutor<'_>,
    email: &str,
    password_cipher: &str,
    nickname: &str,
    daily_limit: i32,
    status: AccountStatus,
) -> AppResult<Account> {
    let result = sqlx::query_as::<_, Account>(&format!(
        "INSERT INTO accounts (id, email, password_cipher, nickname, status, daily_limit) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {ACCOUNT_COLUMNS}"
    ))
    .bind(Uuid::new_v4())
    .bind(email)
    .bind(password_cipher)
    .bind(nickname)
    .bind(status.as_str())
    .bind(daily_limit.clamp(1, 1000))
    .fetch_one(executor)
    .await;

    match result {
        Ok(account) => Ok(account),
        Err(error) if super::is_unique_violation(&error) => {
            Err(AppError::conflict(format!("账号已存在：{email}")))
        }
        Err(error) => Err(error.into()),
    }
}

/// 账号列表。
pub async fn list_accounts(
    executor: impl PgExecutor<'_>,
    status: Option<&str>,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<Account>> {
    let accounts = sqlx::query_as::<_, Account>(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM accounts \
         WHERE ($1::text IS NULL OR status = $1) \
         ORDER BY created_at DESC LIMIT $2 OFFSET $3"
    ))
    .bind(status)
    .bind(limit.clamp(1, 500))
    .bind(offset.max(0))
    .fetch_all(executor)
    .await?;
    Ok(accounts)
}

/// 账号列表总数（可带状态过滤）。
pub async fn count_accounts(executor: impl PgExecutor<'_>, status: Option<&str>) -> AppResult<i64> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM accounts \
         WHERE ($1::text IS NULL OR status = $1)",
    )
    .bind(status)
    .fetch_one(executor)
    .await?;
    Ok(count)
}

/// 按账号状态统计数量，用于账号中心状态卡片。
pub async fn account_status_counts(executor: impl PgExecutor<'_>) -> AppResult<Vec<(String, i64)>> {
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT status, count(*)::bigint FROM accounts GROUP BY status")
            .fetch_all(executor)
            .await?;
    Ok(rows)
}

/// 单个账号。
pub async fn get_account(executor: impl PgExecutor<'_>, id: Uuid) -> AppResult<Account> {
    sqlx::query_as::<_, Account>(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::missing("账号不存在"))
}

/// 改账号状态并记录最近错误。
pub async fn set_account_status(
    executor: impl PgExecutor<'_>,
    id: Uuid,
    status: AccountStatus,
    error: Option<&str>,
) -> AppResult<Account> {
    sqlx::query_as::<_, Account>(&format!(
        "UPDATE accounts SET status = $2, last_error = $3, updated_at = now() \
         WHERE id = $1 RETURNING {ACCOUNT_COLUMNS}"
    ))
    .bind(id)
    .bind(status.as_str())
    .bind(error)
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::missing("账号不存在"))
}

/// 改每日额度上限。
pub async fn set_account_limit(
    executor: impl PgExecutor<'_>,
    id: Uuid,
    daily_limit: i32,
) -> AppResult<Account> {
    sqlx::query_as::<_, Account>(&format!(
        "UPDATE accounts SET daily_limit = $2, updated_at = now() \
         WHERE id = $1 RETURNING {ACCOUNT_COLUMNS}"
    ))
    .bind(id)
    .bind(daily_limit.clamp(1, 1000))
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::missing("账号不存在"))
}

/// 改密码密文。
pub async fn set_account_password(
    executor: impl PgExecutor<'_>,
    id: Uuid,
    password_cipher: &str,
) -> AppResult<()> {
    let affected =
        sqlx::query("UPDATE accounts SET password_cipher = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(password_cipher)
            .execute(executor)
            .await?
            .rows_affected();
    if affected == 0 {
        return Err(AppError::missing("账号不存在"));
    }
    Ok(())
}

/// 删除账号。被会话占用时拒绝，避免正在跑的任务凭据消失。
pub async fn delete_account(executor: impl PgExecutor<'_>, id: Uuid) -> AppResult<()> {
    let affected = sqlx::query("DELETE FROM accounts WHERE id = $1 AND lease_session_id IS NULL")
        .bind(id)
        .execute(executor)
        .await?
        .rows_affected();
    if affected == 0 {
        return Err(AppError::conflict("账号不存在或正被会话占用"));
    }
    Ok(())
}

/// 取账号凭据密文，供下发前解密。
pub async fn account_cipher(
    executor: impl PgExecutor<'_>,
    id: Uuid,
) -> AppResult<(String, String)> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT email, password_cipher FROM accounts WHERE id = $1")
            .bind(id)
            .fetch_optional(executor)
            .await?;
    row.ok_or_else(|| AppError::missing("账号不存在"))
}

/// 跨日重置额度（第 6.2 节）。返回重置了几个账号。
///
/// 以 `reset_date < current_date` 为条件而不是靠定时任务「每天零点跑一次」：
/// 进程重启、时区偏移、任务漏跑都不会造成额度永久卡住，
/// 领取会话前顺手调用一次即可自愈。
pub async fn reset_expired_quota(executor: impl PgExecutor<'_>) -> AppResult<u64> {
    let affected = sqlx::query(
        "UPDATE accounts SET daily_used = 0, reset_date = current_date, \
             status = CASE WHEN status = $1 THEN $2 ELSE status END, updated_at = now() \
         WHERE reset_date < current_date",
    )
    .bind(AccountStatus::ExhaustedToday.as_str())
    .bind(AccountStatus::Registered.as_str())
    .execute(executor)
    .await?
    .rows_affected();
    Ok(affected)
}

/// 手动将「今日额度耗尽」的账号全部恢复为可用（对标原桌面客户端）。
pub async fn reset_exhausted_quota(executor: impl PgExecutor<'_>) -> AppResult<u64> {
    let affected = sqlx::query(
        "UPDATE accounts SET daily_used = 0, reset_date = current_date, \
             status = $1, updated_at = now() \
         WHERE status = $2",
    )
    .bind(AccountStatus::Registered.as_str())
    .bind(AccountStatus::ExhaustedToday.as_str())
    .execute(executor)
    .await?
    .rows_affected();
    Ok(affected)
}

/// 手动将「已禁用」的账号全部恢复为可用（已注册）。
pub async fn reset_disabled_accounts(executor: impl PgExecutor<'_>) -> AppResult<u64> {
    let affected = sqlx::query(
        "UPDATE accounts SET daily_used = 0, reset_date = current_date, \
             status = $1, last_error = NULL, updated_at = now() \
         WHERE status = $2",
    )
    .bind(AccountStatus::Registered.as_str())
    .bind(AccountStatus::Disabled.as_str())
    .execute(executor)
    .await?
    .rows_affected();
    Ok(affected)
}

/// 当前可用账号数（已注册、未被占用、还有额度）。
pub async fn count_available_accounts(executor: impl PgExecutor<'_>) -> AppResult<i64> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM accounts \
         WHERE status = $1 AND lease_session_id IS NULL AND daily_used < daily_limit",
    )
    .bind(AccountStatus::Registered.as_str())
    .fetch_one(executor)
    .await?;
    Ok(count)
}

/// 待注册账号数，用于决定是否派发注册任务。
pub async fn count_pending_registration(executor: impl PgExecutor<'_>) -> AppResult<i64> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM accounts WHERE status = $1 AND lease_session_id IS NULL",
    )
    .bind(AccountStatus::PendingRegistration.as_str())
    .fetch_one(executor)
    .await?;
    Ok(count)
}

// ---------------------------------------------------------------- 代理

/// 新增或更新一条代理。
///
/// 唯一键是 `(provider, host, port, username)`：Webshare 同步会反复推送同一批代理，
/// 靠这个键把「同步」变成幂等操作，而不是每次同步都新增一批重复记录。
#[allow(clippy::too_many_arguments)]
pub async fn upsert_proxy(
    executor: impl PgExecutor<'_>,
    provider: &str,
    external_id: Option<&str>,
    label: &str,
    scheme: &str,
    host: &str,
    port: i32,
    username: Option<&str>,
    password_cipher: Option<&str>,
) -> AppResult<Proxy> {
    let proxy = sqlx::query_as::<_, Proxy>(&format!(
        "INSERT INTO proxies (id, provider, external_id, label, scheme, host, port, \
             username, password_cipher, status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
         ON CONFLICT (provider, host, port, username) DO UPDATE SET \
             external_id = EXCLUDED.external_id, label = EXCLUDED.label, \
             scheme = EXCLUDED.scheme, \
             password_cipher = COALESCE(EXCLUDED.password_cipher, proxies.password_cipher), \
             updated_at = now() \
         RETURNING {PROXY_COLUMNS}"
    ))
    .bind(Uuid::new_v4())
    .bind(provider)
    .bind(external_id)
    .bind(label)
    .bind(scheme)
    .bind(host)
    .bind(port)
    .bind(username)
    .bind(password_cipher)
    .bind(ProxyStatus::Error.as_str())
    .fetch_one(executor)
    .await?;
    Ok(proxy)
}

/// 代理列表。
pub async fn list_proxies(
    executor: impl PgExecutor<'_>,
    status: Option<&str>,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<Proxy>> {
    let proxies = sqlx::query_as::<_, Proxy>(&format!(
        "SELECT {PROXY_COLUMNS} FROM proxies \
         WHERE ($1::text IS NULL OR status = $1) \
         ORDER BY provider, host, port LIMIT $2 OFFSET $3"
    ))
    .bind(status)
    .bind(limit.clamp(1, 500))
    .bind(offset.max(0))
    .fetch_all(executor)
    .await?;
    Ok(proxies)
}

/// 单个代理。
pub async fn get_proxy(executor: impl PgExecutor<'_>, id: Uuid) -> AppResult<Proxy> {
    sqlx::query_as::<_, Proxy>(&format!(
        "SELECT {PROXY_COLUMNS} FROM proxies WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::missing("代理不存在"))
}

/// 改代理状态，可同时设置冷却截止时间。
pub async fn set_proxy_status(
    executor: impl PgExecutor<'_>,
    id: Uuid,
    status: ProxyStatus,
    cooldown_until: Option<DateTime<Utc>>,
) -> AppResult<Proxy> {
    sqlx::query_as::<_, Proxy>(&format!(
        "UPDATE proxies SET status = $2, cooldown_until = $3, updated_at = now() \
         WHERE id = $1 RETURNING {PROXY_COLUMNS}"
    ))
    .bind(id)
    .bind(status.as_str())
    .bind(cooldown_until)
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::missing("代理不存在"))
}

/// 让代理进入冷却（第 10.3 节：疑似限流不立即判定为坏代理）。
pub async fn cool_down_proxy(
    executor: impl PgExecutor<'_>,
    id: Uuid,
    minutes: u64,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE proxies SET status = $2, cooldown_until = $3, \
             throttle_count = throttle_count + 1, lease_session_id = NULL, \
             lease_expires_at = NULL, updated_at = now() \
         WHERE id = $1",
    )
    .bind(id)
    .bind(ProxyStatus::CoolingDown.as_str())
    .bind(Utc::now() + Duration::minutes(minutes.clamp(1, 24 * 60) as i64))
    .execute(executor)
    .await?;
    Ok(())
}

/// 冷却到期的代理回到可用，返回恢复了几个。
///
/// V4 第 15.3 节：冷却到期但 provider 已失效的代理**不得复活**——
/// 它已经从 Webshare 快照消失，恢复可用会让过期代理重新进入分配池。
pub async fn revive_cooled_proxies(executor: impl PgExecutor<'_>) -> AppResult<u64> {
    let affected = sqlx::query(
        "UPDATE proxies SET status = $1, cooldown_until = NULL, updated_at = now() \
         WHERE status = $2 AND cooldown_until IS NOT NULL AND cooldown_until <= now() \
           AND provider_valid = TRUE",
    )
    .bind(ProxyStatus::Available.as_str())
    .bind(ProxyStatus::CoolingDown.as_str())
    .execute(executor)
    .await?
    .rows_affected();
    Ok(affected)
}

/// 删除代理。被占用时拒绝。
pub async fn delete_proxy(executor: impl PgExecutor<'_>, id: Uuid) -> AppResult<()> {
    let affected = sqlx::query("DELETE FROM proxies WHERE id = $1 AND lease_session_id IS NULL")
        .bind(id)
        .execute(executor)
        .await?
        .rows_affected();
    if affected == 0 {
        return Err(AppError::conflict("代理不存在或正被会话占用"));
    }
    Ok(())
}

/// 记录一次连通性检测结果。
pub async fn record_proxy_check(
    executor: impl PgExecutor<'_>,
    id: Uuid,
    reachable: bool,
    exit_ip: Option<&str>,
    latency_ms: Option<i32>,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE proxies SET \
             status = CASE WHEN $2 THEN CASE WHEN status = $5 THEN $5 ELSE $4 END ELSE $6 END, \
             exit_ip = COALESCE($3, exit_ip), latency_ms = $7, \
             last_checked_at = now(), updated_at = now() \
         WHERE id = $1",
    )
    .bind(id)
    .bind(reachable)
    .bind(exit_ip)
    .bind(ProxyStatus::Available.as_str())
    .bind(ProxyStatus::Occupied.as_str())
    .bind(ProxyStatus::Error.as_str())
    .bind(latency_ms)
    .execute(executor)
    .await?;
    Ok(())
}

/// 代理的连接要素与密码密文。
///
/// 与 [`Proxy`] 分开定义：`Proxy` 是要序列化给前端的，这个结构体带着密文，
/// 只在「下发给某个会话」的那条路径上出现，且不实现 `Serialize`。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ProxyEndpoint {
    /// 协议。
    pub scheme: String,
    /// 主机。
    pub host: String,
    /// 端口。
    pub port: i32,
    /// 用户名。
    pub username: Option<String>,
    /// 密码密文。
    pub password_cipher: Option<String>,
}

/// 取代理连接信息与密码密文，供下发前解密。
pub async fn proxy_cipher(executor: impl PgExecutor<'_>, id: Uuid) -> AppResult<ProxyEndpoint> {
    sqlx::query_as::<_, ProxyEndpoint>(
        "SELECT scheme, host, port, username, password_cipher FROM proxies WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::missing("代理不存在"))
}

/// 当前可用代理数。
pub async fn count_available_proxies(executor: impl PgExecutor<'_>) -> AppResult<i64> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxies \
         WHERE status = $1 AND lease_session_id IS NULL \
           AND (cooldown_until IS NULL OR cooldown_until <= now())",
    )
    .bind(ProxyStatus::Available.as_str())
    .fetch_one(executor)
    .await?;
    Ok(count)
}

/// 待检测的代理（最久未检测的优先）。
pub async fn proxies_due_for_check(pool: &PgPool, limit: i64) -> AppResult<Vec<Uuid>> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM proxies WHERE status IN ($1, $2) \
         ORDER BY last_checked_at ASC NULLS FIRST LIMIT $3",
    )
    .bind(ProxyStatus::Available.as_str())
    .bind(ProxyStatus::Error.as_str())
    .bind(limit.clamp(1, 200))
    .fetch_all(pool)
    .await?;
    Ok(ids)
}

/// Webshare 快照同步数据项。
#[derive(Debug, Clone)]
pub struct WebshareProxyData {
    /// 外部编号。
    pub external_id: Option<String>,
    /// 主机。
    pub host: String,
    /// 端口。
    pub port: i32,
    /// 用户名。
    pub username: Option<String>,
    /// 密码密文。
    pub password_cipher: Option<String>,
    /// Webshare 报告的有效性。
    pub valid: bool,
}

/// Webshare 快照同步统计报告。
#[derive(Debug, Clone)]
pub struct WebshareSyncReport {
    /// 总同步数。
    pub total_synced: usize,
    /// 启用/更新数。
    pub enabled_count: usize,
    /// 无效/停用数。
    pub disabled_count: usize,
    /// 消失并停用数。
    pub missing_count: usize,
}

/// 执行一次全量 Webshare 快照同步事务（V4 方案第 15.4 节：同步世代）。
pub async fn sync_webshare_snapshot(
    pool: &PgPool,
    items: &[WebshareProxyData],
) -> AppResult<WebshareSyncReport> {
    let mut tx = pool.begin().await?;

    // 1. 分配新的同步世代（本快照内所有出现过的代理都标记这一代）
    let generation: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(sync_generation), 0) + 1 FROM proxies")
            .fetch_one(&mut *tx)
            .await?;

    let mut enabled_count = 0;
    let mut disabled_count = 0;

    for item in items {
        let label = format!("Webshare-{}:{}", item.host, item.port);

        if item.valid {
            // 带 external_id 的按身份 upsert（同 external_id 地址变化 → 更新，不生成重复记录）；
            // 无 external_id 的回退到 (provider, host, port, username) 兼容键。
            // 新同步的代理初始状态设为「异常」，待 Worker 实测（ProxyCheck）通过后方可转为「可用」
            if let Some(ext_id) = &item.external_id {
                sqlx::query(
                    "INSERT INTO proxies (id, provider, external_id, label, scheme, host, port, username, password_cipher, status, provider_valid, sync_generation, last_seen_at) \
                     VALUES ($1, 'Webshare', $2, $3, 'http', $4, $5, $6, $7, '异常', TRUE, $8, now()) \
                     ON CONFLICT (provider, external_id) WHERE external_id IS NOT NULL DO UPDATE SET \
                         label = EXCLUDED.label, \
                         host = EXCLUDED.host, \
                         port = EXCLUDED.port, \
                         username = EXCLUDED.username, \
                         password_cipher = COALESCE(EXCLUDED.password_cipher, proxies.password_cipher), \
                         provider_valid = TRUE, \
                         sync_generation = EXCLUDED.sync_generation, \
                         last_seen_at = now(), \
                         updated_at = now()",
                )
                .bind(Uuid::new_v4())
                .bind(ext_id)
                .bind(&label)
                .bind(&item.host)
                .bind(item.port)
                .bind(&item.username)
                .bind(&item.password_cipher)
                .bind(generation)
                .execute(&mut *tx)
                .await?;
            } else {
                sqlx::query(
                    "INSERT INTO proxies (id, provider, external_id, label, scheme, host, port, username, password_cipher, status, provider_valid, sync_generation, last_seen_at) \
                     VALUES ($1, 'Webshare', NULL, $2, 'http', $3, $4, $5, $6, '异常', TRUE, $7, now()) \
                     ON CONFLICT (provider, host, port, username) DO UPDATE SET \
                         external_id = EXCLUDED.external_id, \
                         label = EXCLUDED.label, \
                         password_cipher = COALESCE(EXCLUDED.password_cipher, proxies.password_cipher), \
                         provider_valid = TRUE, \
                         sync_generation = EXCLUDED.sync_generation, \
                         last_seen_at = now(), \
                         updated_at = now()",
                )
                .bind(Uuid::new_v4())
                .bind(&label)
                .bind(&item.host)
                .bind(item.port)
                .bind(&item.username)
                .bind(&item.password_cipher)
                .bind(generation)
                .execute(&mut *tx)
                .await?;
            }
            enabled_count += 1;
        } else {
            // 无效代理：标记 provider_valid=false；已占用则延迟退休，不破坏当前书
            if let Some(ext_id) = &item.external_id {
                sqlx::query(
                    "INSERT INTO proxies (id, provider, external_id, label, scheme, host, port, username, password_cipher, status, provider_valid, sync_generation, last_seen_at) \
                     VALUES ($1, 'Webshare', $2, $3, 'http', $4, $5, $6, $7, '已停用', FALSE, $8, now()) \
                     ON CONFLICT (provider, external_id) WHERE external_id IS NOT NULL DO UPDATE SET \
                         host = EXCLUDED.host, \
                         port = EXCLUDED.port, \
                         provider_valid = FALSE, \
                         sync_generation = EXCLUDED.sync_generation, \
                         last_seen_at = now(), \
                         retire_after_release = CASE WHEN proxies.status = '已占用' THEN TRUE ELSE proxies.retire_after_release END, \
                         status = CASE WHEN proxies.status IN ('可用', '冷却中', '异常') THEN '已停用' ELSE proxies.status END, \
                         updated_at = now()",
                )
                .bind(Uuid::new_v4())
                .bind(ext_id)
                .bind(&label)
                .bind(&item.host)
                .bind(item.port)
                .bind(&item.username)
                .bind(&item.password_cipher)
                .bind(generation)
                .execute(&mut *tx)
                .await?;
            } else {
                sqlx::query(
                    "INSERT INTO proxies (id, provider, external_id, label, scheme, host, port, username, password_cipher, status, provider_valid, sync_generation, last_seen_at) \
                     VALUES ($1, 'Webshare', NULL, $2, 'http', $3, $4, $5, $6, '已停用', FALSE, $7, now()) \
                     ON CONFLICT (provider, host, port, username) DO UPDATE SET \
                         provider_valid = FALSE, \
                         sync_generation = EXCLUDED.sync_generation, \
                         last_seen_at = now(), \
                         retire_after_release = CASE WHEN proxies.status = '已占用' THEN TRUE ELSE proxies.retire_after_release END, \
                         status = CASE WHEN proxies.status IN ('可用', '冷却中', '异常') THEN '已停用' ELSE proxies.status END, \
                         updated_at = now()",
                )
                .bind(Uuid::new_v4())
                .bind(&label)
                .bind(&item.host)
                .bind(item.port)
                .bind(&item.username)
                .bind(&item.password_cipher)
                .bind(generation)
                .execute(&mut *tx)
                .await?;
            }
            disabled_count += 1;
        }
    }

    // 2. 只把「旧世代且本次未出现」的代理标记失效（第 15.4 节第 6 条）；
    //    已占用代理延迟退休（第 7 条），不破坏当前书。
    let missing_count = if !items.is_empty() {
        sqlx::query(
            "UPDATE proxies SET provider_valid = FALSE, \
                 retire_after_release = CASE WHEN status = '已占用' THEN TRUE ELSE retire_after_release END, \
                 status = CASE WHEN status IN ('可用', '冷却中', '异常') THEN '已停用' ELSE status END, \
                 updated_at = now() \
             WHERE provider = 'Webshare' AND sync_generation < $1 AND provider_valid = TRUE",
        )
        .bind(generation)
        .execute(&mut *tx)
        .await?
        .rows_affected() as usize
    } else {
        // 空快照（理论上不会发生，因为空抓取已被上层拒绝）：不动任何代理
        0
    };

    tx.commit().await?;

    Ok(WebshareSyncReport {
        total_synced: items.len(),
        enabled_count,
        disabled_count,
        missing_count,
    })
}
