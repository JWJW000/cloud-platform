//! 站点文件走 HTTP 直连，不依赖 Chromium 下载管理器。
//!
//! 浏览器只负责登录、过 JS 挑战、给出 `/dl/...` 临时路径；真正的字节由
//! `ureq` 带着 Cookie/`c_token` 拉取，并可在中断后用 Range 续传。
//! 这与桌面 Tauri 版 `download_via_http` 同一条协议。

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use platform_domain::FailureClass;
use rust_drission::Cookie;
use url::Url;

use crate::cancel::CancelToken;
use crate::engine::EventSink;
use crate::types::AutomationError;

const MAX_RETRIES: usize = 2;
const MAX_TRANSIENT_STATUS_RETRIES: usize = 2;
const MAX_REDIRECTS: usize = 8;

/// 一次 HTTP 下载所需的会话上下文。
pub struct HttpDownloadRequest<'a> {
    /// 本机转发代理，例如 `http://127.0.0.1:19001`。
    pub proxy_url: Option<&'a str>,
    /// 与浏览器一致的 User-Agent。
    pub user_agent: &'a str,
    /// 浏览器当前 Cookie（含 `c_token`）。
    pub cookies: &'a [Cookie],
    /// Referer，通常是搜索页。
    pub referer: &'a str,
    /// `/dl/...` 绝对或相对 URL。
    pub url: Url,
    /// 任务独占暂存目录。
    pub staging_dir: &'a Path,
    /// 目标书名，用于缺省文件名。
    pub title: &'a str,
    /// 任务编号，隔离 `.part` 文件。
    pub task_id: &'a str,
    /// 无数据预算，沿用任务 stall_timeout。
    pub timeout: Duration,
}

/// HTTP 下载结果。
pub enum HttpDownloadOutcome {
    /// 真实文件已落盘。
    File(PathBuf),
    /// 站点用 200 返回了 HTML（限额页或挑战页）。
    Html { path: PathBuf, snippet: String },
}

/// 构造下载 Agent：不走环境代理，重定向手动跟随。
pub fn build_agent(
    user_agent: &str,
    proxy_url: Option<&str>,
) -> Result<ureq::Agent, AutomationError> {
    let mut builder = ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .redirects(0)
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(20))
        .user_agent(user_agent);
    if let Some(proxy_url) = proxy_url {
        let proxy = ureq::Proxy::new(proxy_url).map_err(|err| {
            AutomationError::new(
                FailureClass::ProxyFailure,
                format!("invalid configured proxy URL: {proxy_url}: {err}"),
            )
        })?;
        builder = builder.proxy(proxy);
    }
    Ok(builder.build())
}

/// 按目标 URL 的 domain/path 过滤 Cookie，避免把站点会话送到 CDN。
pub fn cookie_header_for_url(cookies: &[Cookie], url: &Url) -> String {
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    let request_path = url.path();
    cookies
        .iter()
        .filter(|cookie| {
            let domain_matches = cookie.domain.as_deref().is_none_or(|domain| {
                let domain = domain.trim_start_matches('.').to_ascii_lowercase();
                host == domain || host.ends_with(&format!(".{domain}"))
            });
            let path_matches = cookie
                .path
                .as_deref()
                .is_none_or(|path| request_path.starts_with(path));
            domain_matches && path_matches
        })
        .map(|cookie| format!("{}={}", cookie.name, cookie.value))
        .collect::<Vec<_>>()
        .join("; ")
}

