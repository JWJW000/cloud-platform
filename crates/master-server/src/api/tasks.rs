//! 图书下载任务管理接口（第 16.4 节）。

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use crate::api::auth::AuthenticatedUser;
use crate::error::{AppError, AppResult};
use crate::grpc::convert;
use crate::models::{BookTask, TaskExecution};
use crate::state::AppState;
use crate::store;

#[derive(Debug, Deserialize)]
pub struct TaskListQuery {
    pub status: Option<String>,
    pub batch_id: Option<Uuid>,
    pub node_id: Option<Uuid>,
    pub keyword: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    50
}

/// GET /api/tasks
pub async fn list_tasks(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Query(query): Query<TaskListQuery>,
) -> AppResult<Json<Vec<BookTask>>> {
    let filter = store::task::TaskFilter {
        status: query.status,
        batch_id: query.batch_id,
        node_id: query.node_id,
        keyword: query.keyword,
        limit: query.limit,
        offset: query.offset,
    };
    let tasks = store::task::list_tasks(&state.pool, &filter).await?;
    Ok(Json(tasks))
}

/// GET /api/tasks/:id
pub async fn get_task(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<BookTask>> {
    let task = store::task::get_task(&state.pool, id).await?;
    Ok(Json(task))
}

/// GET /api/tasks/:id/executions
pub async fn list_task_executions(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Vec<TaskExecution>>> {
    let executions = store::session::list_executions(&state.pool, id, 50).await?;
    Ok(Json(executions))
}

/// POST /api/tasks/:id/retry
pub async fn retry_task(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<BookTask>> {
    auth.require_write()?;
    let task = store::task::retry_task(&state.pool, id).await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "重试任务",
        &id.to_string(),
        &format!("重试《{}》下载任务", task.title),
    )
    .await?;

    state.events.publish(
        "任务变更",
        serde_json::json!({ "任务": id, "状态": "待处理" }),
    );

    Ok(Json(task))
}

/// POST /api/tasks/:id/cancel
pub async fn cancel_task(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    auth.require_write()?;
    let dispatch_target = store::task::cancel_task(&state.pool, id).await?;

    if let Some((target, stage_version)) = dispatch_target {
        // V4 精确取消：携带 node/session/task/execution/stage_version 全量字段
        let msg = convert::cancel_task_message(
            Some(target.node_id),
            Some(target.session_id),
            id,
            Some(target.execution_id),
            stage_version,
            &format!("管理员 {} 手工取消", auth.username),
        );
        state.links.try_dispatch(target.node_id, msg);
    }

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "取消任务",
        &id.to_string(),
        "管理员取消任务",
    )
    .await?;

    state.events.publish(
        "任务变更",
        serde_json::json!({ "任务": id, "状态": "已取消" }),
    );

    Ok(Json(serde_json::json!({ "message": "任务已取消" })))
}

/// GET /api/tasks/needs-confirm
pub async fn list_needs_confirm(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
) -> AppResult<Json<Vec<BookTask>>> {
    let tasks = store::task::list_needs_confirm(&state.pool, 100).await?;
    Ok(Json(tasks))
}

/// POST /api/tasks/:id/verify-nas
pub async fn trigger_nas_verify(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    auth.require_write()?;
    let task = store::task::get_task(&state.pool, id).await?;

    // R6（V4 第 12.3 节）：核验只发送**已固化**的期望字段，
    // 绝不发送空字符串让 Worker 解释为空表示跳过。
    type FrozenRow = (
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<String>,
        Option<String>,
    );
    let row: Option<FrozenRow> = sqlx::query_as(
        "SELECT expected_nas_relative_path, expected_file_name, expected_size_bytes, \
                    expected_sha256, expected_format \
             FROM book_tasks WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    let (path, file_name, size, sha256, format) = row
        .ok_or_else(|| AppError::missing("任务不存在"))
        .map(|(p, f, s, h, fmt)| {
            (
                p.unwrap_or_default(),
                f.unwrap_or_default(),
                s,
                h.unwrap_or_default(),
                fmt.unwrap_or_default(),
            )
        })?;
    if path.is_empty() {
        return Err(AppError::bad(
            "任务缺少已固化的期望 NAS 路径，无法核验（数据错误）",
        ));
    }
    let expected_format = if format.is_empty() {
        task.format.clone()
    } else {
        format
    };

    // 挑选一个当前在线且 NAS 健康的节点
    let online_nodes = state.links.online_nodes();
    if online_nodes.is_empty() {
        return Err(AppError::bad("当前没有在线的 Worker 节点执行核验"));
    }

    let mut dispatched = false;
    let resolved_file_name = if file_name.is_empty() {
        std::path::Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    } else {
        file_name.clone()
    };
    for node_id in online_nodes {
        let msg = convert::verify_nas_file_message(
            id,
            &path,
            sha256.as_str(),
            size.unwrap_or(0).max(0),
            &expected_format,
            &resolved_file_name,
        );
        if state.links.try_dispatch(node_id, msg) {
            dispatched = true;
            break;
        }
    }

    if !dispatched {
        return Err(AppError::bad("下发核验命令失败：节点不可达"));
    }

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "触发NAS核验",
        &id.to_string(),
        &format!("针对《{}》下发 NAS 核验", task.title),
    )
    .await?;

    Ok(Json(
        serde_json::json!({ "message": "已派发 NAS 核验指令" }),
    ))
}
