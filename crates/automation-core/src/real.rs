//! 真实浏览器自动化引擎（第 8 节）。
//!
//! 相对于早期版本，这里补齐了三件在生产上会直接造成错误结果的事：
//!
//! 1. **站点地址必须是真实配置**：`example.invalid` 这类占位地址在打开浏览器
//!    之前就被拒绝，而不是启动一个注定失败的会话（第 8.1 节）。
//! 2. **下载目录在点击下载之前切到任务独占目录**：通过 CDP
//!    `Browser.setDownloadBehavior` 完成，失败即中止。文件落到公共暂存根目录后，
//!    多槽位并发时归属只能靠「目录里最新那个文件」去猜，而这个猜测一定会错
//!    （第 8.2 节）。
//! 3. **不许点第一个搜索结果**：候选先结构化提取，再交给
//!    [`crate::matching::select_candidate`] 分层匹配；匹配不唯一就报「待确认」，
//!    而不是随便点一个（第 8.3 节）。
//!
//! 另外，所有等待点都接受 [`CancelToken`]：等页面、等候选、等下载、等文件稳定。
//! 忽略取消令牌等于「取消命令只记录日志但不停止执行」，这是第 18 节明确禁止的。
//!
//! 关于阻塞：`rust_drission` 的接口是同步的。这里的做法是**短促的同步突发 +
//! 异步等待**：每次操作页面都单独加锁、立即释放，等待一律用
//! `cancel.sleep(..)`。因此 `MutexGuard` 从不跨越 `await`，
//! 取消延迟也不取决于某一觉睡多久。

use std::ffi::OsString;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use platform_domain::FailureClass;
use rust_drission::{BrowserConfig, ChromiumPage};
use serde_json::json;
use sysinfo::{Pid, Signal, System};

use crate::browser::detect_browser;
use crate::cancel::CancelToken;
use crate::engine::{AutomationEngine, EventSink};
use crate::http_download::{self, HttpDownloadOutcome, HttpDownloadRequest};
use crate::matching::{select_candidate, MatchRecord, MatchTarget};
use crate::site::{self, SiteCard};
use crate::types::{
    AccountCredential, AutomationError, DownloadOutcome, DownloadSpec, RegistrationOutcome,
    RegistrationSpec, SessionHandle, SessionSpec,
};
use crate::verify::{verify_and_collect, VerifyError};

/// 轮询页面状态的间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// 等待搜索结果出现的上限。
const RESULTS_TIMEOUT: Duration = Duration::from_secs(20);
/// 等待登录结果的上限。
const LOGIN_TIMEOUT: Duration = Duration::from_secs(20);
/// Windows 并发启动、首次创建 Profile、杀软扫描时 30 秒仍可能不够。
const BROWSER_START_TIMEOUT: Duration = Duration::from_secs(60);
/// 端口是否黑洞（Windows 保留段）必须短测；HTTP 探测可以稍长。
const LOOPBACK_CLAIM_TIMEOUT: Duration = Duration::from_millis(400);
const CDP_TCP_TIMEOUT: Duration = Duration::from_secs(1);
const CDP_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const CDP_PROBE_INTERVAL: Duration = Duration::from_millis(250);
/// 扫描暂存目录的间隔。
#[cfg(test)]
const SCAN_INTERVAL: Duration = Duration::from_secs(1);
/// 文件大小需要连续多少次不变才算写完。
#[cfg(test)]
const STABLE_SCANS: u32 = 3;

/// 登录弹窗与注册表单（zh.loves.works 真实 DOM，不是通用 /login）。
const LOGIN_LINK: &str = "a[data-action='login']";
const LOGIN_FORM: &str = "#loginForm";
const LOGIN_EMAIL: &str = "#loginForm input[name='email']";
const LOGIN_PASSWORD: &str = "#loginForm input[name='password']";
const LOGIN_SUBMIT: &str = "#loginForm button[type='submit']";
const LOGIN_ERROR: &str = "#loginForm .validation-error";
const REG_FORM: &str = "#registrationForm";
const REG_EMAIL: &str = "#registrationForm input[name='email']";
const REG_PASSWORD: &str = "#registrationForm input[name='password']";
const REG_NAME: &str = "#registrationForm input[name='name']";
const REG_SUBMIT: &str = "#registrationForm button[type='submit']";
const REG_ERROR: &str = "#registrationForm .validation-error";
const VERIFY_INPUT: &str = "#verificationContent input";
const VERIFY_FORM_INPUTS: &str = "#verificationContent form input";
const VERIFY_SUBMIT: &str = "#verificationContent .btn-submit, #verificationContent button";
const BONUS_POPUP: &str = ".btnCloseRegBonusPopup";
const LOGOUT_SELECTORS: &str =
    "a[data-action='logout'], [data-action='logout'], a[href*='/logout'], .logout-link, #logout-link";

/// `ChromiumPage` 本身 Drop 不会关浏览器进程（桌面版因此包了 `WorkerPage`）。
/// 登录失败若直接 `return Err`，Chrome 窗口会留下来，Master 立刻再开下一会话，
/// 5 个槽位限制挡不住窗口堆积。
struct OwnedPage {
    page: Option<ChromiumPage>,
    process: Option<BrowserProcessGuard>,
}

impl OwnedPage {
    fn new(page: ChromiumPage, process: BrowserProcessGuard) -> Self {
        Self {
            page: Some(page),
            process: Some(process),
        }
    }

    fn page(&self) -> &ChromiumPage {
        self.page
            .as_ref()
            .expect("OwnedPage 在 into_inner 之后不可再用")
    }

    fn into_parts(mut self) -> (ChromiumPage, BrowserProcessGuard) {
        let page = self
            .page
            .take()
            .expect("OwnedPage 在 into_parts 之后不可再用");
        let process = self
            .process
            .take()
            .expect("OwnedPage 在 into_parts 之后不可再用");
        (page, process)
    }
}

impl Drop for OwnedPage {
    fn drop(&mut self) {
        if let Some(mut page) = self.page.take() {
            page.close_browser();
        }
        if let Some(mut process) = self.process.take() {
            process.shutdown();
        }
    }
}

/// 当前会话启动的 Chrome 进程所有权。
///
/// Chrome 在 Windows 上可能让最初的 launcher 进程退出、把真正的浏览器留给其他
/// 进程。因此不能只 `Child::kill()`；清理时还要按本会话独占的 Profile 与调试端口
/// 找到根进程及其全部后代。
struct BrowserProcessGuard {
    child: Option<Child>,
    profile_dir: PathBuf,
    debug_port: u16,
}

impl BrowserProcessGuard {
    fn new(child: Child, profile_dir: PathBuf, debug_port: u16) -> Self {
        Self {
            child: Some(child),
            profile_dir,
            debug_port,
        }
    }

    fn for_profile(profile_dir: PathBuf, debug_port: u16) -> Self {
        Self {
            child: None,
            profile_dir,
            debug_port,
        }
    }

    fn set_debug_port(&mut self, port: u16) {
        self.debug_port = port;
    }

