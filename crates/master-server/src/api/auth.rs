//! 鉴权与管理员会话管理（第 15.2 节、V4 方案第 13 节）。
//!
//! V4 修复点（R7）：
//! - **Cookie-only 会话**（V4-10）：登录响应不再返回 JWT，只返回用户公开信息；
//!   Cookie 使用 ASCII 名 `admin_session` 并带 `Secure`。
//! - **强制会话表验证**（V4-11 / V4-12）：`jti` 必须存在且为合法 UUID；
//!   会话必须存在、未撤销、未过期；`session.user_id` 必须等于 `sub`；
//!   请求 token 的 SHA-256 必须与 `token_hash` 常量时间相等。任意一步失败都返回 401，
//!   不允许空 jti 返回 `None` 后继续认证。
//! - **有界双维度限流**（第 13.5 节）：全局容量上限、定期清理、指数退避；
//!   登录成功只清理对应用户名计数，不清空共享 IP 的防护。
//! - **可信代理链**（第 13.4 节）：只信任来自配置网段的 `X-Forwarded-For`。

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, FromRequestParts, State};
use axum::http::header::{HeaderMap, AUTHORIZATION, COOKIE, SET_COOKIE};
use axum::http::request::Parts;
use axum::http::HeaderValue;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::MasterConfig;
use crate::error::{AppError, AppResult};
use crate::models::User;
use crate::security::verify_password;
use crate::state::AppState;
use crate::store;

/// Cookie 名称：使用 ASCII，避免不同代理对非 ASCII Cookie 名处理不一致（第 13.1 节）。
pub const SESSION_COOKIE_NAME: &str = "admin_session";
/// 兼容旧版本的非 ASCII Cookie 名（只读）。
const LEGACY_COOKIE_NAME: &str = "管理会话";

/// 登录限流：IP 与用户名双维度、全局容量上限、指数退避。
///
/// 规则（第 13.5 节）：
/// - IP 15 分钟最多 20 次失败，用户名 15 分钟最多 10 次失败；
/// - 单 key 连续失败按 2^N 秒退避；
/// - 全局键数上限，超出时先清理过期条目，仍超限则拒绝新记录；
/// - 登录成功**只清理对应用户名**计数，绝不清空共享 IP 的防护。
struct LoginRateLimiter {
    entries: Mutex<HashMap<String, Vec<Instant>>>,
}

const RATE_WINDOW: Duration = Duration::from_secs(15 * 60);
const MAX_IP_FAILURES: usize = 20;
const MAX_USER_FAILURES: usize = 10;
const MAX_ENTRIES: usize = 10_000;

impl LoginRateLimiter {
    fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn check_allowed(&self, ip_key: &str, username_key: &str) -> bool {
        let mut map = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();

        // 定期清理过期条目（每次访问都顺带做，无独立定时任务）
        map.retain(|_, list| {
            list.retain(|&t| now.duration_since(t) < RATE_WINDOW);
            !list.is_empty()
        });

        if self.key_allows(&map, &format!("ip:{ip_key}"), MAX_IP_FAILURES, now) {
            self.key_allows(
                &map,
                &format!("user:{username_key}"),
                MAX_USER_FAILURES,
                now,
            )
        } else {
            false
        }
    }

    fn key_allows(
        &self,
        map: &HashMap<String, Vec<Instant>>,
        key: &str,
        limit: usize,
        now: Instant,
    ) -> bool {
        let Some(failures) = map.get(key) else {
            return true;
        };
        if failures.len() < limit {
            return true;
        }
        // 达到阈值后指数退避：block = 2^(count-limit) 分钟，最多 30 分钟
        let extra = failures.len().saturating_sub(limit).min(5) as u32;
        let block = Duration::from_secs(60u64 << extra.min(5));
        let last = *failures.last().unwrap();
        now.duration_since(last) >= block
    }

    fn record_failure(&self, ip_key: &str, username_key: &str) {
        let mut map = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        map.retain(|_, list| {
            list.retain(|&t| now.duration_since(t) < RATE_WINDOW);
            !list.is_empty()
        });
        // 全局容量上限：只拒绝**新增键**；已存在的键仍必须累计失败次数——
        // 否则攻击者用随机用户名填满容量后，暴力登录的失败记录不再累计（P1）。
        let ip_key_owned = format!("ip:{ip_key}");
        let user_key_owned = format!("user:{username_key}");
        let at_capacity = map.len() >= MAX_ENTRIES;
        let both_new = !map.contains_key(&ip_key_owned) && !map.contains_key(&user_key_owned);
        if at_capacity && both_new {
            tracing::warn!(entries = map.len(), "登录限流条目达到上限，拒绝新增键");
            return;
        }
        map.entry(ip_key_owned).or_default().push(now);
        map.entry(user_key_owned).or_default().push(now);
    }

