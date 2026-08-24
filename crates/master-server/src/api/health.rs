//! 健康检查接口（V4 方案第 16.2 节）。
//!
//! - `/health/live`：进程存活，不要求认证；
//! - `/health/ready`：数据库可访问、迁移全部完成、关键配置有效。
//!
//! 健康接口不得泄漏连接串、文件路径、证书内容或内部错误栈——
//! 失败时只返回状态码与「未就绪」摘要，细节进服务端日志。

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::state::AppState;

/// 存活探针：进程在跑就返回 200。
pub async fn live() -> Response {
    (StatusCode::OK, Json(json!({ "status": "ok" }))).into_response()
}

/// 就绪探针：数据库可访问、迁移全部应用、关键配置有效。
///
/// 任何一步失败返回 503，但响应体不携带具体细节（细节进日志），
/// 防止健康检查本身成为信息泄漏面。
pub async fn ready(State(state): State<AppState>) -> Response {
    match check_ready(&state).await {
        Ok(()) => (StatusCode::OK, Json(json!({ "status": "ready" }))).into_response(),
        Err(err) => {
            tracing::warn!(error = %err, "就绪检查未通过");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "status": "not_ready" })),
            )
                .into_response()
        }
    }
}

async fn check_ready(state: &AppState) -> anyhow::Result<()> {
    use anyhow::Context;
    // 1. 数据库可访问
    sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
        .context("数据库不可访问")?;

    // 2. 迁移全部应用：_sqlx_migrations 中不允许存在未成功记录
    let failed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success = FALSE")
            .fetch_one(&state.pool)
            .await
            .context("查询迁移状态失败")?;
    if failed > 0 {
        anyhow::bail!("存在未成功应用的数据库迁移");
    }
    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&state.pool)
        .await?;
    if applied == 0 {
        anyhow::bail!("数据库迁移记录为空，应用未完成初始化");
    }

    // 3. 关键配置有效：站点地址若是占位域名则视为未就绪（启动时也会拒绝）。
    let site = state.config.server.site_base.trim().to_ascii_lowercase();
    if site.contains(".invalid") || site.contains(".example") || site.contains("example.com") {
        anyhow::bail!("站点地址仍为占位域名，配置未完成");
    }

    Ok(())
}
