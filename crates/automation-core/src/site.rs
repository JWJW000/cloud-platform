//! `zh.loves.works`（Z-Library 中文镜像）站点协议。
//!
//! 选择器、页面脚本和判定规则与桌面 Tauri 版 `src/browser/worker.rs` 对齐。
//! 通用 CSS（`.book-item` / `/register` / `/login`）在这个站上是空的，
//! 必须用这里的真实 DOM。

use serde_json::Value;
use url::Url;

use crate::matching::CandidateBook;
use crate::verify::filename_matches_title;

/// 注册页路径（不是 `/register`）。
pub const REGISTRATION_PATH: &str = "/registration";
/// 搜索页标题里必须出现的站点原文，用来跳过反爬挑战首屏。
pub const SEARCH_PAGE_TITLE_MARK: &str = "在 Z-Library 上搜索";
/// 站点挑战 cookie。
pub const CHALLENGE_COOKIE: &str = "c_token";

/// 提取搜索结果卡片：只认 `z-bookcard`，跳过 `hidden`。
pub const CARD_SCRAPE_SCRIPT: &str = r#"
    (() => {
        const cards = document.querySelectorAll('z-bookcard');
        const results = [];
        for (const card of cards) {
            if (card.closest('[hidden]') || card.hidden) {
                continue;
            }
            const root = card.shadowRoot || card;
            const titleEl = root.querySelector('.title, .book-title, h3, .name, a[href*="/book/"]');
            let title = titleEl ? (titleEl.innerText || titleEl.textContent || '').trim() : '';
            if (!title) {
                const href = card.getAttribute('href') || '';
                const slug = href.split('/').pop() || '';
                try {
                    let decoded = decodeURIComponent(slug);
                    decoded = decoded.endsWith('.html') ? decoded.slice(0, -5)
                        : (decoded.endsWith('.htm') ? decoded.slice(0, -4) : decoded);
                    title = decoded.trim();
                } catch (e) { /* 解码失败保持空标题 */ }
            }
            const download = card.getAttribute('download') || '';
            const isbn = card.getAttribute('isbn') || '';
            results.push({ title, download, isbn });
        }
        return results;
    })()
"#;

/// `.notFound` 与「没有找到任何信息」——异步渲染，必须反复检测。
pub const NOT_FOUND_SCRIPT: &str = r#"
    (() => {
        const notFoundEl = document.querySelector('.notFound, .not-found, .search-no-results, #searchResultBox .notFound');
        if (notFoundEl) return true;
        const text = (document.body ? document.body.innerText : '') || '';
        if (text.includes('没有找到任何信息') ||
            text.includes('没有找到相关图书') ||
            text.includes('未找到相关结果') ||
            text.includes('0 本书') ||
            text.includes('No books found')) {
            return true;
        }
        return false;
    })()
"#;

/// 站点判定查询无精确匹配时给出的相似书提示。
pub const SIMILAR_BOOKS_SCRIPT: &str = r#"
    (() => {
        const text = (document.body ? document.body.innerText : '') || '';
        return text.includes('不完全匹配')
            || text.includes('非常相似')
            || /do not fully match/i.test(text);
    })()
"#;

/// 配额指示器 `.caret-scroll__title`，忽略捐款 `$0` 等同 class 文本。
pub const QUOTA_SCRIPT: &str = r#"
    (() => {
        const els = document.querySelectorAll('.caret-scroll__title');
        for (const el of els) {
            const t = (el.innerText || el.textContent || '').trim();
            const m = t.match(/^(\d+)\s*\/\s*(\d+)$/);
            if (m) return [parseInt(m[1], 10), parseInt(m[2], 10)];
        }
        return null;
    })()
"#;

/// 每日限额错误页。
pub const QUOTA_LIMIT_PAGE_SCRIPT: &str = r#"
    (() => {
        if (document.querySelector('.download-limits-error__header, .download-limits-error__member, [class*="download-limits"]')) {
            return true;
        }
        const text = (document.body ? document.body.innerText : '') || '';
        if (text.includes('每日限额已用完') ||
            text.includes('每日限额已达到') ||
            text.includes('每日下载限额') ||
            /daily\s+(download\s+)?limit/i.test(text)) {
            return true;
        }
        return false;
    })()
