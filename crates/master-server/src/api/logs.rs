//! 操作日志与系统告警接口（第 17 节）。

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use crate::api::auth::AuthenticatedUser;
use crate::error::AppResult;
use crate::models::{Alert, OperationLog};
use crate::state::AppState;
use crate::store;

#[derive(Debug, Deserialize)]
pub struct LogListQuery {
    pub level: Option<String>,
    pub source: Option<String>,
    pub keyword: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    50
}

#[derive(Debug, Deserialize)]
pub struct AlertListQuery {
    #[serde(default = "default_only_open")]
    pub only_open: bool,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_only_open() -> bool {
    true
}

/// GET /api/logs
pub async fn list_logs(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Query(query): Query<LogListQuery>,
) -> AppResult<Json<Vec<OperationLog>>> {
    let logs = store::admin::list_logs(
        &state.pool,
        query.level.as_deref(),
        query.source.as_deref(),
        query.keyword.as_deref(),
        query.limit,
        query.offset,
    )
    .await?;
    Ok(Json(logs))
}

/// GET /api/alerts
pub async fn list_alerts(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Query(query): Query<AlertListQuery>,
) -> AppResult<Json<Vec<Alert>>> {
    let alerts = store::admin::list_alerts(&state.pool, query.only_open, query.limit).await?;
    Ok(Json(alerts))
}

/// POST /api/alerts/:id/resolve
pub async fn resolve_alert(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Alert>> {
    auth.require_write()?;
    let alert = store::admin::resolve_alert(&state.pool, id).await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "关闭告警",
        &id.to_string(),
        &format!("关闭告警《{}》", alert.title),
    )
    .await?;

    state.events.publish(
        "告警变更",
        serde_json::json!({ "告警": id, "动作": "已解决" }),
    );

    Ok(Json(alert))
}