    fn shutdown(&mut self) {
        // 先按命令行抓取完整树；若先 kill launcher，子进程可能被重新挂到系统进程，
        // 随后就无法再从 parent 链识别为本会话 Chrome。
        cleanup_browser_process_tree(&self.profile_dir, self.debug_port);
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for BrowserProcessGuard {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 包装一个真实的 ChromiumPage 实例。
struct RealBrowserSession {
    page: ChromiumPage,
    /// 与页面同生命周期；Drop 是所有错误/取消路径的最终进程清理保险。
    _process: BrowserProcessGuard,
    site_base: String,
    /// 会话固定的本机代理转发端口。图书下载会话缺它就不许开始（第 8.1 节）。
    proxy_endpoint: Option<String>,
    /// 会话的目标格式。
    download_format: String,
    /// 当前浏览器实际生效的下载目录（`None` 表示尚未切换）。
    download_dir: Option<PathBuf>,
}

/// 基于 `rust_drission` 的真实浏览器自动化引擎。
pub struct RealAutomationEngine {
    sessions: Mutex<std::collections::HashMap<String, RealBrowserSession>>,
}

impl RealAutomationEngine {
    /// 新建真实自动化引擎。
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// 在一次加锁内操作会话，闭包返回后立即释放锁。
    ///
    /// 刻意不返回 `MutexGuard`：一旦把守卫交出去，调用方就有机会在持锁期间
    /// `await`，那样一个卡住的槽位会连带冻结同一 Worker 上的其它槽位。
    fn with_session<T>(
        &self,
        session_id: &str,
        f: impl FnOnce(&mut RealBrowserSession) -> Result<T, AutomationError>,
    ) -> Result<T, AutomationError> {
        let mut guard = match self.sessions.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let session = guard
            .get_mut(session_id)
            .ok_or_else(|| AutomationError::new(FailureClass::Fatal, "执行会话不存在或已关闭"))?;
        f(session)
    }

    async fn navigate(
        &self,
        session_id: &str,
        url: &str,
        cancel: &CancelToken,
    ) -> Result<(), AutomationError> {
        const MAX_ATTEMPTS: usize = 3;
        for attempt in 1..=MAX_ATTEMPTS {
            cancel.check()?;
            let get_err = self.with_session(session_id, |sess| {
                sess.page
                    .get(url)
                    .map_err(|err| navigation_failure(url, &err.to_string()))
            });
            if let Err(err) = get_err {
                if site::navigation_error_is_site_unavailable(&err.reason) && attempt < MAX_ATTEMPTS
                {
                    if !cancel
                        .sleep(Duration::from_secs(2 + attempt as u64 * 2))
                        .await
                    {
                        return Err(cancel.check().unwrap_err());
                    }
                    continue;
                }
                return Err(err);
            }
            if !cancel.sleep(Duration::from_millis(1500)).await {
                return Err(cancel.check().unwrap_err());
            }

            // 若处于反爬 JS 挑战首屏，等待计算完成
            let challenge_deadline = Instant::now() + Duration::from_secs(12);
            while Instant::now() < challenge_deadline {
                let in_challenge = self
                    .with_session(session_id, |sess| {
                        Ok(js_flag(&sess.page, site::CHALLENGE_PAGE_SCRIPT))
                    })
                    .unwrap_or(false);
                if !in_challenge {
                    break;
                }
                if !cancel.sleep(Duration::from_millis(500)).await {
                    return Err(cancel.check().unwrap_err());
                }
            }

            let blocked = self.with_session(session_id, |sess| {
                if let Some(reason) = site_unavailable_reason(&sess.page) {
                    return Err(unavailable_page_error(reason));
                }
                if js_flag(&sess.page, site::CONNECTION_PROBLEM_SCRIPT) {
                    return Err(AutomationError::new(
                        FailureClass::SiteUnavailable,
                        format!("站点重复返回 Connection Problem 页面: {url}"),
                    ));
                }
                Ok(())
            });
            match blocked {
                Ok(()) => return Ok(()),
                Err(_err) if attempt < MAX_ATTEMPTS => {
                    if !cancel
                        .sleep(Duration::from_secs(2 + attempt as u64 * 2))
                        .await
                    {
                        return Err(cancel.check().unwrap_err());
                    }
                }
                Err(err) => return Err(err),
            }
        }
        Err(AutomationError::new(
            FailureClass::SiteUnavailable,
            format!("页面导航失败: {url}"),
        ))
    }

    async fn wait_for_search_hit(
        &self,
        session: &SessionHandle,
        spec: &DownloadSpec,
        events: &EventSink,
        cancel: &CancelToken,
    ) -> Result<SearchHit, AutomationError> {
        let deadline = Instant::now() + RESULTS_TIMEOUT;
        loop {
            cancel.check()?;
            let snapshot = self.with_session(&session.session_id, |sess| {
                if let Some(reason) = site_unavailable_reason(&sess.page) {
                    return Err(unavailable_page_error(reason));
                }
                if js_flag(&sess.page, site::QUOTA_LIMIT_PAGE_SCRIPT) {
                    let quota = read_quota_from_page(&sess.page);
                    return Err(if site::quota_is_exhausted(quota) {
                        AutomationError::with_quota(
                            FailureClass::AccountQuotaExhausted,
                            "daily download quota exhausted",
                            quota,
                        )
                    } else {
                        AutomationError::with_quota(
                            FailureClass::SiteRateLimited,
                            "site shows quota page but account local usage is low",
                            quota,
                        )
                    });
                }
                if js_flag(&sess.page, site::NOT_FOUND_SCRIPT) {
                    return Err(AutomationError::new(
                        FailureClass::BookNotFound,
                        format!("book not found: {}", spec.book.title),
                    ));
                }
                if js_flag(&sess.page, site::SIMILAR_BOOKS_SCRIPT) {
                    return Err(AutomationError::new(
                        FailureClass::BookNotFound,
                        "站点仅返回相似图书（搜索无精确匹配），已跳过",
                    ));
                }
                Ok((page_title(&sess.page), collect_site_cards(&sess.page)))
            })?;

            let (title, cards) = snapshot;
            if title.contains(site::SEARCH_PAGE_TITLE_MARK) || !cards.is_empty() {
                if let Some(card) = site::find_download_in_cards(
                    &cards,
                    &spec.book.title,
                    spec.book.isbn.as_deref(),
                ) {
                    let target = MatchTarget {
                        title: &spec.book.title,
                        author: spec.book.author.as_deref(),
                        publisher: spec.book.publisher.as_deref(),
                        isbn: spec.book.isbn.as_deref(),
                    };
                    let candidates: Vec<_> = cards.iter().map(SiteCard::to_candidate).collect();
                    let basis = match select_candidate(&target, &candidates) {
                        crate::matching::MatchOutcome::Matched { basis, .. } => basis,
                        _ => crate::matching::MatchBasis::UniqueTitle,
                    };
                    return Ok(SearchHit {
                        card: card.clone(),
                        candidate_count: cards.len(),
                        basis,
                    });
                }
                if !cards.is_empty() && title.contains(site::SEARCH_PAGE_TITLE_MARK) {
                    events.log(
                        "信息",
                        format!("搜索页已渲染 {} 张卡片，但都不匹配目标书", cards.len()),
                    );
                    return Err(AutomationError::new(
                        FailureClass::BookNotFound,
                        format!("book not found: {}", spec.book.title),
                    ));
                }
            }

            if Instant::now() >= deadline {
                return Err(AutomationError::new(
                    FailureClass::BookNotFound,
                    format!("book not found: {}", spec.book.title),
                ));
            }
            if !cancel.sleep(POLL_INTERVAL).await {
                return Err(cancel.check().unwrap_err());
            }
        }
    }

    fn prepare_http_download(
        &self,
        session: &SessionHandle,
        site_base: &str,
        refresh_token: bool,
        events: &EventSink,
    ) -> Result<(Option<String>, String, Vec<rust_drission::Cookie>, String), AutomationError> {
        self.with_session(&session.session_id, |sess| {
            let root = site::site_root_url(site_base);
            if refresh_token || challenge_token_value(&sess.page, &root).is_none() {
                let previous = sess.page.tab().url().ok();
                let before = challenge_token_value(&sess.page, &root);
                let _ = sess.page.get(&root);
                std::thread::sleep(Duration::from_millis(300));
                let deadline = Instant::now() + Duration::from_secs(15);
                loop {
                    let after = challenge_token_value(&sess.page, &root);
                    let page_real = !js_flag(&sess.page, site::CHALLENGE_PAGE_SCRIPT);
                    if site::refresh_decision(before.as_deref(), after.as_deref(), page_real)
                        .is_some()
                    {
                        break;
                    }
                    if Instant::now() >= deadline {
                        events.log("警告", "刷新 c_token 超时，沿用旧 token 继续");
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(300));
                }
                if let Some(previous) = previous {
                    if !previous.eq_ignore_ascii_case(&root) {
                        let _ = sess.page.get(&previous);
                    }
                }
            }
            let proxy_url = sess
                .proxy_endpoint
                .as_ref()
                .map(|endpoint| format!("http://{endpoint}"));
            let user_agent = sess
                .page
                .run_js("(() => navigator.userAgent)()")
                .ok()
                .and_then(|value| {
                    value
                        .get("value")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| "Mozilla/5.0".to_string());
            let cookie_urls = vec![root];
            let cookies = sess.page.cookies(Some(&cookie_urls)).unwrap_or_default();
            let referer = sess
                .page
                .tab()
                .url()
                .unwrap_or_else(|_| site_base.to_string());
            Ok((proxy_url, user_agent, cookies, referer))
        })
    }

    async fn register_once(
        &self,
        session: &SessionHandle,
        spec: &RegistrationSpec,
        events: &EventSink,
    ) -> Result<RegistrationOutcome, AutomationError> {
        events.stage("注册中");
        let site_base =
            self.with_session(&session.session_id, |sess| Ok(sess.site_base.clone()))?;
        self.with_session(&session.session_id, |sess| {
            for selector in LOGOUT_SELECTORS.split(',') {
                if let Ok(Some(node)) = sess.page.ele(&css(selector.trim())) {
                    let _ = node.click();
                    std::thread::sleep(Duration::from_millis(1000));
                    break;
                }
            }
            let _ = sess.page.tab().clear_cache(true, true, true, false);
            Ok(())
        })?;
        let registration_url = format!(
            "{}{}",
            site_base.trim_end_matches('/'),
            site::REGISTRATION_PATH
        );
        self.navigate(&session.session_id, &registration_url, &spec.cancel)
            .await?;

        let deadline = Instant::now() + LOGIN_TIMEOUT;
        loop {
            spec.cancel.check()?;
            let ready = self.with_session(&session.session_id, |sess| {
                Ok(first_page_element(&sess.page, REG_FORM).is_some())
            })?;
            if ready {
                break;
            }
            if Instant::now() >= deadline {
                return Err(AutomationError::new(
                    FailureClass::SiteUnavailable,
                    "注册页未出现 #registrationForm，站点结构可能已变化",
                ));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }

        let nickname = if spec.account.nickname.trim().is_empty() {
            site::nickname_from_email(&spec.account.email)
        } else {
            spec.account.nickname.clone()
        };

        let mail_cursor = if let Some(provider) = &spec.mail_provider {
            match provider
                .prepare(&spec.account.email, Duration::from_secs(10 * 60))
                .await
            {
                Ok(cursor) => Some(cursor),
                Err(err) => {
                    events.log(
                        "警告",
                        format!("邮件验证码 Provider 准备失败（{err}），将尝试人工/降级路径"),
                    );
                    None
                }
            }
        } else {
            None
        };

        self.with_session(&session.session_id, |sess| {
            fill_field(&sess.page, REG_EMAIL, &spec.account.email)?;
            fill_field(&sess.page, REG_PASSWORD, &spec.account.password)?;
            fill_field(&sess.page, REG_NAME, &nickname)?;
            let submit = first_page_element(&sess.page, REG_SUBMIT).ok_or_else(|| {
                AutomationError::new(FailureClass::SiteUnavailable, "注册页未找到提交按钮")
            })?;
            submit.click().map_err(|err| {
                AutomationError::new(FailureClass::Retryable, format!("点击注册按钮失败：{err}"))
            })
        })?;

        let exists_deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < exists_deadline {
            let exists = self.with_session(&session.session_id, |sess| {
                Ok(js_flag(&sess.page, site::EMAIL_EXISTS_SCRIPT))
            })?;
            if exists {
                return Ok(RegistrationOutcome {
                    already_exists: true,
                    awaiting_verification: false,
                });
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }

        let input_deadline = Instant::now() + Duration::from_secs(20);
        loop {
            spec.cancel.check()?;
            let state = self.with_session(&session.session_id, |sess| {
                if js_flag(&sess.page, site::EMAIL_EXISTS_SCRIPT) {
                    return Ok(Err(true));
                }
                if let Some(validation) = first_page_element(&sess.page, REG_ERROR)
                    .and_then(|el| el.text().ok())
                    .map(|text| text.trim().to_string())
                    .filter(|text| !text.is_empty())
                {
                    return Err(AutomationError::new(
                        classify_form_error(&validation),
                        format!("registration rejected: {validation}"),
                    ));
                }
                let ready = first_page_element(&sess.page, VERIFY_INPUT)
                    .and_then(|input| input.is_displayed().ok())
                    .unwrap_or(false);
                Ok(Ok(ready))
            })?;
            match state {
                Err(true) => {
                    return Ok(RegistrationOutcome {
                        already_exists: true,
                        awaiting_verification: false,
                    });
                }
                Ok(true) => break,
                Ok(false) => {}
                Err(_) => {}
            }
            if Instant::now() >= input_deadline {
                return Err(AutomationError::new(
                    FailureClass::Retryable,
                    "verification input did not appear",
                ));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }

        if !spec.needs_mail_code {
            events.log(
                "警告",
                "站点要求邮箱验证码，但本次注册未启用邮箱验证，注册无法自动完成",
            );
            return Ok(RegistrationOutcome {
                already_exists: false,
                awaiting_verification: true,
            });
        }

        if let (Some(provider), Some(cursor)) = (&spec.mail_provider, &mail_cursor) {
            events.stage("自动提取验证码");
            match provider.await_code(cursor, &spec.cancel).await {
                Ok(res) => {
                    return self
                        .submit_verification_code(session, &res.code, events, &spec.cancel)
                        .await;
                }
                Err(crate::mail_code::MailCodeError::Cancelled) => {
                    return Err(spec.cancel.check().unwrap_err());
                }
                Err(err) => {
                    events.log(
                        "警告",
                        format!("自动提取邮件验证码失败（{err}），转为待人工确认"),
                    );
                    return Ok(RegistrationOutcome {
                        already_exists: false,
                        awaiting_verification: true,
                    });
                }
            }
        }

        Ok(RegistrationOutcome {
            already_exists: false,
            awaiting_verification: true,
        })
    }
}

struct SearchHit {
    card: SiteCard,
    candidate_count: usize,
    basis: crate::matching::MatchBasis,
}

fn js_flag_session(engine: &RealAutomationEngine, session: &SessionHandle, script: &str) -> bool {
    engine
        .with_session(&session.session_id, |sess| Ok(js_flag(&sess.page, script)))
        .unwrap_or(false)
}

impl Default for RealAutomationEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// 校验站点根地址（第 8.1 节）。
///
/// 占位地址必须在**启动浏览器之前**被拒绝。V2 第 3.3 节记录的问题正是真实模式
/// 带着 `https://example.invalid` 把浏览器、代理、Profile 全部拉起来，然后卡在
/// 一个永远解析不出来的域名上——而失败看起来像「站点不可用」，
/// 掩盖了「配置根本没填」这个真相。
fn validate_site_base(raw: &str) -> Result<String, AutomationError> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(AutomationError::new(
            FailureClass::Fatal,
            "站点根地址为空：真实模式必须在配置中填写真实站点地址（第 8.1 节）",
        ));
    }
    let url = url::Url::parse(trimmed).map_err(|err| {
        AutomationError::new(
            FailureClass::Fatal,
            format!("站点根地址 {trimmed} 不是合法 URL：{err}"),
        )
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(AutomationError::new(
            FailureClass::Fatal,
            format!(
                "站点根地址 {trimmed} 的协议 `{}` 不受支持，只允许 http 或 https",
                url.scheme()
            ),
        ));
    }
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if host.is_empty() {
        return Err(AutomationError::new(
            FailureClass::Fatal,
            format!("站点根地址 {trimmed} 缺少主机名"),
        ));
    }
    // RFC 2606 的保留域永远解析不到真实站点：出现在这里必定是占位符没被替换。
    const RESERVED: [&str; 6] = [
        ".invalid",
        ".example",
        ".test",
        "example.com",
        "example.net",
        "example.org",
    ];
    if RESERVED
        .iter()
        .any(|suffix| host.ends_with(suffix) || host == suffix.trim_start_matches('.'))
    {
        return Err(AutomationError::new(
            FailureClass::Fatal,
            format!("站点根地址 {trimmed} 是保留测试域名，属于占位配置而不是真实站点（第 8.1 节）"),
        ));
    }
    Ok(trimmed.to_string())
}

/// 目标格式只允许 pdf / epub（第 7.1 节）。
fn normalize_format(raw: &str) -> Result<String, AutomationError> {
    let format = raw.trim().trim_start_matches('.').to_ascii_lowercase();
    match format.as_str() {
        "pdf" | "epub" => Ok(format),
        other => Err(AutomationError::new(
            FailureClass::Fatal,
            format!("不支持的目标格式 `{other}`：只允许 pdf 或 epub"),
        )),
    }
}

fn ensure_loopback_not_proxied() {
    const LOOPBACK: &str = "127.0.0.1,localhost,::1";
    for key in ["NO_PROXY", "no_proxy"] {
        match std::env::var(key) {
            Ok(existing)
                if existing.split(',').any(|item| {
                    let item = item.trim();
                    item == "*" || item == "127.0.0.1" || item == "localhost" || item == "::1"
                }) => {}
            Ok(existing) if !existing.trim().is_empty() => {
                std::env::set_var(key, format!("{existing},{LOOPBACK}"));
            }
            _ => std::env::set_var(key, LOOPBACK),
        }
    }
}

/// Chrome 在 Windows 上把相对 `--user-data-dir` 解析到安装目录，调试端口会被静默丢掉。
fn absolute_browser_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// 认领一个本机真能连上的调试端口。
///
/// Windows Hyper-V/WinNAT 的 excludedportrange 上 `bind` 会成功，但后续 `connect`
/// 一直超时。只检查占用会把这种端口交给 Chrome，然后日志变成「窗口已开、CDP 超时」。
fn allocate_working_debug_port(preferred: u16) -> Result<u16, AutomationError> {
    if preferred != 0 {
        match try_claim_loopback_port(preferred) {
            Ok(port) => return Ok(port),
            Err(err) => tracing::warn!(
                preferred,
                error = %err,
                "首选调试端口不可用（占用或 Windows 保留端口黑洞），改用系统分配端口"
            ),
        }
    }
    try_claim_loopback_port(0).map_err(|err| {
        AutomationError::new(
            FailureClass::Retryable,
            format!("无法分配可用的本机调试端口：{err}"),
        )
    })
}

fn try_claim_loopback_port(port: u16) -> std::io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let addr = listener.local_addr()?;
    let chosen = addr.port();
    listener.set_nonblocking(true)?;
    let connect =
        std::thread::spawn(move || TcpStream::connect_timeout(&addr, LOOPBACK_CLAIM_TIMEOUT));
    let deadline = Instant::now() + LOOPBACK_CLAIM_TIMEOUT;
    while Instant::now() < deadline {
        match listener.accept() {
            Ok(_) => break,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(err) => {
                let _ = connect.join();
                return Err(err);
            }
        }
    }
    match connect.join() {
        Ok(Ok(_)) => Ok(chosen),
        Ok(Err(err)) => Err(err),
        Err(_) => Err(std::io::Error::other("loopback connect thread panicked")),
    }
}

fn proxy_server_arg(endpoint: &str) -> String {
    if endpoint.contains("://") {
        format!("--proxy-server={endpoint}")
    } else {
        format!("--proxy-server=http://{endpoint}")
    }
}

fn tauri_browser_config(
    browser_path: &Path,
    profile_dir: &Path,
    debug_port: u16,
    proxy_endpoint: Option<&str>,
    headless: bool,
) -> BrowserConfig {
    let mut config = BrowserConfig::new()
        .chrome_path(browser_path.display().to_string())
        .user_data_dir(profile_dir.display().to_string())
        .headless(headless)
        .set_local_port(debug_port);
    if let Some(endpoint) = proxy_endpoint {
        let proxy = if endpoint.contains("://") {
            endpoint.to_string()
        } else {
            format!("http://{endpoint}")
        };
        config = config.set_proxy(proxy);
    }
    config
}

/// 与 rust_drission / Tauri 客户端 `launch_chrome` 相同的参数，不含
/// `--remote-debugging-address`、`--proxy-bypass-list=<-loopback>`。
fn managed_chrome_args(
    profile_dir: &Path,
    debug_port: u16,
    proxy_endpoint: Option<&str>,
    headless: bool,
) -> Vec<String> {
    let mut args = vec![
        format!("--remote-debugging-port={debug_port}"),
        format!("--user-data-dir={}", profile_dir.display()),
        "--window-size=1920,1080".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-suggestions-ui".to_string(),
        "--no-first-run".to_string(),
        "--disable-infobars".to_string(),
        "--disable-popup-blocking".to_string(),
        "--hide-crash-restore-bubble".to_string(),
        "--disable-features=PrivacySandboxSettings4".to_string(),
        "--disable-blink-features=AutomationControlled".to_string(),
        "--no-sandbox".to_string(),
    ];
    if headless {
        args.push("--headless=new".to_string());
    }
    if let Some(endpoint) = proxy_endpoint {
        args.push(proxy_server_arg(endpoint));
    }
    args
}

fn clean_profile_locks(profile_dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(profile_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("Singleton") || name == "lockfile" || name.ends_with(".lock") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

fn reset_profile_dir(profile_dir: &Path) {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup = profile_dir.with_extension(format!("corrupt-{timestamp}"));
    let _ = std::fs::rename(profile_dir, &backup);
    let _ = std::fs::create_dir_all(profile_dir);
    clean_profile_locks(profile_dir);
}

fn has_profile_error_tab(page: &ChromiumPage) -> bool {
    let Ok(tabs) = page.tabs() else {
        return false;
    };
    tabs.iter().any(|tab| {
        tab.url()
            .map(|url| {
                url.starts_with("chrome://profile-error") || url.starts_with("edge://profile-error")
            })
            .unwrap_or(false)
    })
}

fn launch_via_rust_drission(
    browser_path: &Path,
    profile_dir: &Path,
    debug_port: u16,
    proxy_endpoint: Option<&str>,
    headless: bool,
) -> Result<ChromiumPage, AutomationError> {
    let config = tauri_browser_config(
        browser_path,
        profile_dir,
        debug_port,
        proxy_endpoint,
        headless,
    );
    ChromiumPage::new(config).map_err(|err| {
        AutomationError::new(FailureClass::Retryable, format!("启动浏览器失败：{err}"))
    })
}

fn cdp_version_is_ready(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("webSocketDebuggerUrl")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|url| !url.trim().is_empty())
}

fn parse_devtools_active_port(contents: &str) -> Option<u16> {
    let line = contents.lines().next()?.trim();
    let port: u16 = line.parse().ok()?;
    (port > 0).then_some(port)
}

fn read_devtools_active_port(profile_dir: &Path) -> Option<u16> {
    let contents = std::fs::read_to_string(profile_dir.join("DevToolsActivePort")).ok()?;
    parse_devtools_active_port(&contents)
}

fn probe_cdp(agent: &ureq::Agent, host: &str, port: u16) -> Result<(), String> {
    let addr = format!("{host}:{port}")
        .parse::<SocketAddr>()
        .map_err(|err| format!("无效探测地址 {host}:{port}：{err}"))?;
    TcpStream::connect_timeout(&addr, CDP_TCP_TIMEOUT).map_err(|err| format!("TCP {err}"))?;
    let endpoint = format!("http://{host}:{port}/json/version");
    match agent.get(&endpoint).call() {
        Ok(response) => match response.into_string() {
            Ok(body) if cdp_version_is_ready(&body) => Ok(()),
            Ok(_) => Err("CDP 响应缺少 webSocketDebuggerUrl".to_string()),
            Err(err) => Err(format!("读取 CDP 响应失败：{err}")),
        },
        Err(err) => Err(err.to_string()),
    }
}

async fn wait_for_cdp(profile_dir: &Path, requested_port: u16) -> Result<u16, AutomationError> {
    let agent = ureq::AgentBuilder::new()
        // CDP 永远是本机控制面，不能继承 Worker 机器上的任何系统代理设置。
        .try_proxy_from_env(false)
        .timeout_connect(CDP_PROBE_TIMEOUT)
        .timeout_read(CDP_PROBE_TIMEOUT)
        .timeout_write(CDP_PROBE_TIMEOUT)
        .build();
    let deadline = Instant::now() + BROWSER_START_TIMEOUT;
    let mut last_error = "尚未探测到调试端口".to_string();
    let mut saw_port_file = false;
    loop {
        let file_port = read_devtools_active_port(profile_dir);
        if file_port.is_some() {
            saw_port_file = true;
        }
        let mut ports = Vec::new();
        if let Some(port) = file_port {
            ports.push(port);
        }
        if requested_port != 0 && !ports.contains(&requested_port) {
            ports.push(requested_port);
        }
        for port in ports {
            for host in ["127.0.0.1", "[::1]"] {
                match probe_cdp(&agent, host, port) {
                    Ok(()) => return Ok(port),
                    Err(err) => last_error = format!("{host}:{port} {err}"),
                }
            }
        }
        if Instant::now() >= deadline {
            let port_file = profile_dir.join("DevToolsActivePort");
            let extra = if saw_port_file {
                format!("已读到 {}", port_file.display())
            } else {
                format!(
                    "未读到 {}，窗口可能被已有 Chrome/Edge 进程接管，调试端口未在本会话 Profile 生效",
                    port_file.display()
                )
            };
            return Err(AutomationError::new(
                FailureClass::Retryable,
                format!(
                    "浏览器已启动，但调试端口 {requested_port} 在 {} 秒内未就绪（{extra}）：{last_error}",
                    BROWSER_START_TIMEOUT.as_secs()
                ),
            ));
        }
        tokio::time::sleep(CDP_PROBE_INTERVAL).await;
    }
}

async fn launch_managed_browser(
    browser_path: &Path,
    profile_dir: &Path,
    debug_port: u16,
    _staging_root: &Path,
    proxy_endpoint: Option<&str>,
    headless: bool,
) -> Result<(ChromiumPage, BrowserProcessGuard), AutomationError> {
    let profile_dir = absolute_browser_path(profile_dir);
    ensure_loopback_not_proxied();
    let _ = std::fs::create_dir_all(&profile_dir);
    cleanup_browser_process_tree(&profile_dir, debug_port);
    clean_profile_locks(&profile_dir);

    tracing::info!(
        browser = %browser_path.display(),
        port = debug_port,
        profile = %profile_dir.display(),
        "正在按 Tauri 客户端方式启动浏览器"
    );

    let page = match launch_via_rust_drission(
        browser_path,
        &profile_dir,
        debug_port,
        proxy_endpoint,
        headless,
    ) {
        Ok(page) if has_profile_error_tab(&page) => {
            tracing::warn!(
                profile = %profile_dir.display(),
                "浏览器打开了 profile-error 页，重置 Profile 后重试"
            );
            let mut page = page;
            page.close_browser();
            cleanup_browser_process_tree(&profile_dir, debug_port);
            reset_profile_dir(&profile_dir);
            launch_via_rust_drission(
                browser_path,
                &profile_dir,
                debug_port,
                proxy_endpoint,
                headless,
            )
        }
        Ok(page) => Ok(page),
        Err(first_error) => {
            tracing::warn!(
                profile = %profile_dir.display(),
                error = %first_error.reason,
                "浏览器启动失败，重置 Profile 后重试"
            );
            cleanup_browser_process_tree(&profile_dir, debug_port);
            reset_profile_dir(&profile_dir);
            launch_via_rust_drission(
                browser_path,
                &profile_dir,
                debug_port,
                proxy_endpoint,
                headless,
            )
        }
    };

    match page {
        Ok(page) => Ok((
            page,
            BrowserProcessGuard::for_profile(profile_dir, debug_port),
        )),
        Err(err) => {
            tracing::warn!(
                error = %err.reason,
                "rust_drission 启动仍失败，回退到加长等待的本机 CDP 探测"
            );
            launch_browser_with_extended_wait(
                browser_path,
                &profile_dir,
                debug_port,
                proxy_endpoint,
                headless,
            )
            .await
        }
    }
}

async fn launch_browser_with_extended_wait(
    browser_path: &Path,
    profile_dir: &Path,
    debug_port: u16,
    proxy_endpoint: Option<&str>,
    headless: bool,
) -> Result<(ChromiumPage, BrowserProcessGuard), AutomationError> {
    cleanup_browser_process_tree(profile_dir, debug_port);
    clean_profile_locks(profile_dir);

    let child = Command::new(browser_path)
        .args(managed_chrome_args(
            profile_dir,
            debug_port,
            proxy_endpoint,
            headless,
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| {
            AutomationError::new(
                FailureClass::Retryable,
                format!("启动浏览器进程失败：{}：{err}", browser_path.display()),
            )
        })?;
    let mut guard = BrowserProcessGuard::new(child, profile_dir.to_path_buf(), debug_port);

    let ready_port = wait_for_cdp(profile_dir, debug_port).await?;
    if ready_port != debug_port {
        tracing::warn!(
            requested = debug_port,
            actual = ready_port,
            "浏览器实际调试端口与请求端口不一致，改连实际端口"
        );
        guard.set_debug_port(ready_port);
    }
    let endpoint = format!("127.0.0.1:{ready_port}");
    let page = ChromiumPage::connect(&endpoint).map_err(|err| {
        AutomationError::new(
            FailureClass::Retryable,
            format!("浏览器调试端口已就绪，但建立 CDP 会话失败：{err}"),
        )
    })?;
    rust_drission::stealth_inject(page.tab()).map_err(|err| {
        AutomationError::new(
            FailureClass::Retryable,
            format!("初始化浏览器反检测脚本失败：{err}"),
        )
    })?;
    Ok((page, guard))
}

fn process_matches_browser(cmd: &[OsString], profile_dir: &Path, debug_port: u16) -> bool {
    let expected_port = format!("--remote-debugging-port={debug_port}");
    let expected_profile = format!("--user-data-dir={}", profile_dir.display());
    cmd.iter().any(|arg| {
        let arg = arg.to_string_lossy();
        arg.eq_ignore_ascii_case(&expected_port) || arg.eq_ignore_ascii_case(&expected_profile)
    })
}

fn distance_to_root(
    system: &System,
    pid: Pid,
    roots: &std::collections::HashSet<Pid>,
) -> Option<usize> {
    let mut current = Some(pid);
    let mut visited = std::collections::HashSet::new();
    let mut distance = 0usize;
    while let Some(candidate) = current {
        if roots.contains(&candidate) {
            return Some(distance);
        }
        if !visited.insert(candidate) {
            return None;
        }
        current = system
            .process(candidate)
            .and_then(|process| process.parent());
        distance = distance.saturating_add(1);
    }
    None
}

fn cleanup_browser_process_tree(profile_dir: &Path, debug_port: u16) {
    let system = System::new_all();
    let roots: std::collections::HashSet<Pid> = system
        .processes()
        .iter()
        .filter_map(|(pid, process)| {
            process_matches_browser(process.cmd(), profile_dir, debug_port).then_some(*pid)
        })
        .collect();
    if roots.is_empty() {
        return;
    }

    let mut targets: Vec<(Pid, usize)> = system
        .processes()
        .keys()
        .copied()
        .filter_map(|pid| distance_to_root(&system, pid, &roots).map(|depth| (pid, depth)))
        .collect();
    // 先杀最深的 renderer/gpu 子进程，最后才杀 browser 根进程。
    targets.sort_unstable_by_key(|(_, depth)| std::cmp::Reverse(*depth));
    for (pid, _) in targets {
        if let Some(process) = system.process(pid) {
            let _ = process
                .kill_with(Signal::Kill)
                .unwrap_or_else(|| process.kill());
        }
    }
}

/// 把选择器显式标成 `css:`。
///
/// `rust_drission` 的定位器会先按第一个冒号切前缀，因此 `button:not(.x)`
/// 这类合法 CSS 会被当成未知定位器类型而报错。显式加前缀就不用再逐个避雷。
fn css(selectors: &str) -> String {
    format!("css:{selectors}")
}

/// 在整页内按优先顺序找第一个存在的元素。
fn first_page_element(page: &ChromiumPage, selectors: &str) -> Option<rust_drission::Element> {
    for selector in selectors.split(',') {
        let selector = selector.trim();
        if selector.is_empty() {
            continue;
        }
        if let Ok(Some(element)) = page.ele(&css(selector)) {
            return Some(element);
        }
    }
    None
}

/// 把浏览器的下载目录切到 `dir`（第 8.2 节）。
///
/// 先用现代的 `Browser.setDownloadBehavior`，失败再退到已废弃的
/// `Page.setDownloadBehavior`；两者都失败就必须报错。**不允许静默继续**：
/// 继续下去文件会落进公共 staging 根目录，多槽位并发时归属只能靠
/// 「目录里最新那个文件」去猜，而这个猜测一定会错。
fn set_download_behavior(page: &ChromiumPage, dir: &str) -> Result<(), AutomationError> {
    let modern = page.tab().run_cdp(
        "Browser.setDownloadBehavior",
        Some(json!({
            "behavior": "allow",
            "downloadPath": dir,
            "eventsEnabled": true,
        })),
    );
    if let Err(modern_err) = modern {
        let legacy = page.tab().run_cdp(
            "Page.setDownloadBehavior",
            Some(json!({
                "behavior": "allow",
                "downloadPath": dir,
            })),
        );
        if let Err(legacy_err) = legacy {
            return Err(AutomationError::new(
                FailureClass::Fatal,
                format!(
                    "切换浏览器下载目录到 {dir} 失败：Browser.setDownloadBehavior 报 {modern_err}；\
                     回退 Page.setDownloadBehavior 也报 {legacy_err}"
                ),
            ));
        }
    }
    Ok(())
}

/// 浏览器写到一半的临时文件后缀。
#[cfg(test)]
const PARTIAL_SUFFIXES: [&str; 3] = [".crdownload", ".tmp", ".part"];

/// 一次任务目录扫描的结果。
#[cfg(test)]
#[derive(Debug)]
struct DirSnapshot {
    /// 目录内最大的非临时文件。
    candidate: Option<(PathBuf, u64)>,
    /// 临时文件的最大字节数。
    partial_bytes: u64,
    /// 是否仍有临时文件（说明浏览器还在写）。
    partial_present: bool,
}

/// 扫描任务独占的暂存目录。
///
/// 刻意**不按扩展名筛选**候选文件：站点把 epub 当 pdf 发下来时，
/// 这里挑出那个文件、交给 [`crate::verify`] 报出「扩展名不符」，
/// 比在这里视而不见然后一路等到停滞超时要诚实得多。
#[cfg(test)]
async fn scan_task_dir(dir: &Path) -> Result<DirSnapshot, AutomationError> {
    let mut entries = tokio::fs::read_dir(dir).await.map_err(|err| {
        AutomationError::new(
            FailureClass::Retryable,
            format!("读取任务暂存目录 {} 失败：{err}", dir.display()),
        )
    })?;
    let mut snapshot = DirSnapshot {
        candidate: None,
        partial_bytes: 0,
        partial_present: false,
    };
    while let Some(entry) = entries.next_entry().await.map_err(|err| {
        AutomationError::new(
            FailureClass::Retryable,
            format!("遍历任务暂存目录 {} 失败：{err}", dir.display()),
        )
    })? {
        let metadata = match entry.metadata().await {
            Ok(metadata) => metadata,
            // 文件可能正好在两次调用之间被浏览器重命名，跳过即可，下一轮会再看到
            Err(_) => continue,
        };
        if !metadata.is_file() {
            continue;
        }
        if crate::verify::is_companion_file(&entry.path()) {
            continue;
        }
        let lower = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if PARTIAL_SUFFIXES
            .iter()
            .any(|suffix| lower.ends_with(suffix))
        {
            snapshot.partial_present = true;
            snapshot.partial_bytes = snapshot.partial_bytes.max(metadata.len());
            continue;
        }
        let size = metadata.len();
        if snapshot
            .candidate
            .as_ref()
            .map(|(_, best)| size > *best)
            .unwrap_or(true)
        {
            snapshot.candidate = Some((entry.path(), size));
        }
    }
    Ok(snapshot)
}

/// 清空任务目录里的文件，返回删除数量。
///
/// 用在两处：开始下载前清历史残留，取消或停滞后清半个文件。
/// 目录是任务独占的（第 8.2 节保证），所以这里不会碰到别人的文件。
async fn discard_dir_files(dir: &Path) -> u32 {
    let mut removed = 0;
    if let Ok(mut entries) = tokio::fs::read_dir(dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry.metadata().await.map(|m| m.is_file()).unwrap_or(false)
                && tokio::fs::remove_file(entry.path()).await.is_ok()
            {
                removed += 1;
            }
        }
    }
    removed
}

/// 等下载真正写完（第 8.2 节的归属 + 第 10.1 节的可取消）。
///
/// 判定「写完」需要三个条件同时成立：目录里没有临时文件、存在一个非临时文件、
/// 且它的大小连续 [`STABLE_SCANS`] 次没有变化。只看「有文件出现」会把
/// 刚创建、还有几百 MB 没写的文件当成结果，后面的哈希与入库全都建立在半个文件上。
///
/// 取消或停滞时删掉目录里的文件：留着半个文件，下一次尝试扫描到它就会
/// 把它当成本次下载的产物。
#[cfg(test)]
async fn wait_for_download(
    spec: &DownloadSpec,
    events: &EventSink,
    cancel: &CancelToken,
) -> Result<(PathBuf, u64), AutomationError> {
    let mut last_bytes = 0u64;
    let mut last_growth = Instant::now();
    let mut stable = 0u32;
    loop {
        if cancel.is_cancelled() {
            discard_dir_files(&spec.staging_dir).await;
            return Err(cancel.check().unwrap_err());
        }
        let snapshot = scan_task_dir(&spec.staging_dir).await?;
        let observed = snapshot
            .candidate
            .as_ref()
            .map(|(_, size)| *size)
            .unwrap_or(0)
            .max(snapshot.partial_bytes);
        if observed > last_bytes {
            last_bytes = observed;
            last_growth = Instant::now();
            stable = 0;
            // 站点很少给出 Content-Length，总字节未知时上报 0（第 13.3 节允许）
            events.progress(observed, 0);
        } else {
            stable += 1;
        }

        if !snapshot.partial_present && stable >= STABLE_SCANS {
            if let Some((path, size)) = snapshot.candidate {
                return Ok((path, size));
            }
        }

        if last_growth.elapsed() >= spec.stall_timeout {
            let removed = discard_dir_files(&spec.staging_dir).await;
            return Err(AutomationError::new(
                FailureClass::Uncertain,
                format!(
                    "download stalled: {} 秒内没有新增字节（已清理 {removed} 个残留文件），结果不确定",
                    spec.stall_timeout.as_secs()
                ),
            ));
        }

        if !cancel.sleep(SCAN_INTERVAL).await {
            discard_dir_files(&spec.staging_dir).await;
            return Err(cancel.check().unwrap_err());
        }
    }
}

/// 读取站点配额指示器，形如 `7/10`（第 10.3 节）。
///
/// 读不到就返回 `None`。**不得**把「读不到」当成「额度耗尽」：
/// 那会因为一次页面改版就把一批正常账号停用掉。
fn read_quota_from_page(page: &ChromiumPage) -> Option<(u32, u32)> {
    let value = page.run_js(site::QUOTA_SCRIPT).ok()?;
    site::parse_quota(&value)
}

fn js_flag(page: &ChromiumPage, script: &str) -> bool {
    page.run_js(script)
        .ok()
        .map(|value| site::js_bool(&value))
        .unwrap_or(false)
}

fn page_title(page: &ChromiumPage) -> String {
    page.tab().title().unwrap_or_default()
}

fn collect_site_cards(page: &ChromiumPage) -> Vec<SiteCard> {
    match page.run_js(site::CARD_SCRAPE_SCRIPT) {
        Ok(value) => site::parse_cards(&value),
        Err(_) => Vec::new(),
    }
}

fn site_unavailable_reason(page: &ChromiumPage) -> Option<String> {
    let current_url = page.tab().url().unwrap_or_default();
    if current_url.starts_with("chrome-error://") {
        return Some(format!("Chrome 网络错误页: {current_url}"));
    }
    let text = page
        .run_js(site::BODY_TEXT_SCRIPT)
        .ok()
        .and_then(|value| {
            value
                .get("value")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    site::site_unavailable_text(&text).then(|| {
        let summary = text
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("网络错误页");
        format!("{} ({current_url})", summary.trim())
    })
}

fn challenge_token_value(page: &ChromiumPage, root_url: &str) -> Option<String> {
    let urls = vec![root_url.to_string()];
    let cookies = page.cookies(Some(&urls)).ok()?;
    cookies
        .iter()
        .find(|cookie| cookie.name == site::CHALLENGE_COOKIE)
        .map(|cookie| cookie.value.clone())
}

fn close_bonus_popup(page: &ChromiumPage) {
    if let Ok(Some(button)) = page.ele(&css(BONUS_POPUP)) {
        let _ = button.click();
    }
}

fn fill_verification_digits(page: &ChromiumPage, code: &str) -> Result<(), AutomationError> {
    let inputs = page.eles(&css(VERIFY_FORM_INPUTS)).map_err(|err| {
        AutomationError::new(
            FailureClass::Retryable,
            format!("读取验证码输入框失败：{err}"),
        )
    })?;
    if inputs.len() < code.chars().count() {
        // 站点偶发单框：退回整串填写
        return fill_field(page, VERIFY_INPUT, code);
    }
    for (input, digit) in inputs.into_iter().zip(code.chars()) {
        input.input(&digit.to_string()).map_err(|err| {
            AutomationError::new(FailureClass::Retryable, format!("填写验证码失败：{err}"))
        })?;
    }
    if let Some(button) = first_page_element(page, VERIFY_SUBMIT) {
        let _ = button.click();
    }
    Ok(())
}

/// 判断表单错误属于「凭据不对」还是「暂时性问题」。
///
/// 分错的代价不对称：把暂时性问题判成 `AuthFailed` 会停用一个好账号，
/// 因此只有出现明确的凭据字样才升级为认证失败。
fn classify_form_error(message: &str) -> FailureClass {
    let lowered = message.to_lowercase();
    const AUTH_MARKS: [&str; 8] = [
        "密码错误",
        "密码不正确",
        "账户不存在",
        "账号不存在",
        "用户不存在",
        "invalid",
        "incorrect",
        "not exist",
    ];
    if AUTH_MARKS.iter().any(|mark| lowered.contains(mark)) {
        FailureClass::AuthFailed
    } else {
        FailureClass::Retryable
    }
}

/// 往表单字段里填值。
///
/// 值不出现在任何错误信息里：这里会填密码，而第 13.1 节禁止日志与错误响应
/// 泄露密码、token 或代理凭据。
fn fill_field(page: &ChromiumPage, selectors: &str, value: &str) -> Result<(), AutomationError> {
    let field = first_page_element(page, selectors).ok_or_else(|| {
        AutomationError::new(
            FailureClass::SiteUnavailable,
            format!("页面上找不到输入框（选择器：{selectors}）"),
        )
    })?;
    let _ = field.clear();
    field.input(value).map_err(|err| {
        AutomationError::new(
            FailureClass::Retryable,
            format!("填写输入框失败（选择器：{selectors}）：{err}"),
        )
    })
}

fn navigation_failure(url: &str, err_text: &str) -> AutomationError {
    let reason = format!("打开首页失败：{err_text} ({url})");
    if site::is_proxy_tunnel_error(err_text) {
        AutomationError::new(FailureClass::ProxyFailure, reason)
    } else {
        AutomationError::new(FailureClass::SiteUnavailable, reason)
    }
}

fn unavailable_page_error(reason: String) -> AutomationError {
    if site::is_proxy_tunnel_error(&reason) {
        AutomationError::new(FailureClass::ProxyFailure, reason)
    } else {
        AutomationError::new(FailureClass::SiteUnavailable, reason)
    }
}

fn should_retry_navigation(err: &AutomationError) -> bool {
    matches!(
        err.class,
        FailureClass::SiteUnavailable | FailureClass::ProxyFailure
    ) || site::navigation_error_is_site_unavailable(&err.reason)
}

/// 与 Tauri `navigate_page` 对齐：网络/`net::err_`/隧道失败重试 3 次，窗口保持打开。
async fn navigate_page(page: &ChromiumPage, url: &str) -> Result<(), AutomationError> {
    const MAX_ATTEMPTS: usize = 3;
    for attempt in 1..=MAX_ATTEMPTS {
        if let Err(err) = page.get(url) {
            let mapped = navigation_failure(url, &err.to_string());
            if should_retry_navigation(&mapped) && attempt < MAX_ATTEMPTS {
                tokio::time::sleep(Duration::from_secs(2 + attempt as u64 * 2)).await;
                continue;
            }
            return Err(mapped);
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;

        let challenge_deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < challenge_deadline {
            if !js_flag(page, site::CHALLENGE_PAGE_SCRIPT) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        let problem_deadline = Instant::now() + Duration::from_secs(6);
        while Instant::now() < problem_deadline {
            if !js_flag(page, site::CONNECTION_PROBLEM_SCRIPT) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1000)).await;
        }

        if js_flag(page, site::CONNECTION_PROBLEM_SCRIPT) {
            let err = AutomationError::new(
                FailureClass::SiteUnavailable,
                format!("站点返回 Connection Problem 页面: {url}"),
            );
            if attempt < MAX_ATTEMPTS {
                tokio::time::sleep(Duration::from_secs(2 + attempt as u64 * 2)).await;
                continue;
            }
            return Err(err);
        }
        if let Some(reason) = site_unavailable_reason(page) {
            let err = unavailable_page_error(reason);
            if should_retry_navigation(&err) && attempt < MAX_ATTEMPTS {
                tokio::time::sleep(Duration::from_secs(2 + attempt as u64 * 2)).await;
                continue;
            }
            return Err(err);
        }
        return Ok(());
    }
    Err(AutomationError::new(
        FailureClass::SiteUnavailable,
        format!("页面导航失败: {url}"),
    ))
}

/// 登录站点：首页弹窗 `#loginForm`，瞬时拒绝最多 3 次并刷新挑战 token。
async fn login_site(
    page: &ChromiumPage,
    site_base: &str,
    account: &AccountCredential,
) -> Result<(), AutomationError> {
    const MAX_LOGIN_ATTEMPTS: usize = 3;
    let mut last_auth: Option<AutomationError> = None;
    for attempt in 1..=MAX_LOGIN_ATTEMPTS {
        match login_attempt(page, site_base, account).await {
            Ok(()) => return Ok(()),
            Err(err) if err.class == FailureClass::AuthFailed => {
                last_auth = Some(err);
                if attempt < MAX_LOGIN_ATTEMPTS {
                    tokio::time::sleep(Duration::from_secs(2 + attempt as u64 * 2)).await;
                }
            }
            Err(err) => return Err(err),
        }
    }
    Err(last_auth.unwrap_or_else(|| {
        AutomationError::new(FailureClass::AuthFailed, "login failed after retries")
    }))
}

async fn login_attempt(
    page: &ChromiumPage,
    site_base: &str,
    account: &AccountCredential,
) -> Result<(), AutomationError> {
    let root = site::site_root_url(site_base);
    navigate_page(page, &root).await?;

    // 新会话即使遇到已有 Cookie，也必须退出后按 Master 下发的账号重新登录。
    // 直接复用“已登录”状态无法证明当前页面属于本会话账号，会造成跨槽位串号。
    if first_page_element(page, LOGIN_LINK).is_none()
        && first_page_element(page, LOGIN_FORM).is_none()
    {
        let logout = first_page_element(page, LOGOUT_SELECTORS).ok_or_else(|| {
            AutomationError::new(
                FailureClass::AuthFailed,
                "页面显示为已登录，但无法找到退出入口核验当前账号，已拒绝复用该登录态",
            )
        })?;
        logout.click().map_err(|err| {
            AutomationError::new(
                FailureClass::Retryable,
                format!("清理既有登录态失败：{err}"),
            )
        })?;
        tokio::time::sleep(Duration::from_secs(1)).await;
        page.get(&root).map_err(|err| {
            AutomationError::new(
                FailureClass::SiteUnavailable,
                format!("退出既有账号后重新打开首页失败：{err}"),
            )
        })?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let deadline = Instant::now() + LOGIN_TIMEOUT;
    loop {
        if let Ok(Some(link)) = page.ele(&css(LOGIN_LINK)) {
            let _ = link.click();
            break;
        }
        if first_page_element(page, LOGIN_FORM).is_some() {
            break;
        }
        if Instant::now() >= deadline {
            return Err(AutomationError::new(
                FailureClass::SiteUnavailable,
                "首页未出现登录入口，站点结构可能已变化",
            ));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let form_deadline = Instant::now() + LOGIN_TIMEOUT;
    loop {
        if first_page_element(page, LOGIN_FORM).is_some() {
            break;
        }
        if Instant::now() >= form_deadline {
            return Err(AutomationError::new(
                FailureClass::SiteUnavailable,
                "登录弹窗 #loginForm 未出现",
            ));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    fill_field(page, LOGIN_EMAIL, &account.email)?;
    fill_field(page, LOGIN_PASSWORD, &account.password)?;
    let submit = first_page_element(page, LOGIN_SUBMIT).ok_or_else(|| {
        AutomationError::new(FailureClass::SiteUnavailable, "登录页未找到提交按钮")
    })?;
    submit.click().map_err(|err| {
        AutomationError::new(FailureClass::Retryable, format!("点击登录按钮失败：{err}"))
    })?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    if let Some(message) = first_page_element(page, LOGIN_ERROR)
        .and_then(|el| el.text().ok())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
    {
        return Err(AutomationError::new(
            FailureClass::AuthFailed,
            format!("login validation error: {message}"),
        ));
    }
    if let Some(form) = first_page_element(page, LOGIN_FORM) {
        if form.is_displayed().unwrap_or(false) {
            return Err(AutomationError::new(
                FailureClass::AuthFailed,
                "login form is still visible after submit",
            ));
        }
    }
    close_bonus_popup(page);
    Ok(())
}

#[async_trait]
impl AutomationEngine for RealAutomationEngine {
    fn name(&self) -> &'static str {
        "真实浏览器引擎"
    }

    async fn open_session(&self, spec: &SessionSpec) -> Result<SessionHandle, AutomationError> {
        // 顺序是刻意的：所有能在本机判定的错误都必须在启动浏览器**之前**报出来。
        // 先拉起浏览器再发现地址是占位符，只会把「配置没填」伪装成「站点不可用」。
        let site_base = validate_site_base(&spec.site_base)?;
        let download_format = normalize_format(&spec.download_format)?;
        if spec.account.email.trim().is_empty() || spec.account.password.is_empty() {
            return Err(AutomationError::new(
                FailureClass::Fatal,
                "会话账号缺少邮箱或密码，拒绝打开真实会话",
            ));
        }
        let browser_path = detect_browser(
            spec.browser_path
                .as_ref()
                .and_then(|p| p.to_str())
                .unwrap_or("auto"),
        )
        .map_err(|err| AutomationError::new(FailureClass::Fatal, err.to_string()))?;

        tokio::fs::create_dir_all(&spec.profile_dir)
            .await
            .map_err(|err| {
                AutomationError::new(
                    FailureClass::Fatal,
                    format!(
                        "创建 Profile 目录 {} 失败：{err}",
                        spec.profile_dir.display()
                    ),
                )
            })?;
        tokio::fs::create_dir_all(&spec.staging_root)
            .await
            .map_err(|err| {
                AutomationError::new(
                    FailureClass::Fatal,
                    format!("创建暂存根目录 {} 失败：{err}", spec.staging_root.display()),
                )
            })?;

        let debug_port = allocate_working_debug_port(spec.browser_debug_port)?;
        let (launched, process) = launch_managed_browser(
            &browser_path,
            &spec.profile_dir,
            debug_port,
            &spec.staging_root,
            spec.proxy_endpoint.as_deref(),
            spec.headless,
        )
        .await?;
        let owned = OwnedPage::new(launched, process);

        if spec.auto_login {
            login_site(owned.page(), &site_base, &spec.account).await?;
        }

        let (page, process) = owned.into_parts();
        let mut guard = match self.sessions.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.insert(
            spec.session_id.clone(),
            RealBrowserSession {
                page,
                _process: process,
                site_base,
                proxy_endpoint: spec.proxy_endpoint.clone(),
                download_format,
                // 会话刚建立时下载目录还是公共暂存根目录：必须等
                // `set_task_download_dir` 切到任务目录后才允许下载（第 8.2 节）。
                download_dir: None,
            },
        );
        drop(guard);

        Ok(SessionHandle {
            session_id: spec.session_id.clone(),
            browser_path,
            profile_dir: spec.profile_dir.clone(),
        })
    }

    async fn set_task_download_dir(
        &self,
        session: &SessionHandle,
        dir: &Path,
    ) -> Result<(), AutomationError> {
        tokio::fs::create_dir_all(dir).await.map_err(|err| {
            AutomationError::new(
                FailureClass::Fatal,
                format!("创建任务下载目录 {} 失败：{err}", dir.display()),
            )
        })?;
        // CDP 只接受字符串路径；非 UTF-8 路径宁可报错，也不能靠有损转换蒙过去
        let dir_text = dir
            .to_str()
            .ok_or_else(|| {
                AutomationError::new(
                    FailureClass::Fatal,
                    format!("任务下载目录 {} 不是有效的 UTF-8 路径", dir.display()),
                )
            })?
            .to_string();
        self.with_session(&session.session_id, |sess| {
            set_download_behavior(&sess.page, &dir_text)?;
            sess.download_dir = Some(PathBuf::from(&dir_text));
            Ok(())
        })
    }

    async fn read_quota_indicator(
        &self,
        session: &SessionHandle,
    ) -> Result<Option<(u32, u32)>, AutomationError> {
        self.with_session(&session.session_id, |sess| {
            Ok(read_quota_from_page(&sess.page))
        })
    }

    async fn close_session(&self, session: &SessionHandle) -> Result<(), AutomationError> {
        let removed = {
            let mut guard = match self.sessions.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.remove(&session.session_id)
        };
        if let Some(mut sess) = removed {
            sess.page.close_browser();
        }
        // Profile 按会话创建，正常结束即删除（第 6.3 节）
        let _ = tokio::fs::remove_dir_all(&session.profile_dir).await;
        Ok(())
    }

    async fn download_book(
        &self,
        session: &SessionHandle,
        spec: &DownloadSpec,
        events: &EventSink,
        cancel: &CancelToken,
    ) -> Result<DownloadOutcome, AutomationError> {
        cancel.check()?;
        let format = normalize_format(&spec.book.format)?;

        // 开工前的两道闸门（第 8.1/8.2 节）。都在同一次加锁内读完，锁随即释放。
        let site_base = self.with_session(&session.session_id, |sess| {
            match sess.download_dir.as_deref() {
                None => {
                    return Err(AutomationError::new(
                        FailureClass::Fatal,
                        "尚未为本任务设置浏览器下载目录，拒绝开始下载（第 8.2 节）",
                    ));
                }
                Some(dir) if dir != spec.staging_dir.as_path() => {
                    return Err(AutomationError::new(
                        FailureClass::Fatal,
                        format!(
                            "浏览器下载目录 {} 与任务暂存目录 {} 不一致，拒绝开始下载（第 8.2 节）",
                            dir.display(),
                            spec.staging_dir.display()
                        ),
                    ));
                }
                Some(_) => {}
            }
            if sess
                .proxy_endpoint
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                return Err(AutomationError::new(
                    FailureClass::Fatal,
                    "会话没有固定代理端口：整本书必须用同一个出口 IP 下完，拒绝开始（第 8.1 节）",
                ));
            }
            if sess.download_format != format {
                return Err(AutomationError::new(
                    FailureClass::Fatal,
                    format!(
                        "任务目标格式 {format} 与会话格式 {} 不一致，拒绝开始下载",
                        sess.download_format
                    ),
                ));
            }
            Ok(sess.site_base.clone())
        })?;

        // 目录卫生：上一次尝试留下的文件会被本次扫描当成结果
        let stale = discard_dir_files(&spec.staging_dir).await;
        if stale > 0 {
            events.log(
                "警告",
                format!("任务暂存目录有 {stale} 个残留文件，已在下载前清理"),
            );
        }

        // 搜索前读配额：10/10 的账号不再浪费一次搜索。
        let quota_before = self
            .with_session(&session.session_id, |sess| {
                Ok(read_quota_from_page(&sess.page))
            })
            .unwrap_or(None);
        if site::quota_is_exhausted(quota_before) {
            return Err(AutomationError::with_quota(
                FailureClass::AccountQuotaExhausted,
                "daily download quota exhausted",
                quota_before,
            ));
        }

        events.stage("搜索中");
        let search_url = site::search_url(&site_base, &spec.book.title, &format);
        self.navigate(&session.session_id, &search_url, cancel)
            .await?;

        let chosen = self
            .wait_for_search_hit(session, spec, events, cancel)
            .await?;
        let record = MatchRecord::new(
            spec.book.title.as_str(),
            chosen.candidate_count,
            &chosen.card.to_candidate(),
            chosen.basis,
        );
        events.log("信息", record.summary());

        events.stage("下载中");
        let download_url =
            site::absolute_download_url(&site_base, &chosen.card.download).map_err(|err| {
                AutomationError::new(
                    FailureClass::Retryable,
                    format!("invalid download URL {}: {err}", chosen.card.download),
                )
            })?;

        let mut last_http_err: Option<AutomationError> = None;
        for token_attempt in 0..=1 {
            cancel.check()?;
            let (proxy_url, user_agent, cookies, referer) =
                self.prepare_http_download(session, &site_base, token_attempt > 0, events)?;
            let proxy_ref = proxy_url.as_deref();
            let request = HttpDownloadRequest {
                proxy_url: proxy_ref,
                user_agent: &user_agent,
                cookies: &cookies,
                referer: &referer,
                url: download_url.clone(),
                staging_dir: &spec.staging_dir,
                title: &spec.book.title,
                task_id: &spec.task_id,
                timeout: spec.stall_timeout,
            };
            let http_result = http_download::download_file(request, events, cancel);
            match http_result {
                Ok(HttpDownloadOutcome::File(staged_file)) => {
                    events.stage("入库中");
                    let evidence = verify_and_collect(
                        &staged_file,
                        &spec.book.title,
                        &format,
                        spec.minimum_size_bytes,
                    )
                    .map_err(|err| {
                        let class = match &err {
                            VerifyError::TooSmall { .. }
                            | VerifyError::BadSignature { .. }
                            | VerifyError::Unreadable { .. } => FailureClass::Retryable,
                            VerifyError::UnexpectedExtension { .. }
                            | VerifyError::TitleMismatch { .. } => FailureClass::Fatal,
                        };
                        AutomationError::new(class, format!("下载文件未通过入库前校验：{err}"))
                    })?;
                    let quota_indicator = self
                        .with_session(&session.session_id, |sess| {
                            Ok(read_quota_from_page(&sess.page))
                        })
                        .unwrap_or(None);
                    if let Some((used, total)) = quota_indicator {
                        events.emit(crate::types::AutomationEvent::Quota { used, total });
                    }
                    return Ok(DownloadOutcome {
                        staged_file,
                        size_bytes: evidence.size_bytes,
                        quota_indicator,
                        evidence: Some(evidence),
                        match_record: Some(record),
                    });
                }
                Ok(HttpDownloadOutcome::Html { snippet, .. }) => {
                    if http_download::html_looks_like_quota(&snippet)
                        || js_flag_session(self, session, site::QUOTA_LIMIT_PAGE_SCRIPT)
                    {
                        let quota = self
                            .with_session(&session.session_id, |sess| {
                                Ok(read_quota_from_page(&sess.page))
                            })
                            .unwrap_or(None);
                        return Err(if site::quota_is_exhausted(quota) {
                            AutomationError::with_quota(
                                FailureClass::AccountQuotaExhausted,
                                "daily download quota exhausted",
                                quota,
                            )
                        } else {
                            AutomationError::with_quota(
                                FailureClass::SiteRateLimited,
                                "site shows quota page but account local usage is low",
                                quota,
                            )
                        });
                    }
                    last_http_err = Some(AutomationError::new(
                        FailureClass::Retryable,
                        "download endpoint returned HTML instead of a book file",
                    ));
                }
                Err(err) => {
                    let refresh_token = err.reason.contains("HTTP 503")
                        || err.reason.contains("HTTP 429")
                        || err.class == FailureClass::SiteUnavailable;
                    last_http_err = Some(err);
                    if refresh_token && token_attempt == 0 {
                        events.log("警告", "下载被站点挑战拦截，正在刷新 c_token 后重试");
                        continue;
                    }
                }
            }
            break;
        }

        Err(last_http_err.unwrap_or_else(|| {
            AutomationError::new(FailureClass::Retryable, "HTTP 下载未得到有效文件")
        }))
    }

    async fn register_account(
        &self,
        session: &SessionHandle,
        spec: &RegistrationSpec,
        events: &EventSink,
    ) -> Result<RegistrationOutcome, AutomationError> {
        if !site::password_length_ok(&spec.account.password) {
            return Err(AutomationError::new(
                FailureClass::Fatal,
                format!(
                    "password length {} out of range 8..=32, registration aborted",
                    spec.account.password.chars().count()
                ),
            ));
        }
        let mut last_error: Option<AutomationError> = None;
        for attempt in 1..=3 {
            match self.register_once(session, spec, events).await {
                Ok(outcome) => return Ok(outcome),
                Err(err) => {
                    let retryable = site::is_retryable_registration_error(&err.reason);
                    last_error = Some(err);
                    if !retryable {
                        break;
                    }
                    events.log("警告", format!("注册第 {attempt} 次失败，正在重试"));
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            }
        }
        Err(last_error.unwrap_or_else(|| {
            AutomationError::new(FailureClass::Retryable, "registration failed")
        }))
    }

    async fn submit_verification_code(
        &self,
        session: &SessionHandle,
        code: &str,
        events: &EventSink,
        cancel: &CancelToken,
    ) -> Result<RegistrationOutcome, AutomationError> {
        events.stage("提交验证码中");
        self.with_session(&session.session_id, |sess| {
            fill_verification_digits(&sess.page, code)
        })?;
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            cancel.check()?;
            let done = self.with_session(&session.session_id, |sess| {
                if js_flag(&sess.page, site::EMAIL_EXISTS_SCRIPT) {
                    return Ok(Some(RegistrationOutcome {
                        already_exists: true,
                        awaiting_verification: false,
                    }));
                }
                if let Some(message) = first_page_element(&sess.page, REG_ERROR)
                    .and_then(|el| el.text().ok())
                    .map(|text| text.trim().to_string())
                    .filter(|text| !text.is_empty())
                {
                    return Err(AutomationError::new(
                        classify_form_error(&message),
                        format!("验证码提交后站点拒绝：{message}"),
                    ));
                }
                let input_visible = first_page_element(&sess.page, VERIFY_INPUT)
                    .and_then(|input| input.is_displayed().ok())
                    .unwrap_or(false);
                Ok(if input_visible {
                    None
                } else {
                    Some(RegistrationOutcome {
                        already_exists: false,
                        awaiting_verification: false,
                    })
                })
            })?;
            if let Some(outcome) = done {
                return Ok(outcome);
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        Err(AutomationError::new(
            FailureClass::Retryable,
            "verification code rejected, code input still visible",
        ))
    }

    // <<<REAL_RS_IMPL>>>
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 第 8.1 节：占位地址必须在启动浏览器之前被拒绝。
    #[test]
    fn placeholder_site_is_rejected_before_launching_a_browser() {
        for placeholder in [
            "https://example.invalid",
            "http://example.invalid/",
            "https://book.example.test",
            "https://www.example.com",
        ] {
            let err = validate_site_base(placeholder).unwrap_err();
            assert_eq!(err.class, FailureClass::Fatal, "{placeholder}");
            assert!(err.reason.contains("保留测试域名"), "{}", err.reason);
        }
    }

    #[test]
    fn empty_and_non_http_site_bases_are_rejected() {
        assert!(validate_site_base("   ")
            .unwrap_err()
            .reason
            .contains("为空"));
        assert!(validate_site_base("ftp://books.internal")
            .unwrap_err()
            .reason
            .contains("协议"));
        assert!(validate_site_base("不是一个地址")
            .unwrap_err()
            .reason
            .contains("不是合法 URL"));
    }

    #[test]
    fn real_site_base_is_accepted_and_trailing_slash_removed() {
        assert_eq!(
            validate_site_base(" https://books.internal.lan/ ").unwrap(),
            "https://books.internal.lan"
        );
        // 本机镜像站是合法的真实配置，不属于 RFC 2606 保留域
        assert_eq!(
            validate_site_base("http://127.0.0.1:8000").unwrap(),
            "http://127.0.0.1:8000"
        );
    }

    #[test]
    fn only_pdf_and_epub_are_accepted_formats() {
        assert_eq!(normalize_format(" .PDF ").unwrap(), "pdf");
        assert_eq!(normalize_format("EPUB").unwrap(), "epub");
        assert!(normalize_format("mobi").is_err());
    }

    #[test]
    fn search_term_is_percent_encoded() {
        assert_eq!(
            site::percent_encode("算法导论"),
            "%E7%AE%97%E6%B3%95%E5%AF%BC%E8%AE%BA"
        );
        assert_eq!(site::percent_encode("C++ Primer"), "C%2B%2B%20Primer");
        assert_eq!(site::percent_encode("a-b_c.d~e"), "a-b_c.d~e");
    }

    #[test]
    fn selectors_are_marked_as_css_so_colons_are_not_parsed_as_prefixes() {
        // `button[type='submit']` 里没有冒号，但 `a:hover` 这类有；统一加前缀就不必逐个避雷
        assert_eq!(css(".book-item"), "css:.book-item");
        assert!(rust_drission::Locator::parse(&css("button:not(.disabled)")).is_ok());
    }

    #[test]
    fn form_errors_only_escalate_to_auth_failed_on_credential_wording() {
        assert_eq!(classify_form_error("密码错误"), FailureClass::AuthFailed);
        assert_eq!(
            classify_form_error("Invalid email or password"),
            FailureClass::AuthFailed
        );
        // 「稍后再试」是暂时性问题：判成认证失败会白白停用一个好账号
        assert_eq!(
            classify_form_error("系统繁忙，请稍后再试"),
            FailureClass::Retryable
        );
    }

    fn download_spec(root: &Path, stall_timeout: Duration) -> DownloadSpec {
        DownloadSpec {
            execution_id: "执行1".to_string(),
            task_id: "任务1".to_string(),
            book: crate::types::BookTarget {
                book_id: "图书1".to_string(),
                book_seq: 1,
                title: "算法导论".to_string(),
                author: None,
                publisher: None,
                isbn: None,
                format: "pdf".to_string(),
            },
            staging_dir: root.join("staging/task-任务1"),
            stall_timeout,
            minimum_size_bytes: 32 * 1024,
            attempt: 1,
        }
    }

    async fn write_bytes(path: &Path, len: usize) {
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(path, vec![b'K'; len]).await.unwrap();
    }

    #[tokio::test]
    async fn scan_separates_partial_files_from_finished_ones() {
        let dir = tempfile::tempdir().unwrap();
        let task_dir = dir.path().join("task-任务1");
        write_bytes(&task_dir.join("算法导论.pdf"), 10).await;
        write_bytes(&task_dir.join("算法导论.pdf.crdownload"), 50).await;

        let snapshot = scan_task_dir(&task_dir).await.unwrap();
        assert!(snapshot.partial_present, "临时文件必须被识别为「还在写」");
        assert_eq!(snapshot.partial_bytes, 50);
        let (path, size) = snapshot.candidate.unwrap();
        assert_eq!(path.file_name().unwrap(), "算法导论.pdf");
        assert_eq!(size, 10);
    }

    #[tokio::test]
    async fn appledouble_companion_files_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let task_dir = dir.path().join("task-任务1");
        write_bytes(&task_dir.join("._算法导论.pdf"), 4096).await;
        write_bytes(&task_dir.join(".DS_Store"), 100).await;
        write_bytes(&task_dir.join("算法导论.pdf"), 2048).await;

        let snapshot = scan_task_dir(&task_dir).await.unwrap();
        let (path, size) = snapshot.candidate.unwrap();
        assert_eq!(path.file_name().unwrap(), "算法导论.pdf");
        assert_eq!(size, 2048);
    }

    #[tokio::test]
    async fn scanning_a_missing_dir_is_retryable_not_a_panic() {
        let err = scan_task_dir(Path::new("/definitely/missing/task-dir"))
            .await
            .unwrap_err();
        assert_eq!(err.class, FailureClass::Retryable);
    }

    #[test]
    fn occupied_preferred_debug_port_falls_back_to_an_ephemeral_port() {
        let blocker = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let occupied = blocker.local_addr().unwrap().port();
        let allocated = allocate_working_debug_port(occupied).unwrap();
        assert_ne!(allocated, occupied);
        assert_ne!(allocated, 0);
        drop(blocker);
        TcpListener::bind(("127.0.0.1", allocated)).unwrap();
    }

    #[test]
    fn allocate_working_debug_port_self_connects_before_handing_out() {
        let port = allocate_working_debug_port(0).unwrap();
        assert_ne!(port, 0);
        TcpListener::bind(("127.0.0.1", port)).unwrap();
    }

    #[test]
    fn managed_launch_args_match_tauri_rust_drission_flags() {
        let args = managed_chrome_args(
            Path::new(r"C:\worker\profiles\session-1"),
            9301,
            Some("127.0.0.1:19004"),
            false,
        );
        assert!(args.iter().any(|arg| arg == "--remote-debugging-port=9301"));
        assert!(args
            .iter()
            .any(|arg| arg == r"--user-data-dir=C:\worker\profiles\session-1"));
        assert!(args
            .iter()
            .any(|arg| arg == "--proxy-server=http://127.0.0.1:19004"));
        assert!(!args
            .iter()
            .any(|arg| arg.contains("remote-debugging-address")));
        assert!(!args.iter().any(|arg| arg.contains("proxy-bypass-list")));
        assert!(!args.iter().any(|arg| arg.contains("remote-allow-origins")));
    }

    #[test]
    fn relative_profile_dir_is_expanded_against_cwd() {
        let relative = Path::new("data/profiles/session-1");
        let absolute = absolute_browser_path(relative);
        assert!(absolute.is_absolute());
        assert!(absolute.ends_with(relative));
    }

    #[test]
    fn cdp_probe_requires_a_non_empty_websocket_url() {
        assert!(cdp_version_is_ready(
            r#"{"Browser":"Chrome","webSocketDebuggerUrl":"ws://127.0.0.1/devtools/browser/1"}"#
        ));
        assert!(!cdp_version_is_ready(r#"{"Browser":"Chrome"}"#));
        assert!(!cdp_version_is_ready(r#"{"webSocketDebuggerUrl":""}"#));
        assert!(!cdp_version_is_ready("not-json"));
    }

    #[test]
    fn devtools_active_port_file_uses_the_first_line() {
        assert_eq!(
            parse_devtools_active_port("42221\n/devtools/browser/abc\n"),
            Some(42221)
        );
        assert_eq!(parse_devtools_active_port("0\n"), None);
        assert_eq!(parse_devtools_active_port("not-a-port\n"), None);
    }

    #[test]
    fn cleanup_match_is_scoped_to_exact_profile_or_debug_port() {
        let cmd = vec![
            OsString::from("chrome.exe"),
            OsString::from("--remote-debugging-port=19220"),
            OsString::from(r"--user-data-dir=C:\worker\profiles\session-a"),
        ];
        assert!(process_matches_browser(
            &cmd,
            Path::new(r"C:\worker\profiles\session-a"),
            19220
        ));
        assert!(!process_matches_browser(
            &cmd,
            Path::new(r"C:\worker\profiles\session-b"),
            19221
        ));
    }

    /// 只有「没有临时文件 + 大小连续不变」才算下载完成。
    #[tokio::test(start_paused = true)]
    async fn wait_returns_only_after_the_file_stops_growing() {
        let dir = tempfile::tempdir().unwrap();
        let spec = download_spec(dir.path(), Duration::from_secs(120));
        write_bytes(&spec.staging_dir.join("算法导论.pdf"), 4096).await;

        let (path, size) = wait_for_download(&spec, &EventSink::discarding(), &CancelToken::new())
            .await
            .unwrap();
        assert_eq!(path.file_name().unwrap(), "算法导论.pdf");
        assert_eq!(size, 4096);
    }

    /// 临时文件还在，就不能把它当成结果交出去。
    #[tokio::test]
    async fn a_partial_file_alone_never_counts_as_finished() {
        let dir = tempfile::tempdir().unwrap();
        let spec = download_spec(dir.path(), Duration::from_millis(300));
        write_bytes(&spec.staging_dir.join("算法导论.pdf.crdownload"), 4096).await;

        let err = wait_for_download(&spec, &EventSink::discarding(), &CancelToken::new())
            .await
            .unwrap_err();
        assert_eq!(err.class, FailureClass::Uncertain);
        assert!(err.reason.contains("download stalled"), "{}", err.reason);
        // 停滞后不能留下半个文件：下一次尝试会把它当成本次下载的产物
        let snapshot = scan_task_dir(&spec.staging_dir).await.unwrap();
        assert!(!snapshot.partial_present);
        assert!(snapshot.candidate.is_none());
    }

    /// 第 10.1 节：等下载的过程中收到取消，必须立刻停，并且不留半个文件。
    #[tokio::test]
    async fn cancel_interrupts_the_wait_and_clears_the_dir() {
        let dir = tempfile::tempdir().unwrap();
        let spec = download_spec(dir.path(), Duration::from_secs(600));
        write_bytes(&spec.staging_dir.join("算法导论.pdf.crdownload"), 4096).await;

        let cancel = CancelToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            trigger.cancel("管理员取消任务");
        });

        let started = Instant::now();
        let err = wait_for_download(&spec, &EventSink::discarding(), &cancel)
            .await
            .unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "取消必须立刻生效"
        );
        assert!(err.reason.contains("管理员取消任务"), "{}", err.reason);
        let snapshot = scan_task_dir(&spec.staging_dir).await.unwrap();
        assert!(snapshot.candidate.is_none() && !snapshot.partial_present);
    }

    #[tokio::test]
    async fn discarding_removes_only_files_and_reports_the_count() {
        let dir = tempfile::tempdir().unwrap();
        let task_dir = dir.path().join("task-任务1");
        write_bytes(&task_dir.join("a.pdf"), 8).await;
        write_bytes(&task_dir.join("b.pdf.crdownload"), 8).await;
        tokio::fs::create_dir_all(task_dir.join("子目录"))
            .await
            .unwrap();

        assert_eq!(discard_dir_files(&task_dir).await, 2);
        assert!(task_dir.join("子目录").exists());
    }
}
