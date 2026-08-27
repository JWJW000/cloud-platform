//! Master 配置（第 16.1 节）。
//!
//! 配置文件是唯一的部署入口，敏感项允许用环境变量覆盖，
//! 这样 Docker Compose 里可以只把密钥放在 `.env` 而不写进镜像。

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// 完整的 Master 配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasterConfig {
    /// 监听设置。
    #[serde(default)]
    pub server: ServerConfig,
    /// 数据库连接。
    pub database: DatabaseConfig,
    /// 安全相关密钥与证书。
    pub security: SecurityConfig,
    /// 调度节奏参数。
    #[serde(default)]
    pub scheduler: SchedulerConfig,
    /// NAS 入库校验参数。
    #[serde(default)]
    pub nas: NasConfig,
    /// Webshare 代理同步。
    #[serde(default)]
    pub webshare: WebshareConfig,
    /// OpenSearch 书目搜索投影。
    #[serde(default)]
    pub opensearch: OpenSearchConfig,
}

/// OpenSearch 搜索投影配置。
#[derive(Clone, Serialize, Deserialize)]
pub struct OpenSearchConfig {
    /// 是否启用 OpenSearch；关闭时检索完全回退 PostgreSQL。
    #[serde(default)]
    pub enabled: bool,
    /// OpenSearch 根地址。容器内可使用 `http://opensearch:9200`，公网必须使用 HTTPS。
    #[serde(default = "default_opensearch_url")]
    pub url: String,
    /// 搜索索引名。
    #[serde(default = "default_opensearch_index")]
    pub index: String,
    /// 可选 Basic Auth 用户名。
    #[serde(default)]
    pub username: String,
    /// 可选 Basic Auth 密码；只允许从部署配置或环境变量注入，禁止写入日志。
    #[serde(default)]
    pub password: String,
    /// 单次 HTTP 请求超时秒数。
    #[serde(default = "default_opensearch_timeout_secs")]
    pub timeout_secs: u64,
    /// Outbox 单批处理数量。
    #[serde(default = "default_opensearch_batch_size")]
    pub batch_size: usize,
    /// Outbox 空闲轮询间隔毫秒。
    #[serde(default = "default_opensearch_poll_millis")]
    pub poll_millis: u64,
}

impl std::fmt::Debug for OpenSearchConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenSearchConfig")
            .field("enabled", &self.enabled)
            .field("url", &self.url)
            .field("index", &self.index)
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .field("timeout_secs", &self.timeout_secs)
            .field("batch_size", &self.batch_size)
            .field("poll_millis", &self.poll_millis)
            .finish()
    }
}

impl Default for OpenSearchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: default_opensearch_url(),
            index: default_opensearch_index(),
            username: String::new(),
            password: String::new(),
            timeout_secs: default_opensearch_timeout_secs(),
            batch_size: default_opensearch_batch_size(),
            poll_millis: default_opensearch_poll_millis(),
        }
    }
}

fn default_opensearch_url() -> String {
    "http://opensearch:9200".to_string()
}

fn default_opensearch_index() -> String {
    "catalog-editions-v1".to_string()
}

fn default_opensearch_timeout_secs() -> u64 {
    10
}

fn default_opensearch_batch_size() -> usize {
    250
}

fn default_opensearch_poll_millis() -> u64 {
    1_000
}

/// 监听设置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// 管理后台 HTTP 监听地址（反向代理在其前面终止 TLS）。
    pub http_listen: String,
    /// Worker gRPC 监听地址。
    pub grpc_listen: String,
    /// 管理后台静态文件目录；为空表示只提供 API。
    pub web_root: Option<PathBuf>,
    /// 站点基础地址，下发给 Worker。
    pub site_base: String,
    /// 允许跨域的 Origin 列表（为空时不启用宽松 CORS）。
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    /// 可信反向代理地址（CIDR 或精确 IP，V4 第 13.4 节）。
    ///
    /// 生产环境必须配置 Caddy 所在网段；只有来自这些地址的
    /// `X-Forwarded-For` 才被信任用于限流与审计，客户端伪造头一律忽略。
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            http_listen: "0.0.0.0:8080".to_string(),
            grpc_listen: "0.0.0.0:9443".to_string(),
            web_root: None,
            // 故意留空而不是填占位域名：占位域名会一路下发到 Worker，
            // 直到浏览器解析失败才暴露，而空值在 Worker 侧就被明确拒绝（第 3.3 节）。
            site_base: String::new(),
            allowed_origins: Vec::new(),
            trusted_proxies: Vec::new(),
        }
    }
}

