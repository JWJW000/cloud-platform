//! 测试用的 Fake 代理运行时实现。

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use async_trait::async_trait;
use url::Url;

use super::{
    ProxyRuntime, ProxyRuntimeError, ProxySessionHandle, SessionProxySpec, VerifiedProxySession,
};

pub struct FakeProxyRuntime {
    pub should_fail: bool,
    pub fake_ip: Option<IpAddr>,
    pub latency: Duration,
}

impl Default for FakeProxyRuntime {
    fn default() -> Self {
        Self {
            should_fail: false,
            fake_ip: Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1))),
            latency: Duration::from_millis(50),
        }
    }
}

impl FakeProxyRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_failure() -> Self {
        Self {
            should_fail: true,
            fake_ip: None,
            latency: Duration::from_millis(100),
        }
    }
}

#[async_trait]
impl ProxyRuntime for FakeProxyRuntime {
    async fn start_verified(
        &self,
        spec: SessionProxySpec,
    ) -> Result<ProxySessionHandle, ProxyRuntimeError> {
        if self.should_fail {
            return Err(ProxyRuntimeError::AuthenticationFailed(
                "Fake 代理认证失败 (407)".to_string(),
            ));
        }

        let browser_proxy_url = Url::parse(&format!("http://127.0.0.1:{}", spec.local_port))
            .map_err(|e| ProxyRuntimeError::InvalidUpstream(e.to_string()))?;

        Ok(ProxySessionHandle {
            session_id: spec.session_id,
            slot_index: spec.slot_index,
            local_port: spec.local_port,
            browser_proxy_url,
            exit_ip: self.fake_ip,
            latency: self.latency,
            config_path: None,
            child: None,
        })
    }

    async fn check_proxy(
        &self,
        spec: SessionProxySpec,
    ) -> Result<VerifiedProxySession, ProxyRuntimeError> {
        if self.should_fail {
            return Err(ProxyRuntimeError::AuthenticationFailed(
                "Fake 代理检测失败".to_string(),
            ));
        }

        let browser_proxy_url = Url::parse(&format!("http://127.0.0.1:{}", spec.local_port))
            .map_err(|e| ProxyRuntimeError::InvalidUpstream(e.to_string()))?;

        Ok(VerifiedProxySession {
            browser_proxy_url,
            exit_ip: self.fake_ip,
            latency: self.latency,
        })
    }
}
