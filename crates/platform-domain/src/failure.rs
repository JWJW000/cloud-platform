//! 自动化失败分类与第 10.3 节「限额归因」。
//!
//! 归因的核心目的：**不要把站点/IP 级限流误算到账号头上**，
//! 也不要让代理故障消耗图书的业务重试次数。

use crate::enums::{AccountStatus, ExecutionResult, ProxyStatus, TaskStatus};

/// 自动化失败的语义分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// 站点配额指示器显示 `10/10`：账号当日额度确实用完。
    AccountQuotaExhausted,
    /// 站点拒绝下载，但账号配额指示器未满：站点或出口 IP 级限流。
    SiteRateLimited,
    /// 代理连接、认证或出口检查失败。
    ProxyFailure,
    /// 站点未收录该图书或没有目标格式。
    BookNotFound,
    /// 账号凭据被拒。
    AuthFailed,
    /// 站点、DNS 暂时不可用。
    SiteUnavailable,
    /// 下载结果不确定，需要核验 NAS。
    Uncertain,
    /// 其他可重试失败。
    Retryable,
    /// 不可重试失败。
    Fatal,
}

/// 一次失败对任务、账号与代理的完整影响。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attribution {
    /// 写入执行记录的执行结果。
    pub result: ExecutionResult,
    /// 任务应进入的状态；`None` 表示保持原状并按重试策略处理。
    pub task_status: Option<TaskStatus>,
    /// 账号应进入的状态；`None` 表示不改动账号。
    pub account_status: Option<AccountStatus>,
    /// 代理应进入的状态；`None` 表示不改动代理。
    pub proxy_status: Option<ProxyStatus>,
    /// 是否消耗该图书的业务重试次数。
    pub consumes_retry: bool,
    /// 是否必须结束当前执行会话（换账号或换代理）。
    pub ends_session: bool,
}

impl FailureClass {
    /// 第 10.3 节归因表。
    pub const fn attribution(self) -> Attribution {
        match self {
            // 账号确实用满：账号置「今日额度耗尽」，代理保持可用
            Self::AccountQuotaExhausted => Attribution {
                result: ExecutionResult::RetryableFailure,
                task_status: Some(TaskStatus::Pending),
                account_status: Some(AccountStatus::ExhaustedToday),
                proxy_status: Some(ProxyStatus::Available),
                consumes_retry: false,
                ends_session: true,
            },
            // 账号未满却被限额：账号保持「已注册」，代理进入「冷却中」
            Self::SiteRateLimited => Attribution {
                result: ExecutionResult::RetryableFailure,
                task_status: Some(TaskStatus::Pending),
                account_status: Some(AccountStatus::Registered),
                proxy_status: Some(ProxyStatus::CoolingDown),
                consumes_retry: false,
                ends_session: true,
            },
            // 代理故障不消耗图书重试次数
            Self::ProxyFailure => Attribution {
                result: ExecutionResult::RetryableFailure,
                task_status: Some(TaskStatus::Pending),
                account_status: None,
                proxy_status: Some(ProxyStatus::Error),
                consumes_retry: false,
                ends_session: true,
            },
            // 未收录是终态，账号与代理继续使用
            Self::BookNotFound => Attribution {
                result: ExecutionResult::Skipped,
                task_status: Some(TaskStatus::Skipped),
                account_status: None,
                proxy_status: None,
                consumes_retry: false,
                ends_session: false,
            },
            Self::AuthFailed => Attribution {
                result: ExecutionResult::RetryableFailure,
                task_status: Some(TaskStatus::Pending),
                account_status: Some(AccountStatus::LoginFailed),
                proxy_status: None,
                consumes_retry: false,
                ends_session: true,
            },
            Self::SiteUnavailable => Attribution {
                result: ExecutionResult::RetryableFailure,
                task_status: Some(TaskStatus::Pending),
                account_status: None,
                proxy_status: None,
                consumes_retry: false,
                ends_session: true,
            },
            // 结果不确定：交给 NAS 核验决定是补记完成还是重新排队
            Self::Uncertain => Attribution {
                result: ExecutionResult::Uncertain,
                task_status: Some(TaskStatus::NeedsConfirm),
                account_status: None,
                proxy_status: None,
                consumes_retry: false,
                ends_session: false,
            },
            Self::Retryable => Attribution {
                result: ExecutionResult::RetryableFailure,
                task_status: None,
                account_status: None,
                proxy_status: None,
                consumes_retry: true,
                ends_session: false,
            },
            Self::Fatal => Attribution {
                result: ExecutionResult::FatalFailure,
                task_status: Some(TaskStatus::Failed),
                account_status: None,
                proxy_status: None,
                consumes_retry: true,
                ends_session: false,
            },
        }
    }
}

