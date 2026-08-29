//! 自动化核心的稳定输入、事件与结果类型（第 19 节阶段一的「稳定接口」）。

use std::path::PathBuf;
use std::time::Duration;

use platform_domain::FailureClass;
use serde::{Deserialize, Serialize};

/// 站点账号凭据。由 Master 下发，仅在内存中短暂存在。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountCredential {
    /// 账号编号（技术标识）。
    pub account_id: String,
    /// 登录邮箱。
    pub email: String,
    /// 登录密码。
    pub password: String,
    /// 站点昵称。
    pub nickname: String,
    /// 当日已用额度。
    pub daily_used: u32,
    /// 当日额度上限。
    pub daily_limit: u32,
}

/// 一次执行会话的规格：`账号 + 固定代理端口 + Profile`（第 6.1 节）。
#[derive(Debug, Clone)]
pub struct SessionSpec {
    /// 会话编号。
    pub session_id: String,
    /// 站点根地址。
    pub site_base: String,
    /// 浏览器可执行文件；`None` 表示自动探测。
    pub browser_path: Option<PathBuf>,
    /// 是否无头运行。
    pub headless: bool,
    /// 会话专属 Profile 目录 `profiles/session-{会话编号}`。
    pub profile_dir: PathBuf,
    /// 本机下载暂存目录 `staging/task-{任务编号}` 的父目录。
    pub staging_root: PathBuf,
    /// 本机固定代理转发地址，例如 `127.0.0.1:19001`。
    ///
    /// 会话内**绝不轮换上游**：这个端口在整个会话生命周期固定指向同一个 Webshare 代理。
    pub proxy_endpoint: Option<String>,
    /// 会话使用的账号。
    pub account: AccountCredential,
    /// 目标文件格式（`pdf`/`epub`，技术标识）。
    pub download_format: String,
    /// 是否在打开会话时自动执行登录（图书下载为 true，账号注册为 false）。
    pub auto_login: bool,
    /// 会话最长时长。
    pub max_duration: Duration,
}

/// 已建立的会话句柄。引擎内部状态（浏览器进程、页面）由实现持有。
#[derive(Debug, Clone)]
pub struct SessionHandle {
    /// 会话编号。
    pub session_id: String,
    /// 实际使用的浏览器路径。
    pub browser_path: PathBuf,
    /// 实际生效的 Profile 目录。
    pub profile_dir: PathBuf,
}

/// 一本书的下载目标。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookTarget {
    /// 图书编号。
    pub book_id: String,
    /// 图书全局序号，用于生成 NAS 文件名前缀。
    pub book_seq: i64,
    /// 原始书名。**搜索一律按书名进行**（历史经验：ISBN 查询经常搜不到）。
    pub title: String,
    /// 作者。
    pub author: Option<String>,
    /// 出版社。
    pub publisher: Option<String>,
    /// ISBN，仅用于搜索结果卡片的辅助匹配。
    pub isbn: Option<String>,
    /// 目标格式。
    pub format: String,
}

/// 一次下载执行的规格。
#[derive(Debug, Clone)]
pub struct DownloadSpec {
    /// 执行编号：每次分配唯一。
    pub execution_id: String,
    /// 任务编号。
    pub task_id: String,
    /// 目标图书。
    pub book: BookTarget,
    /// 本次执行的暂存目录 `staging/task-{任务编号}`。
    pub staging_dir: PathBuf,
    /// 无数据传输的停滞超时（沿用现有默认 120 秒）。
    pub stall_timeout: Duration,
    /// 可接受的最小文件字节数，来自配置的 `minimum_file_bytes`。
    ///
    /// 由调用方下发而不是引擎内置：站点返回的错误页大小随时会变，
    /// 这个下限属于运营参数，写死在代码里就没法在不发版的情况下调整。
    pub minimum_size_bytes: u64,
    /// 当前尝试次数。
    pub attempt: u32,
}