    fn record_success(&self, username_key: &str) {
        let mut map = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        // 只清理用户名计数：攻击者不能通过一次成功清空共享 IP 的防护
        map.remove(&format!("user:{username_key}"));
    }
}

static RATE_LIMITER: LazyLock<LoginRateLimiter> = LazyLock::new(LoginRateLimiter::new);

/// 登录请求。
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    /// 用户名。
    pub username: String,
    /// 登录密码。
    pub password: String,
}

/// 登录响应：**只返回用户公开信息，不返回 JWT**（V4-10 / 第 13.1 节）。
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    /// 用户信息。
    pub user: User,
}

/// 改密请求。
#[derive(Debug, Deserialize)]
pub struct ChangePasswordRequest {
    /// 原密码。
    pub old_password: String,
    /// 新密码。
    pub new_password: String,
}

/// 经过鉴权的用户身份信息（Axum 提取器）。
#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    /// 用户编号。
    pub id: Uuid,
    /// 用户名。
    pub username: String,
    /// 中文角色：超级管理员 / 任务管理员 / 只读用户（从数据库动态读取）。
    pub role: String,
    /// 会话编号（jti）。
    pub session_id: Option<Uuid>,
}

impl AuthenticatedUser {
    /// 是否为超级管理员。
    pub fn is_super_admin(&self) -> bool {
        self.role == "超级管理员"
    }

    /// 是否拥有写权限（超级管理员或任务管理员）。
    pub fn can_write(&self) -> bool {
        self.role == "超级管理员" || self.role == "任务管理员"
    }

    /// 校验超级管理员权限，不足则返回 403。
    pub fn require_super_admin(&self) -> AppResult<()> {
        if self.is_super_admin() {
            Ok(())
        } else {
            Err(AppError::Forbidden("需要超级管理员权限".to_string()))
        }
    }

    /// 校验写权限，不足则返回 403。
    pub fn require_write(&self) -> AppResult<()> {
        if self.can_write() {
            Ok(())
        } else {
            Err(AppError::Forbidden("需要管理员写权限".to_string()))
        }
    }
}

#[axum::async_trait]
impl FromRequestParts<AppState> for AuthenticatedUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = extract_token_from_parts(parts)?;

        let claims = state
            .tokens
            .verify(&token)
            .map_err(|err| AppError::Unauthorized(err.to_string()))?;

        let user_id = Uuid::parse_str(&claims.sub)
            .map_err(|_| AppError::Unauthorized("令牌中用户编号格式无效".to_string()))?;

        // V4-11：jti 必须存在且是合法 UUID——不允许空 jti 跳过会话表验证。
        let session_id = Uuid::parse_str(&claims.jti)
            .map_err(|_| AppError::Unauthorized("令牌缺少合法的会话编号（jti）".to_string()))?;

        // V4-12：会话记录必须存在、未撤销、未过期，且 session.user_id 必须等于 sub；
        // 请求 token 的 SHA-256 必须与 token_hash 常量时间相等。
        let session = store::admin::get_admin_session(&state.pool, session_id)
            .await?
            .ok_or_else(|| AppError::Unauthorized("会话记录不存在".to_string()))?;
        if session.revoked_at.is_some() {
            return Err(AppError::Unauthorized("会话已被撤销".to_string()));
        }
        if session.expires_at < Utc::now() {
            return Err(AppError::Unauthorized("会话已过期".to_string()));
        }
        if session.user_id != user_id {
            return Err(AppError::Unauthorized("会话与令牌用户不一致".to_string()));
        }
        let token_hash = hash_token(&token);
        if !crate::security::constant_time_eq(&session.token_hash, &token_hash) {
            return Err(AppError::Unauthorized("会话令牌校验失败".to_string()));
        }

        // 每次请求查库：验证用户依然存在且状态为「启用」，并且 token_version 匹配
        let user = store::admin::get_user_by_id(&state.pool, user_id)
            .await?
            .ok_or_else(|| AppError::Unauthorized("用户不存在或已被删除".to_string()))?;

        if user.status != "启用" {
            return Err(AppError::Unauthorized("用户已被禁用".to_string()));
        }

        if user.token_version != claims.ver {
            return Err(AppError::Unauthorized(
                "会话已失效（密码或权限已变更）".to_string(),
            ));
        }

        // 会话最后访问时间限频更新：失败不影响业务请求（第 13.3 节）
        let _ = store::admin::touch_admin_session_limited(&state.pool, session_id).await;

        Ok(AuthenticatedUser {
            id: user.id,
            username: user.username,
            role: user.role, // 使用数据库中的当前最新角色
            session_id: Some(session_id),
        })
    }
}