/// 根据错误文本与站点配额指示器判定失败分类。
///
/// `quota_indicator` 为站点 `.caret-scroll__title` 读到的 `(已用, 总额)`；
/// 只有 `已用 >= 总额` 才允许把账号标记为「今日额度耗尽」。
pub fn classify_failure(reason: &str, quota_indicator: Option<(u32, u32)>) -> FailureClass {
    let text = reason.to_lowercase();
    let quota_full = matches!(quota_indicator, Some((used, total)) if total > 0 && used >= total);

    let hits = |needles: &[&str]| needles.iter().any(|needle| text.contains(needle));

    if hits(&[
        "proxy failed",
        "proxy connect",
        "代理连接",
        "代理认证",
        "proxy authentication",
        "err_tunnel",
        "err_proxy",
        "tunnel_connection",
        "proxy_connection",
        "err_no_supported_proxies",
        "chrome-error://",
        "出口 ip",
        "代理连通",
        "代理检测失败",
    ]) {
        return FailureClass::ProxyFailure;
    }
    if hits(&[
        "quota",
        "限额",
        "额度",
        "download limit",
        "too many requests",
        "429",
    ]) {
        return if quota_full {
            FailureClass::AccountQuotaExhausted
        } else {
            FailureClass::SiteRateLimited
        };
    }
    if hits(&[
        "book not found",
        "not found",
        "未收录",
        "没有找到任何信息",
        "no such book",
    ]) {
        return FailureClass::BookNotFound;
    }
    // 这两类错误只能说明站点页面状态未能确认，不能证明账号密码错误。
    // 旧版 Worker 会给它们加上 `authentication failed:` 前缀，因此必须在
    // 通用认证标记之前截获，避免 Master 继续停用有效账号。
    if hits(&[
        "页面显示为已登录，但无法找到退出入口",
        "login form is still visible after submit",
        "登录提交后未确认成功，登录表单仍可见",
    ]) {
        return FailureClass::Retryable;
    }
    if hits(&[
        "auth_failed",
        "authentication failed",
        "invalid password",
        "incorrect password",
        "invalid email or password",
        "incorrect email or password",
        "invalid credentials",
        "密码错误",
        "密码不正确",
        "登录失败",
        "凭据",
    ]) {
        return FailureClass::AuthFailed;
    }
    if hits(&[
        "site temporarily unavailable",
        "dns",
        "connection refused",
        "站点不可用",
        "timeout while loading",
    ]) {
        return FailureClass::SiteUnavailable;
    }
    if hits(&["uncertain", "结果不确定", "stalled", "停滞"]) {
        return FailureClass::Uncertain;
    }
    if hits(&[
        "filename mismatch",
        "文件名不匹配",
        "checksum",
        "哈希不一致",
        "unsupported format",
    ]) {
        return FailureClass::Fatal;
    }
    FailureClass::Retryable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_indicator_blames_account_and_keeps_proxy() {
        let class = classify_failure("daily download quota exhausted", Some((10, 10)));
        assert_eq!(class, FailureClass::AccountQuotaExhausted);
        let attribution = class.attribution();
        assert_eq!(
            attribution.account_status,
            Some(AccountStatus::ExhaustedToday)
        );
        assert_eq!(attribution.proxy_status, Some(ProxyStatus::Available));
        assert!(!attribution.consumes_retry);
    }

    #[test]
    fn unfilled_indicator_blames_exit_ip_not_account() {
        let class = classify_failure("site shows quota page", Some((7, 10)));
        assert_eq!(class, FailureClass::SiteRateLimited);
        let attribution = class.attribution();
        assert_eq!(attribution.account_status, Some(AccountStatus::Registered));
        assert_eq!(attribution.proxy_status, Some(ProxyStatus::CoolingDown));
    }

    #[test]
    fn proxy_failure_does_not_consume_book_retry() {
        let attribution = classify_failure("proxy connect failed: 502", None).attribution();
        assert!(!attribution.consumes_retry);
        assert_eq!(attribution.proxy_status, Some(ProxyStatus::Error));
        assert_eq!(attribution.task_status, Some(TaskStatus::Pending));
    }

    #[test]
    fn explicit_login_rejection_marks_account_and_ends_session() {
        for reason in [
            "login validation error: Invalid email or password",
            "登录失败：密码不正确",
        ] {
            let attribution = classify_failure(reason, None).attribution();
            assert_eq!(attribution.account_status, Some(AccountStatus::LoginFailed));
            assert_eq!(attribution.task_status, Some(TaskStatus::Pending));
            assert!(!attribution.consumes_retry);
            assert!(attribution.ends_session);
        }
    }

    #[test]
    fn ambiguous_login_page_state_never_disables_account() {
        for reason in [
            "authentication failed: login form is still visible after submit",
            "会话异常退出：authentication failed: 页面显示为已登录，但无法找到退出入口核验当前账号，已拒绝复用该登录态",
            "登录提交后未确认成功，登录表单仍可见",
        ] {
            let attribution = classify_failure(reason, None).attribution();
            assert!(attribution.account_status.is_none(), "{reason}");
            assert_ne!(
                classify_failure(reason, None),
                FailureClass::AuthFailed,
                "{reason}"
            );
        }
    }

    #[test]
    fn tunnel_and_chrome_error_pages_are_proxy_failures() {
        for reason in [
            "打开首页失败：Page.navigate failed: net::ERR_TUNNEL_CONNECTION_FAILED",
            "会话异常退出：打开首页失败：net::ERR_PROXY_CONNECTION_FAILED",
            "Chrome 网络错误页: chrome-error://chromewebdata/",
            "代理连通性或出口 IP 探测失败：出口 IP 探测失败: Proxy failed to connect",
        ] {
            assert_eq!(
                classify_failure(reason, None),
                FailureClass::ProxyFailure,
                "{reason}"
            );
            assert_eq!(
                classify_failure(reason, None).attribution().proxy_status,
                Some(ProxyStatus::Error)
            );
        }
    }

    #[test]
    fn not_found_skips_task_and_keeps_resources() {
        let attribution = classify_failure("book not found: 某本书", None).attribution();
        assert_eq!(attribution.task_status, Some(TaskStatus::Skipped));
        assert_eq!(attribution.result, ExecutionResult::Skipped);
        assert!(attribution.account_status.is_none());
        assert!(attribution.proxy_status.is_none());
        assert!(!attribution.ends_session);
    }

    #[test]
    fn uncertain_goes_to_needs_confirm() {
        let attribution = classify_failure("download stalled after 120s", None).attribution();
        assert_eq!(attribution.task_status, Some(TaskStatus::NeedsConfirm));
        assert_eq!(attribution.result, ExecutionResult::Uncertain);
    }

    #[test]
    fn filename_mismatch_is_fatal() {
        let attribution = classify_failure("文件名不匹配：下载到别的书", None).attribution();
        assert_eq!(attribution.task_status, Some(TaskStatus::Failed));
        assert!(attribution.consumes_retry);
    }

    #[test]
    fn unknown_reason_defaults_to_retryable_without_state_change() {
        let attribution = classify_failure("something odd happened", None).attribution();
        assert_eq!(attribution.result, ExecutionResult::RetryableFailure);
        assert!(attribution.task_status.is_none());
        assert!(attribution.consumes_retry);
    }
}
