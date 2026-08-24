//! 图书主数据与已入库文件管理接口（第 16.3 节）。

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use crate::api::auth::AuthenticatedUser;
use crate::error::AppResult;
use crate::models::{Book, BookFile};
use crate::state::AppState;
use crate::store;

#[derive(Debug, Deserialize)]
pub struct BookListQuery {
    pub keyword: Option<String>,
    pub verify_status: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    50
}

#[derive(Debug, Deserialize)]
pub struct MergeBookRequest {
    /// 合并目标图书编号（保留的正本）。
    pub target_book_id: Uuid,
}

/// GET /api/books
pub async fn list_books(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Query(query): Query<BookListQuery>,
) -> AppResult<Json<Vec<Book>>> {
    let books = store::catalog::list_books(
        &state.pool,
        query.keyword.as_deref(),
        query.verify_status.as_deref(),
        query.limit,
        query.offset,
    )
    .await?;
    Ok(Json(books))
}

/// GET /api/books/:id
pub async fn get_book(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Book>> {
    let book = store::catalog::get_book(&state.pool, id).await?;
    Ok(Json(book))
}

/// GET /api/books/:id/files
pub async fn list_book_files(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Vec<BookFile>>> {
    let files = store::catalog::list_book_files(&state.pool, id).await?;
    Ok(Json(files))
}

/// POST /api/books/:id/confirm
pub async fn confirm_book(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Book>> {
    auth.require_write()?;
    let book = store::catalog::confirm_book(&state.pool, id).await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "人工确认图书",
        &id.to_string(),
        &format!("《{}》已人工确认", book.raw_title),
    )
    .await?;

    Ok(Json(book))
}

/// POST /api/books/:id/merge
pub async fn merge_books(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
    Json(req): Json<MergeBookRequest>,
) -> AppResult<Json<serde_json::Value>> {
    auth.require_write()?;
    store::catalog::merge_books(&state.pool, id, req.target_book_id).await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "合并图书",
        &id.to_string(),
        &format!("将图书 {id} 合并入 {}", req.target_book_id),
    )
    .await?;

    Ok(Json(serde_json::json!({ "message": "图书合并完成" })))
}
