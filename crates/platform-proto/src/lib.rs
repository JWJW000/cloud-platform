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
/// 协议版本元数据键。
pub const METADATA_PROTOCOL_VERSION: &str = "x-protocol-version";

/// 把运行时平台名称规范为数据库允许的 Worker OS 值。
pub fn canonical_worker_os(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "windows" => Some("Windows"),
        "macos" | "darwin" => Some("macOS"),
        "linux" => Some("Linux"),
        _ => None,
    }
}

/// 从 CSR PEM 提取 EC 公钥原始字节。
///
/// Worker 与 Master 必须共享这个实现；若一端摘要整个 CSR、另一端只摘要公钥，
/// 私钥持有证明的规范消息会不一致，所有首次注册都会被误判为未授权。
pub fn csr_public_key(csr_pem: &str) -> anyhow::Result<Vec<u8>> {
    use x509_parser::certification_request::X509CertificationRequest;
    use x509_parser::pem::parse_x509_pem;
    use x509_parser::prelude::FromDer;

    let (_, pem) =
        parse_x509_pem(csr_pem.as_bytes()).map_err(|e| anyhow::anyhow!("CSR 不是合法 PEM：{e}"))?;
    let (_, csr) = X509CertificationRequest::from_der(&pem.contents)
        .map_err(|e| anyhow::anyhow!("CSR DER 解析失败：{e}"))?;
    Ok(csr
        .certification_request_info
        .subject_pki
        .subject_public_key
        .data
        .to_vec())
}

/// CSR 公钥 SHA-256 指纹（小写十六进制）。
pub fn csr_public_key_fingerprint(csr_pem: &str) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(csr_public_key(csr_pem)?);
    Ok(hex::encode(hasher.finalize()))
}

/// 构造 EnsureRegistration 私钥持有证明的规范化待签名消息（V7 第 5.2 节）。
pub fn format_ensure_registration_proof(
    protocol_version: u32,
    installation_id: &str,
    csr_sha256: &str,
    nonce: &str,
    requested_at: &str,
) -> String {
    format!(
        "v{}:{}:{}:{}:{}",
        protocol_version,
        installation_id.trim(),
        csr_sha256.trim(),
        nonce.trim(),
        requested_at.trim()
    )
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_os_values_match_database_constraint() {
        assert_eq!(canonical_worker_os("windows"), Some("Windows"));
        assert_eq!(canonical_worker_os("Windows"), Some("Windows"));
        assert_eq!(canonical_worker_os("macos"), Some("macOS"));
        assert_eq!(canonical_worker_os("darwin"), Some("macOS"));
        assert_eq!(canonical_worker_os("linux"), Some("Linux"));
        assert_eq!(canonical_worker_os("freebsd"), None);
    }
}