/// 从请求头（Authorization 或 Cookie）中提取 JWT 令牌。
pub fn extract_token_from_parts(parts: &Parts) -> AppResult<String> {
    if let Some(auth_header) = parts
        .headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        let token = if let Some(stripped) = auth_header.strip_prefix("Bearer ") {
            stripped.trim()
        } else {
            auth_header.trim()
        };
        if !token.is_empty() {
            return Ok(token.to_string());
        }
    }

    if let Some(cookie_header) = parts.headers.get(COOKIE).and_then(|v| v.to_str().ok()) {
        for cookie in cookie_header.split(';') {
            let cookie = cookie.trim();
            if let Some((name, val)) = cookie.split_once('=') {
                let name = name.trim();
                if name == SESSION_COOKIE_NAME || name == LEGACY_COOKIE_NAME {
                    let val = val.trim();
                    if !val.is_empty() {
                        return Ok(val.to_string());
                    }
                }
            }
        }
    }

    Err(AppError::Unauthorized("请求缺少认证凭据".to_string()))
}

/// 计算令牌 SHA-256 散列，用于安全存储在 admin_sessions 表中。
fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

/// 解析客户端真实 IP（V4 第 13.4 / 13.5 节）。
///
/// 只有来自可信代理网段（`trusted_proxies`）的连接才信任 `X-Forwarded-For`；
/// 未配置可信代理时完全忽略转发头（防伪造）。
fn client_ip(config: &MasterConfig, peer: Option<SocketAddr>, headers: &HeaderMap) -> String {
    let peer_ip: Option<IpAddr> = peer.map(|p| p.ip());
    let trust_xff = !config.server.trusted_proxies.is_empty()
        && peer_ip
            .map(|ip| is_trusted_proxy(&config.server.trusted_proxies, ip))
            .unwrap_or(false);

    if trust_xff {
        if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(first) = xff.split(',').next().map(|s| s.trim()) {
                if !first.is_empty() {
                    return first.to_string();
                }
            }
        }
    }
    peer_ip
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "127.0.0.1".to_string())
}

/// 判断 IP 是否属于可信代理网段（CIDR 或精确 IP）。
fn is_trusted_proxy(trusted: &[String], ip: IpAddr) -> bool {
    trusted.iter().any(|entry| {
        let entry = entry.trim();
        if let Ok(cidr) = entry.parse::<ipnet::IpNet>() {
            cidr.contains(&ip)
        } else if let Ok(addr) = entry.parse::<IpAddr>() {
            addr == ip
        } else {
            false
        }
    })
}

/// POST /api/auth/login
pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LoginRequest>,
) -> AppResult<Response> {
    let username = req.username.trim();
    let ip_str = client_ip(&state.config, Some(peer), &headers);

    if !RATE_LIMITER.check_allowed(&ip_str, username) {
        return Err(AppError::TooManyRequests(
            "登录尝试过于频繁，请稍后再试".to_string(),
        ));
    }

    let creds = match store::admin::find_credentials(&state.pool, username).await? {
        Some(c) => c,
        None => {
            RATE_LIMITER.record_failure(&ip_str, username);
            store::admin::log_security_failure(&state.pool, &ip_str, username, "账户不存在")
                .await?;
            return Err(AppError::Unauthorized("用户名或密码错误".to_string()));
        }
    };

    if creds.status != "启用" {
        // P2：统一返回「用户名或密码错误」+ 401——独立的 403 与文案会暴露
        // 有效用户名，便于账户枚举。禁用原因只进安全审计日志。
        RATE_LIMITER.record_failure(&ip_str, username);
        store::admin::log_security_failure(&state.pool, &ip_str, username, "账户已禁用").await?;
        return Err(AppError::Unauthorized("用户名或密码错误".to_string()));
    }

    let ok = verify_password(&req.password, &creds.password_hash)
        .map_err(|_| AppError::Unauthorized("用户名或密码错误".to_string()))?;
    if !ok {
        RATE_LIMITER.record_failure(&ip_str, username);
        store::admin::log_security_failure(&state.pool, &ip_str, username, "密码错误").await?;
        return Err(AppError::Unauthorized("用户名或密码错误".to_string()));
    }

    // 登录成功：只清理该用户名的计数（不清空共享 IP 防护）
    RATE_LIMITER.record_success(username);

    let session_id = Uuid::new_v4();
    let token = state
        .tokens
        .issue(
            &creds.id.to_string(),
            &session_id.to_string(),
            &creds.username,
            &creds.role,
            creds.token_version,
        )
        .map_err(AppError::Internal)?;

    let token_hash = hash_token(&token);
    let validity_hours = state.tokens.validity_hours();
    let expires_at = Utc::now() + ChronoDuration::hours(validity_hours);

    let user_agent = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(|ua| {
            let mut hasher = Sha256::new();
            hasher.update(ua.as_bytes());
            hex::encode(hasher.finalize())
        });

    store::admin::create_admin_session(
        &state.pool,
        session_id,
        creds.id,
        &token_hash,
        expires_at,
        user_agent.as_deref(),
        Some(&ip_str),
    )
    .await?;

    store::admin::touch_login(&state.pool, creds.id).await?;

    // V4-10 / 第 13.1 节：Cookie-only，响应体不含 JWT；Cookie 带 Secure。
    let max_age = validity_hours * 3600;
    let secure_flag = if state.config.security.cookie_secure {
        "Secure; "
    } else {
        ""
    };
    let cookie_value = format!(
        "{SESSION_COOKIE_NAME}={token}; Path=/; HttpOnly; {secure_flag}SameSite=Lax; Max-Age={max_age}"
    );

    let mut response = Json(LoginResponse { user: creds.user() }).into_response();

    if let Ok(header_val) = HeaderValue::from_str(&cookie_value) {
        response.headers_mut().insert(SET_COOKIE, header_val);
    }

    Ok(response)
}

