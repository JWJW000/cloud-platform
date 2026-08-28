//! GOST 代理运行时适配器（方案第 5.1 节）。
//!
//! 生产环境通过启动隔离的 GOST 进程承担 HTTP/CONNECT 代理协议与身份认证转发。

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::net::TcpStream;
use tokio::process::Command;
use url::Url;

use super::{
    ProxyRuntime, ProxyRuntimeError, ProxySessionHandle, SessionProxySpec, VerifiedProxySession,
};

/// 允许探测的出口 IP 检测端点（安全白名单）。
const IP_CHECK_URLS: &[&str] = &[
    "https://api.ipify.org",
    "https://ifconfig.me/ip",
    "https://icanhazip.com",
];

/// 生产 GOST 代理运行时实现。
#[derive(Clone)]
pub struct GostProxyRuntime {
    gost_bin: PathBuf,
    work_dir: PathBuf,
}

impl GostProxyRuntime {
    pub fn new(work_dir: PathBuf) -> Self {
        // 查找优先级：
        // 1. 同级目录下的 gost / gost.exe
        // 2. PATH 环境变量中的 gost
        let current_exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()));
        let mut gost_bin = PathBuf::from(if cfg!(windows) { "gost.exe" } else { "gost" });

        if let Some(dir) = current_exe_dir {
            let local_bin = dir.join(if cfg!(windows) { "gost.exe" } else { "gost" });
            if local_bin.exists() {
                gost_bin = local_bin;
            }
        }

        Self { gost_bin, work_dir }
    }

    pub fn with_custom_bin(gost_bin: PathBuf, work_dir: PathBuf) -> Self {
        Self { gost_bin, work_dir }
    }

    /// 写入私有配置文件，严格限制权限（Unix 0600）。
    fn write_private_config(
        &self,
        spec: &SessionProxySpec,
    ) -> Result<PathBuf, ProxyRuntimeError> {
        let configs_dir = self.work_dir.join("proxy_configs");
        std::fs::create_dir_all(&configs_dir)?;

        let filename = format!("gost-session-{}.yml", spec.session_id);
        let config_path = configs_dir.join(filename);

        // 构造 GOST v2 / v3 兼容 YAML 配置
        let upstream = &spec.upstream;
        let scheme = if upstream.scheme.trim().is_empty() {
            "http"
        } else {
            &upstream.scheme
        };

        let u_str = upstream.username.trim();
        let p_str = upstream.password.trim();

        let auth_str = if !u_str.is_empty() && !p_str.is_empty() {
            format!("{}:{}@", percent_encoding::utf8_percent_encode(u_str, percent_encoding::NON_ALPHANUMERIC), percent_encoding::utf8_percent_encode(p_str, percent_encoding::NON_ALPHANUMERIC))
        } else if !u_str.is_empty() {
            format!("{}@", percent_encoding::utf8_percent_encode(u_str, percent_encoding::NON_ALPHANUMERIC))
        } else {
            String::new()
        };

        let forward_node = format!("{}://{}{}:{}", scheme, auth_str, upstream.host, upstream.port);
        let local_addr = format!("127.0.0.1:{}", spec.local_port);

        let yaml_content = format!(
            "services:\n  - name: slot-{}\n    addr: {}\n    handler:\n      type: http\n    listener:\n      type: tcp\n    forwarder:\n      nodes:\n        - name: upstream\n          addr: {}\n",
            spec.slot_index, local_addr, forward_node
        );

        #[cfg(unix)]
        {
            use std::fs::OpenOptions;
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;

            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .open(&config_path)
                .map_err(|e| ProxyRuntimeError::ConfigWriteFailed(e.to_string()))?;

            file.write_all(yaml_content.as_bytes())
                .map_err(|e| ProxyRuntimeError::ConfigWriteFailed(e.to_string()))?;
        }

        #[cfg(not(unix))]
        {
            std::fs::write(&config_path, yaml_content)
                .map_err(|e| ProxyRuntimeError::ConfigWriteFailed(e.to_string()))?;
        }

        Ok(config_path)
    }

    /// 探测本地 listener 是否已绑定并准备就绪。
    async fn wait_listener_ready(&self, port: u16, timeout: Duration) -> Result<(), ProxyRuntimeError> {
        let deadline = Instant::now() + timeout;
        let addr = SocketAddr::from(([127, 0, 0, 1], port));

        while Instant::now() < deadline {
            match TcpStream::connect(addr).await {
                Ok(_) => return Ok(()),
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }

        Err(ProxyRuntimeError::ListenerTimeout(port))
    }

    /// 通过本地代理测试公网连通性并获取出口 IP。
    async fn probe_exit_ip(&self, local_port: u16, timeout: Duration) -> Result<(Option<IpAddr>, Duration), ProxyRuntimeError> {
        let proxy_url = format!("http://127.0.0.1:{}", local_port);
        let proxy = ureq::Proxy::new(&proxy_url)
            .map_err(|e| ProxyRuntimeError::ProbeFailed(format!("无效的代理地址: {e}")))?;

        let agent = ureq::AgentBuilder::new()
            .proxy(proxy)
            .timeout(timeout)
            .build();

        let start = Instant::now();
        let mut last_err = String::new();

        for endpoint in IP_CHECK_URLS {
            match agent.get(endpoint).call() {
                Ok(resp) => {
                    let latency = start.elapsed();
                    let body = resp.into_string().unwrap_or_default();
                    let ip_str = body.trim();
                    if let Ok(ip) = ip_str.parse::<IpAddr>() {
                        return Ok((Some(ip), latency));
                    }
                }
                Err(e) => {
                    last_err = format!("{e}");
                }
            }
        }

        // 如果白名单端点均未能成功获取 IP，但连接通畅，回退记录耗时
        if !last_err.is_empty() {
            Err(ProxyRuntimeError::ProbeFailed(format!("出口 IP 探测失败: {last_err}")))
        } else {
            Ok((None, start.elapsed()))
        }
    }
}

