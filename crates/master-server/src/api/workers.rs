//! Worker 节点与槽位管理接口（第 16.2 节、V5 第 8 节）。

use axum::extract::{Path, Query, State};
use axum::Json;
use platform_domain::WorkerStatus;
use platform_proto::v1 as pb;
use serde::Deserialize;
use uuid::Uuid;

use crate::api::auth::AuthenticatedUser;
use crate::error::{AppError, AppResult};
use crate::grpc::convert;
use crate::models::{NodeCertificate, WorkerNode, WorkerSlot};
use crate::state::AppState;
use crate::store;

#[derive(Debug, Deserialize)]
pub struct UpdateStatusRequest {
    /// 中文 Worker 状态。
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCapacityRequest {
    pub max_slots: i32,
    pub upload_concurrency: i32,
}

/// V5：批准节点请求体（直连注册节点：批准时签发证书并设置实际槽位）。
#[derive(Debug, Deserialize, Default)]
pub struct ApproveRequest {
    /// 批准的实际槽位数（1..50；缺省取节点申请值）。
    pub configured_slots: Option<i32>,
    /// 批准备注（审计）。
    pub remark: Option<String>,
}

/// V5：拒绝节点请求体。
#[derive(Debug, Deserialize)]
pub struct RejectRequest {
    /// 拒绝原因（中文，必填）。
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateDiagnosticsRequest {
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct RevokeCertificateRequest {
    pub reason: String,
}

/// GET /api/workers?registration_status=待审核
pub async fn list_workers(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Query(params): Query<WorkerListParams>,
) -> AppResult<Json<Vec<WorkerNode>>> {
    let nodes = store::node::list_nodes_by_registration(
        &state.pool,
        params
            .registration_status
            .as_deref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty()),
    )
    .await?;
    Ok(Json(nodes))
}

/// Worker 列表查询参数（V5：注册状态过滤）。
#[derive(Debug, Default, Deserialize)]
pub struct WorkerListParams {
    /// 中文注册状态：待审核 / 已批准 / 已拒绝 / 已过期。
    pub registration_status: Option<String>,
}

/// GET /api/workers/:id
pub async fn get_worker(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<WorkerNode>> {
    let node = store::node::get_node(&state.pool, id).await?;
    Ok(Json(node))
}

/// POST /api/workers/:id/approve
///
/// V5：只有超级管理员可批准。直连注册节点（有待审核注册会话）在批准时
/// **才签发**正式客户端证书与节点令牌（第 6.7/6.8 节）；旧注册码节点的证书
/// 已在注册时签发，走兼容审批路径。
pub async fn approve_worker(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
    body: Option<Json<ApproveRequest>>,
) -> AppResult<Json<WorkerNode>> {
    auth.require_super_admin()?;
    // 兼容旧前端：批准可以不携带请求体；直连注册节点的槽位缺省取申请值。
    let req = body.map(|Json(r)| r).unwrap_or_default();

    let node = store::node::get_node(&state.pool, id).await?;
    let has_session = store::registration::find_active_session(&state.pool, id)
        .await?
        .is_some();

    let node = if has_session {
        // 直连注册：批准时才签发证书与令牌（单事务、行锁、幂等）
        let configured_slots = req
            .configured_slots
            .unwrap_or_else(|| node.requested_slots.or(Some(node.max_slots)).unwrap_or(5));
        let node_id = id;
        let (approved, _session, _token) = store::registration::approve_registration(
            &state.pool,
            node_id,
            auth.id,
            configured_slots,
            req.remark.as_deref(),
            move |csr| {
                state
                    .ca
                    .sign_csr(csr, &node_id.to_string())
                    .map_err(|e| crate::error::AppError::bad(e.to_string()))
            },
        )
        .await?;
        approved
    } else {
        // 旧注册码节点：证书已签发，这里只置为离线 + 刷新在线
        let approved = store::node::approve_node(&state.pool, id, Some(auth.id)).await?;
        match state.links.sender(id) {
            Some(sender) => {
                let node =
                    store::node::set_node_status(&state.pool, id, WorkerStatus::Online).await?;
                store::node::refresh_available_slots(&state.pool, id).await?;
                crate::grpc::inbound::send_node_config(&state, &node, &sender).await;
                store::node::get_node(&state.pool, id).await?
            }
            None => approved,
        }
    };

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "批准节点",
        &id.to_string(),
        &format!(
            "节点 {} 审核通过（槽位 {}，备注 {}）",
            node.name,
            node.configured_slots.unwrap_or(node.max_slots),
            req.remark.as_deref().unwrap_or("-")
        ),
    )
    .await?;

    state.events.publish(
        "节点变更",
        serde_json::json!({ "节点": id, "动作": "审核通过" }),
    );

    Ok(Json(node))
}

/// POST /api/workers/:id/reject（V5：拒绝直连注册申请，需填写中文原因）
pub async fn reject_worker(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
    Json(req): Json<RejectRequest>,
) -> AppResult<Json<WorkerNode>> {
    auth.require_super_admin()?;
    let reason = req.reason.trim();
    if reason.is_empty() {
        return Err(AppError::bad("拒绝原因不能为空"));
    }
    if reason.chars().count() > 500 {
        return Err(AppError::bad("拒绝原因过长"));
    }

    let mut tx = state.pool.begin().await?;
    let node = sqlx::query_as::<_, WorkerNode>(&format!(
        "SELECT {} FROM worker_nodes WHERE id = $1 FOR UPDATE",
        store::node::NODE_COLUMNS
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::missing("节点不存在"))?;
    if node.registration_status != "待审核" {
        return Err(AppError::conflict(format!(
            "节点当前注册状态为「{}」，只有待审核节点可以被拒绝",
            node.registration_status
        )));
    }
    store::registration::set_node_rejected(&mut tx, id, auth.id, reason).await?;
    let node = sqlx::query_as::<_, WorkerNode>(&format!(
        "SELECT {} FROM worker_nodes WHERE id = $1",
        store::node::NODE_COLUMNS
    ))
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Warn,
        &auth.username,
        "拒绝节点",
        &id.to_string(),
        &format!("拒绝节点 {}，原因：{reason}", node.name),
    )
    .await?;
    state.events.publish(
        "节点变更",
        serde_json::json!({ "节点": id, "动作": "拒绝" }),
    );

    Ok(Json(node))
}

/// POST /api/workers/:id/disable（V5 别名：禁用节点）
pub async fn disable_worker(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<WorkerNode>> {
    update_worker_status(
        State(state),
        auth,
        Path(id),
        Json(UpdateStatusRequest {
            status: WorkerStatus::Disabled.as_str().to_string(),
        }),
    )
    .await
}

/// POST /api/workers/:id/enable（V5 别名：恢复节点为在线）
pub async fn enable_worker(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<WorkerNode>> {
    update_worker_status(
        State(state),
        auth,
        Path(id),
        Json(UpdateStatusRequest {
            status: WorkerStatus::Online.as_str().to_string(),
        }),
    )
    .await
}

/// PUT /api/workers/:id/status
pub async fn update_worker_status(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateStatusRequest>,
) -> AppResult<Json<WorkerNode>> {
    auth.require_write()?;
    let target_status = req.status.parse::<WorkerStatus>()?;
    let node = store::node::set_node_status(&state.pool, id, target_status).await?;

    if target_status == WorkerStatus::Disabled {
        state.links.force_disconnect(id);
    }

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "更新节点状态",
        &id.to_string(),
        &format!("状态变更为 {}", target_status),
    )
    .await?;

    state.events.publish(
        "节点变更",
        serde_json::json!({ "节点": id, "状态": target_status.as_str() }),
    );

    Ok(Json(node))
}

/// PUT /api/workers/:id/capacity
pub async fn update_worker_capacity(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateCapacityRequest>,
) -> AppResult<Json<WorkerNode>> {
    auth.require_super_admin()?;
    let mut tx = state.pool.begin().await?;

    let node =
        store::node::set_node_capacity(&mut *tx, id, req.max_slots, req.upload_concurrency).await?;

    store::node::ensure_slots(&mut tx, id, node.max_slots).await?;
    tx.commit().await?;

    // 如果在线，推送更新配置
    if let Some(sender) = state.links.sender(id) {
        crate::grpc::inbound::send_node_config(&state, &node, &sender).await;
    }

    state.events.publish(
        "节点变更",
        serde_json::json!({ "节点": id, "槽位": node.max_slots }),
    );

    Ok(Json(node))
}

/// PUT /api/workers/:id/diagnostics
pub async fn update_worker_diagnostics(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateDiagnosticsRequest>,
) -> AppResult<Json<WorkerNode>> {
    auth.require_write()?;
    let node = store::node::set_diagnostics(&state.pool, id, req.enabled).await?;

    if let Some(sender) = state.links.sender(id) {
        crate::grpc::inbound::send_node_config(&state, &node, &sender).await;
    }

    Ok(Json(node))
}

/// POST /api/workers/:id/pause
pub async fn pause_worker(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<WorkerNode>> {
    auth.require_write()?;
    let node = store::node::set_node_status(&state.pool, id, WorkerStatus::Paused).await?;

    let msg = pb::MasterMessage::new(
        convert::now_rfc3339(),
        pb::master_message::Payload::PauseNode(pb::PauseNode {
            reason: format!("管理员 {} 手工暂停", auth.username),
            finish_current_task: true,
        }),
    );
    state.links.try_dispatch(id, msg);

    state.events.publish(
        "节点变更",
        serde_json::json!({ "节点": id, "动作": "暂停" }),
    );

    Ok(Json(node))
}

/// POST /api/workers/:id/resume
pub async fn resume_worker(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<WorkerNode>> {
    auth.require_write()?;
    let node = store::node::set_node_status(&state.pool, id, WorkerStatus::Online).await?;

    let msg = pb::MasterMessage::new(
        convert::now_rfc3339(),
        pb::master_message::Payload::ResumeNode(pb::ResumeNode {
            reason: format!("管理员 {} 恢复运行", auth.username),
        }),
    );
    state.links.try_dispatch(id, msg);

    state.events.publish(
        "节点变更",
        serde_json::json!({ "节点": id, "动作": "恢复" }),
    );

    Ok(Json(node))
}

/// GET /api/workers/:id/slots
pub async fn list_worker_slots(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Vec<WorkerSlot>>> {
    let slots = store::node::list_slots(&state.pool, id).await?;
    Ok(Json(slots))
}

/// GET /api/slots
pub async fn list_all_slots(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
) -> AppResult<Json<Vec<WorkerSlot>>> {
    let slots = store::node::list_all_slots(&state.pool).await?;
    Ok(Json(slots))
}

/// GET /api/workers/:id/certificates
pub async fn list_worker_certificates(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Vec<NodeCertificate>>> {
    let certs = store::node::list_certificates(&state.pool, id).await?;
    Ok(Json(certs))
}

/// POST /api/workers/certificates/:fingerprint/revoke
pub async fn revoke_certificate(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(fingerprint): Path<String>,
    Json(req): Json<RevokeCertificateRequest>,
) -> AppResult<Json<serde_json::Value>> {
    auth.require_super_admin()?;
    let owner_node_id: Option<Uuid> =
        sqlx::query_scalar("SELECT node_id FROM node_certificates WHERE fingerprint = $1")
            .bind(&fingerprint)
            .fetch_optional(&state.pool)
            .await?;

    store::node::revoke_certificate(&state.pool, &fingerprint, &req.reason).await?;

    if let Some(node_id) = owner_node_id {
        state.links.force_disconnect(node_id);
    }

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Warn,
        &auth.username,
        "撤销节点证书",
        &fingerprint,
        &req.reason,
    )
    .await?;

    Ok(Json(serde_json::json!({ "message": "证书已撤销" })))
}