/// 流式下载到任务目录。`cancel` 在读循环里检查。
pub fn download_file(
    request: HttpDownloadRequest<'_>,
    events: &EventSink,
    cancel: &CancelToken,
) -> Result<HttpDownloadOutcome, AutomationError> {
    let agent = build_agent(request.user_agent, request.proxy_url)?;
    let transfer_deadline = Instant::now() + request.timeout.max(Duration::from_secs(20));

    let (first_response, mut resolved_url) = request_download_response(
        &agent,
        &request.url,
        request.cookies,
        request.referer,
        None,
        cancel,
    )?;

    let content_type = first_response
        .header("Content-Type")
        .unwrap_or("")
        .to_ascii_lowercase();
    if content_type.contains("text/html") {
        let html_path = unique_download_path(
            request.staging_dir,
            &format!("{}.html", sanitize_filename(request.title)),
        );
        let mut file = File::create(&html_path).map_err(|err| {
            AutomationError::new(
                FailureClass::Retryable,
                format!("failed to create {}: {err}", html_path.display()),
            )
        })?;
        let mut reader = first_response.into_reader();
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).map_err(|err| {
            AutomationError::new(
                FailureClass::Retryable,
                format!("failed to save HTML response: {err}"),
            )
        })?;
        file.write_all(&buf).map_err(|err| {
            AutomationError::new(
                FailureClass::Retryable,
                format!("failed to write HTML response: {err}"),
            )
        })?;
        let snippet = String::from_utf8_lossy(&buf).chars().take(400).collect();
        return Ok(HttpDownloadOutcome::Html {
            path: html_path,
            snippet,
        });
    }

    let file_name =
        response_file_name(&first_response, &resolved_url, request.title, &content_type);
    let final_path = unique_download_path(request.staging_dir, &file_name);
    let part_path = request
        .staging_dir
        .join(format!(".task-{}.part", sanitize_filename(request.task_id)));

    let mut response = Some(first_response);
    let saved_len = std::fs::metadata(&part_path)
        .map(|meta| meta.len())
        .unwrap_or(0);
    if saved_len > 0 {
        let (resume_response, resume_url) = request_download_response(
            &agent,
            &request.url,
            request.cookies,
            request.referer,
            Some(saved_len),
            cancel,
        )?;
        response = Some(resume_response);
        resolved_url = resume_url;
    }

    let mut last_error: Option<AutomationError> = None;
    for attempt in 0..=MAX_RETRIES {
        cancel.check()?;
        if Instant::now() >= transfer_deadline {
            return Err(AutomationError::new(
                FailureClass::Uncertain,
                format!(
                    "direct HTTP download exceeded total time budget of {}s",
                    request.timeout.as_secs().max(20)
                ),
            ));
        }

        let existing_len = std::fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
        if attempt > 0 {
            match request_download_response(
                &agent,
                &resolved_url,
                request.cookies,
                request.referer,
                Some(existing_len),
                cancel,
            )
            .or_else(|_| {
                request_download_response(
                    &agent,
                    &request.url,
                    request.cookies,
                    request.referer,
                    Some(existing_len),
                    cancel,
                )
            }) {
                Ok((next_response, next_url)) => {
                    response = Some(next_response);
                    resolved_url = next_url;
                }
                Err(err) => {
                    last_error = Some(err);
                    if attempt < MAX_RETRIES {
                        std::thread::sleep(Duration::from_secs(1_u64 << attempt.min(1)));
                        continue;
                    }
                    break;
                }
            }
        }

        let response = response.take().ok_or_else(|| {
            AutomationError::new(FailureClass::Retryable, "missing HTTP download response")
        })?;
        let expected_total = match expected_download_size(&response, existing_len) {
            Ok(expected) => expected,
            Err(err) => {
                let _ = std::fs::remove_file(&part_path);
                last_error = Some(err);
                if attempt < MAX_RETRIES {
                    std::thread::sleep(Duration::from_secs(1_u64 << attempt.min(1)));
                    continue;
                }
                break;
            }
        };

        let append = existing_len > 0 && response.status() == 206;
        let mut output = OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(&part_path)
            .map_err(|err| {
                AutomationError::new(
                    FailureClass::Retryable,
                    format!("failed to open {}: {err}", part_path.display()),
                )
            })?;
        let mut reader = response.into_reader();
        let mut buffer = [0_u8; 64 * 1024];
        let mut written = existing_len;
        let mut transfer_err: Option<AutomationError> = None;
        loop {
            if cancel.is_cancelled() {
                transfer_err = Some(cancel.check().unwrap_err());
                break;
            }
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    if let Err(err) = output.write_all(&buffer[..count]) {
                        transfer_err = Some(AutomationError::new(
                            FailureClass::Retryable,
                            format!("write download stream failed: {err}"),
                        ));
                        break;
                    }
                    written += count as u64;
                    events.progress(written, expected_total.unwrap_or(0));
                }
                Err(err) => {
                    transfer_err = Some(classify_stream_error(&err));
                    break;
                }
            }
        }
        let _ = output.flush();
        let _ = output.sync_all();

        if let Some(err) = transfer_err {
            last_error = Some(err);
            if attempt < MAX_RETRIES && !cancel.is_cancelled() {
                std::thread::sleep(Duration::from_secs(1_u64 << attempt.min(1)));
                continue;
            }
            break;
        }

        let actual_size = std::fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
        if let Err(err) = validate_download_size(expected_total, actual_size) {
            last_error = Some(err);
            if attempt < MAX_RETRIES {
                std::thread::sleep(Duration::from_secs(1_u64 << attempt.min(1)));
                continue;
            }
            break;
        }

        std::fs::rename(&part_path, &final_path).map_err(|err| {
            AutomationError::new(
                FailureClass::Retryable,
                format!(
                    "failed to finalize download {} -> {}: {err}",
                    part_path.display(),
                    final_path.display()
                ),
            )
        })?;
        return Ok(HttpDownloadOutcome::File(final_path));
    }

    Err(last_error.unwrap_or_else(|| {
        AutomationError::new(
            FailureClass::Retryable,
            format!("direct HTTP download failed after {MAX_RETRIES} retries"),
        )
    }))
}

