//! 账号注册任务 API（V6 方案）。

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::auth::AuthenticatedUser;
use crate::error::AppResult;
use crate::models::AccountRegistrationTask;
use crate::state::AppState;
use crate::store;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_tasks))
        .route("/:id", get(get_task))
        .route("/:id/retry", post(retry_task))
        .route("/:id/cancel", post(cancel_task))
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListTasksQuery {
    pub status: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    50
}

async fn list_tasks(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Query(query): Query<ListTasksQuery>,
) -> AppResult<Json<Vec<AccountRegistrationTask>>> {
    let tasks = store::account_registration::list_all_tasks(
        &state.pool,
        query.status.as_deref(),
        query.limit,
        query.offset,
    )
    .await?;

    Ok(Json(tasks))
}

async fn get_task(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<AccountRegistrationTask>> {
    let task = store::account_registration::get_task(&state.pool, id).await?;
    Ok(Json(task))
}

async fn retry_task(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<AccountRegistrationTask>> {
    auth.require_super_admin()?;

    store::account_registration::retry_task(&state.pool, id).await?;

    let _ = crate::scheduler::trigger_scheduler_sweep(&state).await;

    let task = store::account_registration::get_task(&state.pool, id).await?;
    Ok(Json(task))
}

async fn cancel_task(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<AccountRegistrationTask>> {
    auth.require_super_admin()?;

    store::account_registration::cancel_task(&state.pool, id).await?;

    let task = store::account_registration::get_task(&state.pool, id).await?;
    Ok(Json(task))
}
