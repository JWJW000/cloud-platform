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

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use platform_domain::FailureClass;
use rust_drission::{BrowserConfig, ChromiumPage};
use serde_json::json;

use crate::browser::{detect_browser, launch_args};
use crate::cancel::CancelToken;
use crate::engine::{AutomationEngine, EventSink};
use crate::matching::{
    select_candidate, CandidateBook, MatchBasis, MatchOutcome, MatchRecord, MatchTarget,
};
use crate::types::{
    AccountCredential, AutomationError, DownloadOutcome, DownloadSpec, RegistrationOutcome,
    RegistrationSpec, SessionHandle, SessionSpec,
};
use crate::verify::{normalize_title, verify_and_collect, VerifyError};

/// 轮询页面状态的间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// 等待搜索结果出现的上限。
const RESULTS_TIMEOUT: Duration = Duration::from_secs(20);
/// 等待登录结果的上限。
const LOGIN_TIMEOUT: Duration = Duration::from_secs(20);
/// 扫描暂存目录的间隔。
const SCAN_INTERVAL: Duration = Duration::from_secs(1);
/// 文件大小需要连续多少次不变才算写完。
const STABLE_SCANS: u32 = 3;

/// 搜索结果卡片的候选选择器。
const CARD_SELECTORS: &str = ".book-item, .search-result-item, .card";
/// 卡片内书名、作者、出版社、ISBN 的候选选择器。
const TITLE_SELECTORS: &str = ".book-title, .title, h3, h2, a";
const AUTHOR_SELECTORS: &str = ".book-author, .author, .authors";
const PUBLISHER_SELECTORS: &str = ".book-publisher, .publisher, .press";
const ISBN_SELECTORS: &str = ".book-isbn, .isbn";
/// 下载按钮的候选选择器。
const DOWNLOAD_SELECTORS: &str = ".download-btn, a[href*='download'], button.btn-download";
/// 站点配额指示器（第 10.3 节：形如 `7/10`）。
const QUOTA_SELECTORS: &str = ".caret-scroll__title, .quota-badge, .user-quota";

