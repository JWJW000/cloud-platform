//! Master 下发动态配置的当前快照（第 7.1 节）。
//!
//! 这是 V2 方案里 `WorkerRuntime.ConfigState` 的实现。它存在的理由很实际：
//! 站点地址、槽位上限、下载格式这些值只有 Master 知道，而槽位是长期协程，
//! 不能每次都去问一遍。于是 `NodeConfig` 到达时**整体替换**一份不可变快照，
//! 槽位在新建会话时读一次最新值。
//!
//! 两条刻意的设计：
//! 1. **整体替换而不是逐字段改**。一份配置内部是互相自洽的（比如
//!    `max_session_duration_secs` 与 `stall_timeout_secs` 的相对大小）；
//!    逐字段更新会让某个瞬间出现两份配置的混合体。
//! 2. **先校验再替换**。Master 的一次误配置不该让 Worker 拿着
//!    `site_base = ""` 去启动浏览器。校验失败时保留旧快照并上报原因，
//!    这比带着坏配置继续跑更容易排查。

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use platform_proto::v1 as pb;

/// Master 未下发 `minimum_file_bytes` 时使用的下限，与 Master 默认值一致。
///
/// 不接受 0：阈值为 0 等于关掉「错误页伪装成图书」这道闸门，
/// 而站点返回的登录页往往只有几 KB，恰好是这道闸门唯一能拦住的东西。
pub const DEFAULT_MINIMUM_FILE_BYTES: u64 = 32 * 1024;

/// 单项配置校验失败的原因（中文，直接用于日志与上报）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRejection {
    /// 出问题的字段名（技术标识，便于定位代码）。
    pub field: &'static str,
    /// 中文原因说明。
    pub reason: String,
}

impl std::fmt::Display for ConfigRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}：{}", self.field, self.reason)
    }
}

/// Master 下发的运行配置快照。
///
/// 所有字段都是「已经校验过」的值：读到这个结构体的代码不需要再判空或兜底。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicConfig {
    /// 配置版本号，心跳里原样回报给 Master 用于确认已生效。
    pub config_version: String,
    /// 节点展示名。
    pub node_name: String,
    /// 中文节点状态。
    pub node_status: String,
    /// 云端批准的最大槽位数——运行中的槽位数量不得超过它。
    pub max_slots: u32,
    /// NAS 上传并发度。
    pub upload_concurrency: u32,
    /// 心跳间隔。
    pub heartbeat_interval_secs: u32,
    /// 会话续租间隔。
    pub session_renew_secs: u32,
    /// 进度上报最小间隔（节流）。
    pub progress_min_interval_secs: u32,
    /// 进度上报最小字节增量（节流）。
    pub progress_min_bytes: u64,
    /// 单次会话最长时长。
    pub max_session_duration_secs: u32,
    /// 下载停滞判定超时。
    pub stall_timeout_secs: u32,
    /// NAS 相对根目录。
    pub nas_relative_root: String,
    /// NAS 最小剩余空间（GB），低于此值不再领取下载会话。
    pub minimum_free_gb: u64,
    /// 可接受的最小文件字节数，低于此值视为站点错误页。
    pub minimum_file_bytes: u64,
    /// 真实站点根地址——第 3.3 节的核心：这个值只能来自 Master。
    pub site_base: String,
    /// 默认下载格式（技术标识 pdf / epub）。
    pub download_format: String,
    /// 下载搜索 URL 的 `order` 参数。
    pub search_order: String,
    /// 下载搜索 URL 的 `extensions[index]` 参数；空数组表示按任务格式自动生成。
    pub search_extensions: Vec<String>,
    /// 是否开启诊断上传。
    pub diagnostics_enabled: bool,
}

impl Default for DynamicConfig {
    /// 尚未收到 `NodeConfig` 时的初始快照。
    ///
    /// `site_base` 故意留空：空地址会被 [`DynamicConfig::require_site_base`]
    /// 拒绝，于是「还没拿到配置就去开浏览器」这条路是走不通的——
    /// 这正是我们想要的，而不是悄悄用一个占位域名把问题推到运行时。
    fn default() -> Self {
        Self {
            config_version: String::new(),
            node_name: String::new(),
            node_status: String::new(),
            max_slots: 0,
            upload_concurrency: 1,
            heartbeat_interval_secs: 15,
            session_renew_secs: 60,
            progress_min_interval_secs: 2,
            progress_min_bytes: 5 * 1024 * 1024,
            max_session_duration_secs: 3600,
            stall_timeout_secs: 300,
            nas_relative_root: String::new(),
            minimum_free_gb: 10,
            minimum_file_bytes: DEFAULT_MINIMUM_FILE_BYTES,
            site_base: String::new(),
            download_format: "pdf".to_string(),
            search_order: "bestmatch".to_string(),
            search_extensions: Vec::new(),
            diagnostics_enabled: false,
        }
    }
}