"#;

/// 邮箱已存在（异步渲染，扫整页文本）。
pub const EMAIL_EXISTS_SCRIPT: &str = r#"
    (() => {
        const text = (document.body ? document.body.innerText : '') || '';
        return text.includes('已存在带有该电子邮件')
            || text.includes('已存在带有该电子邮箱')
            || text.includes('请登录您的账户')
            || /already\s+exists/i.test(text)
            || /account\s+with\s+this\s+email/i.test(text)
            || /already\s+registered/i.test(text);
    })()
"#;

/// 连接问题拦截页。
pub const CONNECTION_PROBLEM_SCRIPT: &str = r#"
    (() => {
        const text = (document.body ? document.body.innerText : '') || '';
        return text.includes('Connection Problem Detected')
            || text.includes('Something’s stopping this page')
            || text.includes("Something's stopping this page")
            || text.includes('Opps, Connection Problem')
            || text.includes('blackbox@z-library');
    })()
"#;

/// 反爬挑战页。
pub const CHALLENGE_PAGE_SCRIPT: &str = r#"
    (() => {
        const text = (document.body ? document.body.innerText : '') || '';
        return text.includes('checking your browser')
            || text.includes('Checking your browser')
            || text.includes('just a moment')
            || text.includes('verify you are human');
    })()
"#;

/// 页面正文前 4000 字，用于识别 Chrome 网络错误页。
pub const BODY_TEXT_SCRIPT: &str = r#"
    (() => {
        const text = (document.body ? document.body.innerText : '') || '';
        return text.slice(0, 4000);
    })()
"#;

/// 一张站点搜索卡片。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteCard {
    /// 可见卡片下标（跳过 hidden 之后）。
    pub index: usize,
    /// 书名。
    pub title: String,
    /// ISBN，可能为空。
    pub isbn: String,
    /// `/dl/...` 下载路径；空字符串表示这张卡不能下。
    pub download: String,
}

impl SiteCard {
    /// 转成匹配层候选。作者/出版社这个站的卡片属性里没有，留空走书名/ISBN。
    pub fn to_candidate(&self) -> CandidateBook {
        CandidateBook {
            index: self.index,
            title: self.title.clone(),
            author: String::new(),
            publisher: String::new(),
            isbn: self.isbn.clone(),
        }
    }
}

/// 从 `run_js` 返回值解析卡片列表。
pub fn parse_cards(value: &Value) -> Vec<SiteCard> {
    let array = value
        .get("value")
        .and_then(Value::as_array)
        .or_else(|| value.as_array());
    let Some(array) = array else {
        return Vec::new();
    };
    let mut cards = Vec::new();
    for item in array {
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let download = item
            .get("download")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if title.is_empty() || download.is_empty() {
            continue;
        }
        let isbn = item
            .get("isbn")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        cards.push(SiteCard {
            index: cards.len(),
            title,
            isbn,
            download,
        });
    }
    cards
}

/// 在卡片中挑目标书：ISBN 优先，否则书名双向包含。
pub fn find_download_in_cards<'a>(
    cards: &'a [SiteCard],
    title: &str,
    isbn: Option<&str>,
) -> Option<&'a SiteCard> {
    let target_isbn = isbn.map(normalize_isbn).filter(|s| !s.is_empty());
    if let Some(target) = &target_isbn {
        if let Some(card) = cards.iter().find(|card| {
            let card_isbn = normalize_isbn(&card.isbn);
            !card_isbn.is_empty() && card_isbn == *target
        }) {
            return Some(card);
        }
    }
    cards
        .iter()
        .find(|card| filename_matches_title(std::path::Path::new(&card.title), title))
}

/// ISBN 归一化：字母数字并转大写。
pub fn normalize_isbn(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// 从 JS 返回值读布尔。
pub fn js_bool(value: &Value) -> bool {
    value
        .get("value")
        .and_then(Value::as_bool)
        .or_else(|| value.as_bool())
        .unwrap_or(false)
}

/// 从配额脚本结果读 `(已用, 总额)`。
pub fn parse_quota(value: &Value) -> Option<(u32, u32)> {
    let arr = value
        .get("value")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())?;
    if arr.len() != 2 {
        return None;
    }
    let used = arr[0]
        .as_u64()
        .or_else(|| arr[0].as_i64().map(|n| n as u64))? as u32;
    let total = arr[1]
        .as_u64()
        .or_else(|| arr[1].as_i64().map(|n| n as u64))? as u32;
    Some((used, total))
}

