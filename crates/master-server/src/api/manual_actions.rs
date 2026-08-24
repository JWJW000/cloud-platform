//! 待确认事项 API（人工确认流程，V6 方案第 9 节）。

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::auth::AuthenticatedUser;
use crate::error::{AppError, AppResult};
use crate::grpc::convert;
use crate::models::ManualAction;
use crate::state::AppState;
use crate::store;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_actions))
        .route("/:id", get(get_action))
        .route("/:id/resolve", post(resolve_action))
        .route("/:id/cancel", post(cancel_action))
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListActionsQuery {
    pub status: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    50
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResolveActionRequest {
    pub input_code: String,
}

async fn list_actions(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Query(query): Query<ListActionsQuery>,
) -> AppResult<Json<Vec<ManualAction>>> {
    let actions =
        store::manual_action::list_actions(&state.pool, query.status.as_deref(), query.limit)
            .await?;

    Ok(Json(actions))
}

async fn get_action(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<ManualAction>> {
    let action = store::manual_action::get_action(&state.pool, id).await?;
    Ok(Json(action))
}

async fn resolve_action(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
    Json(req): Json<ResolveActionRequest>,
) -> AppResult<Json<ManualAction>> {
    auth.require_write()?;

    if req.input_code.trim().is_empty() {
        return Err(AppError::bad("验证码或确认内容不能为空"));
    }

    let action = store::manual_action::resolve_action(
        &state.pool,
        id,
        Some(req.input_code.trim()),
        Some(auth.id),
    )
    .await?;

    // 如果关联的 Worker 节点在线，下发 ContinueManualAction 消息
    if let (Some(node_id), Some(exec_id)) = (action.node_id, action.execution_id) {
        if state.links.is_online(node_id) {
            let msg = convert::continue_manual_action_message(
                action.id,
                exec_id,
                &action.action_type,
                req.input_code.trim(),
            );
            state.links.try_dispatch(node_id, msg);
        }
    }

    state.events.publish(
        "人工确认变更",
        serde_json::json!({ "事项": id, "状态": "已解决" }),
    );

    Ok(Json(action))
}

async fn cancel_action(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<ManualAction>> {
    auth.require_write()?;

    let action = store::manual_action::cancel_action(&state.pool, id, Some(auth.id)).await?;

    state.events.publish(
        "人工确认变更",
        serde_json::json!({ "事项": id, "状态": "已取消" }),
    );

    Ok(Json(action))
}
