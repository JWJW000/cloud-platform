//! 站点账号管理接口（第 6.2 节）。

use axum::extract::{Path, Query, State};
use axum::Json;
use platform_domain::AccountStatus;
use serde::Deserialize;
use uuid::Uuid;

use crate::api::auth::AuthenticatedUser;
use crate::error::{AppError, AppResult};
use crate::models::Account;
use crate::state::AppState;
use crate::store;

#[derive(Debug, Deserialize)]
pub struct AccountListQuery {
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
pub struct CreateAccountRequest {
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub nickname: String,
    #[serde(default = "default_daily_limit")]
    pub daily_limit: i32,
    #[serde(default = "default_account_status")]
    pub status: String,
}

fn default_daily_limit() -> i32 {
    10
}

fn default_account_status() -> String {
    "已注册".to_string()
}

#[derive(Debug, Deserialize)]
pub struct UpdateAccountStatusRequest {
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAccountLimitRequest {
    pub daily_limit: i32,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAccountPasswordRequest {
    pub password: String,
}

/// GET /api/accounts
pub async fn list_accounts(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Query(query): Query<AccountListQuery>,
) -> AppResult<Json<Vec<Account>>> {
    let accounts = store::resource::list_accounts(
        &state.pool,
        query.status.as_deref(),
        query.limit,
        query.offset,
    )
    .await?;
    Ok(Json(accounts))
}

/// GET /api/accounts/:id
pub async fn get_account(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Account>> {
    let account = store::resource::get_account(&state.pool, id).await?;
    Ok(Json(account))
}

/// POST /api/accounts
pub async fn create_account(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Json(req): Json<CreateAccountRequest>,
) -> AppResult<Json<Account>> {
    auth.require_write()?;
    let email = req.email.trim();
    if email.is_empty() {
        return Err(AppError::bad("邮箱不能为空"));
    }
    if req.password.trim().is_empty() {
        return Err(AppError::bad("密码不能为空"));
    }

    let cipher_text = state
        .cipher
        .encrypt(req.password.trim())
        .map_err(AppError::Internal)?;

    let status = req.status.parse::<AccountStatus>()?;
    let nickname = if req.nickname.trim().is_empty() {
        email.split('@').next().unwrap_or(email).to_string()
    } else {
        req.nickname.trim().to_string()
    };

    let account = store::resource::create_account(
        &state.pool,
        email,
        &cipher_text,
        &nickname,
        req.daily_limit,
        status,
    )
    .await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "添加账号",
        &account.id.to_string(),
        &format!("添加账号 {email}"),
    )
    .await?;

    Ok(Json(account))
}

/// PUT /api/accounts/:id/status
pub async fn update_account_status(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateAccountStatusRequest>,
) -> AppResult<Json<Account>> {
    auth.require_write()?;
    let status = req.status.parse::<AccountStatus>()?;
    let account = store::resource::set_account_status(&state.pool, id, status, None).await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "修改账号状态",
        &id.to_string(),
        &format!("账号 {} 状态变更为 {status}", account.email),
    )
    .await?;

    Ok(Json(account))
}

/// PUT /api/accounts/:id/limit
pub async fn update_account_limit(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateAccountLimitRequest>,
) -> AppResult<Json<Account>> {
    auth.require_write()?;
    let account = store::resource::set_account_limit(&state.pool, id, req.daily_limit).await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "调整账号额度",
        &id.to_string(),
        &format!("账号 {} 额度上限变更为 {}", account.email, req.daily_limit),
    )
    .await?;

    Ok(Json(account))
}

/// PUT /api/accounts/:id/password
pub async fn update_account_password(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateAccountPasswordRequest>,
) -> AppResult<Json<serde_json::Value>> {
    auth.require_write()?;
    let password = req.password.trim();
    if password.is_empty() {
        return Err(AppError::bad("密码不能为空"));
    }

    let cipher_text = state.cipher.encrypt(password).map_err(AppError::Internal)?;

    store::resource::set_account_password(&state.pool, id, &cipher_text).await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "修改账号密码",
        &id.to_string(),
        "更新加密密码",
    )
    .await?;

    Ok(Json(serde_json::json!({ "message": "密码修改成功" })))
}

/// DELETE /api/accounts/:id
pub async fn delete_account(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    auth.require_write()?;
    let account = store::resource::get_account(&state.pool, id).await?;
    store::resource::delete_account(&state.pool, id).await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Warn,
        &auth.username,
        "删除账号",
        &id.to_string(),
        &format!("删除账号 {}", account.email),
    )
    .await?;

    Ok(Json(serde_json::json!({ "message": "账号已删除" })))
}