/// 站点侧额度是否已用完。读不到指示器时返回 false。
pub fn quota_is_exhausted(quota: Option<(u32, u32)>) -> bool {
    matches!(quota, Some((used, total)) if total > 0 && used >= total)
}

/// Chrome 错误页 / 站点不可达正文。
pub fn site_unavailable_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    text.contains("找不到该网址")
        || text.contains("无法访问此网站")
        || text.contains("无法连接到互联网")
        || text.contains("请检查互联网连接")
        || text.contains("连接已重置")
        || lower.contains("this site can't be reached")
        || lower.contains("this site can’t be reached")
        || lower.contains("server ip address could not be found")
        || lower.contains("dns_probe_finished")
        || lower.contains("err_name_not_resolved")
        || lower.contains("err_connection_refused")
        || lower.contains("err_connection_reset")
        || lower.contains("err_connection_timed_out")
}

/// 导航错误是否属于站点暂时不可用。
pub fn navigation_error_is_site_unavailable(message: &str) -> bool {
    site_unavailable_text(message)
        || message.to_ascii_lowercase().contains("net::err_")
        || message.to_ascii_lowercase().contains("dns")
}

/// Chrome 经本地 GOST 做 HTTPS CONNECT 失败，或落到 `chrome-error://` 拦截页。
pub fn is_proxy_tunnel_error(message: &str) -> bool {
    let text = message.to_ascii_lowercase();
    text.contains("err_tunnel")
        || text.contains("err_proxy")
        || text.contains("tunnel_connection")
        || text.contains("proxy_connection")
        || text.contains("err_no_supported_proxies")
        || text.contains("chrome-error://")
        || text.contains("proxy failed")
}

/// 密码是否满足站点 9..=32 限制。
pub fn password_length_ok(password: &str) -> bool {
    (9..=32).contains(&password.chars().count())
}

/// 昵称：账号昵称为空时用邮箱 @ 前一段。
pub fn nickname_from_email(email: &str) -> String {
    email
        .split('@')
        .next()
        .unwrap_or("reader")
        .chars()
        .take(32)
        .collect()
}

/// 最小 percent-encoding：书名里的空格、中文与标点必须编码后才能进 URL。
pub fn percent_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() * 3);
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// 搜索 URL：按书名搜，带站点排序；按目标格式过滤扩展名。
pub fn search_url(site_base: &str, title: &str, format: &str) -> String {
    let encoded = percent_encode(title);
    let mut url = format!(
        "{}/s/{}?order=bestmatch",
        site_base.trim_end_matches('/'),
        encoded
    );
    let ext = format.trim().trim_start_matches('.').to_ascii_lowercase();
    if ext == "pdf" || ext == "epub" {
        url.push_str(&format!("&extensions%5B0%5D={ext}"));
    }
    url
}

/// 站点根 URL（带尾斜杠），用于读挑战 cookie。
pub fn site_root_url(site_base: &str) -> String {
    format!("{}/", site_base.trim_end_matches('/'))
}

/// 把相对 `/dl/...` 拼成绝对地址。
pub fn absolute_download_url(site_base: &str, download_path: &str) -> Result<Url, url::ParseError> {
    Url::parse(download_path).or_else(|_| {
        let base = Url::parse(&format!("{}/", site_base.trim_end_matches('/')))?;
        base.join(download_path)
    })
}

/// 挑战刷新判定。`None` 表示证据不足、继续等。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChallengeRefresh {
    /// 拿到了新 token。
    Refreshed,
    /// 旧 token 仍被接受。
    AlreadyValid,
    /// 期限内没有 token。
    Missing,
    /// 超时，沿用旧 token。
    TimedOut,
}

