//! Worker Agent 配置（第 16.1 节、V3 方案第 7 节）。
//!
//! Worker 本地配置极其精简：包含连接 Master 的地址、证书路径、数据目录以及 NAS 挂载路径。
//! 节奏参数（心跳间隔、续租间隔、节流参数等）全部由 Master 动态下发。

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Worker Agent 本地配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerConfig {
    /// 连接 Master 的配置。
    pub master: MasterLinkConfig,
    /// 存储与工作目录配置。
    #[serde(default)]
    pub storage: StorageConfig,
    /// 槽位与并发设置。
    #[serde(default)]
    pub execution: ExecutionConfig,
}

/// 连接 Master 的配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasterLinkConfig {
    /// Master gRPC 地址（如 https://worker.example.com 或 http://127.0.0.1:9443）。
    pub endpoint: String,
    /// 注册专用 gRPC 地址（可选，默认同 endpoint）。
    pub enroll_endpoint: Option<String>,
    /// 本地持久化身份文件（保存 node_id、node_token 与指纹，JSON 格式，禁止存私钥）。
    #[serde(default = "default_identity_file")]
    pub identity_file: PathBuf,
    /// 客户端证书路径（PKCS#8 PEM 格式）。
    #[serde(default = "default_client_cert_file")]
    pub client_cert_file: PathBuf,
    /// 客户端私钥路径（PKCS#8 PEM 格式，必须 0600 权限）。
    #[serde(default = "default_client_key_file")]
    pub client_key_file: PathBuf,
    /// Master 签发的 Node CA 证书路径（用于 mTLS 双向校验）。
    #[serde(default = "default_node_ca_file")]
    pub node_ca_file: PathBuf,
    /// 云端服务端 CA 证书路径（可选，当服务端使用内部自签证书时配置）。
    pub server_ca_file: Option<PathBuf>,
    /// 域名覆盖（用于 TLS SNI 校验）。
    pub tls_domain: Option<String>,
    /// **仅本地开发**：允许 http 明文直连 Master，不要求客户端证书/服务端 CA。
    ///
    /// 生产必须保持 `false`（默认）——`run` 会 fail-closed 要求 HTTPS + mTLS；
    /// 本开关只是让「本机无 Caddy 的联调」能跑起来，身份（node_token）校验仍生效。
    #[serde(default)]
    pub insecure: bool,
}

fn default_identity_file() -> PathBuf {
    PathBuf::from("data/identity.json")
}

fn default_client_cert_file() -> PathBuf {
    PathBuf::from("data/client.crt")
}

fn default_client_key_file() -> PathBuf {
    PathBuf::from("data/client.key")
}

fn default_node_ca_file() -> PathBuf {
    PathBuf::from("data/node_ca.crt")
}

/// 证书与凭据物理路径集合。
#[derive(Debug, Clone)]
pub struct WorkerIdentityPaths {
    pub identity_file: PathBuf,
    pub client_key_file: PathBuf,
    pub client_cert_file: PathBuf,
    pub node_ca_file: PathBuf,
    pub server_ca_file: Option<PathBuf>,
}

/// 存储目录配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// 本地数据目录（用于 SQLite、暂存文件、Profile 缓存）。
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// 局域网 NAS 挂载点根目录（不同操作系统路径不同）。
    #[serde(default = "default_nas_mount")]
    pub nas_mount: PathBuf,
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("data")
}

fn default_nas_mount() -> PathBuf {
    PathBuf::from("/mnt/nas")
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            nas_mount: default_nas_mount(),
        }
    }
}

/// 执行参数配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConfig {
    /// 请求开启的最大槽位数。
    #[serde(default = "default_requested_slots")]
    pub requested_slots: u32,
    /// 是否使用模拟自动化引擎（用于测试或无桌面环境）。
    #[serde(default)]
    pub simulated: bool,
}

fn default_requested_slots() -> u32 {
    5
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            requested_slots: default_requested_slots(),
            simulated: false,
        }
    }
}