/// 站点地址被拒绝的原因（第 8.1 节的启动前检查）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SiteBaseError {
    /// 空地址：说明还没收到有效的 `NodeConfig`。
    Empty,
    /// 占位域名：V2 第 18 节明令禁止继续保留。
    Placeholder,
    /// 不是 HTTP/HTTPS。
    NotHttp,
}

impl std::fmt::Display for SiteBaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "站点地址为空：尚未收到 Master 下发的有效运行配置"),
            Self::Placeholder => {
                write!(
                    f,
                    "站点地址是保留的占位域名（.invalid / example.com 一类）：拒绝启动真实会话"
                )
            }
            Self::NotHttp => write!(f, "站点地址必须以 http:// 或 https:// 开头"),
        }
    }
}

/// RFC 2606 / RFC 6761 保留给文档与测试的名字。出现在生产配置里一定是漏改。
///
/// 与 `automation-core` 的同名检查、Master 启动校验保持同一份名单：
/// 三层各用一份名单，就会出现「Master 放行、Worker 收下、浏览器打不开」。
const RESERVED_NAMES: [&str; 6] = [
    ".invalid",
    ".example",
    ".test",
    "example.com",
    "example.net",
    "example.org",
];

/// 判断一个站点地址能否用于启动真实浏览器会话。
pub fn validate_site_base(site_base: &str) -> Result<(), SiteBaseError> {
    let trimmed = site_base.trim();
    if trimmed.is_empty() {
        return Err(SiteBaseError::Empty);
    }
    let lower = trimmed.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(SiteBaseError::NotHttp);
    }
    // 占位域名检查放在协议检查之后：先说清楚「格式不对」，再说「域名不对」。
    let host = lower
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_string();
    for name in RESERVED_NAMES {
        if host.ends_with(name) || host == name.trim_start_matches('.') {
            return Err(SiteBaseError::Placeholder);
        }
    }
    Ok(())
}

impl DynamicConfig {
    /// 从 `NodeConfig` 构造快照，同时完成校验。
    ///
    /// 校验只拦「会让后续逻辑出错」的值，不拦「不理想」的值：
    /// 比如心跳间隔 3600 秒虽然离谱，但不会让 Worker 崩，因此只夹取到合理区间。
    /// 而 `download_format` 写成 `docx` 会一路传到文件校验才失败，必须当场拒绝。
    pub fn from_proto(cfg: &pb::NodeConfig) -> Result<Self, ConfigRejection> {
        let format = cfg.download_format.trim().to_ascii_lowercase();
        let download_format = if format.is_empty() {
            "pdf".to_string()
        } else if format == "pdf" || format == "epub" {
            format
        } else {
            return Err(ConfigRejection {
                field: "download_format",
                reason: format!("不支持的下载格式 {format}，只允许 pdf 或 epub"),
            });
        };

        // 空 order 来自旧版 Master，必须保持升级前的 bestmatch 行为。
        let search_order = if cfg.search_order.trim().is_empty() {
            "bestmatch".to_string()
        } else {
            normalize_search_token("search_order", &cfg.search_order, 32)?
        };
        if cfg.search_extensions.len() > 10 {
            return Err(ConfigRejection {
                field: "search_extensions",
                reason: "最多允许 10 个扩展名".to_string(),
            });
        }
        let mut seen_extensions = HashSet::new();
        let mut search_extensions = Vec::new();
        for raw in &cfg.search_extensions {
            let normalized = normalize_search_token(
                "search_extensions",
                raw.trim().trim_start_matches('.'),
                16,
            )?;
            if seen_extensions.insert(normalized.clone()) {
                search_extensions.push(normalized);
            }
        }

        // 站点地址允许暂时为空（节点刚注册、管理员还没配站点），
        // 但不允许是一个格式错误或占位的地址——那是配置错误而不是「还没配」。
        let site_base = cfg.site_base.trim().to_string();
        if !site_base.is_empty() {
            if let Err(err) = validate_site_base(&site_base) {
                return Err(ConfigRejection {
                    field: "site_base",
                    reason: err.to_string(),
                });
            }
        }

        if cfg.node_status.trim().is_empty() {
            return Err(ConfigRejection {
                field: "node_status",
                reason: "节点状态不能为空".to_string(),
            });
        }

        Ok(Self {
            config_version: cfg.config_version.trim().to_string(),
            node_name: cfg.node_name.trim().to_string(),
            node_status: cfg.node_status.trim().to_string(),
            max_slots: cfg.max_slots,
            upload_concurrency: cfg.upload_concurrency.clamp(1, 16),
            heartbeat_interval_secs: cfg.heartbeat_interval_secs.clamp(5, 300),
            session_renew_secs: cfg.session_renew_secs.clamp(10, 1800),
            progress_min_interval_secs: cfg.progress_min_interval_secs.clamp(1, 300),
            progress_min_bytes: cfg.progress_min_bytes.max(1),
            max_session_duration_secs: cfg.max_session_duration_secs.clamp(60, 24 * 3600),
            stall_timeout_secs: cfg.stall_timeout_secs.clamp(30, 24 * 3600),
            nas_relative_root: cfg.nas_relative_root.trim().to_string(),
            minimum_free_gb: cfg.minimum_free_gb,
            // 0 表示 Master 版本较旧（字段还不存在）或配置被清空，两种情况都回退到默认下限
            minimum_file_bytes: if cfg.minimum_file_bytes == 0 {
                DEFAULT_MINIMUM_FILE_BYTES
            } else {
                cfg.minimum_file_bytes
            },
            site_base,
            download_format,
            search_order,
            search_extensions,
            diagnostics_enabled: cfg.diagnostics_enabled,
        })
    }

