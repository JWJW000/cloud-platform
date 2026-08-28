//! Worker 会话级代理运行时抽象与实现（方案第 5.1 节）。
//!
//! 将代理生命周期封装为深模块：
//! - 生成严格保护的临时配置（Unix 0600 / Windows ACL）；
//! - 启动 GOST 独立进程转发 HTTP/CONNECT；
//! - 进行 listener 就绪探测与出口 IP/连通性验证；
//! - 句柄 Drop 或显式关闭时立即安全清理配置并终止进程。

use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use platform_proto::v1::ProxyCredential;
use thiserror::Error;
use url::Url;
use uuid::Uuid;

pub mod fake;
pub mod gost;

pub use fake::FakeProxyRuntime;
pub use gost::GostProxyRuntime;

/// 代理运行时错误分类。
#[derive(Debug, Error)]
pub enum ProxyRuntimeError {
    #[error("上游代理配置不完整：{0}")]
    InvalidUpstream(String),

    #[error("生成代理私有配置文件失败：{0}")]
    ConfigWriteFailed(String),

    #[error("GOST 进程启动失败：{0}")]
    ProcessSpawnFailed(String),

    #[error("本地 listener 等待就绪超时（端口：{0}）")]
    ListenerTimeout(u16),

    #[error("GOST 进程意外退出（exit code: {0:?}）")]
    ProcessExited(Option<i32>),

    #[error("代理连通性或出口 IP 探测失败：{0}")]
    ProbeFailed(String),

    #[error("代理认证失败或目标拒绝：{0}")]
    AuthenticationFailed(String),

    #[error("I/O 错误：{0}")]
    Io(#[from] std::io::Error),
}

/// 会话代理规格输入。
#[derive(Debug, Clone)]
pub struct SessionProxySpec {
    /// 执行会话编号。
    pub session_id: Uuid,
    /// 槽位序号。
    pub slot_index: u32,
    /// 本地转发端口（通常为 19001 + slot_index）。
    pub local_port: u16,
    /// 上游 Webshare/自定义代理凭据。
    pub upstream: ProxyCredential,
}

/// 已验证代理会话的输出要素。
#[derive(Debug, Clone)]
pub struct VerifiedProxySession {
    /// 浏览器与 HTTP 下载使用的本地代理 URL，例如 `http://127.0.0.1:19001`。
    pub browser_proxy_url: Url,
    /// 实测出口 IP。
    pub exit_ip: Option<IpAddr>,
    /// 探测延迟。
    pub latency: Duration,
}

/// 代理会话句柄：控制 GOST 进程及配置生命周期。
pub struct ProxySessionHandle {
    session_id: Uuid,
    slot_index: u32,
    local_port: u16,
    browser_proxy_url: Url,
    exit_ip: Option<IpAddr>,
    latency: Duration,
    config_path: Option<PathBuf>,
    child: Option<tokio::process::Child>,
}

impl ProxySessionHandle {
    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    pub fn slot_index(&self) -> u32 {
        self.slot_index
    }

    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    pub fn browser_proxy_url(&self) -> &Url {
        &self.browser_proxy_url
    }

    pub fn exit_ip(&self) -> Option<IpAddr> {
        self.exit_ip
    }

    pub fn latency(&self) -> Duration {
        self.latency
    }

    /// 检查 GOST 进程是否依然存活。
    pub fn is_alive(&mut self) -> bool {
        if let Some(ref mut child) = self.child {
            match child.try_wait() {
                Ok(None) => true,
                Ok(Some(_status)) => false,
                Err(_) => false,
            }
        } else {
            false
        }
    }

    /// 显式关闭并回收所有资源。
    pub async fn shutdown(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        if let Some(path) = self.config_path.take() {
            let _ = std::fs::remove_file(&path);
        }
    }
}

impl Drop for ProxySessionHandle {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
        if let Some(path) = self.config_path.take() {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// 代理运行时抽象接口。
#[async_trait]
pub trait ProxyRuntime: Send + Sync {
    /// 启动并验证一个会话代理。
    async fn start_verified(
        &self,
        spec: SessionProxySpec,
    ) -> Result<ProxySessionHandle, ProxyRuntimeError>;

    /// 仅进行代理连通性、延迟与出口 IP 探测（用于 ProxyCheck 任务）。
    async fn check_proxy(
        &self,
        spec: SessionProxySpec,
    ) -> Result<VerifiedProxySession, ProxyRuntimeError>;
}