/// 节点已保存的长期身份（禁止包含私钥）。
///
/// V5 直连注册（第 6.9 节）新增字段：
/// - `installation_id`：Worker 首次运行生成的随机 UUID，重启复用，杜绝重复建节点；
/// - `registration_session` / `registration_challenge`：待审核期的短期注册会话
///   与挑战值（明文只写本机 0600 身份文件；批准领取后清空）；
/// - `status`：本地视角的注册状态（待审核 / 已批准），便于识别身份异常。
///
/// 证书与 Node CA 的 PEM 只作进程内传递，**不序列化**进身份文件
/// （证书正文落 client.crt / node_ca.crt，身份文件只记指纹）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedIdentity {
    /// 节点 UUID。
    pub node_id: String,
    /// 节点通信长期凭据。
    pub node_token: String,
    /// 节点名称。
    pub node_name: Option<String>,
    /// 客户端证书 SHA-256 指纹。
    pub certificate_fingerprint: Option<String>,
    /// V5：Worker 安装标识（唯一身份，重启复用）。
    #[serde(default)]
    pub installation_id: Option<String>,
    /// V5：待审核期注册会话令牌（明文只在本机，批准后清空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_session: Option<String>,
    /// V5：服务端挑战值（私钥持有证明用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_challenge: Option<String>,
    /// V5：本地注册状态（待审核 / 已批准）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// 批准后领取的客户端证书 PEM（仅进程内传递，不落盘到身份文件）。
    #[serde(skip)]
    pub client_certificate_pem: Option<String>,
    /// 批准后领取的 Node CA PEM（仅进程内传递，不落盘到身份文件）。
    #[serde(skip)]
    pub ca_certificate_pem: Option<String>,
}