/// 数据库连接。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// PostgreSQL 连接串。
    pub url: String,
    /// 连接池上限。
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
    /// 启动时是否自动执行迁移。
    #[serde(default = "default_true")]
    pub auto_migrate: bool,
}

fn default_max_connections() -> u32 {
    20
}

fn default_true() -> bool {
    true
}

/// 安全配置（第 15 节）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// 管理后台会话签名密钥。
    pub jwt_secret: String,
    /// 会话有效小时数。
    #[serde(default = "default_jwt_hours")]
    pub jwt_hours: i64,
    /// 字段级加密密钥：base64 编码的 32 字节（AES-256-GCM）。
    pub field_key_base64: String,
    /// 节点 CA 证书路径；不存在时自动生成自签 CA。
    pub ca_cert_path: PathBuf,
    /// 节点 CA 私钥路径。
    pub ca_key_path: PathBuf,
    /// 签发给节点的证书有效天数。
    #[serde(default = "default_cert_days")]
    pub node_cert_days: i64,
    /// 是否强制要求 Worker 出示客户端证书（mTLS）。
    ///
    /// TLS 在反向代理处终止（第 18 节的部署形态），因此 Master 进程本身看不到
    /// 对端证书，只能读代理透传的 `x-client-cert-fingerprint`。默认 `false`
    /// 是为了让「先跑起来」的单机部署不必先配一套 mTLS；
    /// 一旦开启，缺少指纹或指纹不属于该节点的连接会被直接拒绝。
    ///
    /// 无论开关如何，**指纹存在时一定会校验**——放行一个对不上号的指纹
    /// 比根本不检查更糟：它会让人误以为 mTLS 在生效。
    #[serde(default = "default_true")]
    pub require_client_cert: bool,
    /// 会话 Cookie 是否带 `Secure` 属性（V4 第 13.1 节）。
    ///
    /// 生产必须为 `true`；本地 HTTP 调试可设为 `false` 让浏览器接受 Cookie。
    #[serde(default = "default_true")]
    pub cookie_secure: bool,
}

fn default_jwt_hours() -> i64 {
    12
}

fn default_cert_days() -> i64 {
    365
}

fn default_session_max_duration_secs() -> u64 {
    2 * 3600
}

/// 调度节奏（第 3.4 / 6.4 节的时间常量集中在此，便于压测调参）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerConfig {
    /// 心跳间隔。
    pub heartbeat_interval_secs: u64,
    /// 会话续租间隔。
    pub session_renew_secs: u64,
    /// 任务租约时长。
    pub task_lease_secs: u64,
    /// 断线保护窗口。
    pub disconnect_protection_secs: u64,
    /// 下载停滞判定时长。
    pub stall_timeout_secs: u64,
    /// 回收巡检间隔。
    pub reaper_interval_secs: u64,
    /// 单任务最大尝试次数。
    pub max_attempts: i32,
    /// 逐次退避秒数，超出长度后沿用最后一项。
    pub retry_backoff_secs: Vec<u64>,
    /// 单会话最多下载多少本后主动结束。
    pub session_max_downloads: i32,
    /// 单会话硬性时长上限（秒），下发给 Worker。
    ///
    /// 与 `session_max_downloads` 是两条独立的收尾线：数量上限管的是「账号被用得太狠」，
    /// 时长上限管的是「浏览器实例活得太久」——长跑的实例会累积内存与 Cookie 状态，
    /// 一本都没下完也应该定期换新。
    #[serde(default = "default_session_max_duration_secs")]
    pub session_max_duration_secs: u64,
    /// 进度上报最小间隔（下发给 Worker）。
    pub progress_min_interval_secs: u64,
    /// 进度上报最小字节增量（下发给 Worker）。
    pub progress_min_bytes: u64,
    /// 节点离线判定：超过心跳间隔的倍数。
    pub offline_missed_heartbeats: u32,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval_secs: 15,
            session_renew_secs: 30,
            task_lease_secs: 120,
            disconnect_protection_secs: 900,
            stall_timeout_secs: 120,
            reaper_interval_secs: 5,
            max_attempts: 3,
            retry_backoff_secs: vec![60, 300, 900],
            session_max_downloads: 10,
            session_max_duration_secs: default_session_max_duration_secs(),
            progress_min_interval_secs: 2,
            progress_min_bytes: 5 * 1024 * 1024,
            offline_missed_heartbeats: 3,
        }
    }
}

