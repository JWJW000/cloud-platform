//! Outlook HTTP 邮件验证码适配器与请求边界。

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use automation_core::cancel::CancelToken;
use automation_core::mail_code::{MailCodeCursor, MailCodeError, MailCodeProvider, MailCodeResult};
use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use std::sync::LazyLock;

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_EMAILS: usize = 100;
const MAX_SUBJECT_CHARS: usize = 512;
const MAX_PREVIEW_CHARS: usize = 8 * 1024;

static CODE_PATTERNS: LazyLock<Vec<regex::Regex>> = LazyLock::new(|| {
    [
        r"(?i)(?:code|verification|confirmation|pin|验证码|确认码|校验码|动态码)[^\d]{0,16}(\d{4,8})",
        r"(?i)(\d{4,8})[^\d]{0,16}(?:is your|为您的|是您的|验证码|确认码)",
        r"\b(\d{4,8})\b",
    ]
    .into_iter()
    .map(|pattern| regex::Regex::new(pattern).expect("verification regex is valid"))
    .collect()
});

#[derive(Clone)]
pub struct OutlookConfig {
    pub endpoint: String,
    pub api_key: String,
    pub poll_interval: Duration,
    pub timeout: Duration,
    pub allowed_hosts: Vec<String>,
    pub allowed_senders: Vec<String>,
}

impl std::fmt::Debug for OutlookConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutlookConfig")
            .field("endpoint", &self.endpoint)
            .field("api_key", &"[REDACTED]")
            .field("poll_interval", &self.poll_interval)
            .field("timeout", &self.timeout)
            .field("allowed_hosts", &self.allowed_hosts)
            .field("allowed_senders", &self.allowed_senders)
            .finish()
    }
}

impl Default for OutlookConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            api_key: String::new(),
            poll_interval: Duration::from_secs(3),
            timeout: Duration::from_secs(120),
            allowed_hosts: Vec::new(),
            allowed_senders: Vec::new(),
        }
    }
}

pub fn is_forbidden_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.octets()[0] == 0
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| is_forbidden_ip(&IpAddr::V4(v4)))
        }
    }
}

#[derive(Debug, Deserialize)]
struct MailListResponse {
    #[serde(default)]
    emails: Vec<MailItem>,
}

#[derive(Debug, Deserialize)]
struct MailItem {
    #[serde(default)]
    subject: Option<String>,
    #[serde(default, alias = "preview")]
    body_preview: Option<String>,
    #[serde(default, alias = "from", alias = "sender_address")]
    sender: Option<String>,
    #[serde(default, alias = "receivedDateTime", alias = "created_at")]
    received_at: Option<DateTime<Utc>>,
}

pub fn extract_verification_code(text: &str) -> Option<String> {
    CODE_PATTERNS.iter().find_map(|re| {
        re.captures(text)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
    })
}

fn classify_status(status: StatusCode) -> Result<(), MailCodeError> {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(MailCodeError::AuthFailed),
        StatusCode::TOO_MANY_REQUESTS => Err(MailCodeError::RateLimited),
        status if status.is_server_error() => Err(MailCodeError::Unavailable(
            "Outlook 服务暂时不可用".to_string(),
        )),
        status if !status.is_success() => Err(MailCodeError::Unavailable(format!(
            "Outlook 服务拒绝请求（HTTP {}）",
            status.as_u16()
        ))),
        _ => Ok(()),
    }
}

#[derive(Debug, Default)]
struct FetchedCodes {
    codes: HashSet<String>,
    timestamp_confirmed_new: HashSet<String>,
}

fn first_new_code(batch: FetchedCodes, baseline: &HashSet<String>) -> Option<String> {
    batch
        .timestamp_confirmed_new
        .into_iter()
        .next()
        .or_else(|| {
            batch
                .codes
                .into_iter()
                .find(|code| !baseline.contains(code))
        })
}

