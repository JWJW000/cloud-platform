//! 批次与导入管理接口（第 16.3 节）。

use axum::extract::{Path, State};
use axum::Json;
use platform_domain::BatchStatus;
use platform_proto::v1 as pb;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::auth::AuthenticatedUser;
use crate::error::{AppError, AppResult};
use crate::grpc::convert;
use crate::models::{BatchProgress, DownloadBatch, ImportRow, ImportSummary};
use crate::state::AppState;
use crate::store;
use crate::store::task::BatchExportRow;

#[derive(Debug, Deserialize)]
pub struct ImportBatchRequest {
    /// 批次名称。
    pub batch_name: String,
    /// 来源文件名（可选）。
    pub source_file: Option<String>,
    /// 下载格式（pdf / epub）。
    #[serde(default = "default_format")]
    pub format: String,
    /// 优先级。
    #[serde(default)]
    pub priority: i32,
    /// 单任务最大重试次数。
    #[serde(default = "default_max_attempts")]
    pub max_attempts: i32,
    /// 导入图书列表。
    #[serde(default)]
    pub rows: Vec<ImportRow>,
    /// CSV 文本内容（与 rows 二选一，若 rows 为空则解析 csv_text）。
    pub csv_text: Option<String>,
}

fn default_format() -> String {
    "pdf".to_string()
}

fn default_max_attempts() -> i32 {
    3
}

#[derive(Debug, Deserialize)]
pub struct UpdatePriorityRequest {
    pub priority: i32,
}

#[derive(Debug, Deserialize)]
pub struct UpdateGlobalDownloadControlRequest {
    pub paused: bool,
}

#[derive(Debug, Serialize)]
pub struct GlobalDownloadControlResponse {
    pub paused: bool,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub running_tasks: i64,
}

/// GET /api/download-control
pub async fn get_global_download_control(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
) -> AppResult<Json<GlobalDownloadControlResponse>> {
    let control = crate::scheduler::get_global_download_control(&state.pool).await?;
    Ok(Json(global_control_response(&state, control).await?))
}

/// PUT /api/download-control
pub async fn update_global_download_control(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Json(req): Json<UpdateGlobalDownloadControlRequest>,
) -> AppResult<Json<GlobalDownloadControlResponse>> {
    auth.require_write()?;
    let control = crate::scheduler::set_global_download_paused(&state.pool, req.paused).await?;

    let action = if req.paused {
        "全局暂停下载"
    } else {
        "恢复全局下载"
    };
    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Warn,
        &auth.username,
        action,
        "global-download-control",
        if req.paused {
            "已停止派发新的图书下载任务；执行中任务继续安全收尾"
        } else {
            "已恢复图书下载任务派发"
        },
    )
    .await?;

    if !req.paused {
        let _ = crate::scheduler::trigger_scheduler_sweep(&state).await;
    }
    state.events.publish(
        "全局下载控制变更",
        serde_json::json!({ "已暂停": req.paused, "操作人": auth.username }),
    );
    Ok(Json(global_control_response(&state, control).await?))
}

async fn global_control_response(
    state: &AppState,
    control: crate::scheduler::GlobalDownloadControl,
) -> AppResult<GlobalDownloadControlResponse> {
    let running_tasks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM book_tasks WHERE status IN ('已分配', '执行中', '等待入库')",
    )
    .fetch_one(&state.pool)
    .await?;
    Ok(GlobalDownloadControlResponse {
        paused: control.paused,
        updated_at: control.updated_at,
        running_tasks,
    })
}

/// GET /api/batches
pub async fn list_batches(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
) -> AppResult<Json<Vec<DownloadBatch>>> {
    let batches = store::catalog::list_batches(&state.pool).await?;
    Ok(Json(batches))
}

