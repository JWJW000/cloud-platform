//! 节点接入鉴权（第 15.1、15.2 节）。
//!
//! Worker 的身份由两层证据共同确定，缺一层都不足：
//! 1. **节点凭据**（`x-node-token`）：注册时一次性下发的高熵随机串，
//!    数据库只存 SHA-256，比较走常量时间；
//! 2. **客户端证书**（mTLS）：TLS 在反向代理处终止（第 18 节的部署形态），
//!    因此 Master 进程读不到对端证书本身，只能读代理透传的指纹头。
//!
//! 关于指纹的取舍写在这里，因为它容易被误解：**指纹存在时一定校验**，
//! 存在与否则由 [`crate::config::SecurityConfig::require_client_cert`] 决定是否强制。
//! 「有指纹但不检查」是最糟的一种状态——它让部署方以为 mTLS 在生效。

use platform_proto::{
    METADATA_AGENT_VERSION, METADATA_CLIENT_CERT_FINGERPRINT, METADATA_NODE_ID, METADATA_NODE_TOKEN,
};
use tonic::metadata::MetadataMap;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::WorkerNode;
use crate::security::normalize_fingerprint;
use crate::state::AppState;
use crate::store;

/// 一条已通过鉴权的链路背后的节点身份。
#[derive(Debug, Clone)]
pub struct NodeIdentity {
    /// 节点当前数据库快照。
    ///
    /// 刻意留一份快照而不是每次用到都回查：链路存续期间这些字段几乎不变，
    /// 真正需要最新值的地方（状态、槽位数）都会自己重新读一次。
    pub node: WorkerNode,
    /// 节点自报的 Agent 版本，写心跳时用。
    pub agent_version: String,
    /// 反向代理透传的客户端证书指纹（已归一化）。
    pub fingerprint: Option<String>,
}

impl NodeIdentity {
    /// 节点编号的便捷访问。
    pub fn node_id(&self) -> Uuid {
        self.node.id
    }
}

/// 从 gRPC 元数据里认出一个节点。
///
/// `require_cert`：是否强制要求客户端证书指纹（V5 第 6.2 节）。
/// - `OpenLink` 等正式任务链路必须为 `true`——即使入口代理把客户端证书配成
///   「可选请求」，Master 方法级也绝不放行无证书链路；
/// - `false` 保留给配置兼容期（`require_client_cert=false` 的旧部署形态）。
///
/// 失败一律回中文原因，但**不含**凭据本身：错误消息会进 Worker 日志。
pub async fn authenticate(
    state: &AppState,
    metadata: &MetadataMap,
    require_cert: bool,
) -> AppResult<NodeIdentity> {
    let raw_id = required(metadata, METADATA_NODE_ID, "节点编号")?;
    let node_id = Uuid::parse_str(raw_id.trim())
        .map_err(|_| AppError::Unauthorized("节点编号不是合法编号".to_string()))?;
    let token = required(metadata, METADATA_NODE_TOKEN, "节点凭据")?;

    let node = store::node::authenticate_node(&state.pool, node_id, token).await?;

    if node.status == platform_domain::WorkerStatus::Disabled.as_str() {
        return Err(AppError::Forbidden("节点已被禁用".to_string()));
    }

    // V5：注册 ≠ 授权。只有「已批准」节点能建立正式任务链路；
    // 待审核 / 已拒绝 / 已过期一律拒绝（第 6.1 节）。
    if node.registration_status != "已批准" {
        return Err(AppError::Forbidden(format!(
            "节点未获批准（当前注册状态：{}），无法建立任务链路",
            node.registration_status
        )));
    }

    let fingerprint =
        optional(metadata, METADATA_CLIENT_CERT_FINGERPRINT).map(normalize_fingerprint);
    match &fingerprint {
        Some(fingerprint) => {
            let owner = store::node::fingerprint_owner(&state.pool, fingerprint).await?;
            if owner != Some(node_id) {
                // 指纹对不上号有两种可能：证书被吊销/过期，或者它属于另一个节点。
                // 两者都不该放行，也都不该在消息里告诉对方是哪一种。
                return Err(AppError::Forbidden(
                    "客户端证书无效或不属于该节点".to_string(),
                ));
            }
        }
        None if require_cert || state.config.security.require_client_cert => {
            return Err(AppError::Unauthorized(
                "缺少客户端证书，服务器要求 mTLS 接入".to_string(),
            ));
        }
        None => {}
    }

    Ok(NodeIdentity {
        agent_version: optional(metadata, METADATA_AGENT_VERSION)
            .unwrap_or_default()
            .to_string(),
        fingerprint,
        node,
    })
}

/// 取一个必填的元数据项。
fn required<'a>(metadata: &'a MetadataMap, key: &str, field: &str) -> AppResult<&'a str> {
    let value = optional(metadata, key)
        .ok_or_else(|| AppError::Unauthorized(format!("请求缺少{field}")))?;
    if value.trim().is_empty() {
        return Err(AppError::Unauthorized(format!("请求的{field}为空")));
    }
    Ok(value)
}

/// 取一个可选的元数据项；非 ASCII 的头按「没有」处理。
fn optional<'a>(metadata: &'a MetadataMap, key: &str) -> Option<&'a str> {
    metadata.get(key)?.to_str().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(pairs: &[(&'static str, &'static str)]) -> MetadataMap {
        let mut map = MetadataMap::new();
        for (key, value) in pairs {
            map.insert(*key, value.parse().unwrap());
        }
        map
    }

    #[test]
    fn missing_metadata_is_reported_in_chinese() {
        let map = metadata(&[]);
        let error = required(&map, METADATA_NODE_ID, "节点编号").unwrap_err();
        assert!(matches!(error, AppError::Unauthorized(_)));
        assert!(error.to_string().contains("节点编号"));
    }

    #[test]
    fn blank_metadata_counts_as_missing() {
        let map = metadata(&[(METADATA_NODE_TOKEN, "   ")]);
        assert!(required(&map, METADATA_NODE_TOKEN, "节点凭据").is_err());
    }

    #[test]
    fn present_metadata_is_returned_verbatim() {
        let map = metadata(&[(METADATA_AGENT_VERSION, "0.1.0")]);
        assert_eq!(optional(&map, METADATA_AGENT_VERSION), Some("0.1.0"));
        assert_eq!(optional(&map, METADATA_NODE_ID), None);
    }

    #[test]
    fn fingerprints_are_compared_after_normalisation() {
        // 代理透传的指纹大小写与冒号写法不统一，比较前必须先归一化
        assert_eq!(
            normalize_fingerprint("AB:CD:ef"),
            normalize_fingerprint("abcdef")
        );
    }
}
