//! 管理员账户、操作日志、告警、设置与每日统计。

use chrono::{DateTime, NaiveDate, Utc};
use platform_domain::{AlertLevel, LogLevel, OperationSource};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::{AdminSession, Alert, DailyStat, OperationLog, User};

const USER_COLUMNS: &str = "id, username, role, status, token_version, created_at, last_login_at";
const SESSION_COLUMNS: &str = "id, user_id, token_hash, issued_at, expires_at, revoked_at, revoke_reason, last_seen_at, user_agent_hash, ip_prefix";

/// 新建管理员。用户名重复时返回 409。
pub async fn create_user(
    executor: impl PgExecutor<'_>,
    username: &str,
    password_hash: &str,
    role: &str,
) -> AppResult<User> {
    let result = sqlx::query_as::<_, User>(&format!(
        "INSERT INTO users (id, username, password_hash, role) \
         VALUES ($1, $2, $3, $4) RETURNING {USER_COLUMNS}"
    ))
    .bind(Uuid::new_v4())
    .bind(username)
    .bind(password_hash)
    .bind(role)
    .fetch_one(executor)
    .await;

    match result {
        Ok(user) => Ok(user),
        Err(error) if super::is_unique_violation(&error) => {
            Err(AppError::conflict(format!("用户名已存在：{username}")))
        }
        Err(error) => Err(error.into()),
    }
}

/// 登录校验用的一行。
///
/// 密码散列只在这条查询里离开数据库，并且本结构体**不实现** `Serialize`，
/// 因此它从类型上就进不了任何 HTTP 响应体；需要返回给前端时只能先 [`Credentials::user`]。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Credentials {
    /// 用户编号。
    pub id: Uuid,
    /// 用户名。
    pub username: String,
    /// 中文角色。
    pub role: String,
    /// 中文状态。
    pub status: String,
    /// 令牌世代版本号。
    pub token_version: i64,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 最近登录时间。
    pub last_login_at: Option<DateTime<Utc>>,
    /// Argon2id 密码散列。
    pub password_hash: String,
}

impl Credentials {
    /// 取出可以安全序列化的用户资料。
    pub fn user(&self) -> User {
        User {
            id: self.id,
            username: self.username.clone(),
            role: self.role.clone(),
            status: self.status.clone(),
            token_version: self.token_version,
            created_at: self.created_at,
            last_login_at: self.last_login_at,
        }
    }
}

/// 按 ID 获取用户资料。
pub async fn get_user_by_id(
    executor: impl PgExecutor<'_>,
    user_id: Uuid,
) -> AppResult<Option<User>> {
    let row = sqlx::query_as::<_, User>(&format!("SELECT {USER_COLUMNS} FROM users WHERE id = $1"))
        .bind(user_id)
        .fetch_optional(executor)
        .await?;
    Ok(row)
}

/// 递增用户 token_version 并撤销该用户全部已有会话。
pub async fn invalidate_all_user_sessions(
    pool: &PgPool,
    user_id: Uuid,
    reason: &str,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE users SET token_version = token_version + 1 WHERE id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE admin_sessions SET revoked_at = now(), revoke_reason = $2 \
         WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(user_id)
    .bind(reason)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// 创建管理员会话记录。
pub async fn create_admin_session(
    executor: impl PgExecutor<'_>,
    session_id: Uuid,
    user_id: Uuid,
    token_hash: &str,
    expires_at: DateTime<Utc>,
    user_agent_hash: Option<&str>,
    ip_prefix: Option<&str>,
) -> AppResult<AdminSession> {
    let session = sqlx::query_as::<_, AdminSession>(&format!(
        "INSERT INTO admin_sessions (id, user_id, token_hash, issued_at, expires_at, user_agent_hash, ip_prefix) \
         VALUES ($1, $2, $3, now(), $4, $5, $6) RETURNING {SESSION_COLUMNS}"
    ))
    .bind(session_id)
    .bind(user_id)
    .bind(token_hash)
    .bind(expires_at)
    .bind(user_agent_hash)
    .bind(ip_prefix)
    .fetch_one(executor)
    .await?;
    Ok(session)
}

/// 查询会话记录（通过会话 ID）。
pub async fn get_admin_session(
    executor: impl PgExecutor<'_>,
    session_id: Uuid,
) -> AppResult<Option<AdminSession>> {
    let session = sqlx::query_as::<_, AdminSession>(&format!(
        "SELECT {SESSION_COLUMNS} FROM admin_sessions WHERE id = $1"
    ))
    .bind(session_id)
    .fetch_optional(executor)
    .await?;
    Ok(session)
}

/// 刷新会话最近活跃时间。
pub async fn touch_admin_session(executor: impl PgExecutor<'_>, session_id: Uuid) -> AppResult<()> {
    sqlx::query("UPDATE admin_sessions SET last_seen_at = now() WHERE id = $1")
        .bind(session_id)
        .execute(executor)
        .await?;
    Ok(())
}

/// 限频刷新会话最近活跃时间（V4 第 13.3 节）。
///
/// 60 秒内不重复写库；更新失败不应影响业务请求，由调用方吞掉错误。
pub async fn touch_admin_session_limited(
    executor: impl PgExecutor<'_>,
    session_id: Uuid,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE admin_sessions SET last_seen_at = now() \
         WHERE id = $1 AND (last_seen_at IS NULL OR last_seen_at < now() - interval '60 seconds')",
    )
    .bind(session_id)
    .execute(executor)
    .await?;
    Ok(())
}