/// 包装一个真实的 ChromiumPage 实例。
struct RealBrowserSession {
    page: ChromiumPage,
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

/// 把选择器显式标成 `css:`。
///
/// `rust_drission` 的定位器会先按第一个冒号切前缀，因此 `button:not(.x)`
/// 这类合法 CSS 会被当成未知定位器类型而报错。显式加前缀就不用再逐个避雷。
fn css(selectors: &str) -> String {
    format!("css:{selectors}")
}

/// 最小 percent-encoding：书名里的空格、中文与标点必须编码后才能进 URL。
///
/// 只保留 RFC 3986 的 unreserved 集合。规则短到可以一眼看完，
/// 也不必为此多引一个依赖。
fn urlencoding_simple(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() * 3);
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// 按**优先顺序**逐个试选择器，取第一个非空文本。
///
/// 不把逗号列表整体丢给一次查询：那样返回的是文档顺序里最靠前的元素，
/// 而不是选择器列表里最可信的那个。`.book-title` 必须优先于兜底的 `h3`/`a`，
/// 否则卡片上的一段促销文字就可能被当成书名，进而让匹配层拿错数据去判断。
fn first_text(card: &rust_drission::Element, selectors: &str) -> String {
    for selector in selectors.split(',') {
        let selector = selector.trim();
        if selector.is_empty() {
            continue;
        }
        if let Ok(Some(text)) = card.element_text(&css(selector)) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    String::new()
}

/// 在卡片内按优先顺序找第一个存在的元素。
fn first_child_element(
    card: &rust_drission::Element,
    selectors: &str,
) -> Option<rust_drission::Element> {
    for selector in selectors.split(',') {
        let selector = selector.trim();
        if selector.is_empty() {
            continue;
        }
        if let Ok(Some(element)) = card.element(&css(selector)) {
            return Some(element);
        }
    }
    None
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

/// 把搜索结果页解析成结构化候选（第 8.3 节的输入）。
///
/// 读不出书名的卡片直接丢弃：没有书名就无法参与任何一层匹配，
/// 留着只会把候选数量撑大，让「候选唯一」这个判断失真。
fn collect_candidates(page: &ChromiumPage) -> Vec<CandidateBook> {
    let cards = match page.eles(&css(CARD_SELECTORS)) {
        Ok(cards) => cards,
        Err(_) => return Vec::new(),
    };
    let mut candidates = Vec::new();
    for (index, card) in cards.iter().enumerate() {
        let title = first_text(card, TITLE_SELECTORS);
        if title.is_empty() {
            continue;
        }
        let mut isbn = first_text(card, ISBN_SELECTORS);
        if isbn.is_empty() {
            // 有些站点只把 ISBN 放在卡片属性里
            if let Ok(value) = card.attr("data-isbn") {
                isbn = value.trim().to_string();
            }
        }
        candidates.push(CandidateBook {
            index,
            title,
            author: first_text(card, AUTHOR_SELECTORS),
            publisher: first_text(card, PUBLISHER_SELECTORS),
            isbn,
        });
    }
    candidates
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
const PARTIAL_SUFFIXES: [&str; 3] = [".crdownload", ".tmp", ".part"];

/// 一次任务目录扫描的结果。
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
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if PARTIAL_SUFFIXES.iter().any(|suffix| name.ends_with(suffix)) {
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

/// 登录/注册表单的候选选择器。
const EMAIL_SELECTORS: &str =
    "input[name='email'], input[type='email'], #email, input[name='username']";
const PASSWORD_SELECTORS: &str = "input[name='password'], input[type='password'], #password";
const NICKNAME_SELECTORS: &str = "input[name='nickname'], #nickname, input[name='name']";
const SUBMIT_SELECTORS: &str =
    "button[type='submit'], input[type='submit'], .login-btn, .btn-login, .register-btn";
/// 站点报错提示。
const FORM_ERROR_SELECTORS: &str = ".alert-danger, .error-msg, .login-error, .form-error";
/// 已登录标识：读到其中任意一个才算登录成功。
const LOGGED_IN_SELECTORS: &str = ".caret-scroll__title, .quota-badge, .user-quota, .user-info, \
     .user-nickname, a[href*='logout'], .logout";
/// 邮箱验证码输入框：出现说明注册还没走完。
const MAIL_CODE_SELECTORS: &str =
    "input[name='code'], input[name='verify_code'], .mail-code, .verify-code";

/// 读取站点配额指示器，形如 `7/10`（第 10.3 节）。
///
/// 读不到就返回 `None`。**不得**把「读不到」当成「额度耗尽」：
/// 那会因为一次页面改版就把一批正常账号停用掉。
fn read_quota_from_page(page: &ChromiumPage) -> Option<(u32, u32)> {
    let element = first_page_element(page, QUOTA_SELECTORS)?;
    let text = element.text().ok()?;
    let pattern = regex::Regex::new(r"(\d+)\s*/\s*(\d+)").ok()?;
    let captures = pattern.captures(&text)?;
    let used = captures.get(1)?.as_str().parse::<u32>().ok()?;
    let total = captures.get(2)?.as_str().parse::<u32>().ok()?;
    Some((used, total))
}

/// 读取表单错误提示文本。
fn form_error_text(page: &ChromiumPage) -> Option<String> {
    let element = first_page_element(page, FORM_ERROR_SELECTORS)?;
    let text = element.text().ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
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

/// 登录站点，并**确认**登录成功（第 8.1 节）。
///
/// 只检查「有没有错误提示」是不够的：站点可能既不报错也没登录成功（验证码、
/// 风控跳转、表单静默失败）。那种情况下继续搜索会得到一个空结果页，
/// 于是任务被记成「站点未收录」——一个错误的终态。
/// 所以确认不到已登录标识时返回可重试，而不是继续往下走。
async fn login_site(
    page: &ChromiumPage,
    site_base: &str,
    account: &AccountCredential,
) -> Result<(), AutomationError> {
    page.get(&format!("{site_base}/login")).map_err(|err| {
        AutomationError::new(
            FailureClass::SiteUnavailable,
            format!("打开登录页失败：{err}"),
        )
    })?;

    // Profile 里可能还留着有效会话，此时登录页会直接跳回站内
    let deadline = Instant::now() + LOGIN_TIMEOUT;
    loop {
        if first_page_element(page, LOGGED_IN_SELECTORS).is_some() {
            return Ok(());
        }
        if first_page_element(page, EMAIL_SELECTORS).is_some() {
            break;
        }
        if Instant::now() >= deadline {
            return Err(AutomationError::new(
                FailureClass::SiteUnavailable,
                "登录页既没有已登录标识也没有邮箱输入框，站点结构可能已变化",
            ));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    fill_field(page, EMAIL_SELECTORS, &account.email)?;
    fill_field(page, PASSWORD_SELECTORS, &account.password)?;
    let submit = first_page_element(page, SUBMIT_SELECTORS).ok_or_else(|| {
        AutomationError::new(FailureClass::SiteUnavailable, "登录页未找到提交按钮")
    })?;
    submit.click().map_err(|err| {
        AutomationError::new(FailureClass::Retryable, format!("点击登录按钮失败：{err}"))
    })?;

    let deadline = Instant::now() + LOGIN_TIMEOUT;
    loop {
        if let Some(message) = form_error_text(page) {
            return Err(AutomationError::new(
                classify_form_error(&message),
                format!("站点拒绝登录：{message}"),
            ));
        }
        if first_page_element(page, LOGGED_IN_SELECTORS).is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(AutomationError::new(
                FailureClass::Retryable,
                "登录状态无法确认：既没有错误提示，也没有出现已登录标识",
            ));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
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

        let mut config = BrowserConfig::new()
            .chrome_path(browser_path.display().to_string())
            .headless(spec.headless);
        for arg in launch_args(
            &spec.profile_dir,
            &spec.staging_root,
            spec.proxy_endpoint.as_deref(),
            spec.headless,
        ) {
            config = config.set_argument(arg, None::<String>);
        }
        // 启动是一段有界的同步过程；之后所有等待都改用异步，不再阻塞运行时线程。
        let page = ChromiumPage::new(config).map_err(|err| {
            AutomationError::new(FailureClass::Retryable, format!("启动浏览器失败：{err}"))
        })?;

        if spec.auto_login {
            login_site(&page, &site_base, &spec.account).await?;
        }

        let mut guard = match self.sessions.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.insert(
            spec.session_id.clone(),
            RealBrowserSession {
                page,
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

        events.stage("搜索中");
        // 搜索一律按书名：ISBN 查询在实战中经常搜不到（`BookTarget::title` 的注释）
        let search_url = format!("{site_base}/s/{}", urlencoding_simple(&spec.book.title));
        self.with_session(&session.session_id, |sess| {
            sess.page.get(&search_url).map_err(|err| {
                AutomationError::new(
                    FailureClass::SiteUnavailable,
                    format!("打开搜索页失败：{err}"),
                )
            })
        })?;

        // 等结果渲染。每一轮都先查一次取消：等待期间收到取消要立刻停，
        // 而不是等这一觉睡满（第 10.1 节）。
        let deadline = Instant::now() + RESULTS_TIMEOUT;
        let candidates = loop {
            cancel.check()?;
            let found = self.with_session(&session.session_id, |sess| {
                Ok(collect_candidates(&sess.page))
            })?;
            if !found.is_empty() {
                break found;
            }
            if Instant::now() >= deadline {
                // 超时后仍然空：交给匹配层判成「站点未收录」，措辞与其它路径保持一致
                break Vec::new();
            }
            if !cancel.sleep(POLL_INTERVAL).await {
                return Err(cancel.check().unwrap_err());
            }
        };

        // 第 8.3 节：分层匹配。宁可报「待确认」，也不点第一个搜索结果。
        let target = MatchTarget {
            title: &spec.book.title,
            author: spec.book.author.as_deref(),
            publisher: spec.book.publisher.as_deref(),
            isbn: spec.book.isbn.as_deref(),
        };
        let (chosen, basis): (CandidateBook, MatchBasis) =
            match select_candidate(&target, &candidates) {
                MatchOutcome::Matched { candidate, basis } => (candidate, basis),
                MatchOutcome::NeedsConfirm { reason } => {
                    return Err(AutomationError::new(
                        FailureClass::Uncertain,
                        format!("候选无法确定唯一结果，转人工确认：{reason}"),
                    ));
                }
                MatchOutcome::NotFound { reason } => {
                    return Err(AutomationError::new(
                        FailureClass::BookNotFound,
                        format!("book not found: {reason}"),
                    ));
                }
            };
        let record = MatchRecord::new(spec.book.title.as_str(), candidates.len(), &chosen, basis);
        events.log("信息", record.summary());

        events.stage("下载中");
        // 重新查一次卡片再点。DOM 可能在匹配期间刷新过：下标还在、书却换了的情况
        // 必须被发现，否则「按 ISBN 匹配成功」的结论会被用到另一本书上。
        let clicked_in_card = self.with_session(&session.session_id, |sess| {
            let cards = sess.page.eles(&css(CARD_SELECTORS)).map_err(|err| {
                AutomationError::new(
                    FailureClass::Retryable,
                    format!("重新读取搜索结果失败：{err}"),
                )
            })?;
            let card = cards.get(chosen.index).ok_or_else(|| {
                AutomationError::new(
                    FailureClass::Retryable,
                    "搜索结果在点击前发生变化，本次不下载",
                )
            })?;
            let current = first_text(card, TITLE_SELECTORS);
            if normalize_title(&current) != normalize_title(&chosen.title) {
                return Err(AutomationError::new(
                    FailureClass::Retryable,
                    format!(
                        "搜索结果在点击前发生变化（原《{}》，现《{current}》），拒绝盲点",
                        chosen.title
                    ),
                ));
            }
            if let Some(button) = first_child_element(card, DOWNLOAD_SELECTORS) {
                button.click().map_err(|err| {
                    AutomationError::new(
                        FailureClass::Retryable,
                        format!("点击下载按钮失败：{err}"),
                    )
                })?;
                return Ok(true);
            }
            // 卡片上没有下载控件：点开详情页再找
            card.click().map_err(|err| {
                AutomationError::new(
                    FailureClass::Retryable,
                    format!("打开图书详情页失败：{err}"),
                )
            })?;
            Ok(false)
        })?;

        if !clicked_in_card {
            let deadline = Instant::now() + RESULTS_TIMEOUT;
            loop {
                cancel.check()?;
                let clicked = self.with_session(&session.session_id, |sess| {
                    match first_page_element(&sess.page, DOWNLOAD_SELECTORS) {
                        Some(button) => {
                            button.click().map_err(|err| {
                                AutomationError::new(
                                    FailureClass::Retryable,
                                    format!("点击下载按钮失败：{err}"),
                                )
                            })?;
                            Ok(true)
                        }
                        None => Ok(false),
                    }
                })?;
                if clicked {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(AutomationError::new(
                        FailureClass::Uncertain,
                        "详情页没有出现下载入口，结果待确认",
                    ));
                }
                if !cancel.sleep(POLL_INTERVAL).await {
                    return Err(cancel.check().unwrap_err());
                }
            }
        }

        let (staged_file, size_bytes) = wait_for_download(spec, events, cancel).await?;

        events.stage("入库中");
        // 「下完了」和「下对了」是两件事：扩展名、书名、大小、签名四项必须全过。
        // 归因也要分开：内容不是一本书（错误页/过小）换一次会话很可能就好了，
        // 而扩展名或书名对不上说明拿到的是**另一个东西**，重试只会得到同样的结果。
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
                VerifyError::UnexpectedExtension { .. } | VerifyError::TitleMismatch { .. } => {
                    FailureClass::Fatal
                }
            };
            AutomationError::new(class, format!("下载文件未通过入库前校验：{err}"))
        })?;
        if evidence.size_bytes != size_bytes {
            return Err(AutomationError::new(
                FailureClass::Retryable,
                format!(
                    "文件在校验期间仍在变化（{size_bytes} → {} 字节），本次结果不采用",
                    evidence.size_bytes
                ),
            ));
        }

        // 配额指示器读不到就是 `None`：绝不能因此把账号判成额度耗尽（第 10.3 节）
        let quota_indicator = self
            .with_session(&session.session_id, |sess| {
                Ok(read_quota_from_page(&sess.page))
            })
            .unwrap_or(None);
        if let Some((used, total)) = quota_indicator {
            events.emit(crate::types::AutomationEvent::Quota { used, total });
        }

        Ok(DownloadOutcome {
            staged_file,
            size_bytes: evidence.size_bytes,
            quota_indicator,
            evidence: Some(evidence),
            match_record: Some(record),
        })
    }

    async fn register_account(
        &self,
        session: &SessionHandle,
        spec: &RegistrationSpec,
        events: &EventSink,
    ) -> Result<RegistrationOutcome, AutomationError> {
        events.stage("注册中");
        let site_base =
            self.with_session(&session.session_id, |sess| Ok(sess.site_base.clone()))?;
        let register_url = format!("{site_base}/register");
        self.with_session(&session.session_id, |sess| {
            sess.page.get(&register_url).map_err(|err| {
                AutomationError::new(
                    FailureClass::SiteUnavailable,
                    format!("打开注册页失败：{err}"),
                )
            })
        })?;

        let deadline = Instant::now() + LOGIN_TIMEOUT;
        loop {
            let ready = self.with_session(&session.session_id, |sess| {
                Ok(first_page_element(&sess.page, EMAIL_SELECTORS).is_some())
            })?;
            if ready {
                break;
            }
            if Instant::now() >= deadline {
                return Err(AutomationError::new(
                    FailureClass::SiteUnavailable,
                    "注册页未出现邮箱输入框，站点结构可能已变化",
                ));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }

        // 提交前准备邮件提取游标（记录开始时间戳）
        let mail_cursor = if let Some(provider) = &spec.mail_provider {
            // 人工 Provider 的有效期与 Worker 创建的人工事项保持一致（10 分钟）。
            // 自动 Provider 会在自身更短的配置时限内结束，随后由 Router 另开完整
            // 的人工输入窗口，不会挤占用户处理验证码的时间。
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
            if first_page_element(&sess.page, NICKNAME_SELECTORS).is_some() {
                fill_field(&sess.page, NICKNAME_SELECTORS, &spec.account.nickname)?;
            }
            fill_field(&sess.page, EMAIL_SELECTORS, &spec.account.email)?;
            fill_field(&sess.page, PASSWORD_SELECTORS, &spec.account.password)?;
            let submit = first_page_element(&sess.page, SUBMIT_SELECTORS).ok_or_else(|| {
                AutomationError::new(FailureClass::SiteUnavailable, "注册页未找到提交按钮")
            })?;
            submit.click().map_err(|err| {
                AutomationError::new(FailureClass::Retryable, format!("点击注册按钮失败：{err}"))
            })
        })?;

        // 判定注册结果。三种结局都必须有明确证据，看不到证据就报「无法确认」，
        // 不能默认成功——把没注册成功的账号记成可用，会在后面每次调度时都失败一次。
        let deadline = Instant::now() + LOGIN_TIMEOUT;
        loop {
            let verdict = self.with_session(&session.session_id, |sess| {
                if let Some(message) = form_error_text(&sess.page) {
                    return Ok(Some(Err(message)));
                }
                if first_page_element(&sess.page, MAIL_CODE_SELECTORS).is_some() {
                    return Ok(Some(Ok(true)));
                }
                if first_page_element(&sess.page, LOGGED_IN_SELECTORS).is_some() {
                    return Ok(Some(Ok(false)));
                }
                Ok(None)
            })?;
            match verdict {
                Some(Ok(awaiting_verification)) => {
                    if awaiting_verification {
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

                        // 如果配置了 mail_provider 且拥有 cursor，尝试自动取码并输入
                        if let (Some(provider), Some(cursor)) = (&spec.mail_provider, &mail_cursor)
                        {
                            events.stage("自动提取验证码");
                            events.log(
                                "信息",
                                format!(
                                    "检测到验证码输入框，通过 Provider [{}] 自动获取验证码...",
                                    provider.name()
                                ),
                            );
                            match provider.await_code(cursor, &spec.cancel).await {
                                Ok(res) => {
                                    events.stage("提交验证码中");
                                    events.log("信息", "已成功获取验证码，正在输入并提交验证...");
                                    self.with_session(&session.session_id, |sess| {
                                        fill_field(&sess.page, MAIL_CODE_SELECTORS, &res.code)?;
                                        if let Some(btn) =
                                            first_page_element(&sess.page, SUBMIT_SELECTORS)
                                        {
                                            let _ = btn.click();
                                        }
                                        Ok(())
                                    })?;

                                    // 等待登录成功或错误
                                    let verify_deadline = Instant::now() + Duration::from_secs(15);
                                    while Instant::now() < verify_deadline {
                                        let logged_in =
                                            self.with_session(&session.session_id, |sess| {
                                                if let Some(message) = form_error_text(&sess.page) {
                                                    return Err(AutomationError::new(
                                                        classify_form_error(&message),
                                                        format!("验证码提交后站点拒绝：{message}"),
                                                    ));
                                                }
                                                Ok(first_page_element(
                                                    &sess.page,
                                                    LOGGED_IN_SELECTORS,
                                                )
                                                .is_some())
                                            })?;
                                        if logged_in {
                                            events.stage("注册完成");
                                            return Ok(RegistrationOutcome {
                                                already_exists: false,
                                                awaiting_verification: false,
                                            });
                                        }
                                        tokio::time::sleep(POLL_INTERVAL).await;
                                    }
                                    return Err(AutomationError::new(
                                        FailureClass::Retryable,
                                        "验证码已提交，但站点未在时限内确认注册成功",
                                    ));
                                }
                                Err(crate::mail_code::MailCodeError::ManualFallbackRequired) => {
                                    events.log("警告", "自动提取邮件验证码已降级为人工处理");
                                }
                                Err(err) => {
                                    events.log(
                                        "警告",
                                        format!("自动提取邮件验证码失败（{err}），转为待人工确认"),
                                    );
                                }
                            }
                        }

                        return Ok(RegistrationOutcome {
                            already_exists: false,
                            awaiting_verification: true,
                        });
                    }

                    return Ok(RegistrationOutcome {
                        already_exists: false,
                        awaiting_verification: false,
                    });
                }
                Some(Err(message)) => {
                    const EXISTS_MARKS: [&str; 4] = ["已存在", "已注册", "already", "exists"];
                    let lowered = message.to_lowercase();
                    if EXISTS_MARKS.iter().any(|mark| lowered.contains(mark)) {
                        // 站点已有同邮箱账号：账号应被停用而不是反复重试
                        return Ok(RegistrationOutcome {
                            already_exists: true,
                            awaiting_verification: false,
                        });
                    }
                    return Err(AutomationError::new(
                        classify_form_error(&message),
                        format!("站点拒绝注册：{message}"),
                    ));
                }
                None => {}
            }
            if Instant::now() >= deadline {
                return Err(AutomationError::new(
                    FailureClass::Retryable,
                    "注册结果无法确认：既没有错误提示，也没有验证码输入框或已登录标识",
                ));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
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
            urlencoding_simple("算法导论"),
            "%E7%AE%97%E6%B3%95%E5%AF%BC%E8%AE%BA"
        );
        assert_eq!(urlencoding_simple("C++ Primer"), "C%2B%2B%20Primer");
        assert_eq!(urlencoding_simple("a-b_c.d~e"), "a-b_c.d~e");
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
    async fn scanning_a_missing_dir_is_retryable_not_a_panic() {
        let err = scan_task_dir(Path::new("/definitely/missing/task-dir"))
            .await
            .unwrap_err();
        assert_eq!(err.class, FailureClass::Retryable);
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
