//! 馆藏扫描与审核 REST 管理接口（方案第 10 节）。

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    api::auth::AuthenticatedUser,
    catalog::{confirm_inventory_review, recompute_acquisition_state},
    error::{AppError, AppResult},
    state::AppState,
    store::inventory::{
        create_scan_job, get_scan_job, list_pending_reviews, list_scan_jobs,
        list_storage_locations, update_scan_job_status,
    },
};

/// 馆藏扫描路由组。
pub fn inventory_routes() -> Router<AppState> {
    Router::new()
        .route("/storage-locations", get(handle_list_storage_locations))
        .route(
            "/inventory/scans",
            get(handle_list_scans).post(handle_create_scan),
        )
        .route("/inventory/scans/:id", get(handle_get_scan))
        .route("/inventory/scans/:id/cancel", post(handle_cancel_scan))
        .route("/inventory/reviews", get(handle_list_reviews))
        .route(
            "/inventory/reviews/:id/confirm",
            post(handle_confirm_review),
        )
        .route("/inventory/reviews/:id/ignore", post(handle_ignore_review))
        .route(
            "/inventory/recompute/:edition_id",
            post(handle_recompute_state),
        )
}

async fn handle_list_storage_locations(
    State(state): State<AppState>,
    _user: AuthenticatedUser,
) -> AppResult<Json<serde_json::Value>> {
    let list = list_storage_locations(&state.pool)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(Json(serde_json::json!({
        "success": true,
        "locations": list,
    })))
}

#[derive(Debug, Deserialize)]
pub struct CreateScanRequest {
    pub node_id: Uuid,
    pub storage_location_id: Uuid,
    pub scan_mode: String,
}

async fn handle_create_scan(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(req): Json<CreateScanRequest>,
) -> AppResult<Json<serde_json::Value>> {
    user.require_write()?;
    let mode = match req.scan_mode.as_str() {
        "全量复核" => "全量复核",
        _ => "增量",
    };

    let job = create_scan_job(
        &state.pool,
        req.node_id,
        req.storage_location_id,
        mode,
        Some(user.id),
    )
    .await
    .map_err(|e| AppError::bad(format!("创建扫描任务失败: {e}")))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "job": job,
    })))
}

async fn handle_list_scans(
    State(state): State<AppState>,
    _user: AuthenticatedUser,
) -> AppResult<Json<serde_json::Value>> {
    let list = list_scan_jobs(&state.pool)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(Json(serde_json::json!({
        "success": true,
        "jobs": list,
    })))
}

async fn handle_get_scan(
    State(state): State<AppState>,
    _user: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    let job = get_scan_job(&state.pool, id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .ok_or_else(|| AppError::NotFound("扫描任务不存在".to_string()))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "job": job,
    })))
}

async fn handle_cancel_scan(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    user.require_write()?;
    update_scan_job_status(&state.pool, id, "已取消", Some("管理员手动取消"))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "扫描任务已取消",
    })))
}

async fn handle_list_reviews(
    State(state): State<AppState>,
    _user: AuthenticatedUser,
) -> AppResult<Json<serde_json::Value>> {
    let list = list_pending_reviews(&state.pool, 100)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(Json(serde_json::json!({
        "success": true,
        "reviews": list,
    })))
}

#[derive(Debug, Deserialize)]
pub struct ConfirmReviewRequest {
    pub edition_id: Uuid,
}

async fn handle_confirm_review(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(id): Path<Uuid>,
    Json(req): Json<ConfirmReviewRequest>,
) -> AppResult<Json<serde_json::Value>> {
    user.require_super_admin()?;
    confirm_inventory_review(&state.pool, id, req.edition_id)
        .await
        .map_err(|e| AppError::bad(format!("确认候选失败: {e}")))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "馆藏匹配已成功确认并关联",
    })))
}

async fn handle_ignore_review(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    user.require_super_admin()?;
    sqlx::query(
        "UPDATE inventory_scan_entries SET resolution_status = '已忽略', updated_at = now() WHERE id = $1"
    )
    .bind(id)
    .execute(&state.pool)
    .await
    .map_err(|e: sqlx::Error| AppError::internal(e.to_string()))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "已标记忽略该条目",
    })))
}

async fn handle_recompute_state(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(edition_id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    user.require_super_admin()?;
    let status = recompute_acquisition_state(&state.pool, edition_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "status": status,
    })))
}
