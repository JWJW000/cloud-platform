//! 图书馆总库与索引 REST API 接口（第 16 节）。

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use crate::api::auth::AuthenticatedUser;
use crate::catalog::acquisition::{
    claim_acquisition_task, report_acquisition_task, retry_acquisition_target,
    set_acquisition_priority, AcquisitionAssignment, AcquisitionReportRequest,
    WorkerClaimRequest,
};
use crate::catalog::ingestion::{
    execute_import, preview_import, ImportExecutionResult, ImportManifestRequest,
    ImportPreviewResult, StartImportRequest,
};
use crate::catalog::outbox::process_outbox_events;
use crate::catalog::search::{
    get_catalog_edition_detail, search_catalog, CatalogSearchParams, CatalogSearchResponse,
};
use crate::catalog::storage::{
    commit_library_file, CommitLibraryFileRequest, CommitLibraryFileResult,
};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::store::catalog_v1::{
    get_catalog_stats, get_import_run, get_or_create_source, list_import_runs,
    list_quarantined_records, list_sources, CatalogSourceRow, CatalogStats,
    EditionDetail, ImportRunRow, QuarantinedRecordRow,
};

/// 创建数据源请求。
#[derive(Debug, Deserialize)]
pub struct CreateSourceRequest {
    /// 来源名称。
    pub name: String,
    /// 来源类型。
    pub source_type: Option<String>,
    /// 描述。
    pub description: Option<String>,
    /// 优先级。
    pub priority: Option<i32>,
}

/// 调整优先级请求。
#[derive(Debug, Deserialize)]
pub struct UpdatePriorityRequest {
    /// 优先级。
    pub priority: i32,
}

/// 合并作品请求。
#[derive(Debug, Deserialize)]
pub struct MergeWorksRequest {
    /// 源作品编号（被合并）。
    pub source_work_id: Uuid,
    /// 目标作品编号（保留的正本）。
    pub target_work_id: Uuid,
}

/// 隔离记录解决请求。
#[derive(Debug, Deserialize)]
pub struct ResolveQuarantineRequest {
    /// 修正后的书名。
    pub corrected_title: Option<String>,
    /// 修正后的作者。
    pub corrected_author: Option<String>,
    /// 修正后的出版社。
    pub corrected_publisher: Option<String>,
    /// 修正后的 ISBN。
    pub corrected_isbn: Option<String>,
}

/// GET /api/catalog/stats
pub async fn get_stats(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
) -> AppResult<Json<CatalogStats>> {
    let stats = get_catalog_stats(&state.pool).await?;
    Ok(Json(stats))
}

/// GET /api/catalog/search 与 GET /api/catalog/editions
pub async fn search_editions_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Query(params): Query<CatalogSearchParams>,
) -> AppResult<Json<CatalogSearchResponse>> {
    let res = search_catalog(&state.pool, &params).await?;
    Ok(Json(res))
}

/// GET /api/catalog/editions/:id
pub async fn get_edition_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<EditionDetail>> {
    let detail = get_catalog_edition_detail(&state.pool, id).await?;
    Ok(Json(detail))
}

/// GET /api/catalog/sources
pub async fn list_sources_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
) -> AppResult<Json<Vec<CatalogSourceRow>>> {
    let sources = list_sources(&state.pool).await?;
    Ok(Json(sources))
}

/// POST /api/catalog/sources
pub async fn create_source_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Json(req): Json<CreateSourceRequest>,
) -> AppResult<Json<CatalogSourceRow>> {
    let source = get_or_create_source(
        &state.pool,
        &req.name,
        req.source_type.as_deref().unwrap_or("excel"),
        req.description.as_deref(),
        req.priority.unwrap_or(0),
    )
    .await?;
    Ok(Json(source))
}

/// POST /api/catalog/imports/preview
pub async fn preview_import_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Json(req): Json<ImportManifestRequest>,
) -> AppResult<Json<ImportPreviewResult>> {
    let preview = preview_import(&state.pool, &req).await?;
    Ok(Json(preview))
}

/// POST /api/catalog/imports/submit
pub async fn submit_import_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Json(req): Json<StartImportRequest>,
) -> AppResult<Json<ImportExecutionResult>> {
    let result = execute_import(&state.pool, &req).await?;
    Ok(Json(result))
}

/// GET /api/catalog/imports/runs
pub async fn list_import_runs_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
) -> AppResult<Json<Vec<ImportRunRow>>> {
    let runs = list_import_runs(&state.pool, 100).await?;
    Ok(Json(runs))
}

/// GET /api/catalog/imports/runs/:id
pub async fn get_import_run_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<ImportRunRow>> {
    let run = get_import_run(&state.pool, id).await?;
    Ok(Json(run))
}

/// GET /api/catalog/imports/quarantine
pub async fn list_quarantined_records_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
) -> AppResult<Json<Vec<QuarantinedRecordRow>>> {
    let records = list_quarantined_records(&state.pool, None, 100, 0).await?;
    Ok(Json(records))
}