fn request_download_response(
    agent: &ureq::Agent,
    initial_url: &Url,
    cookies: &[Cookie],
    referer: &str,
    range_start: Option<u64>,
    cancel: &CancelToken,
) -> Result<(ureq::Response, Url), AutomationError> {
    let mut current = initial_url.clone();
    for _ in 0..MAX_REDIRECTS {
        cancel.check()?;
        let mut transient_attempt = 0;
        let response = loop {
            cancel.check()?;
            let mut request = agent
                .get(current.as_str())
                .set("Accept", "*/*")
                .set("Accept-Encoding", "identity")
                .set("Referer", referer);
            let cookie_header = cookie_header_for_url(cookies, &current);
            if !cookie_header.is_empty() {
                request = request.set("Cookie", &cookie_header);
            }
            if let Some(offset) = range_start.filter(|offset| *offset > 0) {
                request = request.set("Range", &format!("bytes={offset}-"));
            }
            match request.call() {
                Ok(response) => break response,
                Err(ureq::Error::Status(status, response))
                    if is_transient_download_status(status)
                        && transient_attempt < MAX_TRANSIENT_STATUS_RETRIES =>
                {
                    transient_attempt += 1;
                    let delay = retry_after_delay(&response, transient_attempt);
                    std::thread::sleep(delay);
                }
                Err(ureq::Error::Status(status, response)) => {
                    return Err(AutomationError::new(
                        class_for_http_status(status),
                        format!(
                            "download server returned HTTP {status} for {current} ({})",
                            response.status_text()
                        ),
                    ));
                }
                Err(err) if transient_attempt < MAX_TRANSIENT_STATUS_RETRIES => {
                    transient_attempt += 1;
                    std::thread::sleep(Duration::from_secs(1_u64 << (transient_attempt - 1)));
                    let _ = err;
                }
                Err(ureq::Error::Transport(transport)) => {
                    return Err(classify_transport(&transport, &current));
                }
            }
        };
        if !(300..400).contains(&response.status()) {
            return Ok((response, current));
        }
        let location = response.header("Location").ok_or_else(|| {
            AutomationError::new(
                FailureClass::Retryable,
                format!("download redirect from {current} has no Location header"),
            )
        })?;
        current = current.join(location).map_err(|err| {
            AutomationError::new(
                FailureClass::Retryable,
                format!("invalid download redirect from {current} to {location}: {err}"),
            )
        })?;
    }
    Err(AutomationError::new(
        FailureClass::Retryable,
        format!("download redirect limit exceeded for {initial_url}"),
    ))
}

