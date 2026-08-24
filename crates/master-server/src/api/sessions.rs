//! 执行会话管理接口（第 6.4 节）。

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use crate::api::auth::AuthenticatedUser;
use crate::error::AppResult;
use crate::grpc::convert;
use crate::models::ExecutionSession;
use crate::state::AppState;
use crate::store;

#[derive(Debug, Deserialize)]
pub struct SessionListQuery {
    pub status: Option<String>,
    pub node_id: Option<Uuid>,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    50
}

/// GET /api/sessions
pub async fn list_sessions(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Query(query): Query<SessionListQuery>,
) -> AppResult<Json<Vec<ExecutionSession>>> {
    let sessions = store::session::list_sessions(
        &state.pool,
        query.status.as_deref(),
        query.node_id,
        query.limit,
    )
    .await?;
    Ok(Json(sessions))
}

/// GET /api/sessions/:id
pub async fn get_session(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<ExecutionSession>> {
    let session = store::session::get_session(&state.pool, id).await?;
    Ok(Json(session))
}

/// POST /api/sessions/:id/terminate
pub async fn terminate_session(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    auth.require_write()?;
    let session = store::session::get_session(&state.pool, id).await?;

    let msg =
        convert::end_session_message(id, &format!("管理员 {} 手工结束会话", auth.username), false);
    state.links.try_dispatch(session.node_id, msg);

    crate::scheduler::allocate::close_session(
        &state,
        id,
        platform_domain::SessionStatus::Ended,
        &format!("管理员 {} 手工结束", auth.username),
    )
    .await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "结束执行会话",
        &id.to_string(),
        "管理员远程终止执行会话",
    )
    .await?;

    state.events.publish(
        "会话变更",
        serde_json::json!({ "会话": id, "状态": "已结束" }),
    );

    Ok(Json(serde_json::json!({ "message": "会话已终止" })))
}