/// 由刷新前后 token 与页面是否已是真实站点决定结果。
pub fn refresh_decision(
    before: Option<&str>,
    after: Option<&str>,
    page_real: bool,
) -> Option<ChallengeRefresh> {
    match (before, after) {
        (Some(before_token), Some(after_token)) if before_token != after_token => {
            Some(ChallengeRefresh::Refreshed)
        }
        (None, Some(_)) => Some(ChallengeRefresh::Refreshed),
        (Some(_), Some(_)) if page_real => Some(ChallengeRefresh::AlreadyValid),
        (Some(_), None) if page_real => Some(ChallengeRefresh::AlreadyValid),
        _ => None,
    }
}

/// 可重试的注册错误（验证码/上下文/超时），终态错误不重试。
pub fn is_retryable_registration_error(message: &str) -> bool {
    if message.contains("已存在")
        || message.contains("already exists")
        || message.contains("password length")
        || message.contains("out of range 9..=32")
    {
        return false;
    }
    message.contains("Timed out")
        || message.contains("verification input did not appear")
        || message.contains("returned no confirmation code")
        || message.contains("registration rejected")
        || message.contains("verification code rejected")
        || message.contains("Cannot find context")
        || message.contains("验证码")
        || message.contains("注册页未出现")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hidden_and_empty_download_cards_are_dropped() {
        let value = json!([
            {"title": "孤独天涯行", "download": "", "isbn": "123"},
            {"title": "目标书", "download": "/dl/abc", "isbn": "9787111407010"}
        ]);
        let cards = parse_cards(&value);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].title, "目标书");
        assert_eq!(cards[0].download, "/dl/abc");
    }

    #[test]
    fn isbn_match_beats_title() {
        let cards = vec![
            SiteCard {
                index: 0,
                title: "算法导论习题解答".into(),
                isbn: "9780000000000".into(),
                download: "/dl/wrong".into(),
            },
            SiteCard {
                index: 1,
                title: "别的".into(),
                isbn: "978-7-111-40701-0".into(),
                download: "/dl/right".into(),
            },
        ];
        let chosen = find_download_in_cards(&cards, "算法导论", Some("9787111407010")).unwrap();
        assert_eq!(chosen.download, "/dl/right");
    }

    #[test]
    fn edition_suffix_title_still_matches() {
        let cards = vec![SiteCard {
            index: 0,
            title: "水利工程建设投资控制 第2版".into(),
            isbn: String::new(),
            download: "/dl/ok".into(),
        }];
        let chosen = find_download_in_cards(&cards, "水利工程建设投资控制", None).unwrap();
        assert_eq!(chosen.download, "/dl/ok");
    }

    #[test]
    fn quota_parser_ignores_non_pair_values() {
        assert_eq!(parse_quota(&json!({"value": [7, 10]})), Some((7, 10)));
        assert_eq!(parse_quota(&json!({"value": null})), None);
        assert!(quota_is_exhausted(Some((10, 10))));
        assert!(!quota_is_exhausted(Some((7, 10))));
        assert!(!quota_is_exhausted(None));
    }

    #[test]
    fn password_and_nickname_helpers() {
        assert!(password_length_ok("abcdefghij"));
        assert!(!password_length_ok("short"));
        assert_eq!(nickname_from_email("alice@example.com"), "alice");
    }

    #[test]
    fn search_url_uses_title_and_format_filter() {
        let url = search_url("https://zh.loves.works", "算法导论", "pdf");
        assert!(url.starts_with("https://zh.loves.works/s/"));
        assert!(url.contains("order=bestmatch"));
        assert!(url.contains("extensions%5B0%5D=pdf"));
        assert!(!url.contains("register"));
    }

    #[test]
    fn challenge_refresh_table() {
        assert_eq!(
            refresh_decision(None, Some("new"), false),
            Some(ChallengeRefresh::Refreshed)
        );
        assert_eq!(
            refresh_decision(Some("a"), Some("b"), false),
            Some(ChallengeRefresh::Refreshed)
        );
        assert_eq!(
            refresh_decision(Some("a"), Some("a"), true),
            Some(ChallengeRefresh::AlreadyValid)
        );
        assert_eq!(refresh_decision(Some("a"), Some("a"), false), None);
    }

    #[test]
    fn exists_error_is_not_retryable() {
        assert!(!is_retryable_registration_error(
            "email already exists (该邮箱已注册，自动停止注册)"
        ));
        assert!(is_retryable_registration_error(
            "verification input did not appear"
        ));
    }
}
