//! 节点正式长连接链路身份鉴权（V7 实施方案第 5.5 节、8.3 节）。
//!
//! 正式链路唯一权威身份来源：
//! ```text
//! Caddy 实际收到的客户端证书
//!         ↓ SHA-256 fingerprint (x-client-cert-fingerprint)
//! Master 查询 node_certificates (未吊销、未过期)
//!         ↓
//! 得到 node_id，并校验批准、禁用状态
//! ```

use platform_proto::{
    METADATA_AGENT_VERSION, METADATA_CLIENT_CERT_FINGERPRINT, METADATA_NODE_ID, METADATA_NODE_TOKEN,
};
use tonic::metadata::MetadataMap;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::grpc::auth::NodeIdentity;
use crate::security::normalize_fingerprint;
use crate::state::AppState;
use crate::store;

/// 从 gRPC 元数据中鉴权正式链路（V7 第 5.5 节）。
///
/// 1. 优先且强制通过客户端证书指纹（`x-client-cert-fingerprint`）确认身份；
/// 2. 证书必须有效、未过期、未吊销，且在 `node_certificates` 绑定有效节点；
/// 3. 节点必须处于 `已批准` 状态且未被 `已禁用`；
/// 4. 兼容双栈：若节点仍处于旧 `token_and_certificate` 模式且携带了 token，则同时校验 token。
pub async fn authenticate_link(
    state: &AppState,
    metadata: &MetadataMap,
    require_cert: bool,
) -> AppResult<NodeIdentity> {
    let raw_fingerprint = metadata
        .get(METADATA_CLIENT_CERT_FINGERPRINT)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(normalize_fingerprint);

    let agent_version = metadata
        .get(METADATA_AGENT_VERSION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    let node_id = match raw_fingerprint {
        Some(ref fp) => {
            let owner = store::node::fingerprint_owner(&state.pool, fp).await?;
            owner.ok_or_else(|| AppError::Forbidden("客户端证书无效或不属于该节点".to_string()))?
        }
        None => {
            // 没有证书指纹时：若强制要求证书或系统开启了安全证书要求，则直接拒绝
            if require_cert || state.config.security.require_client_cert {
                return Err(AppError::Unauthorized(
                    "缺少客户端证书，服务器要求 mTLS 接入".to_string(),
                ));
            }

            // 兼容非证书本地开发测试模式：尝试通过 node_id 与 node_token 鉴权
            let raw_id = metadata
                .get(METADATA_NODE_ID)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| AppError::Unauthorized("缺少节点编号".to_string()))?;
            let parsed_id = Uuid::parse_str(raw_id.trim())
                .map_err(|_| AppError::Unauthorized("节点编号不是合法编号".to_string()))?;
            let token = metadata
                .get(METADATA_NODE_TOKEN)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| AppError::Unauthorized("缺少节点凭据".to_string()))?;

            let node = store::node::authenticate_node(&state.pool, parsed_id, token).await?;
            if node.status == platform_domain::WorkerStatus::Disabled.as_str() {
                return Err(AppError::Forbidden("节点已被禁用".to_string()));
            }
            if node.registration_status != "已批准" {
                return Err(AppError::Forbidden(format!(
                    "节点未获批准（当前注册状态：{}），无法建立任务链路",
                    node.registration_status
                )));
            }

            return Ok(NodeIdentity {
                node,
                agent_version,
                fingerprint: None,
            });
        }
    };

    // 取节点信息
    let node = store::node::get_node(&state.pool, node_id).await?;

    if node.status == platform_domain::WorkerStatus::Disabled.as_str() {
        return Err(AppError::Forbidden("节点已被禁用".to_string()));
    }

    if node.registration_status != "已批准" {
        return Err(AppError::Forbidden(format!(
            "节点未获批准（当前注册状态：{}），无法建立任务链路",
            node.registration_status
        )));
    }

    // 双栈兼容检查：如果旧节点是 token_and_certificate 模式且提供了 token
    if node.credential_mode == "token_and_certificate" {
        if let Some(token) = metadata
            .get(METADATA_NODE_TOKEN)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let _ = store::node::authenticate_node(&state.pool, node_id, token).await?;
        }
    }

    Ok(NodeIdentity {
        node,
        agent_version,
        fingerprint: raw_fingerprint,
    })
}