fn parse_mail_codes(
    bytes: &[u8],
    allowed_senders: &[String],
    started_at: Option<DateTime<Utc>>,
) -> Result<FetchedCodes, MailCodeError> {
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(MailCodeError::Unavailable(
            "Outlook 响应超过 1 MiB 上限".to_string(),
        ));
    }
    let response: MailListResponse = serde_json::from_slice(bytes)
        .map_err(|_| MailCodeError::Unavailable("Outlook 返回了非法 JSON".to_string()))?;
    if response.emails.len() > MAX_EMAILS {
        return Err(MailCodeError::Unavailable(
            "Outlook 返回的邮件数量超过上限".to_string(),
        ));
    }

    let mut result = FetchedCodes::default();
    for item in response.emails {
        if let Some(sender) = item.sender.as_deref() {
            if !allowed_senders.is_empty()
                && !allowed_senders
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(sender.trim()))
            {
                continue;
            }
        } else if !allowed_senders.is_empty() {
            continue;
        }
        let timestamp_confirms_new = match (started_at, item.received_at) {
            (Some(start), Some(received)) if received < start => continue,
            (Some(_), Some(_)) => true,
            _ => false,
        };
        let subject = item.subject.unwrap_or_default();
        let preview = item.body_preview.unwrap_or_default();
        if subject.chars().count() > MAX_SUBJECT_CHARS
            || preview.chars().count() > MAX_PREVIEW_CHARS
        {
            continue;
        }
        if let Some(code) = extract_verification_code(&format!("{subject}\n{preview}")) {
            if timestamp_confirms_new {
                result.timestamp_confirmed_new.insert(code.clone());
            }
            result.codes.insert(code);
        }
    }
    Ok(result)
}

#[derive(Debug, Clone)]
pub struct OutlookHttpMailCodeAdapter {
    config: OutlookConfig,
    request_url: reqwest::Url,
}

impl OutlookHttpMailCodeAdapter {
    pub fn new(mut config: OutlookConfig) -> Result<Self, MailCodeError> {
        let endpoint = reqwest::Url::parse(config.endpoint.trim())
            .map_err(|_| MailCodeError::Unavailable("Outlook 端点不是合法 URL".to_string()))?;
        if !matches!(endpoint.scheme(), "http" | "https") {
            return Err(MailCodeError::Unavailable(
                "SSRF 防护：Outlook 服务端点必须使用 HTTP 或 HTTPS".to_string(),
            ));
        }
        if !endpoint.username().is_empty() || endpoint.password().is_some() {
            return Err(MailCodeError::Unavailable(
                "Outlook 端点不得包含 URL 用户信息".to_string(),
            ));
        }
        let host = endpoint
            .host_str()
            .ok_or_else(|| MailCodeError::Unavailable("Outlook 端点缺少主机名".to_string()))?;
        config.allowed_hosts = config
            .allowed_hosts
            .into_iter()
            .map(|item| item.trim().to_ascii_lowercase())
            .filter(|item| !item.is_empty())
            .collect();
        if config.allowed_hosts.is_empty()
            || !config
                .allowed_hosts
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(host))
        {
            return Err(MailCodeError::Unavailable(
                "SSRF 防护：端点主机必须命中非空部署允许列表".to_string(),
            ));
        }
        if config.api_key.trim().is_empty() || config.api_key.len() > 4096 {
            return Err(MailCodeError::AuthFailed);
        }
        if !(Duration::from_secs(1)..=Duration::from_secs(60)).contains(&config.poll_interval) {
            return Err(MailCodeError::Unavailable(
                "轮询间隔必须在 1–60 秒之间".to_string(),
            ));
        }
        if !(Duration::from_secs(10)..=Duration::from_secs(300)).contains(&config.timeout) {
            return Err(MailCodeError::Unavailable(
                "取码超时必须在 10–300 秒之间".to_string(),
            ));
        }

