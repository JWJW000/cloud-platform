//! 出版社管理 REST API 接口。

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use crate::api::auth::AuthenticatedUser;
use crate::error::AppResult;
use crate::state::AppState;
use crate::store::catalog_v1::EditionSearchItem;
use crate::store::publishers::{
    add_publisher_alias, get_or_create_publisher, get_publisher_detail, list_publisher_editions,
    list_publishers, merge_publishers, sync_publishers_from_editions, update_publisher,
    PublisherAliasRow, PublisherDetail, PublisherListParams, PublisherListResponse, PublisherRow,
};

/// 创建出版社请求。
#[derive(Debug, Deserialize)]
pub struct CreatePublisherRequest {
    pub name: String,
    pub country: Option<String>,
}

/// 更新出版社请求。
#[derive(Debug, Deserialize)]
pub struct UpdatePublisherRequest {
    pub name: String,
    pub country: Option<String>,
    pub website: Option<String>,
    pub description: Option<String>,
}

/// 添加别名请求。
#[derive(Debug, Deserialize)]
pub struct AddAliasRequest {
    pub alias_name: String,
}

/// 合并出版社请求。
#[derive(Debug, Deserialize)]
pub struct MergePublishersRequest {
    pub source_id: Uuid,
    pub target_id: Uuid,
}

/// 出版社专属书目查询参数。
#[derive(Debug, Deserialize)]
pub struct PublisherEditionsQuery {
    pub status: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// 出版社专属书目分页响应。
#[derive(Debug, serde::Serialize)]
pub struct PublisherEditionsResponse {
    pub items: Vec<EditionSearchItem>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

/// GET /api/publishers
pub async fn list_publishers_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Query(params): Query<PublisherListParams>,
) -> AppResult<Json<PublisherListResponse>> {
    let res = list_publishers(&state.pool, &params).await?;
    Ok(Json(res))
}

/// POST /api/publishers
pub async fn create_publisher_handler(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Json(req): Json<CreatePublisherRequest>,
) -> AppResult<Json<PublisherRow>> {
    auth.require_write()?;
    let row = get_or_create_publisher(&state.pool, &req.name, req.country.as_deref()).await?;
    Ok(Json(row))
}

/// GET /api/publishers/:id
pub async fn get_publisher_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<PublisherDetail>> {
    let detail = get_publisher_detail(&state.pool, id).await?;
    Ok(Json(detail))
}

/// PUT /api/publishers/:id
pub async fn update_publisher_handler(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdatePublisherRequest>,
) -> AppResult<Json<PublisherRow>> {
    auth.require_write()?;
    let row = update_publisher(
        &state.pool,
        id,
        &req.name,
        req.country.as_deref(),
        req.website.as_deref(),
        req.description.as_deref(),
    )
    .await?;
    Ok(Json(row))
}

/// POST /api/publishers/:id/aliases
pub async fn add_alias_handler(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
    Json(req): Json<AddAliasRequest>,
) -> AppResult<Json<PublisherAliasRow>> {
    auth.require_write()?;
    let alias = add_publisher_alias(&state.pool, id, &req.alias_name).await?;
    Ok(Json(alias))
}

/// POST /api/publishers/merge
pub async fn merge_publishers_handler(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Json(req): Json<MergePublishersRequest>,
) -> AppResult<Json<serde_json::Value>> {
    auth.require_write()?;
    merge_publishers(&state.pool, req.source_id, req.target_id).await?;
    Ok(Json(serde_json::json!({
        "success": true,
        "message": "出版社已成功合并"
    })))
}

/// GET /api/publishers/:id/editions
pub async fn list_publisher_editions_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
    Query(query): Query<PublisherEditionsQuery>,
) -> AppResult<Json<PublisherEditionsResponse>> {
    let limit = query.limit.unwrap_or(20);
    let offset = query.offset.unwrap_or(0);
    let (items, total) =
        list_publisher_editions(&state.pool, id, query.status.as_deref(), limit, offset).await?;

    Ok(Json(PublisherEditionsResponse {
        items,
        total,
        limit,
        offset,
    }))
}

/// POST /api/publishers/sync-from-editions
pub async fn sync_publishers_handler(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
) -> AppResult<Json<serde_json::Value>> {
    auth.require_super_admin()?;
    let inserted = sync_publishers_from_editions(&state.pool).await?;
    Ok(Json(serde_json::json!({
        "success": true,
        "message": format!("已成功从图书总库中同步初始化 {inserted} 家出版社主档")
    })))
}