    /// 取出可用于启动浏览器的站点地址，去掉尾部斜杠。
    pub fn require_site_base(&self) -> Result<String, SiteBaseError> {
        validate_site_base(&self.site_base)?;
        Ok(self.site_base.trim_end_matches('/').to_string())
    }
}

fn normalize_search_token(
    field: &'static str,
    raw: &str,
    max_len: usize,
) -> Result<String, ConfigRejection> {
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.is_empty()
        || normalized.len() > max_len
        || !normalized
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(ConfigRejection {
            field,
            reason: format!("必须是 1–{max_len} 位字母、数字、下划线或连字符"),
        });
    }
    Ok(normalized)
}

/// 线程安全的动态配置快照容器。
///
/// 存的是 `Arc<DynamicConfig>` 而不是 `DynamicConfig`：读取路径只克隆一个指针，
/// 于是「读快照」不会在持锁期间做任何分配，也不会因为槽位拿着快照干活而挡住写入。
#[derive(Debug)]
pub struct ConfigState {
    current: RwLock<Arc<DynamicConfig>>,
}

impl Default for ConfigState {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigState {
    /// 建立初始（尚未收到下发配置）的状态。
    pub fn new() -> Self {
        Self {
            current: RwLock::new(Arc::new(DynamicConfig::default())),
        }
    }