fn is_transient_download_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}

fn class_for_http_status(status: u16) -> FailureClass {
    match status {
        429 | 503 | 502 | 504 | 500 => FailureClass::SiteUnavailable,
        402 | 407 => FailureClass::ProxyFailure,
        _ => FailureClass::Retryable,
    }
}

fn retry_after_delay(response: &ureq::Response, attempt: usize) -> Duration {
    response
        .header("Retry-After")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|seconds| Duration::from_secs(seconds.min(30)))
        .unwrap_or_else(|| Duration::from_secs(1_u64 << attempt.saturating_sub(1).min(4)))
}

fn classify_transport(transport: &ureq::Transport, url: &Url) -> AutomationError {
    let message = transport.to_string();
    let lower = message.to_ascii_lowercase();
    let class = if lower.contains("proxy")
        || lower.contains("407")
        || lower.contains("connect")
        || lower.contains("reset")
    {
        FailureClass::ProxyFailure
    } else {
        FailureClass::SiteUnavailable
    };
    AutomationError::new(
        class,
        format!("download request failed for {url}: {message}"),
    )
}

fn classify_stream_error(err: &std::io::Error) -> AutomationError {
    let class = match err.kind() {
        std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::UnexpectedEof
        | std::io::ErrorKind::ConnectionRefused => FailureClass::ProxyFailure,
        _ => FailureClass::Retryable,
    };
    AutomationError::new(class, format!("download stream failed: {err}"))
}

fn expected_download_size(
    response: &ureq::Response,
    existing_len: u64,
) -> Result<Option<u64>, AutomationError> {
    if response.status() == 206 {
        if let Some(content_range) = response.header("Content-Range") {
            let range = content_range
                .strip_prefix("bytes ")
                .and_then(|value| value.split_once('/'));
            if let Some((bounds, total)) = range {
                let start = bounds
                    .split_once('-')
                    .and_then(|(start, _)| start.parse::<u64>().ok());
                if start != Some(existing_len) {
                    return Err(AutomationError::new(
                        FailureClass::Retryable,
                        format!(
                            "download resume range mismatch: requested {existing_len}, received {content_range}"
                        ),
                    ));
                }
                if total != "*" {
                    return total.parse::<u64>().map(Some).map_err(|err| {
                        AutomationError::new(
                            FailureClass::Retryable,
                            format!("invalid Content-Range total: {content_range}: {err}"),
                        )
                    });
                }
            }
        }
        return Ok(response
            .header("Content-Length")
            .and_then(|value| value.parse::<u64>().ok())
            .map(|remaining| existing_len + remaining));
    }
    Ok(response
        .header("Content-Length")
        .and_then(|value| value.parse::<u64>().ok()))
}

fn validate_download_size(
    expected_size: Option<u64>,
    actual_size: u64,
) -> Result<(), AutomationError> {
    if actual_size == 0 {
        return Err(AutomationError::new(
            FailureClass::Retryable,
            "download completed with an empty file",
        ));
    }
    if let Some(expected_size) = expected_size {
        if actual_size != expected_size {
            return Err(AutomationError::new(
                FailureClass::Retryable,
                format!(
                    "download ended before the complete file arrived: expected {expected_size} bytes, got {actual_size}"
                ),
            ));
        }
    }
    Ok(())
}