        let request_url = if endpoint
            .path()
            .trim_end_matches('/')
            .ends_with("/api/external/emails")
        {
            endpoint
        } else {
            endpoint
                .join("api/external/emails")
                .map_err(|_| MailCodeError::Unavailable("无法构造 Outlook 邮件接口".to_string()))?
        };
        Ok(Self {
            config,
            request_url,
        })
    }

    async fn validate_target(&self) -> Result<(String, SocketAddr), MailCodeError> {
        let host = self
            .request_url
            .host_str()
            .ok_or_else(|| MailCodeError::Unavailable("Outlook 端点缺少主机名".to_string()))?;
        let port = self.request_url.port_or_known_default().unwrap_or(443);
        let addresses: Vec<_> = tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| MailCodeError::Network("Outlook 主机 DNS 解析失败".to_string()))?
            .collect();
        if addresses.is_empty() || addresses.iter().any(|addr| is_forbidden_ip(&addr.ip())) {
            return Err(MailCodeError::Unavailable(
                "SSRF 防护：Outlook 主机解析到受限或空地址集合".to_string(),
            ));
        }
        Ok((host.to_string(), addresses[0]))
    }

    async fn pinned_client(&self) -> Result<Client, MailCodeError> {
        let (host, address) = self.validate_target().await?;
        Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            // 使用已经检查过的固定地址，消除“安全校验后再次 DNS 解析”的
            // rebinding 窗口；HTTPS 证书验证仍按 URL 原始主机名执行。
            .resolve(&host, address)
            .build()
            .map_err(|_| MailCodeError::Unavailable("无法创建 Outlook HTTP 客户端".to_string()))
    }

    async fn read_limited(mut response: reqwest::Response) -> Result<Vec<u8>, MailCodeError> {
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(MailCodeError::Unavailable(
                "Outlook 响应超过 1 MiB 上限".to_string(),
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| MailCodeError::Network("读取 Outlook 响应失败".to_string()))?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(MailCodeError::Unavailable(
                    "Outlook 响应超过 1 MiB 上限".to_string(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    async fn fetch_codes(
        &self,
        email: &str,
        started_at: Option<DateTime<Utc>>,
    ) -> Result<FetchedCodes, MailCodeError> {
        let client = self.pinned_client().await?;
        let response = client
            .get(self.request_url.clone())
            .query(&[("email", email), ("folder", "all"), ("top", "10")])
            .header("X-API-Key", self.config.api_key.trim())
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|_| MailCodeError::Network("Outlook 请求失败".to_string()))?;
        classify_status(response.status())?;

        let bytes = Self::read_limited(response).await?;
        parse_mail_codes(&bytes, &self.config.allowed_senders, started_at)
    }
}

#[async_trait]
impl MailCodeProvider for OutlookHttpMailCodeAdapter {
    fn name(&self) -> &'static str {
        "outlook_http"
    }

    async fn prepare(
        &self,
        email: &str,
        timeout: Duration,
    ) -> Result<MailCodeCursor, MailCodeError> {
        if email.is_empty() || email.len() > 320 || !email.contains('@') {
            return Err(MailCodeError::Unavailable("注册邮箱格式无效".to_string()));
        }
        // 先记录墙上时钟再抓基线，消除“基线响应返回后、started_at 赋值前”
        // 新邮件落入空窗而被永久过滤的竞态。
        let started_at = SystemTime::now();
        let now = Instant::now();
        let baseline_codes = self.fetch_codes(email, None).await?.codes;
        Ok(MailCodeCursor {
            email: email.to_string(),
            start_time: now,
            started_at,
            deadline: now + timeout,
            provider_version: 0,
            prepared_by: self.name(),
            baseline_codes,
        })
    }

    async fn await_code(
        &self,
        cursor: &MailCodeCursor,
        cancel: &CancelToken,
    ) -> Result<MailCodeResult, MailCodeError> {
        let started_at: DateTime<Utc> = cursor.started_at.into();
        let automatic_deadline = cursor.deadline.min(cursor.start_time + self.config.timeout);
        let mut rate_limited = false;
        loop {
            if cancel.is_cancelled() {
                return Err(MailCodeError::Cancelled);
            }
            if Instant::now() >= automatic_deadline {
                return Err(if rate_limited {
                    MailCodeError::RateLimited
                } else {
                    MailCodeError::Timeout
                });
            }

            match self.fetch_codes(&cursor.email, Some(started_at)).await {
                Ok(codes) => {
                    if let Some(code) = first_new_code(codes, &cursor.baseline_codes) {
                        return Ok(MailCodeResult { code });
                    }
                }
                Err(MailCodeError::RateLimited) => rate_limited = true,
                Err(MailCodeError::Network(_) | MailCodeError::Unavailable(_)) => {}
                Err(error) => return Err(error),
            }

            let remaining = cursor
                .deadline
                .min(automatic_deadline)
                .saturating_duration_since(Instant::now());
            if !cancel.sleep(self.config.poll_interval.min(remaining)).await {
                return Err(MailCodeError::Cancelled);
            }
        }
    }

    async fn health(&self) -> Result<(), MailCodeError> {
        let client = self.pinned_client().await?;
        let response = client
            .get(self.request_url.clone())
            .query(&[
                ("email", "healthcheck-invalid@example.invalid"),
                ("top", "1"),
            ])
            .header("X-API-Key", self.config.api_key.trim())
            .send()
            .await
            .map_err(|_| MailCodeError::Network("Outlook 健康检查请求失败".to_string()))?;
        match response.status() {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(MailCodeError::AuthFailed),
            StatusCode::TOO_MANY_REQUESTS => Err(MailCodeError::RateLimited),
            status if status.is_success() => Ok(()),
            _ => Err(MailCodeError::Unavailable(
                "Outlook 健康检查未通过".to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_chinese_and_english_codes() {
        assert_eq!(
            extract_verification_code("您的验证码：838621，请勿泄露"),
            Some("838621".to_string())
        );
        assert_eq!(
            extract_verification_code("Your confirmation code is 4821"),
            Some("4821".to_string())
        );
    }

    #[test]
    fn rejects_unsafe_endpoint_and_empty_allowlist() {
        let mut config = OutlookConfig {
            endpoint: "http://127.0.0.1:8080".to_string(),
            api_key: "secret".to_string(),
            ..Default::default()
        };
        assert!(OutlookHttpMailCodeAdapter::new(config.clone()).is_err());
        config.endpoint = "https://mail.example.com".to_string();
        assert!(OutlookHttpMailCodeAdapter::new(config.clone()).is_err());
        config.allowed_hosts = vec!["mail.example.com".to_string()];
        assert!(OutlookHttpMailCodeAdapter::new(config.clone()).is_ok());
        config.endpoint = "http://mail.example.com:5000/".to_string();
        assert!(
            OutlookHttpMailCodeAdapter::new(config).is_ok(),
            "桌面版正在使用的 HTTP Outlook 端点必须能通过允许列表校验"
        );
    }

    #[test]
    fn blocks_private_and_metadata_ranges() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "::1",
        ] {
            assert!(is_forbidden_ip(&ip.parse().unwrap()), "{ip}");
        }
        assert!(!is_forbidden_ip(&"1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn excludes_codes_seen_before_registration_submission() {
        let baseline = HashSet::from(["111111".to_string()]);
        let current = FetchedCodes {
            codes: HashSet::from(["111111".to_string(), "222222".to_string()]),
            ..Default::default()
        };
        assert_eq!(
            first_new_code(current, &baseline),
            Some("222222".to_string())
        );
        assert_eq!(
            first_new_code(
                FetchedCodes {
                    codes: HashSet::from(["111111".to_string()]),
                    ..Default::default()
                },
                &baseline,
            ),
            None
        );
        assert_eq!(
            first_new_code(
                FetchedCodes {
                    codes: HashSet::from(["111111".to_string()]),
                    timestamp_confirmed_new: HashSet::from(["111111".to_string()]),
                },
                &baseline,
            ),
            Some("111111".to_string()),
            "带新时间戳的邮件即使验证码碰巧重复也必须被接受"
        );
    }

    #[test]
    fn classifies_auth_rate_limit_and_server_errors() {
        assert_eq!(
            classify_status(StatusCode::UNAUTHORIZED),
            Err(MailCodeError::AuthFailed)
        );
        assert_eq!(
            classify_status(StatusCode::FORBIDDEN),
            Err(MailCodeError::AuthFailed)
        );
        assert_eq!(
            classify_status(StatusCode::TOO_MANY_REQUESTS),
            Err(MailCodeError::RateLimited)
        );
        assert!(matches!(
            classify_status(StatusCode::BAD_GATEWAY),
            Err(MailCodeError::Unavailable(_))
        ));
        assert!(classify_status(StatusCode::OK).is_ok());
    }

    #[test]
    fn rejects_invalid_oversized_and_excessive_json() {
        assert!(matches!(
            parse_mail_codes(b"not-json", &[], None),
            Err(MailCodeError::Unavailable(_))
        ));
        assert!(matches!(
            parse_mail_codes(&vec![b'x'; MAX_RESPONSE_BYTES + 1], &[], None),
            Err(MailCodeError::Unavailable(_))
        ));
        let too_many = serde_json::json!({
            "emails": (0..=MAX_EMAILS)
                .map(|_| serde_json::json!({"subject": "code 123456"}))
                .collect::<Vec<_>>()
        });
        assert!(matches!(
            parse_mail_codes(&serde_json::to_vec(&too_many).unwrap(), &[], None),
            Err(MailCodeError::Unavailable(_))
        ));
    }

    #[test]
    fn bounds_fields_and_enforces_sender_and_time_window() {
        let now = Utc::now();
        let payload = serde_json::json!({
            "emails": [
                {"subject": format!("验证码 111111{}", "x".repeat(MAX_SUBJECT_CHARS)), "sender": "allowed@example.com"},
                {"subject": "验证码 222222", "sender": "blocked@example.com", "received_at": now},
                {"subject": "验证码 333333", "sender": "allowed@example.com", "received_at": now - chrono::Duration::minutes(1)},
                {"subject": "验证码 444444", "sender": "allowed@example.com", "received_at": now + chrono::Duration::seconds(1)}
            ]
        });
        let codes = parse_mail_codes(
            &serde_json::to_vec(&payload).unwrap(),
            &["allowed@example.com".to_string()],
            Some(now),
        )
        .unwrap();
        assert_eq!(codes.codes, HashSet::from(["444444".to_string()]));
        assert_eq!(
            codes.timestamp_confirmed_new,
            HashSet::from(["444444".to_string()])
        );
    }

    fn test_adapter() -> OutlookHttpMailCodeAdapter {
        OutlookHttpMailCodeAdapter::new(OutlookConfig {
            endpoint: "https://mail.example.com".to_string(),
            api_key: "secret".to_string(),
            allowed_hosts: vec!["mail.example.com".to_string()],
            timeout: Duration::from_secs(10),
            ..Default::default()
        })
        .unwrap()
    }

    fn expired_cursor() -> MailCodeCursor {
        let now = Instant::now();
        MailCodeCursor {
            email: "reader@example.com".to_string(),
            start_time: now,
            started_at: SystemTime::now(),
            deadline: now,
            provider_version: 1,
            prepared_by: "outlook_http",
            baseline_codes: HashSet::new(),
        }
    }

    #[tokio::test]
    async fn timeout_and_cancellation_stop_before_network_access() {
        let adapter = test_adapter();
        assert_eq!(
            adapter
                .await_code(&expired_cursor(), &CancelToken::new())
                .await
                .unwrap_err(),
            MailCodeError::Timeout
        );

        let cancel = CancelToken::new();
        cancel.cancel("test");
        assert_eq!(
            adapter
                .await_code(&expired_cursor(), &cancel)
                .await
                .unwrap_err(),
            MailCodeError::Cancelled
        );
    }
}