/// GET /api/batches/:id
pub async fn get_batch(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<DownloadBatch>> {
    let batch = store::catalog::get_batch(&state.pool, id).await?;
    Ok(Json(batch))
}

/// GET /api/batches/:id/progress
pub async fn get_batch_progress(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<BatchProgress>> {
    let progress = store::catalog::batch_progress(&state.pool, id).await?;
    Ok(Json(progress))
}

/// POST /api/batches/import
pub async fn import_batch(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Json(mut req): Json<ImportBatchRequest>,
) -> AppResult<Json<ImportSummary>> {
    auth.require_write()?;

    if req.batch_name.trim().is_empty() {
        return Err(AppError::bad("批次名称不能为空"));
    }

    let rows = if !req.rows.is_empty() {
        req.rows
    } else if let Some(csv_str) = req.csv_text.take() {
        parse_csv_rows(&csv_str)?
    } else {
        return Err(AppError::bad("导入列表不能为空"));
    };

    if rows.is_empty() {
        return Err(AppError::bad("有效图书行数为零"));
    }

    let import_req = store::catalog::ImportRequest {
        batch_name: req.batch_name.trim(),
        source_file: req.source_file.as_deref(),
        format: req.format.trim(),
        priority: req.priority,
        created_by: Some(auth.id),
        max_attempts: req.max_attempts,
    };

    let summary = store::catalog::import_books(&state.pool, &import_req, &rows).await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "导入批次",
        &summary
            .batch_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
        &format!(
            "批次《{}》导入：总行数 {}，新建 {}，去重 {}，已有文件 {}",
            req.batch_name,
            summary.total_rows,
            summary.new_books,
            summary.deduplicated,
            summary.already_ingested
        ),
    )
    .await?;

    state.events.publish(
        "批次变更",
        serde_json::json!({
            "动作": "导入",
            "批次": summary.batch_id,
        }),
    );

    Ok(Json(summary))
}

/// POST /api/batches/:id/start
pub async fn start_batch(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<DownloadBatch>> {
    auth.require_write()?;
    let batch = store::catalog::set_batch_status(&state.pool, id, BatchStatus::Running).await?;

    let _ = crate::scheduler::trigger_scheduler_sweep(&state).await;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "开始批次",
        &id.to_string(),
        &format!("批次《{}》已开始执行", batch.name),
    )
    .await?;

    state.events.publish(
        "批次变更",
        serde_json::json!({ "批次": id, "状态": "执行中" }),
    );

    Ok(Json(batch))
}

/// POST /api/batches/:id/pause
pub async fn pause_batch(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<DownloadBatch>> {
    auth.require_write()?;
    let batch = store::catalog::set_batch_status(&state.pool, id, BatchStatus::Paused).await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "暂停批次",
        &id.to_string(),
        &format!("批次《{}》已暂停", batch.name),
    )
    .await?;

    state.events.publish(
        "批次变更",
        serde_json::json!({ "批次": id, "状态": "已暂停" }),
    );

    Ok(Json(batch))
}

/// POST /api/batches/:id/resume
pub async fn resume_batch(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<DownloadBatch>> {
    auth.require_write()?;
    let batch = store::catalog::set_batch_status(&state.pool, id, BatchStatus::Running).await?;

    let _ = crate::scheduler::trigger_scheduler_sweep(&state).await;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "恢复批次",
        &id.to_string(),
        &format!("批次《{}》已恢复执行", batch.name),
    )
    .await?;

    state.events.publish(
        "批次变更",
        serde_json::json!({ "批次": id, "状态": "执行中" }),
    );

    Ok(Json(batch))
}

/// POST /api/batches/:id/cancel
pub async fn cancel_batch(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<DownloadBatch>> {
    auth.require_write()?;
    let (batch, outcome) = store::task::cancel_batch(&state.pool, id).await?;

    // 事务提交后，向正在运行且在线的 Worker 精确下发 CancelTask（V4 精确取消）
    for target in &outcome.running_targets {
        let msg = pb::MasterMessage::new(
            convert::now_rfc3339(),
            pb::master_message::Payload::CancelTask(pb::CancelTask {
                node_id: target.node_id.to_string(),
                session_id: target.session_id.to_string(),
                task_id: target.task_id.to_string(),
                execution_id: target.execution_id.to_string(),
                stage_version: target.stage_version.max(0) as u32,
                reason: format!("批次《{}》已被管理员取消", batch.name),
            }),
        );
        state.links.try_dispatch(target.node_id, msg);
    }

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Warn,
        &auth.username,
        "取消批次",
        &id.to_string(),
        &format!(
            "批次《{}》已取消：直接取消 {} 个待处理任务，通知中断 {} 个进行中任务，保留 {} 个共享任务",
            batch.name,
            outcome.directly_cancelled_task_ids.len(),
            outcome.running_targets.len(),
            outcome.shared_task_ids.len()
        ),
    )
    .await?;

    state.events.publish(
        "批次变更",
        serde_json::json!({ "批次": id, "状态": "已取消" }),
    );

    Ok(Json(batch))
}

/// PUT /api/batches/:id/priority
pub async fn update_batch_priority(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdatePriorityRequest>,
) -> AppResult<Json<DownloadBatch>> {
    auth.require_write()?;
    let batch = store::catalog::set_batch_priority(&state.pool, id, req.priority).await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "调整批次优先级",
        &id.to_string(),
        &format!("批次《{}》优先级变更为 {}", batch.name, req.priority),
    )
    .await?;

    state.events.publish(
        "批次变更",
        serde_json::json!({ "批次": id, "优先级": req.priority }),
    );

    Ok(Json(batch))
}

/// POST /api/batches/:id/retry-failed
pub async fn retry_failed_tasks(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    auth.require_write()?;
    let count = store::task::retry_failed_in_batch(&state.pool, id).await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "重试批次失败任务",
        &id.to_string(),
        &format!("重试了 {count} 个任务"),
    )
    .await?;

    state.events.publish(
        "批次变更",
        serde_json::json!({ "批次": id, "重试任务数": count }),
    );

    Ok(Json(serde_json::json!({ "retried_count": count })))
}