fn response_file_name(
    response: &ureq::Response,
    resolved_url: &Url,
    title: &str,
    content_type: &str,
) -> String {
    if let Some(name) = response
        .header("Content-Disposition")
        .and_then(content_disposition_filename)
    {
        return sanitize_filename(&name);
    }
    let url_name = resolved_url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .map(percent_decode)
        .filter(|name| name.contains('.'));
    if let Some(name) = url_name {
        return sanitize_filename(&name);
    }
    let extension = if content_type.contains("application/pdf") {
        "pdf"
    } else if content_type.contains("application/epub") {
        "epub"
    } else {
        "bin"
    };
    format!("{}.{}", sanitize_filename(title), extension)
}

fn content_disposition_filename(value: &str) -> Option<String> {
    for part in value.split(';').map(str::trim) {
        if let Some(encoded) = part.strip_prefix("filename*=") {
            let encoded = encoded.trim_matches('"');
            let encoded = encoded
                .split_once("''")
                .map(|(_, value)| value)
                .unwrap_or(encoded);
            return Some(percent_decode(encoded));
        }
    }
    value
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("filename="))
        .map(|name| name.trim_matches('"').to_string())
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    if !bytes.contains(&b'%') {
        return raw.to_string();
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Some(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|text| u8::from_str_radix(text, 16).ok())
            {
                out.push(hex);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn sanitize_filename(name: &str) -> String {
    let leaf = Path::new(name)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download.bin");
    let sanitized: String = leaf
        .chars()
        .map(|ch| {
            if matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                ch
            }
        })
        .collect();
    let sanitized = sanitized.trim().trim_matches('.');
    if sanitized.is_empty() {
        "download.bin".to_string()
    } else {
        sanitized.to_string()
    }
}

fn unique_download_path(dir: &Path, file_name: &str) -> PathBuf {
    let candidate = dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }
    let path = Path::new(file_name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    let extension = path.extension().and_then(|value| value.to_str());
    for index in 1..10_000 {
        let name = match extension {
            Some(extension) => format!("{stem} ({index}).{extension}"),
            None => format!("{stem} ({index})"),
        };
        let candidate = dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(file_name)
}

/// HTML 响应是否像限额页。
pub fn html_looks_like_quota(snippet: &str) -> bool {
    let lower = snippet.to_ascii_lowercase();
    snippet.contains("每日限额")
        || snippet.contains("下载限额")
        || lower.contains("daily") && lower.contains("limit")
        || lower.contains("download-limits")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(name: &str, value: &str, domain: &str, path: &str) -> Cookie {
        Cookie {
            name: name.to_string(),
            value: value.to_string(),
            domain: Some(domain.to_string()),
            path: Some(path.to_string()),
        }
    }

    #[test]
    fn cookies_are_not_sent_to_cross_domain_cdn() {
        let cookies = vec![cookie("c_token", "abc", "zh.loves.works", "/")];
        let site = Url::parse("https://zh.loves.works/dl/x").unwrap();
        let cdn = Url::parse("https://cdn.example.net/file.pdf").unwrap();
        assert!(cookie_header_for_url(&cookies, &site).contains("c_token=abc"));
        assert!(cookie_header_for_url(&cookies, &cdn).is_empty());
    }

    #[test]
    fn content_disposition_prefers_rfc5987() {
        assert!(content_disposition_filename(
            "attachment; filename=\"fallback.pdf\"; filename*=UTF-8''%E7%AE%97%E6%B3%95.pdf"
        )
        .unwrap()
        .contains("算法"));
    }

    #[test]
    fn quota_html_is_detected() {
        assert!(html_looks_like_quota("每日限额已用完"));
        assert!(!html_looks_like_quota("<html>hello</html>"));
    }

    #[test]
    fn sanitize_strips_path_separators() {
        assert_eq!(sanitize_filename("../../a:b.pdf"), "a_b.pdf");
    }
}
