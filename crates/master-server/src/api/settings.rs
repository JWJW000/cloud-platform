//! 系统设置与枚举字典接口（第 11 节）。

use axum::extract::{Path, State};
use axum::Json;
use platform_domain::{
    AccountStatus, AlertLevel, BatchStatus, ExecutionResult, LogLevel, OperationSource,
    ProxyStatus, SessionStatus, SlotStatus, TaskStatus, TaskType, VerifyStatus, WorkerStatus,
};
use serde::{Deserialize, Serialize};

use crate::api::auth::AuthenticatedUser;
use crate::error::AppResult;
use crate::state::AppState;
use crate::store;

#[derive(Debug, Deserialize)]
pub struct PutSettingRequest {
    pub value: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct EnumDict {
    pub account_status: Vec<&'static str>,
    pub task_status: Vec<&'static str>,
    pub batch_status: Vec<&'static str>,
    pub worker_status: Vec<&'static str>,
    pub slot_status: Vec<&'static str>,
    pub session_status: Vec<&'static str>,
    pub proxy_status: Vec<&'static str>,
    pub task_type: Vec<&'static str>,
    pub execution_result: Vec<&'static str>,
    pub log_level: Vec<&'static str>,
    pub operation_source: Vec<&'static str>,
    pub verify_status: Vec<&'static str>,
    pub alert_level: Vec<&'static str>,
}

/// GET /api/settings
pub async fn list_settings(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
) -> AppResult<Json<Vec<(String, serde_json::Value)>>> {
    let settings = store::admin::list_settings(&state.pool).await?;
    Ok(Json(settings))
}

/// GET /api/settings/:key
pub async fn get_setting(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(key): Path<String>,
) -> AppResult<Json<Option<serde_json::Value>>> {
    let val = store::admin::get_setting(&state.pool, &key).await?;
    Ok(Json(val))
}

/// PUT /api/settings/:key
pub async fn put_setting(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(key): Path<String>,
    Json(req): Json<PutSettingRequest>,
) -> AppResult<Json<serde_json::Value>> {
    auth.require_super_admin()?;
    store::admin::put_setting(&state.pool, &key, &req.value).await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "修改系统设置",
        &key,
        &req.value.to_string(),
    )
    .await?;

    Ok(Json(serde_json::json!({ "message": "设置已保存" })))
}

/// GET /api/dict
pub async fn get_dict() -> Json<EnumDict> {
    Json(EnumDict {
        account_status: AccountStatus::all_values(),
        task_status: TaskStatus::all_values(),
        batch_status: BatchStatus::all_values(),
        worker_status: WorkerStatus::all_values(),
        slot_status: SlotStatus::all_values(),
        session_status: SessionStatus::all_values(),
        proxy_status: ProxyStatus::all_values(),
        task_type: TaskType::all_values(),
        execution_result: ExecutionResult::all_values(),
        log_level: LogLevel::all_values(),
        operation_source: OperationSource::all_values(),
        verify_status: VerifyStatus::all_values(),
        alert_level: AlertLevel::all_values(),
    })
}