/// GET /api/batches/:id/export
pub async fn export_batch(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Vec<BatchExportRow>>> {
    let rows = store::task::export_batch(&state.pool, id).await?;
    Ok(Json(rows))
}

/// 从 CSV 字符串解析出图书行。
///
/// 兼容两种格式（本地联调发现：前端直接粘贴「书名,作者,出版社,ISBN」行，
/// 无表头，而旧实现 `has_headers(true)` 会把第一本书当成表头丢掉）：
/// - 带头行：`书名,作者,出版社,ISBN`（列名可含这些关键字，顺序不限）；
/// - 无头行：按位置 0..4 映射 书名/作者/出版社/ISBN。
fn parse_csv_rows(csv_text: &str) -> AppResult<Vec<ImportRow>> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(csv_text.as_bytes());

    let mut rows = Vec::new();
    let mut header_indices: Option<[Option<usize>; 4]> = None; // 书名/作者/出版社/ISBN

    for result in reader.records() {
        let record = result.map_err(|e| AppError::bad(format!("CSV 行读取失败：{e}")))?;
        let cells: Vec<String> = record.iter().map(|c| c.trim().to_string()).collect();
        if cells.is_empty() || cells.iter().all(|c| c.is_empty()) {
            continue;
        }

        // 第一行：判断是否为表头，并决定列映射。
        // 表头判定只看**第一列**（书名列）：数据行的书名不会包含「书名」二字，
        // 而出版社名（如「机械工业出版社」）常含「出版社」，不能用来判表头。
        if header_indices.is_none() {
            let first = cells[0].to_ascii_lowercase();
            let looks_like_header = cells[0].contains("书名")
                || matches!(
                    first.as_str(),
                    "title" | "author" | "publisher" | "isbn" | "标题"
                );
            if looks_like_header {
                header_indices = Some([
                    cells
                        .iter()
                        .position(|c| c.contains("书名") || c.eq_ignore_ascii_case("title")),
                    cells
                        .iter()
                        .position(|c| c.contains("作者") || c.eq_ignore_ascii_case("author")),
                    cells
                        .iter()
                        .position(|c| c.contains("出版社") || c.eq_ignore_ascii_case("publisher")),
                    cells
                        .iter()
                        .position(|c| c.contains("ISBN") || c.eq_ignore_ascii_case("isbn")),
                ]);
                continue; // 表头不当作数据行
            }
            // 无表头：按位置映射
            header_indices = Some([
                Some(0),
                (cells.len() > 1).then_some(1),
                (cells.len() > 2).then_some(2),
                (cells.len() > 3).then_some(3),
            ]);
        }

        let indices = header_indices.unwrap();
        let title = indices[0]
            .and_then(|i| cells.get(i))
            .cloned()
            .unwrap_or_default();
        if title.is_empty() {
            continue;
        }
        rows.push(ImportRow {
            title,
            author: indices[1]
                .and_then(|i| cells.get(i))
                .cloned()
                .filter(|s| !s.is_empty()),
            publisher: indices[2]
                .and_then(|i| cells.get(i))
                .cloned()
                .filter(|s| !s.is_empty()),
            isbn: indices[3]
                .and_then(|i| cells.get(i))
                .cloned()
                .filter(|s| !s.is_empty()),
        });
    }

    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::parse_csv_rows;

    #[test]
    fn headerless_rows_are_all_parsed() {
        let csv = "算法导论,Thomas H. Cormen,机械工业出版社,9787111407010\n计算机网络,Andrew S. Tanenbaum,清华大学出版社,9787302165891";
        let rows = parse_csv_rows(csv).unwrap();
        assert_eq!(
            rows.len(),
            2,
            "两行数据都必须解析出来（不能把第一行当表头）"
        );
        assert_eq!(rows[0].title, "算法导论");
        assert_eq!(rows[1].title, "计算机网络");
    }

    #[test]
    fn header_row_is_recognized_and_skipped() {
        let csv = "书名,作者,出版社,ISBN\n三体,刘慈欣,重庆出版社,9787536692930";
        let rows = parse_csv_rows(csv).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "三体");
        assert_eq!(rows[0].isbn.as_deref(), Some("9787536692930"));
    }

    #[test]
    fn publisher_name_containing_出版社_is_not_mistaken_for_header() {
        // 回归：出版社名「机械工业出版社」含「出版社」字样，绝不能把数据行当表头
        let csv = "算法导论,Thomas H. Cormen,机械工业出版社,9787111407010\n计算机网络,Andrew S. Tanenbaum,清华大学出版社,9787302165891";
        let rows = parse_csv_rows(csv).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].publisher.as_deref(), Some("清华大学出版社"));
    }

    #[test]
    fn header_order_is_irrelevant() {
        let csv = "ISBN,书名,作者\n9787111407010,算法导论,Thomas H. Cormen";
        let rows = parse_csv_rows(csv).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "算法导论");
        assert_eq!(rows[0].isbn.as_deref(), Some("9787111407010"));
    }
}
