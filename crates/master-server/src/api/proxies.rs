//! 代理池管理接口（第 6.3 节）。

use axum::extract::{Path, Query, State};
use axum::Json;
use platform_domain::ProxyStatus;
use serde::Deserialize;
use uuid::Uuid;

use crate::api::auth::AuthenticatedUser;
use crate::error::{AppError, AppResult};
use crate::models::Proxy;
use crate::state::AppState;
use crate::store;

#[derive(Debug, Deserialize)]
pub struct ProxyListQuery {
    pub status: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    50
}

#[derive(Debug, Deserialize)]
pub struct CreateProxyRequest {
    #[serde(default = "default_provider")]
    pub provider: String,
    pub external_id: Option<String>,
    pub label: String,
    #[serde(default = "default_scheme")]
    pub scheme: String,
    pub host: String,
    pub port: i32,
    pub username: Option<String>,
    pub password: Option<String>,
}

fn default_provider() -> String {
    "webshare".to_string()
}

fn default_scheme() -> String {
    "http".to_string()
}

#[derive(Debug, Deserialize)]
pub struct UpdateProxyStatusRequest {
    pub status: String,
}

/// GET /api/proxies
pub async fn list_proxies(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Query(query): Query<ProxyListQuery>,
) -> AppResult<Json<Vec<Proxy>>> {
    let proxies = store::resource::list_proxies(
        &state.pool,
        query.status.as_deref(),
        query.limit,
        query.offset,
    )
    .await?;
    Ok(Json(proxies))
}

/// GET /api/proxies/:id
pub async fn get_proxy(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Proxy>> {
    let proxy = store::resource::get_proxy(&state.pool, id).await?;
    Ok(Json(proxy))
}

/// POST /api/proxies
pub async fn create_proxy(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Json(req): Json<CreateProxyRequest>,
) -> AppResult<Json<Proxy>> {
    auth.require_write()?;
    let host = req.host.trim();
    if host.is_empty() {
        return Err(AppError::bad("主机地址不能为空"));
    }

    let password_cipher = if let Some(pwd) = req.password {
        if !pwd.trim().is_empty() {
            Some(
                state
                    .cipher
                    .encrypt(pwd.trim())
                    .map_err(AppError::Internal)?,
            )
        } else {
            None
        }
    } else {
        None
    };

    let proxy = store::resource::upsert_proxy(
        &state.pool,
        &req.provider,
        req.external_id.as_deref(),
        &req.label,
        &req.scheme,
        host,
        req.port,
        req.username.as_deref(),
        password_cipher.as_deref(),
    )
    .await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "添加代理",
        &proxy.id.to_string(),
        &format!("添加代理 {}:{}", proxy.host, proxy.port),
    )
    .await?;

    Ok(Json(proxy))
}

/// PUT /api/proxies/:id/status
pub async fn update_proxy_status(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateProxyStatusRequest>,
) -> AppResult<Json<Proxy>> {
    auth.require_write()?;
    let status = req.status.parse::<ProxyStatus>()?;
    let proxy = store::resource::set_proxy_status(&state.pool, id, status, None).await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "修改代理状态",
        &id.to_string(),
        &format!("代理状态变更为 {status}"),
    )
    .await?;

    Ok(Json(proxy))
}

/// DELETE /api/proxies/:id
pub async fn delete_proxy(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    auth.require_write()?;
    store::resource::delete_proxy(&state.pool, id).await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Warn,
        &auth.username,
        "删除代理",
        &id.to_string(),
        "删除代理记录",
    )
    .await?;

    Ok(Json(serde_json::json!({ "message": "代理已删除" })))
}