    /// 读取当前快照。
    pub fn snapshot(&self) -> Arc<DynamicConfig> {
        // 锁只在读期间被毒化才会 panic，而本模块内没有任何可能 panic 的持锁代码，
        // 因此这里用 `unwrap_or_else` 取出内层值而不是传播错误。
        match self.current.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// 当前已生效的配置版本，用于心跳里的 `applied_config_version`。
    pub fn applied_version(&self) -> String {
        self.snapshot().config_version.clone()
    }

    /// 校验并原子替换快照。
    ///
    /// 返回 `Ok(true)` 表示确实换了一份新配置，`Ok(false)` 表示与当前完全相同
    /// （Master 周期性重发同一份配置时会走到这里，不必惊动槽位）。
    pub fn apply(&self, cfg: &pb::NodeConfig) -> Result<bool, ConfigRejection> {
        let next = DynamicConfig::from_proto(cfg)?;
        let mut guard = match self.current.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if **guard == next {
            return Ok(false);
        }
        *guard = Arc::new(next);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proto() -> pb::NodeConfig {
        pb::NodeConfig {
            node_id: "节点".to_string(),
            node_name: "书房台式机".to_string(),
            node_status: "在线".to_string(),
            max_slots: 3,
            upload_concurrency: 2,
            heartbeat_interval_secs: 15,
            session_renew_secs: 60,
            progress_min_interval_secs: 2,
            progress_min_bytes: 5 * 1024 * 1024,
            max_session_duration_secs: 3600,
            stall_timeout_secs: 300,
            nas_relative_root: "文件".to_string(),
            minimum_free_gb: 20,
            minimum_file_bytes: 64 * 1024,
            site_base: "https://books.internal.lan/".to_string(),
            download_format: "pdf".to_string(),
            config_version: "v7".to_string(),
            min_agent_version: String::new(),
            diagnostics_enabled: false,
            search_order: "bestmatch".to_string(),
            search_extensions: Vec::new(),
        }
    }

    #[test]
    fn placeholder_site_is_rejected() {
        // 第 18 节：不得保留 example.invalid 再声称「可配置」
        assert_eq!(
            validate_site_base("https://example.invalid"),
            Err(SiteBaseError::Placeholder)
        );
    }

    #[test]
    fn empty_site_is_rejected() {
        assert_eq!(validate_site_base("   "), Err(SiteBaseError::Empty));
    }

    #[test]
    fn documentation_domains_are_rejected_too() {
        // 三层（Master 校验、Worker 快照、自动化引擎）必须用同一份保留名单，
        // 否则会出现「Master 放行、Worker 收下、浏览器打不开」
        for site in [
            "https://books.example.com",
            "http://site.example.net/",
            "https://mirror.test",
            "https://demo.example",
        ] {
            assert_eq!(
                validate_site_base(site),
                Err(SiteBaseError::Placeholder),
                "{site} 应被判为占位域名"
            );
        }
    }

    #[test]
    fn non_http_site_is_rejected() {
        assert_eq!(
            validate_site_base("ftp://books.internal.lan"),
            Err(SiteBaseError::NotHttp)
        );
        assert_eq!(
            validate_site_base("books.internal.lan"),
            Err(SiteBaseError::NotHttp)
        );
    }

    #[test]
    fn real_site_is_accepted_and_trimmed() {
        let cfg = DynamicConfig::from_proto(&proto()).expect("配置应通过校验");
        assert_eq!(
            cfg.require_site_base().unwrap(),
            "https://books.internal.lan"
        );
    }

    #[test]
    fn unknown_download_format_is_rejected() {
        let mut p = proto();
        p.download_format = "docx".to_string();
        let err = DynamicConfig::from_proto(&p).expect_err("应拒绝未知格式");
        assert_eq!(err.field, "download_format");
    }

    #[test]
    fn search_options_are_normalized_and_old_master_keeps_defaults() {
        let mut p = proto();
        p.search_order = " Newest ".to_string();
        p.search_extensions = vec![".PDF".to_string(), "pdf".to_string(), "EPUB".to_string()];
        let cfg = DynamicConfig::from_proto(&p).unwrap();
        assert_eq!(cfg.search_order, "newest");
        assert_eq!(cfg.search_extensions, vec!["pdf", "epub"]);

        p.search_order.clear();
        p.search_extensions.clear();
        let old_master = DynamicConfig::from_proto(&p).unwrap();
        assert_eq!(old_master.search_order, "bestmatch");
        assert!(old_master.search_extensions.is_empty());
    }

    #[test]
    fn search_options_reject_query_injection() {
        let mut p = proto();
        p.search_order = "bestmatch&admin=true".to_string();
        assert_eq!(
            DynamicConfig::from_proto(&p).unwrap_err().field,
            "search_order"
        );
    }

    #[test]
    fn invalid_site_base_keeps_previous_snapshot() {
        // Master 的一次误配置不该把已经能用的站点地址擦掉
        let state = ConfigState::new();
        assert!(state.apply(&proto()).unwrap());
        let good = state.snapshot();

        let mut bad = proto();
        bad.site_base = "https://example.invalid".to_string();
        bad.config_version = "v8".to_string();
        assert!(state.apply(&bad).is_err());

        assert_eq!(state.snapshot().site_base, good.site_base);
        assert_eq!(state.applied_version(), "v7");
    }

    #[test]
    fn identical_config_is_not_reapplied() {
        let state = ConfigState::new();
        assert!(state.apply(&proto()).unwrap(), "首次应视为变更");
        assert!(!state.apply(&proto()).unwrap(), "重复下发不应视为变更");
    }

    #[test]
    fn empty_site_base_is_allowed_but_unusable() {
        // 「还没配站点」与「配错了站点」是两件事：前者允许上线，只是不能开浏览器
        let mut p = proto();
        p.site_base = String::new();
        let cfg = DynamicConfig::from_proto(&p).expect("空站点允许下发");
        assert_eq!(cfg.require_site_base(), Err(SiteBaseError::Empty));
    }

    #[test]
    fn out_of_range_intervals_are_clamped() {
        let mut p = proto();
        p.heartbeat_interval_secs = 0;
        p.stall_timeout_secs = 1;
        let cfg = DynamicConfig::from_proto(&p).unwrap();
        assert_eq!(cfg.heartbeat_interval_secs, 5);
        assert_eq!(cfg.stall_timeout_secs, 30);
    }

    #[test]
    fn zero_minimum_file_bytes_falls_back_to_the_default() {
        // 阈值为 0 等于关掉「几 KB 的错误页也算下载成功」这道闸门
        let mut p = proto();
        p.minimum_file_bytes = 0;
        let cfg = DynamicConfig::from_proto(&p).unwrap();
        assert_eq!(cfg.minimum_file_bytes, DEFAULT_MINIMUM_FILE_BYTES);
    }
}