/// 登录安全事件审计（V4 第 13.5 节：达到阈值/失败时记录，但绝不记录密码）。
pub async fn log_security_failure(
    executor: impl PgExecutor<'_>,
    ip: &str,
    username: &str,
    reason: &str,
) -> AppResult<()> {
    log(
        executor,
        OperationSource::Admin,
        LogLevel::Warn,
        "系统",
        "登录失败",
        &format!("user:{username}"),
        &format!("来源 IP：{ip}，原因：{reason}"),
    )
    .await
}

/// 撤销单个会话。
pub async fn revoke_admin_session(
    executor: impl PgExecutor<'_>,
    session_id: Uuid,
    reason: &str,
) -> AppResult<bool> {
    let affected = sqlx::query(
        "UPDATE admin_sessions SET revoked_at = now(), revoke_reason = $2 \
         WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(session_id)
    .bind(reason)
    .execute(executor)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// 登录用：连同密码散列一起取出。
pub async fn find_credentials(
    executor: impl PgExecutor<'_>,
    username: &str,
) -> AppResult<Option<Credentials>> {
    let row = sqlx::query_as::<_, Credentials>(&format!(
        "SELECT {USER_COLUMNS}, password_hash FROM users WHERE username = $1"
    ))
    .bind(username)
    .fetch_optional(executor)
    .await?;
    Ok(row)
}

/// 记录登录时间。
pub async fn touch_login(executor: impl PgExecutor<'_>, user_id: Uuid) -> AppResult<()> {
    sqlx::query("UPDATE users SET last_login_at = now() WHERE id = $1")
        .bind(user_id)
        .execute(executor)
        .await?;
    Ok(())
}

/// 列出全部管理员。
pub async fn list_users(executor: impl PgExecutor<'_>) -> AppResult<Vec<User>> {
    let users = sqlx::query_as::<_, User>(&format!(
        "SELECT {USER_COLUMNS} FROM users ORDER BY created_at"
    ))
    .fetch_all(executor)
    .await?;
    Ok(users)
}

/// 启用或禁用管理员。
pub async fn set_user_status(pool: &PgPool, user_id: Uuid, status: &str) -> AppResult<User> {
    let mut tx = pool.begin().await?;
    let user = sqlx::query_as::<_, User>(&format!(
        "UPDATE users SET status = $2, token_version = CASE WHEN $2 = '已禁用' THEN token_version + 1 ELSE token_version END, updated_at = now() \
         WHERE id = $1 RETURNING {USER_COLUMNS}"
    ))
    .bind(user_id)
    .bind(status)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::missing("管理员不存在"))?;

    if status == "已禁用" {
        sqlx::query(
            "UPDATE admin_sessions SET revoked_at = now(), revoke_reason = '管理员已禁用' \
             WHERE user_id = $1 AND revoked_at IS NULL",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(user)
}

/// 改密码。
pub async fn set_user_password(pool: &PgPool, user_id: Uuid, password_hash: &str) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    let affected = sqlx::query(
        "UPDATE users SET password_hash = $2, token_version = token_version + 1 WHERE id = $1",
    )
    .bind(user_id)
    .bind(password_hash)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(AppError::missing("管理员不存在"));
    }
    sqlx::query(
        "UPDATE admin_sessions SET revoked_at = now(), revoke_reason = '密码已修改' \
         WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// 是否已存在任何管理员，决定首次部署是否需要引导创建。
pub async fn has_any_user(executor: impl PgExecutor<'_>) -> AppResult<bool> {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(executor)
        .await?;
    Ok(count > 0)
}

// ---------------------------------------------------------------- 操作日志

/// 写一条操作日志。
///
/// 参数用强类型枚举而不是字符串：日志是「事后唯一能看的东西」，
/// 这里如果允许随手传中文字面量，迟早会写进 CHECK 约束不认的值并让业务事务回滚。
pub async fn log(
    executor: impl PgExecutor<'_>,
    source: OperationSource,
    level: LogLevel,
    actor: &str,
    action: &str,
    target: &str,
    detail: &str,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO operation_logs (id, source, level, actor, action, target, detail) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(Uuid::new_v4())
    .bind(source.as_str())
    .bind(level.as_str())
    .bind(actor)
    .bind(action)
    .bind(target)
    .bind(detail)
    .execute(executor)
    .await?;
    Ok(())
}

/// 分页查询日志。
pub async fn list_logs(
    executor: impl PgExecutor<'_>,
    level: Option<&str>,
    source: Option<&str>,
    keyword: Option<&str>,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<OperationLog>> {
    let logs = sqlx::query_as::<_, OperationLog>(
        "SELECT id, source, level, actor, action, target, detail, created_at \
         FROM operation_logs \
         WHERE ($1::text IS NULL OR level = $1) \
           AND ($2::text IS NULL OR source = $2) \
           AND ($3::text IS NULL OR action ILIKE '%' || $3 || '%' \
                OR target ILIKE '%' || $3 || '%' OR detail ILIKE '%' || $3 || '%') \
         ORDER BY created_at DESC LIMIT $4 OFFSET $5",
    )
    .bind(level)
    .bind(source)
    .bind(keyword)
    .bind(limit.clamp(1, 500))
    .bind(offset.max(0))
    .fetch_all(executor)
    .await?;
    Ok(logs)
}

// ---------------------------------------------------------------- 告警

/// 触发告警。
///
/// `dedup_key` 非空时依赖 `idx_alerts_open_dedup` 这个「仅未解决行」的部分唯一索引：
/// 同一个问题持续存在只会留下一条未解决告警，解决后再次发生才会新建。
/// 返回是否真的新建了告警，调用方据此决定要不要推送事件。
pub async fn raise_alert(
    executor: impl PgExecutor<'_>,
    level: AlertLevel,
    category: &str,
    title: &str,
    detail: &str,
    node_id: Option<Uuid>,
    dedup_key: Option<&str>,
) -> AppResult<bool> {
    let inserted: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO alerts (id, level, category, title, detail, node_id, dedup_key) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (dedup_key) WHERE resolved_at IS NULL DO NOTHING \
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(level.as_str())
    .bind(category)
    .bind(title)
    .bind(detail)
    .bind(node_id)
    .bind(dedup_key)
    .fetch_optional(executor)
    .await?;
    Ok(inserted.is_some())
}

/// 按去重键关闭告警（问题恢复时调用）。返回关闭了几条。
pub async fn resolve_alert_by_key(
    executor: impl PgExecutor<'_>,
    dedup_key: &str,
) -> AppResult<u64> {
    let affected = sqlx::query(
        "UPDATE alerts SET resolved_at = now() WHERE dedup_key = $1 AND resolved_at IS NULL",
    )
    .bind(dedup_key)
    .execute(executor)
    .await?
    .rows_affected();
    Ok(affected)
}

/// 管理员手工关闭一条告警。
pub async fn resolve_alert(executor: impl PgExecutor<'_>, id: Uuid) -> AppResult<Alert> {
    sqlx::query_as::<_, Alert>(
        "UPDATE alerts SET resolved_at = now() WHERE id = $1 \
         RETURNING id, level, category, title, detail, node_id, resolved_at, created_at",
    )
    .bind(id)
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::missing("告警不存在"))
}

/// 查询告警。`only_open` 为真时只返回未解决的。
pub async fn list_alerts(
    executor: impl PgExecutor<'_>,
    only_open: bool,
    limit: i64,
) -> AppResult<Vec<Alert>> {
    let alerts = sqlx::query_as::<_, Alert>(
        "SELECT id, level, category, title, detail, node_id, resolved_at, created_at \
         FROM alerts WHERE ($1 = FALSE OR resolved_at IS NULL) \
         ORDER BY created_at DESC LIMIT $2",
    )
    .bind(only_open)
    .bind(limit.clamp(1, 500))
    .fetch_all(executor)
    .await?;
    Ok(alerts)
}

/// 未解决告警条数，用于总览页角标。
pub async fn open_alert_count(executor: impl PgExecutor<'_>) -> AppResult<i64> {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM alerts WHERE resolved_at IS NULL")
        .fetch_one(executor)
        .await?;
    Ok(count)
}

// ---------------------------------------------------------------- 设置

/// 读取一项设置。
pub async fn get_setting(
    executor: impl PgExecutor<'_>,
    key: &str,
) -> AppResult<Option<serde_json::Value>> {
    let value: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = $1")
            .bind(key)
            .fetch_optional(executor)
            .await?;
    Ok(value)
}

/// 写入一项设置。
pub async fn put_setting(
    executor: impl PgExecutor<'_>,
    key: &str,
    value: &serde_json::Value,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES ($1, $2) \
         ON CONFLICT (key) DO UPDATE SET value = $2, updated_at = now()",
    )
    .bind(key)
    .bind(value)
    .execute(executor)
    .await?;
    Ok(())
}

/// 全部设置，按键排序。
pub async fn list_settings(
    executor: impl PgExecutor<'_>,
) -> AppResult<Vec<(String, serde_json::Value)>> {
    let rows: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT key, value FROM settings ORDER BY key")
            .fetch_all(executor)
            .await?;
    Ok(rows)
}

// ---------------------------------------------------------------- 每日统计

/// 累加当日统计。
///
/// 统计表是「顺手维护的物化计数」：总览页不必对 `task_executions` 做全表聚合。
pub async fn bump_daily_stat(
    executor: impl PgExecutor<'_>,
    completed: i64,
    failed: i64,
    skipped: i64,
    bytes_total: i64,
    account_used: i64,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO daily_stats (stat_date, completed, failed, skipped, bytes_total, account_used) \
         VALUES (current_date, $1, $2, $3, $4, $5) \
         ON CONFLICT (stat_date) DO UPDATE SET \
             completed = daily_stats.completed + $1, \
             failed = daily_stats.failed + $2, \
             skipped = daily_stats.skipped + $3, \
             bytes_total = daily_stats.bytes_total + $4, \
             account_used = daily_stats.account_used + $5, \
             updated_at = now()",
    )
    .bind(completed)
    .bind(failed)
    .bind(skipped)
    .bind(bytes_total)
    .bind(account_used)
    .execute(executor)
    .await?;
    Ok(())
}

/// 最近若干天的统计，按日期升序返回，便于前端直接画折线。
pub async fn recent_daily_stats(pool: &PgPool, days: i64) -> AppResult<Vec<DailyStat>> {
    let stats = sqlx::query_as::<_, DailyStat>(
        "SELECT stat_date, completed, failed, skipped, bytes_total, account_used \
         FROM daily_stats WHERE stat_date > current_date - $1::int \
         ORDER BY stat_date",
    )
    .bind(days.clamp(1, 400) as i32)
    .fetch_all(pool)
    .await?;
    Ok(stats)
}

/// 指定日期的统计。
pub async fn daily_stat(pool: &PgPool, date: NaiveDate) -> AppResult<Option<DailyStat>> {
    let stat = sqlx::query_as::<_, DailyStat>(
        "SELECT stat_date, completed, failed, skipped, bytes_total, account_used \
         FROM daily_stats WHERE stat_date = $1",
    )
    .bind(date)
    .fetch_optional(pool)
    .await?;
    Ok(stat)
}