impl SchedulerConfig {
    /// 第 `attempt` 次失败后的退避时长（`attempt` 从 1 开始）。
    pub fn backoff_for(&self, attempt: i32) -> Duration {
        if self.retry_backoff_secs.is_empty() {
            return Duration::from_secs(60);
        }
        let index = (attempt.max(1) as usize - 1).min(self.retry_backoff_secs.len() - 1);
        Duration::from_secs(self.retry_backoff_secs[index])
    }

    /// 判定节点离线的心跳超时。
    pub fn offline_after(&self) -> Duration {
        Duration::from_secs(self.heartbeat_interval_secs * self.offline_missed_heartbeats as u64)
    }
}

/// NAS 入库校验参数（第 9.2 节）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NasConfig {
    /// 低于此大小视为站点错误页。
    pub minimum_file_bytes: u64,
    /// NAS 剩余空间低于此值时告警。
    pub free_space_alert_gb: i64,
}

impl Default for NasConfig {
    fn default() -> Self {
        Self {
            minimum_file_bytes: 32 * 1024,
            free_space_alert_gb: 50,
        }
    }
}

/// Webshare 同步配置（第 15.3 节：API Key 只存在于 Master）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebshareConfig {
    /// 是否启用定时同步。
    #[serde(default)]
    pub enabled: bool,
    /// API Key。
    #[serde(default)]
    pub api_key: String,
    /// 同步间隔分钟数。
    #[serde(default = "default_sync_minutes")]
    pub sync_minutes: u64,
    /// 代理故障后的冷却分钟数。
    #[serde(default = "default_cooldown_minutes")]
    pub cooldown_minutes: u64,
}

fn default_sync_minutes() -> u64 {
    30
}

fn default_cooldown_minutes() -> u64 {
    10
}

