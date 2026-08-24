//! Worker ⇄ Master gRPC 协议的生成代码与便捷构造函数。
//!
//! 协议本身的消息名使用英文技术标识，但**消息内的业务值全部是中文字符串**，
//! 与 PostgreSQL 存储值一致（设计方案第 13.2 节）。

#![allow(clippy::large_enum_variant)]
#![allow(clippy::result_large_err)]
#![allow(clippy::mixed_attributes_style)]

/// 协议技术枚举 → 中文展示值映射（第 10.1 节，唯一实现来源）。
pub mod display;

/// tonic/prost 生成的协议代码。
pub mod v1 {
    tonic::include_proto!("platform.worker.v1");
}

pub use v1::*;

/// gRPC 元数据键：节点编号。
pub const METADATA_NODE_ID: &str = "x-node-id";
/// gRPC 元数据键：节点长期凭据。
pub const METADATA_NODE_TOKEN: &str = "x-node-token";
/// gRPC 元数据键：Agent 版本，Master 依此拒绝过旧节点。
pub const METADATA_AGENT_VERSION: &str = "x-agent-version";
/// 反向代理转发客户端证书指纹时使用的元数据键（mTLS 在边缘终止时）。
pub const METADATA_CLIENT_CERT_FINGERPRINT: &str = "x-client-cert-fingerprint";

impl WorkerMessage {
    /// 构造一条带唯一事件编号与发送时间的 Worker 上行消息。
    pub fn new(
        event_id: impl Into<String>,
        sent_at: impl Into<String>,
        payload: worker_message::Payload,
    ) -> Self {
        Self {
            event_id: event_id.into(),
            sent_at: sent_at.into(),
            replayed: false,
            payload: Some(payload),
        }
    }

    /// 标记为 outbox 补报事件（第 14.2 节）。
    pub fn replayed(mut self) -> Self {
        self.replayed = true;
        self
    }
}

impl MasterMessage {
    /// 构造一条 Master 下行消息。
    pub fn new(sent_at: impl Into<String>, payload: master_message::Payload) -> Self {
        Self {
            sent_at: sent_at.into(),
            payload: Some(payload),
        }
    }
}