#[async_trait]
impl ProxyRuntime for GostProxyRuntime {
    async fn start_verified(
        &self,
        spec: SessionProxySpec,
    ) -> Result<ProxySessionHandle, ProxyRuntimeError> {
        if spec.upstream.host.trim().is_empty() || spec.upstream.port <= 0 {
            return Err(ProxyRuntimeError::InvalidUpstream(format!(
                "主机或端口非法: {}:{}",
                spec.upstream.host, spec.upstream.port
            )));
        }

        let config_path = self.write_private_config(&spec)?;

        // 启动 GOST 子进程，参数只传入配置文件路径，凭据绝不出现在命令行参数或环境变量中
        let mut cmd = Command::new(&self.gost_bin);
        cmd.arg("-C")
            .arg(&config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let mut child = cmd
            .spawn()
            .map_err(|e| ProxyRuntimeError::ProcessSpawnFailed(format!("无法执行 GOST ({}): {e}", self.gost_bin.display())))?;

        // 检查进程启动后是否立即退出
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Ok(Some(status)) = child.try_wait() {
            let _ = std::fs::remove_file(&config_path);
            return Err(ProxyRuntimeError::ProcessExited(status.code()));
        }

        // 等待本地 listener 就绪
        if let Err(e) = self.wait_listener_ready(spec.local_port, Duration::from_secs(3)).await {
            let _ = child.kill().await;
            let _ = std::fs::remove_file(&config_path);
            return Err(e);
        }

        // 探测出口 IP 与延迟
        let (exit_ip, latency) = match self.probe_exit_ip(spec.local_port, Duration::from_secs(8)).await {
            Ok(res) => res,
            Err(e) => {
                let _ = child.kill().await;
                let _ = std::fs::remove_file(&config_path);
                return Err(e);
            }
        };

        let browser_proxy_url = Url::parse(&format!("http://127.0.0.1:{}", spec.local_port))
            .map_err(|e| ProxyRuntimeError::InvalidUpstream(e.to_string()))?;

        Ok(ProxySessionHandle {
            session_id: spec.session_id,
            slot_index: spec.slot_index,
            local_port: spec.local_port,
            browser_proxy_url,
            exit_ip,
            latency,
            config_path: Some(config_path),
            child: Some(child),
        })
    }

    async fn check_proxy(
        &self,
        spec: SessionProxySpec,
    ) -> Result<VerifiedProxySession, ProxyRuntimeError> {
        let mut handle = self.start_verified(spec).await?;
        let result = VerifiedProxySession {
            browser_proxy_url: handle.browser_proxy_url().clone(),
            exit_ip: handle.exit_ip(),
            latency: handle.latency(),
        };
        handle.shutdown().await;
        Ok(result)
    }
}
