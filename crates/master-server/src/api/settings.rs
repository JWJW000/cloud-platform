//! 系统设置与枚举字典接口（第 11 节）。

use axum::extract::{Path, State};
use axum::Json;
use platform_domain::{
    AccountStatus, AlertLevel, BatchStatus, ExecutionResult, LogLevel, OperationSource,
    ProxyStatus, SessionStatus, SlotStatus, TaskStatus, TaskType, VerifyStatus, WorkerStatus,
};
use serde::{Deserialize, Serialize};

use crate::api::auth::AuthenticatedUser;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::store;

#[derive(Debug, Serialize)]
pub struct SettingResponse {
    pub key: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct PutSettingRequest {
    pub value: serde_json::Value,
}

fn compatible_setting_value(existing: &serde_json::Value, replacement: &serde_json::Value) -> bool {
    matches!(
        (existing, replacement),
        (serde_json::Value::Bool(_), serde_json::Value::Bool(_))
            | (serde_json::Value::Number(_), serde_json::Value::Number(_))
            | (serde_json::Value::String(_), serde_json::Value::String(_))
            | (serde_json::Value::Array(_), serde_json::Value::Array(_))
            | (serde_json::Value::Object(_), serde_json::Value::Object(_))
            | (serde_json::Value::Null, serde_json::Value::Null)
    )
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
    auth: AuthenticatedUser,
) -> AppResult<Json<Vec<SettingResponse>>> {
    auth.require_super_admin()?;
    let settings = store::admin::list_settings(&state.pool).await?;
    Ok(Json(
        settings
            .into_iter()
            .map(|(key, value)| SettingResponse { key, value })
            .collect(),
    ))
}

/// GET /api/settings/:key
pub async fn get_setting(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(key): Path<String>,
) -> AppResult<Json<Option<serde_json::Value>>> {
    auth.require_super_admin()?;
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
    let normalized = key.trim().to_ascii_lowercase();
    if key.len() > 128
        || key.is_empty()
        || matches!(
            normalized.as_str(),
            "mail_code_provider" | "global_download_paused" | "webhook_notification_config"
        )
    {
        return Err(AppError::bad("该设置键无效或必须使用类型化设置接口"));
    }
    if ["secret", "password", "token", "api_key"]
        .iter()
        .any(|marker| normalized.contains(marker))
    {
        return Err(AppError::bad("敏感值不得通过通用设置接口保存"));
    }
    let serialized =
        serde_json::to_vec(&req.value).map_err(|error| AppError::Internal(error.into()))?;
    if serialized.len() > 16 * 1024 {
        return Err(AppError::bad("设置值超过 16 KiB 上限"));
    }
    let existing = store::admin::get_setting(&state.pool, &key)
        .await?
        .ok_or_else(|| AppError::bad("未知设置不能通过兼容接口创建"))?;
    if !compatible_setting_value(&existing, &req.value) {
        return Err(AppError::bad("设置值类型与现有定义不一致"));
    }
    store::admin::put_setting(&state.pool, &key, &req.value).await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "修改系统设置",
        &key,
        &format!("value_type=compatible,size_bytes={}", serialized.len()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatibility_editor_cannot_change_json_types() {
        assert!(compatible_setting_value(
            &serde_json::json!(10),
            &serde_json::json!(20)
        ));
        assert!(!compatible_setting_value(
            &serde_json::json!(10),
            &serde_json::json!("20")
        ));
        assert!(!compatible_setting_value(
            &serde_json::Value::Null,
            &serde_json::json!({})
        ));
    }
}