/// 下载成功的结果。哈希与 NAS 入库由 `worker-agent` 负责，本层只交付本机文件。
#[derive(Debug, Clone)]
pub struct DownloadOutcome {
    /// 落在暂存目录中的文件。
    pub staged_file: PathBuf,
    /// 文件大小。
    pub size_bytes: u64,
    /// 站点配额指示器读数 `(已用, 总额)`，用于第 10.3 节限额归因。
    pub quota_indicator: Option<(u32, u32)>,
    /// 结构化校验证据（第 15.3 节）。
    ///
    /// 引擎在返回成功之前就必须完成扩展名、书名、大小与文件签名校验——
    /// 「下完了」和「下对了」是两件事。证据带出来是为了让 Worker 复用其中的
    /// SHA-256，而不必为同一个几百 MB 的文件再哈希一遍。
    pub evidence: Option<crate::verify::FileEvidence>,
    /// 候选匹配记录（第 8.3 节）。
    pub match_record: Option<crate::matching::MatchRecord>,
}

/// 账号注册规格。
#[derive(Clone)]
pub struct RegistrationSpec {
    /// 执行编号。
    pub execution_id: String,
    /// 待注册账号。
    pub account: AccountCredential,
    /// 是否需要邮箱验证码。
    pub needs_mail_code: bool,
    /// 邮件验证码 Provider（可选，用于自动提取邮件验证码）。
    pub mail_provider: Option<std::sync::Arc<dyn crate::mail_code::MailCodeProvider>>,
    /// 注册任务取消令牌；自动取码与人工等待必须共享它。
    pub cancel: crate::cancel::CancelToken,
}

impl std::fmt::Debug for RegistrationSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistrationSpec")
            .field("execution_id", &self.execution_id)
            .field("account", &self.account)
            .field("needs_mail_code", &self.needs_mail_code)
            .field(
                "mail_provider",
                &self.mail_provider.as_ref().map(|p| p.name()),
            )
            .field("cancelled", &self.cancel.is_cancelled())
            .finish()
    }
}

/// 注册结果。
#[derive(Debug, Clone)]
pub struct RegistrationOutcome {
    /// 站点是否已存在同邮箱账号（此时账号应被停用，不再重试）。
    pub already_exists: bool,
    /// 是否仍在等待邮箱验证。
    pub awaiting_verification: bool,
}

/// 自动化执行过程中的事件，由引擎推送给 Worker，再按第 13.3 节节流上报。
#[derive(Debug, Clone)]
pub enum AutomationEvent {
    /// 阶段变化，`stage` 使用中文描述，如「搜索中」「下载中」。
    Stage {
        /// 中文阶段名。
        stage: String,
    },
    /// 下载进度。
    Progress {
        /// 已下载字节。
        downloaded_bytes: u64,
        /// 总字节（未知时为 0）。
        total_bytes: u64,
    },
    /// 读取到站点配额指示器。
    Quota {
        /// 已用额度。
        used: u32,
        /// 总额度。
        total: u32,
    },
    /// 引擎日志，`level` 使用中文日志级别。
    Log {
        /// 中文日志级别。
        level: String,
        /// 日志内容。
        message: String,
    },
}

/// 自动化失败。`class` 直接给出第 10.3 节的归因分类，Worker 不再猜测语义。
#[derive(Debug, Clone, thiserror::Error)]
#[error("{reason}")]
pub struct AutomationError {
    /// 失败分类。
    pub class: FailureClass,
    /// 人类可读原因（会出现在执行记录与告警中）。
    pub reason: String,
    /// 失败发生时读到的站点配额 `(已用, 总额)`。
    ///
    /// Master 会独立重做失败归因；因此额度耗尽不能只依赖错误类别或文本，
    /// 必须把站点读数作为结构化证据随失败结果一起上报。
    pub quota_indicator: Option<(u32, u32)>,
}

impl AutomationError {
    /// 构造一个带明确分类的失败。
    pub fn new(class: FailureClass, reason: impl Into<String>) -> Self {
        Self {
            class,
            reason: reason.into(),
            quota_indicator: None,
        }
    }

    /// 构造一个携带站点配额证据的失败。
    pub fn with_quota(
        class: FailureClass,
        reason: impl Into<String>,
        quota_indicator: Option<(u32, u32)>,
    ) -> Self {
        Self {
            class,
            reason: reason.into(),
            quota_indicator,
        }
    }

    /// 从任意错误按文本推断分类（`quota_indicator` 参与限额归因）。
    pub fn from_reason(reason: impl Into<String>, quota_indicator: Option<(u32, u32)>) -> Self {
        let reason = reason.into();
        let class = platform_domain::classify_failure(&reason, quota_indicator);
        Self {
            class,
            reason,
            quota_indicator,
        }
    }
}
