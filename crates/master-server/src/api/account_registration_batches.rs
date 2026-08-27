//! 账号注册批次管理 API（V6 方案）。

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use platform_domain::BatchStatus;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::auth::AuthenticatedUser;
use crate::error::{AppError, AppResult};
use crate::models::{
    AccountRegistrationBatch, AccountRegistrationBatchProgress, AccountRegistrationTask,
};
use crate::scheduler;
use crate::state::AppState;
use crate::store;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_batches).post(create_batch))
        .route("/:id", get(get_batch))
        .route("/:id/start", post(start_batch))
        .route("/:id/pause", post(pause_batch))
        .route("/:id/resume", post(resume_batch))
        .route("/:id/cancel", post(cancel_batch))
        .route("/:id/priority", patch(set_priority))
        .route("/:id/tasks", get(list_batch_tasks))
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListBatchesQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    50
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchWithProgress {
    #[serde(flatten)]
    pub batch: AccountRegistrationBatch,
    pub progress: AccountRegistrationBatchProgress,
}

async fn list_batches(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Query(query): Query<ListBatchesQuery>,
) -> AppResult<Json<Vec<BatchWithProgress>>> {
    let batches = store::account_registration::list_batches(&state.pool, query.limit).await?;
    let mut list = Vec::with_capacity(batches.len());

    for b in batches {
        let progress = store::account_registration::batch_progress(&state.pool, b.id).await?;
        list.push(BatchWithProgress { batch: b, progress });
    }

    Ok(Json(list))
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateBatchRequest {
    pub name: String,
    pub source_file: Option<String>,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub account_ids: Vec<Uuid>,
    /// 是否将全部未入队的待注册账号自动打包入本批次
    #[serde(default)]
    pub include_all_pending: bool,
    /// 创建后是否立即启动注册
    #[serde(default)]
    pub start_immediately: bool,
}

async fn create_batch(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Json(req): Json<CreateBatchRequest>,
) -> AppResult<(StatusCode, Json<AccountRegistrationBatch>)> {
    auth.require_super_admin()?;

    if req.name.trim().is_empty() {
        return Err(AppError::bad("批次名称不能为空"));
    }

    let mut tx = state.pool.begin().await?;

    let batch = store::account_registration::create_batch(
        &mut *tx,
        &store::account_registration::NewAccountRegistrationBatch {
            name: req.name.trim().to_string(),
            source_file: req.source_file,
            priority: req.priority,
            created_by: Some(auth.id),
        },
    )
    .await?;

    let mut target_account_ids = req.account_ids;
    if req.include_all_pending {
        let pending_ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT a.id FROM accounts a \
             WHERE a.status = '待注册' \
             AND NOT EXISTS ( \
                 SELECT 1 FROM account_registration_tasks t \
                 WHERE t.account_id = a.id AND t.status IN ('待认领', '执行中', '成功') \
             )",
        )
        .fetch_all(&mut *tx)
        .await?;
        for pid in pending_ids {
            if !target_account_ids.contains(&pid) {
                target_account_ids.push(pid);
            }
        }
    }

    for acc_id in target_account_ids {
        store::account_registration::create_task(&mut *tx, batch.id, acc_id, req.priority).await?;
    }

    if req.start_immediately {
        store::account_registration::update_batch_status(
            &mut *tx,
            batch.id,
            BatchStatus::NotStarted,
            BatchStatus::Running,
        )
        .await?;
    }

    tx.commit().await?;

    if req.start_immediately {
        let _ = scheduler::trigger_scheduler_sweep(&state).await;
    }

    state.events.publish(
        "账号注册批次变更",
        serde_json::json!({ "批次": batch.id, "动作": "创建" }),
    );

    let created = store::account_registration::get_batch(&state.pool, batch.id).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn get_batch(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<BatchWithProgress>> {
    let batch = store::account_registration::get_batch(&state.pool, id).await?;
    let progress = store::account_registration::batch_progress(&state.pool, id).await?;

    Ok(Json(BatchWithProgress { batch, progress }))
}

async fn start_batch(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<AccountRegistrationBatch>> {
    auth.require_super_admin()?;

    store::account_registration::update_batch_status(
        &state.pool,
        id,
        BatchStatus::NotStarted,
        BatchStatus::Running,
    )
    .await?;

    let _ = scheduler::trigger_scheduler_sweep(&state).await;

    state.events.publish(
        "账号注册批次变更",
        serde_json::json!({ "批次": id, "状态": "执行中" }),
    );

    let batch = store::account_registration::get_batch(&state.pool, id).await?;
    Ok(Json(batch))
}

async fn pause_batch(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<AccountRegistrationBatch>> {
    auth.require_super_admin()?;

    store::account_registration::update_batch_status(
        &state.pool,
        id,
        BatchStatus::Running,
        BatchStatus::Paused,
    )
    .await?;

    state.events.publish(
        "账号注册批次变更",
        serde_json::json!({ "批次": id, "状态": "已暂停" }),
    );

    let batch = store::account_registration::get_batch(&state.pool, id).await?;
    Ok(Json(batch))
}

async fn resume_batch(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<AccountRegistrationBatch>> {
    auth.require_super_admin()?;

    store::account_registration::update_batch_status(
        &state.pool,
        id,
        BatchStatus::Paused,
        BatchStatus::Running,
    )
    .await?;

    let _ = scheduler::trigger_scheduler_sweep(&state).await;

    state.events.publish(
        "账号注册批次变更",
        serde_json::json!({ "批次": id, "状态": "执行中" }),
    );

    let batch = store::account_registration::get_batch(&state.pool, id).await?;
    Ok(Json(batch))
}

async fn cancel_batch(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<AccountRegistrationBatch>> {
    auth.require_super_admin()?;

    let mut tx = state.pool.begin().await?;

    let batch = store::account_registration::get_batch(&mut *tx, id).await?;
    let status = batch.status.parse::<BatchStatus>()?;
    if status == BatchStatus::Completed || status == BatchStatus::Cancelled {
        return Err(AppError::conflict("批次已完成或已取消，无法再次取消"));
    }

    sqlx::query(
        "UPDATE account_registration_batches SET status = '已取消', updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;

    // 取消未开始的任务
    sqlx::query(
        "UPDATE account_registration_tasks SET status = '已取消', cancel_requested = TRUE, updated_at = now() \
         WHERE batch_id = $1 AND status = '待处理'",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;

    // 标记进行中的任务请求取消
    sqlx::query(
        "UPDATE account_registration_tasks SET cancel_requested = TRUE, updated_at = now() \
         WHERE batch_id = $1 AND status IN ('已分配', '执行中', '等待人工确认', '正在重试')",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    state.events.publish(
        "账号注册批次变更",
        serde_json::json!({ "批次": id, "状态": "已取消" }),
    );

    let batch = store::account_registration::get_batch(&state.pool, id).await?;
    Ok(Json(batch))
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetPriorityRequest {
    pub priority: i32,
}

async fn set_priority(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
    Json(req): Json<SetPriorityRequest>,
) -> AppResult<Json<AccountRegistrationBatch>> {
    auth.require_super_admin()?;

    store::account_registration::update_batch_priority(&state.pool, id, req.priority).await?;

    let batch = store::account_registration::get_batch(&state.pool, id).await?;
    Ok(Json(batch))
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListTasksQuery {
    pub status: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

async fn list_batch_tasks(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
    Query(query): Query<ListTasksQuery>,
) -> AppResult<Json<Vec<AccountRegistrationTask>>> {
    let tasks = store::account_registration::list_tasks_by_batch(
        &state.pool,
        id,
        query.status.as_deref(),
        query.limit,
        query.offset,
    )
    .await?;

    Ok(Json(tasks))
}