impl WorkerConfig {
    /// 读取本地配置文件。
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("读取 Worker 配置文件失败：{}", path.display()))?;
        let mut config: Self = toml::from_str(&text)
            .with_context(|| format!("解析 Worker 配置文件失败：{}", path.display()))?;
        config.apply_env();
        config.validate()?;
        Ok(config)
    }

    /// 环境变量覆盖。
    pub fn apply_env(&mut self) {
        if let Ok(ep) = std::env::var("MASTER_ENDPOINT") {
            if !ep.is_empty() {
                self.master.endpoint = ep;
            }
        }
        if let Ok(ep) = std::env::var("MASTER_ENROLL_ENDPOINT") {
            if !ep.is_empty() {
                self.master.enroll_endpoint = Some(ep);
            }
        }
        if let Ok(nas) = std::env::var("NAS_MOUNT") {
            if !nas.is_empty() {
                self.storage.nas_mount = PathBuf::from(nas);
            }
        }
    }

    /// 获取凭据与证书路径结构。
    pub fn identity_paths(&self) -> WorkerIdentityPaths {
        WorkerIdentityPaths {
            identity_file: self.master.identity_file.clone(),
            client_key_file: self.master.client_key_file.clone(),
            client_cert_file: self.master.client_cert_file.clone(),
            node_ca_file: self.master.node_ca_file.clone(),
            server_ca_file: self.master.server_ca_file.clone(),
        }
    }

    /// 校验配置与权限（enroll 与 run 共用：只做结构性校验）。
    pub fn validate(&self) -> Result<()> {
        if self.master.endpoint.trim().is_empty() {
            bail!("Master gRPC endpoint 不能为空");
        }
        let endpoint = self.master.endpoint.trim().to_ascii_lowercase();
        if !(endpoint.starts_with("https://") || endpoint.starts_with("http://")) {
            bail!("Master endpoint 必须以 https:// 或 http:// 开头（生产必须 https）");
        }
        if endpoint.contains(".invalid")
            || endpoint.contains(".example")
            || endpoint.contains("example.com")
        {
            bail!("Master endpoint 是占位域名，必须改成真实地址后再启动");
        }

        // 校验客户端私钥权限（Unix 下必须为 0600/0400；收紧失败必须报错而不是继续）
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if self.master.client_key_file.exists() {
                let meta = std::fs::metadata(&self.master.client_key_file)?;
                let mode = meta.permissions().mode() & 0o777;
                if mode != 0o600 && mode != 0o400 {
                    tracing::warn!(
                        path = %self.master.client_key_file.display(),
                        mode = format!("{mode:o}"),
                        "客户端私钥文件权限过宽，正在收紧为 0600"
                    );
                    std::fs::set_permissions(
                        &self.master.client_key_file,
                        std::fs::Permissions::from_mode(0o600),
                    )
                    .with_context(|| {
                        format!(
                            "无法收紧私钥文件权限（{}）：fail closed",
                            self.master.client_key_file.display()
                        )
                    })?;
                }
            }
        }

        Ok(())
    }

    /// 运行前 fail-closed 校验（V4 方案第 8.3 节）。
    ///
    /// `worker-agent run` 启动前必须满足：
    /// - endpoint 是 HTTPS（`insecure=true` 的本地开发除外）；
    /// - 身份、客户端证书、客户端私钥存在（insecure 时证书非必需）；
    /// - 私有 Server CA 配置了但文件不存在：失败；
    /// - 证书/私钥不匹配或证书已过期：由 tls.rs 在建立通道时校验。
    pub fn validate_run_ready(&self) -> Result<()> {
        self.validate()?;
        if self.master.insecure {
            // 本地开发：允许 http 明文直连；身份（node_token）仍然必需。
            if !self.master.identity_file.is_file() {
                bail!(
                    "身份文件不存在（{}），请先执行 `worker-agent enroll --code <注册码>`",
                    self.master.identity_file.display()
                );
            }
            return Ok(());
        }
        if !self.master.endpoint.trim().starts_with("https://") {
            bail!("生产模式下 WorkerLink endpoint 必须是 https://，拒绝明文连接");
        }
        if !self.master.identity_file.is_file() {
            bail!(
                "身份文件不存在（{}），请先执行 `worker-agent enroll --code <注册码>`",
                self.master.identity_file.display()
            );
        }
        if !self.master.client_cert_file.is_file() {
            bail!(
                "客户端证书不存在（{}）：mTLS 缺配置时启动必须失败",
                self.master.client_cert_file.display()
            );
        }
        if !self.master.client_key_file.is_file() {
            bail!(
                "客户端私钥不存在（{}）：mTLS 缺配置时启动必须失败",
                self.master.client_key_file.display()
            );
        }
        if let Some(server_ca) = &self.master.server_ca_file {
            if !server_ca.is_file() {
                bail!(
                    "已配置 server_ca_file 但文件不存在（{}）：私有 Server CA 部署必须预先分发根证书",
                    server_ca.display()
                );
            }
        }
        // Node CA 只用于审计，不能充当服务端信任根；配置了但缺失应告警而不是失败
        if !self.master.node_ca_file.is_file() {
            tracing::warn!(
                path = %self.master.node_ca_file.display(),
                "Node CA 文件缺失（仅影响审计，不影响 TLS 握手）"
            );
        }
        Ok(())
    }

    /// 读取已保存的身份。
    pub fn load_identity(&self) -> Result<Option<SavedIdentity>> {
        let path = &self.master.identity_file;
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(path)?;
        let id: SavedIdentity = serde_json::from_str(&text)?;
        Ok(Some(id))
    }

    /// 持久化保存身份。
    pub fn save_identity(&self, identity: &SavedIdentity) -> Result<()> {
        let path = &self.master.identity_file;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(identity)?;

        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(path)
                .with_context(|| format!("写入身份文件失败：{}", path.display()))?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(path, text)
                .with_context(|| format!("写入身份文件失败：{}", path.display()))?;
        }
        Ok(())
    }
}