/// GET /api/auth/me
pub async fn me(State(state): State<AppState>, auth: AuthenticatedUser) -> AppResult<Json<User>> {
    let user = store::admin::get_user_by_id(&state.pool, auth.id)
        .await?
        .ok_or_else(|| AppError::missing("用户不存在"))?;
    Ok(Json(user))
}

/// POST /api/auth/logout
///
/// 不要求有效会话（P1）：Cookie 已过期、会话已撤销或令牌损坏时，请求也必须在
/// 进入处理函数后拿到清除 Cookie 的响应——用 `AuthenticatedUser` 提取器会在
/// 令牌失效时提前返回 401，浏览器就永远清不掉那条坏 Cookie（V4 第 13.3 节）。
pub async fn logout(
    State(state): State<AppState>,
    req: axum::extract::Request,
) -> AppResult<Response> {
    // 尽力解析并撤销会话；任何失败（过期/损坏/未知 jti）都不影响清除 Cookie
    if let Ok(token) = extract_token_from_parts(&req.into_parts().0) {
        if let Ok(claims) = state.tokens.verify(&token) {
            if let Ok(session_id) = Uuid::parse_str(&claims.jti) {
                let _ = store::admin::revoke_admin_session(&state.pool, session_id, "用户主动退出")
                    .await;
            }
        }
    }

    // 无论 Cookie 是否过期都返回清除指令；新旧两个名字都清（第 13.3 节）
    let secure_flag = if state.config.security.cookie_secure {
        "Secure; "
    } else {
        ""
    };
    let mut response = Json(serde_json::json!({ "message": "已退出登录" })).into_response();
    for name in [SESSION_COOKIE_NAME, LEGACY_COOKIE_NAME] {
        let cookie_clear =
            format!("{name}=; Path=/; HttpOnly; {secure_flag}SameSite=Lax; Max-Age=0");
        if let Ok(header_val) = HeaderValue::from_str(&cookie_clear) {
            response.headers_mut().append(SET_COOKIE, header_val);
        }
    }

    Ok(response)
}

/// PUT /api/auth/password
pub async fn change_password(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Json(req): Json<ChangePasswordRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let creds = store::admin::find_credentials(&state.pool, &auth.username)
        .await?
        .ok_or_else(|| AppError::missing("用户不存在"))?;

    let ok = verify_password(&req.old_password, &creds.password_hash)
        .map_err(|_| AppError::Unauthorized("原密码不正确".to_string()))?;
    if !ok {
        return Err(AppError::Unauthorized("原密码不正确".to_string()));
    }

    let new_hash = crate::security::hash_password(&req.new_password)
        .map_err(|err| AppError::bad(format!("新密码不合法：{err}")))?;

    // set_user_password 会更新密码、自增 token_version 并撤销该用户全部已有会话
    store::admin::set_user_password(&state.pool, auth.id, &new_hash).await?;

    Ok(Json(
        serde_json::json!({ "message": "密码修改成功，请重新登录" }),
    ))
}