impl MasterConfig {
    /// 读取配置文件并应用环境变量覆盖。
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("读取配置文件失败：{}", path.display()))?;
        let mut config: Self = toml::from_str(&text)
            .with_context(|| format!("解析配置文件失败：{}", path.display()))?;
        config.apply_env();
        config.validate()?;
        Ok(config)
    }

    /// 环境变量覆盖：容器部署时只需注入密钥。
    pub fn apply_env(&mut self) {
        if let Ok(url) = std::env::var("DATABASE_URL") {
            if !url.is_empty() {
                self.database.url = url;
            }
        }
        if let Ok(secret) = std::env::var("MASTER_JWT_SECRET") {
            if !secret.is_empty() {
                self.security.jwt_secret = secret;
            }
        }
        if let Ok(key) = std::env::var("MASTER_FIELD_KEY") {
            if !key.is_empty() {
                self.security.field_key_base64 = key;
            }
        }
        if let Ok(key) = std::env::var("WEBSHARE_API_KEY") {
            if !key.is_empty() {
                self.webshare.api_key = key;
                self.webshare.enabled = true;
            }
        }
        if let Ok(enabled) = std::env::var("OPENSEARCH_ENABLED") {
            self.opensearch.enabled = enabled == "1" || enabled.eq_ignore_ascii_case("true");
        }
        if let Ok(url) = std::env::var("OPENSEARCH_URL") {
            if !url.trim().is_empty() {
                self.opensearch.url = url;
            }
        }
        if let Ok(index) = std::env::var("OPENSEARCH_INDEX") {
            if !index.trim().is_empty() {
                self.opensearch.index = index;
            }
        }
        if let Ok(username) = std::env::var("OPENSEARCH_USERNAME") {
            self.opensearch.username = username;
        }
        if let Ok(password) = std::env::var("OPENSEARCH_PASSWORD") {
            self.opensearch.password = password;
        }
        if let Ok(site) = std::env::var("MASTER_SITE_BASE") {
            if !site.is_empty() {
                self.server.site_base = site;
            }
        }
        if let Ok(require_cert) = std::env::var("MASTER_REQUIRE_CLIENT_CERT") {
            if require_cert == "1" || require_cert.eq_ignore_ascii_case("true") {
                self.security.require_client_cert = true;
            } else if require_cert == "0" || require_cert.eq_ignore_ascii_case("false") {
                self.security.require_client_cert = false;
            }
        }
        if let Ok(origins) = std::env::var("MASTER_ALLOWED_ORIGINS") {
            if !origins.trim().is_empty() {
                self.server.allowed_origins = origins
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
        if let Ok(proxies) = std::env::var("MASTER_TRUSTED_PROXIES") {
            if !proxies.trim().is_empty() {
                self.server.trusted_proxies = proxies
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
        if let Ok(secure) = std::env::var("MASTER_COOKIE_SECURE") {
            if secure == "0" || secure.eq_ignore_ascii_case("false") {
                self.security.cookie_secure = false;
            }
        }
    }

    fn validate(&self) -> Result<()> {
        if self.database.url.trim().is_empty() {
            bail!("数据库连接串为空，请在配置文件或 DATABASE_URL 中提供");
        }
        // V4 第 13.6 节：JWT secret 至少 32 个随机字节，不接受示例弱密钥用于生产
        if self.security.jwt_secret.len() < 32 {
            bail!("会话签名密钥过短：至少 32 个字符（推荐 64 字节随机串）");
        }
        for weak in [
            "0123456789abcdef",
            "0123456789abcdef0123456789abcdef",
            "change-me",
            "changeme",
            "secret",
            "password",
        ] {
            if self.security.jwt_secret.contains(weak) {
                bail!("会话签名密钥是示例弱密钥，生产启动必须拒绝（第 13.6 节）");
            }
        }
        if self.scheduler.task_lease_secs <= self.scheduler.session_renew_secs {
            bail!(
                "任务租约（{} 秒）必须长于会话续租间隔（{} 秒），否则续租来不及",
                self.scheduler.task_lease_secs,
                self.scheduler.session_renew_secs
            );
        }
        if self.scheduler.session_max_duration_secs <= self.scheduler.task_lease_secs {
            bail!(
                "会话时长上限（{} 秒）必须长于任务租约（{} 秒），否则会话会在第一本书下完前就被叫停",
                self.scheduler.session_max_duration_secs,
                self.scheduler.task_lease_secs
            );
        }
        // 站点地址允许暂时为空（还没配），但绝不允许是占位域名：
        // 空值会被 Worker 明确拒绝，而 `.invalid` 会伪装成「已经配好了」。
        let site = self.server.site_base.trim().to_ascii_lowercase();
        if !site.is_empty() {
            if !(site.starts_with("http://") || site.starts_with("https://")) {
                bail!("站点地址必须以 http:// 或 https:// 开头，当前为 {site}");
            }
            if site.contains(".invalid")
                || site.contains(".example")
                || site.contains("example.com")
            {
                bail!("站点地址 {site} 是占位域名，必须改成真实站点地址后再启动（第 3.3 节）");
            }
        }
        if self.nas.minimum_file_bytes == 0 {
            bail!("最小文件字节数不能为 0：站点错误页往往只有几 KB，阈值为 0 等于取消这道闸门");
        }
        if self.opensearch.enabled {
            let url = url::Url::parse(&self.opensearch.url).context("OpenSearch URL 格式无效")?;
            if !matches!(url.scheme(), "http" | "https") {
                bail!("OpenSearch URL 只允许 http/https");
            }
            let host = url.host_str().unwrap_or_default();
            if url.scheme() == "http"
                && !matches!(host, "opensearch" | "localhost" | "127.0.0.1" | "::1")
            {
                bail!("非本机或 Docker 内网的 OpenSearch 必须使用 HTTPS");
            }
            if self.opensearch.index.is_empty()
                || self.opensearch.index.len() > 128
                || self.opensearch.index.starts_with(['_', '-', '+'])
                || !self
                    .opensearch
                    .index
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
            {
                bail!("OpenSearch 索引名无效：只允许小写字母、数字、-、_");
            }
            if self.opensearch.timeout_secs == 0 || self.opensearch.timeout_secs > 120 {
                bail!("OpenSearch 请求超时必须在 1..=120 秒之间");
            }
            if !(1..=2_000).contains(&self.opensearch.batch_size) {
                bail!("OpenSearch 批量大小必须在 1..=2000 之间");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MasterConfig {
        MasterConfig {
            server: ServerConfig::default(),
            database: DatabaseConfig {
                url: "postgres://localhost/x".to_string(),
                max_connections: 5,
                auto_migrate: true,
            },
            security: SecurityConfig {
                jwt_secret: "kX8pQ2mN7vR4tW9yA3cF6hJ1lS5uB0eG".to_string(),
                jwt_hours: 12,
                field_key_base64: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string(),
                ca_cert_path: PathBuf::from("ca.crt"),
                ca_key_path: PathBuf::from("ca.key"),
                node_cert_days: 365,
                require_client_cert: false,
                cookie_secure: true,
            },
            scheduler: SchedulerConfig::default(),
            nas: NasConfig::default(),
            webshare: WebshareConfig::default(),
            opensearch: OpenSearchConfig::default(),
        }
    }

    #[test]
    fn backoff_is_monotonic_and_saturates() {
        let sched = SchedulerConfig::default();
        assert_eq!(sched.backoff_for(1), Duration::from_secs(60));
        assert_eq!(sched.backoff_for(2), Duration::from_secs(300));
        assert_eq!(sched.backoff_for(3), Duration::from_secs(900));
        // 超出配置长度后沿用最后一项，而不是回到最小值
        assert_eq!(sched.backoff_for(9), Duration::from_secs(900));
    }

    #[test]
    fn lease_must_outlive_renew_interval() {
        let mut config = sample();
        config.scheduler.task_lease_secs = 10;
        config.scheduler.session_renew_secs = 30;
        assert!(config.validate().is_err());
    }

    #[test]
    fn short_secret_is_rejected() {
        let mut config = sample();
        config.security.jwt_secret = "短".to_string();
        assert!(config.validate().is_err());
        // 少于 32 字节也拒绝
        config.security.jwt_secret = "short-secret".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn example_weak_secret_is_rejected_for_production() {
        // V4 第 13.6 节：示例弱密钥不得被生产启动接受
        let mut config = sample();
        config.security.jwt_secret = "0123456789abcdef0123456789abcdef".to_string();
        assert!(config.validate().is_err());
        config.security.jwt_secret = "change-me-please".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn session_duration_must_outlive_a_single_task_lease() {
        let mut config = sample();
        config.scheduler.session_max_duration_secs = 60;
        config.scheduler.task_lease_secs = 120;
        assert!(config.validate().is_err());
    }

    #[test]
    fn client_certificates_are_optional_by_default() {
        // TLS 在反向代理终止，单机部署应当能不配 mTLS 直接跑起来
        assert!(!sample().security.require_client_cert);
    }

    #[test]
    fn placeholder_site_base_refuses_to_start() {
        // 第 18 节：不得保留 example.invalid 再声称「可配置」
        let mut config = sample();
        config.server.site_base = "https://example.invalid".to_string();
        assert!(config.validate().is_err());
        config.server.site_base = "https://books.example.com".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn empty_site_base_still_starts() {
        // 「还没配站点」不该挡住 Master 启动：管理员需要先登录后台才能配
        let mut config = sample();
        config.server.site_base = String::new();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn real_site_base_is_accepted() {
        let mut config = sample();
        config.server.site_base = "https://books.internal.lan".to_string();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn zero_minimum_file_bytes_refuses_to_start() {
        let mut config = sample();
        config.nas.minimum_file_bytes = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn remote_plaintext_opensearch_is_rejected() {
        let mut config = sample();
        config.opensearch.enabled = true;
        config.opensearch.url = "http://search.example.org:9200".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn docker_internal_and_remote_tls_opensearch_are_accepted() {
        let mut config = sample();
        config.opensearch.enabled = true;
        config.opensearch.url = "http://opensearch:9200".to_string();
        assert!(config.validate().is_ok());
        config.opensearch.url = "https://search.internal.lan".to_string();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn unsafe_opensearch_index_name_is_rejected() {
        let mut config = sample();
        config.opensearch.enabled = true;
        config.opensearch.index = "../../_all".to_string();
        assert!(config.validate().is_err());
    }
}
