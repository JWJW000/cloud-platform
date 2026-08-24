//! 待确认事项存储（人工确认流程）。

use chrono::{DateTime, Utc};
use platform_domain::{ManualActionStatus, ManualActionType, TaskType};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::ManualAction;

const ACTION_COLUMNS: &str = "id, task_type, registration_task_id, book_task_id, execution_id, \
     node_id, session_id, action_type, prompt, status, artifact_url, input_code, expires_at, \
     resolved_at, resolved_by, created_at, updated_at";

/// 新建待确认事项。
#[derive(Debug, Clone)]
pub struct NewManualAction {
    /// 任务类型。
    pub task_type: TaskType,
    /// 注册任务编号。
    pub registration_task_id: Option<Uuid>,
    /// 图书任务编号。
    pub book_task_id: Option<Uuid>,
    /// 执行编号。
    pub execution_id: Option<Uuid>,
    /// 节点编号。
    pub node_id: Option<Uuid>,
    /// 会话编号。
    pub session_id: Option<Uuid>,
    /// 确认类型。
    pub action_type: ManualActionType,
    /// 说明提示。
    pub prompt: String,
    /// 截图证据。
    pub artifact_url: Option<String>,
    /// 过期时间。
    pub expires_at: DateTime<Utc>,
}

/// 创建待确认事项。
pub async fn create_action(
    executor: impl PgExecutor<'_>,
    new: &NewManualAction,
) -> AppResult<ManualAction> {
    let id = Uuid::new_v4();
    let action = sqlx::query_as::<_, ManualAction>(&format!(
        "INSERT INTO manual_actions \
             (id, task_type, registration_task_id, book_task_id, execution_id, node_id, \
              session_id, action_type, prompt, status, artifact_url, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
         RETURNING {ACTION_COLUMNS}"
    ))
    .bind(id)
    .bind(new.task_type.as_str())
    .bind(new.registration_task_id)
    .bind(new.book_task_id)
    .bind(new.execution_id)
    .bind(new.node_id)
    .bind(new.session_id)
    .bind(new.action_type.as_str())
    .bind(&new.prompt)
    .bind(ManualActionStatus::Pending.as_str())
    .bind(&new.artifact_url)
    .bind(new.expires_at)
    .fetch_one(executor)
    .await?;
    Ok(action)
}

/// 获取单个待确认事项。
pub async fn get_action(executor: impl PgExecutor<'_>, id: Uuid) -> AppResult<ManualAction> {
    let action = sqlx::query_as::<_, ManualAction>(&format!(
        "SELECT {ACTION_COLUMNS} FROM manual_actions WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::missing("待确认事项不存在"))?;
    Ok(action)
}

/// 列出待确认事项。
pub async fn list_actions(
    executor: impl PgExecutor<'_>,
    status: Option<&str>,
    limit: i64,
) -> AppResult<Vec<ManualAction>> {
    let actions = sqlx::query_as::<_, ManualAction>(&format!(
        "SELECT {ACTION_COLUMNS} FROM manual_actions \
         WHERE ($1::text IS NULL OR status = $1) \
         ORDER BY created_at DESC LIMIT $2"
    ))
    .bind(status)
    .bind(limit.clamp(1, 200))
    .fetch_all(executor)
    .await?;
    Ok(actions)
}

/// 解决待确认事项。
pub async fn resolve_action(
    executor: impl PgExecutor<'_>,
    id: Uuid,
    input_code: Option<&str>,
    user_id: Option<Uuid>,
) -> AppResult<ManualAction> {
    let action = sqlx::query_as::<_, ManualAction>(&format!(
        "UPDATE manual_actions SET \
             status = $2, input_code = $3, resolved_at = now(), resolved_by = $4, updated_at = now() \
         WHERE id = $1 AND status = $5 AND expires_at > now() \
         RETURNING {ACTION_COLUMNS}"
    ))
    .bind(id)
    .bind(ManualActionStatus::Resolved.as_str())
    .bind(input_code)
    .bind(user_id)
    .bind(ManualActionStatus::Pending.as_str())
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::conflict("事项不存在、已处理或已过期"))?;
    Ok(action)
}

/// 取消待确认事项。
pub async fn cancel_action(
    executor: impl PgExecutor<'_>,
    id: Uuid,
    user_id: Option<Uuid>,
) -> AppResult<ManualAction> {
    let action = sqlx::query_as::<_, ManualAction>(&format!(
        "UPDATE manual_actions SET \
             status = $2, resolved_at = now(), resolved_by = $3, updated_at = now() \
         WHERE id = $1 AND status = $4 \
         RETURNING {ACTION_COLUMNS}"
    ))
    .bind(id)
    .bind(ManualActionStatus::Cancelled.as_str())
    .bind(user_id)
    .bind(ManualActionStatus::Pending.as_str())
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::conflict("事项不存在或已处理"))?;
    Ok(action)
}

/// 过期未处理的事项置为已过期。
pub async fn expire_pending_actions(pool: &PgPool) -> AppResult<u64> {
    let count = sqlx::query(
        "UPDATE manual_actions SET status = $1, updated_at = now() \
         WHERE status = $2 AND expires_at <= now()",
    )
    .bind(ManualActionStatus::Expired.as_str())
    .bind(ManualActionStatus::Pending.as_str())
    .execute(pool)
    .await?
    .rows_affected();
    Ok(count)
}