/// POST /api/catalog/imports/quarantine/:id/resolve
pub async fn resolve_quarantine_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
    Json(req): Json<ResolveQuarantineRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let mut tx = state.pool.begin().await?;

    let q_row: Option<QuarantinedRecordRow> = sqlx::query_as(
        "SELECT id, import_run_id, import_file_id, sheet_name, row_number, raw_content, error_reason, resolved, resolved_at, created_at \
         FROM quarantined_records WHERE id = $1"
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;

    let Some(quarantine) = q_row else {
        return Err(AppError::missing("隔离记录不存在"));
    };

    let title = req.corrected_title.unwrap_or_else(|| "未命名书目".to_string());
    let source_id: Uuid = sqlx::query_scalar(
        "SELECT source_id FROM import_files WHERE id = $1"
    )
    .bind(quarantine.import_file_id)
    .fetch_one(&mut *tx)
    .await?;

    let item = crate::catalog::resolution::ParsedCatalogItem {
        raw_title: title,
        raw_author: req.corrected_author,
        raw_publisher: req.corrected_publisher,
        raw_isbn: req.corrected_isbn,
        raw_payload: quarantine.raw_content,
        ..Default::default()
    };

    let source_record_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO source_records \
             (id, source_id, import_file_id, sheet_name, row_number, raw_payload, normalized_title, normalized_author, normalized_publisher, raw_isbn) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
    )
    .bind(source_record_id)
    .bind(source_id)
    .bind(quarantine.import_file_id)
    .bind(&quarantine.sheet_name)
    .bind(quarantine.row_number)
    .bind(&item.raw_payload)
    .bind(&item.raw_title)
    .bind(&item.raw_author)
    .bind(&item.raw_publisher)
    .bind(&item.raw_isbn)
    .execute(&mut *tx)
    .await?;

    let res = crate::catalog::resolution::resolve_item(&mut tx, source_id, source_record_id, &item).await?;

    sqlx::query(
        "UPDATE quarantined_records SET resolved = TRUE, resolved_at = now() WHERE id = $1"
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(Json(serde_json::json!({
        "success": true,
        "work_id": res.work_id,
        "edition_id": res.edition_id,
    })))
}

/// GET /api/catalog/acquisitions
pub async fn list_acquisitions_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Query(params): Query<CatalogSearchParams>,
) -> AppResult<Json<CatalogSearchResponse>> {
    let res = search_catalog(&state.pool, &params).await?;
    Ok(Json(res))
}

/// POST /api/catalog/acquisitions/:id/retry
pub async fn retry_acquisition_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    retry_acquisition_target(&state.pool, id).await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// POST /api/catalog/acquisitions/:id/priority
pub async fn update_acquisition_priority_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdatePriorityRequest>,
) -> AppResult<Json<serde_json::Value>> {
    set_acquisition_priority(&state.pool, id, req.priority).await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// POST /api/catalog/acquisitions/claim
pub async fn claim_acquisition_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Json(req): Json<WorkerClaimRequest>,
) -> AppResult<Json<Option<AcquisitionAssignment>>> {
    let assignment = claim_acquisition_task(&state.pool, &req, 300).await?;
    Ok(Json(assignment))
}

/// POST /api/catalog/acquisitions/report
pub async fn report_acquisition_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Json(req): Json<AcquisitionReportRequest>,
) -> AppResult<Json<serde_json::Value>> {
    report_acquisition_task(&state.pool, &req).await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// POST /api/catalog/storage/commit
pub async fn commit_storage_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Json(req): Json<CommitLibraryFileRequest>,
) -> AppResult<Json<CommitLibraryFileResult>> {
    let result = commit_library_file(&state.pool, &req).await?;
    Ok(Json(result))
}

/// POST /api/catalog/resolutions/merge
pub async fn merge_works_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Json(req): Json<MergeWorksRequest>,
) -> AppResult<Json<serde_json::Value>> {
    if req.source_work_id == req.target_work_id {
        return Err(AppError::bad("不能合并相同作品"));
    }

    let mut tx = state.pool.begin().await?;

    // 将源作品下的全部版本转移到目标作品
    sqlx::query(
        "UPDATE editions SET work_id = $2, updated_at = now() WHERE work_id = $1"
    )
    .bind(req.source_work_id)
    .bind(req.target_work_id)
    .execute(&mut *tx)
    .await?;

    // 标记源作品为已合并
    sqlx::query(
        "UPDATE works SET resolution_status = '已合并', merged_into_work_id = $2, updated_at = now() WHERE id = $1"
    )
    .bind(req.source_work_id)
    .bind(req.target_work_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(Json(serde_json::json!({ "success": true })))
}

/// POST /api/catalog/outbox/process
pub async fn process_outbox_handler(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
) -> AppResult<Json<serde_json::Value>> {
    let processed = process_outbox_events(&state.pool, 100).await?;
    Ok(Json(serde_json::json!({ "processed": processed })))
}
