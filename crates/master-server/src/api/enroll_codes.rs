//! 一次性节点注册码管理接口（第 15.1 节）。

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;

use crate::api::auth::AuthenticatedUser;
use crate::error::AppResult;
use crate::models::EnrollCode;
use crate::security::new_enroll_code;
use crate::state::AppState;
use crate::store;

#[derive(Debug, Deserialize)]
pub struct IssueCodeRequest {
    /// 备注说明。
    pub note: Option<String>,
    /// 允许该机器开启的最大槽位数。
    #[serde(default = "default_max_slots")]
    pub max_slots: i32,
    /// 有效小时数。
    #[serde(default = "default_valid_hours")]
    pub valid_hours: i64,
}

fn default_max_slots() -> i32 {
    5
}

fn default_valid_hours() -> i64 {
    24
}

/// GET /api/enroll-codes
pub async fn list_enroll_codes(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
) -> AppResult<Json<Vec<EnrollCode>>> {
    let codes = store::node::list_enroll_codes(&state.pool).await?;
    Ok(Json(codes))
}

/// POST /api/enroll-codes
pub async fn create_enroll_code(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Json(req): Json<IssueCodeRequest>,
) -> AppResult<Json<EnrollCode>> {
    auth.require_super_admin()?;
    let code_str = new_enroll_code();

    let code = store::node::issue_enroll_code(
        &state.pool,
        &code_str,
        req.note.as_deref(),
        req.max_slots,
        req.valid_hours,
        Some(auth.id),
    )
    .await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "生成注册码",
        &code_str,
        &format!(
            "槽位上限 {}，有效期 {} 小时",
            req.max_slots, req.valid_hours
        ),
    )
    .await?;

    Ok(Json(code))
}

/// DELETE /api/enroll-codes/:code
pub async fn delete_enroll_code(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(code): Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    auth.require_super_admin()?;
    store::node::delete_enroll_code(&state.pool, &code).await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "删除注册码",
        &code,
        "作废未使用的注册码",
    )
    .await?;

    Ok(Json(serde_json::json!({ "message": "注册码已删除" })))
}
