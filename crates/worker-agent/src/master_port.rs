//! Worker MasterPort 端口抽象与领域错误（V7 实施方案第 4.3 节、第 10 节）。
//!
//! 定义 Worker 核心状态机与远端 Master 通信的边界。

use std::pin::Pin;
use std::time::Duration;

use async_trait::async_trait;
use futures::Stream;
use platform_proto::v1 as pb;

/// Worker 连接与协议领域错误（V7 第 10 节）。
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// 网络故障（可指数退避重试）。
    #[error("网络连接异常：{detail}；将在 {retry_after:?} 后重试")]
    Network {
        /// 建议重试间隔。
        retry_after: Option<Duration>,
        /// 可安全展示的底层 DNS、TLS、HTTP/2 或 gRPC 错误摘要。
        detail: String,
    },

    /// 请求超频限流（按 retry_after 等待）。
    #[error("请求被限流，将在 {retry_after:?} 后重试")]
    RateLimited {
        /// 限流退避时长。
        retry_after: Duration,
    },

    /// 审批中等待（正常等待，不计入故障退避）。
    #[error("节点处于待审核状态")]
    PendingApproval {
        /// 建议下次查询间隔。
        retry_after: Duration,
    },

    /// 管理员拒绝注册。
    #[error("管理员拒绝了节点的注册申请：{reason}")]
    Rejected {
        /// 拒绝原因。
        reason: String,
    },

    /// 注册申请已过期。
    #[error("节点注册申请已过期")]
    RegistrationExpired,

    /// 身份冲突（安装标识被占用或公钥突变）。
    #[error("节点身份冲突或公钥不匹配，禁止自动覆盖")]
    IdentityConflict,

    /// 客户端证书已过期。
    #[error("客户端证书已过期，需重新申请或更新")]
    CertificateExpired,

    /// 客户端证书已被吊销。
    #[error("客户端证书已被吊销，拒绝接入")]
    CertificateRevoked,

    /// 未授权（证书无效或未被信任）。
    #[error("身份认证失败：未授权")]
    Unauthorized,

    /// 服务端不支持新协议（双栈兼容期可回退旧 RPC）。
    #[error("服务端未实现新注册协议 (UNIMPLEMENTED)")]
    ProtocolMismatch,

    /// 本地身份凭据损坏或公私钥不匹配。
    #[error("本地凭据文件损坏或不匹配，拒绝静默覆盖：{0}")]
    LocalCredentialCorrupt(String),

    /// 不可恢复的致命错误。
    #[error("致命错误：{0}")]
    Fatal(#[from] anyhow::Error),
}

/// EnsureRegistration 请求数据传输对象。
#[derive(Debug, Clone)]
pub struct EnsureRegistrationRequestDto {
    /// 协议版本（默认为 1）。
    pub protocol_version: u32,
    /// 首次安装生成的 UUID 标识。
    pub installation_id: String,
    /// 节点名称。
    pub node_name: String,
    /// 操作系统类型。
    pub os_type: String,
    /// 操作系统版本。
    pub os_version: String,
    /// Agent 版本。
    pub agent_version: String,
    /// 申请槽位数。
    pub requested_slots: u32,
    /// CSR PEM。
    pub csr_pem: String,
    /// 请求随机挑战值。
    pub request_nonce: String,
    /// 请求发起时间（RFC 3339 字符串）。
    pub requested_at: String,
    /// 私钥持有证明签名（十六进制）。
    pub proof_signature: String,
    /// 可选长轮询等待秒数（0..30）。
    pub wait_seconds: u32,
}

/// EnsureRegistration 结果。
#[derive(Debug, Clone)]
pub enum RegistrationOutcome {
    /// 待审核状态。
    Pending {
        /// 节点编号。
        node_id: String,
        /// 建议等待时长。
        retry_after: Duration,
    },
    /// 已批准状态。
    Approved {
        /// 节点编号。
        node_id: String,
        /// 批准槽位数。
        approved_slots: u32,
        /// 签发的客户端证书 PEM。
        client_certificate_pem: String,
    },
    /// 已拒绝状态。
    Rejected {
        /// 拒绝原因。
        reason: String,
    },
    /// 已过期状态。
    Expired,
}

/// 建立正式长连接所需的客户端凭据。
#[derive(Debug, Clone)]
pub struct ClientCredential {
    /// 节点 UUID。
    pub node_id: String,
    /// 安装标识 UUID。
    pub installation_id: String,
    /// 私钥 PEM。
    pub client_key_pem: String,
    /// 客户端证书 PEM。
    pub client_cert_pem: String,
}

/// 已建立的长连接会话抽象。
pub trait MasterLinkSession: Send {
    /// 发送上行 WorkerMessage。
    fn send(&mut self, msg: pb::WorkerMessage) -> Result<(), ConnectError>;

    /// 接收下行 MasterMessage 流。
    fn inbound_stream(
        &mut self,
    ) -> Pin<Box<dyn Stream<Item = Result<pb::MasterMessage, ConnectError>> + Send + 'static>>;
}

/// Worker 内部 Master 端口契约。
#[async_trait]
pub trait MasterPort: Send + Sync {
    /// 确保注册并查询审批结果（幂等）。
    async fn ensure_registration(
        &self,
        request: EnsureRegistrationRequestDto,
    ) -> Result<RegistrationOutcome, ConnectError>;

    /// 打开 mTLS 正式长连接通道。
    async fn open_link(
        &self,
        credential: &ClientCredential,
    ) -> Result<Box<dyn MasterLinkSession>, ConnectError>;
}
