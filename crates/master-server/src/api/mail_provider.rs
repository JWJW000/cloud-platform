//! 邮件验证码 Provider 配置、密钥引用与连通性测试 API。

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use axum::extract::State;
use axum::Json;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};

use crate::api::auth::AuthenticatedUser;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::store;
use crate::store::mail_provider::{
    get_active_config, get_secret_ciphertext, save_active_config, save_secret_ciphertext,
    MailProviderConfigRecord, UpsertMailProviderConfig,
};

const MAX_ENDPOINT_LEN: usize = 2048;
const MAX_API_KEY_LEN: usize = 4096;
const MAX_LIST_ITEMS: usize = 32;

#[derive(Debug, Clone, Serialize)]
pub struct MailProviderConfigResponse {
    pub provider_type: String,
    pub endpoint: String,
    pub has_api_key: bool,
    pub poll_interval_secs: i32,
    pub timeout_secs: i32,
    pub allowed_hosts: Vec<String>,
    pub allowed_senders: Vec<String>,
    pub version: i64,
    pub is_active: bool,
    pub updated_by: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<MailProviderConfigRecord> for MailProviderConfigResponse {
    fn from(record: MailProviderConfigRecord) -> Self {
        Self {
            provider_type: record.provider_type,
            endpoint: record.endpoint,
            has_api_key: record.api_key_secret_ref.is_some(),
            poll_interval_secs: record.poll_interval_secs,
            timeout_secs: record.timeout_secs,
            allowed_hosts: record.allowed_hosts,
            allowed_senders: record.allowed_senders,
            version: record.version,
            is_active: record.is_active,
            updated_by: record.updated_by,
            updated_at: record.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MailProviderStatusResponse {
    pub provider_type: String,
    pub version: i64,
    pub is_active: bool,
    pub has_api_key: bool,
    pub health: String,
    pub workers_applied: i64,
    pub workers_online: i64,
}

#[derive(Clone, Deserialize)]
pub struct UpdateMailProviderConfigRequest {
    pub provider_type: String,
    pub endpoint: String,
    pub api_key: Option<String>,
    pub poll_interval_secs: Option<i32>,
    pub timeout_secs: Option<i32>,
    pub allowed_hosts: Option<Vec<String>>,
    pub allowed_senders: Option<Vec<String>>,
}

impl std::fmt::Debug for UpdateMailProviderConfigRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateMailProviderConfigRequest")
            .field("provider_type", &self.provider_type)
            .field("endpoint", &self.endpoint)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("poll_interval_secs", &self.poll_interval_secs)
            .field("timeout_secs", &self.timeout_secs)
            .field("allowed_hosts", &self.allowed_hosts)
            .field("allowed_senders", &self.allowed_senders)
            .finish()
    }
}

#[derive(Clone, Deserialize)]
pub struct TestMailProviderRequest {
    pub provider_type: String,
    pub endpoint: String,
    pub api_key: Option<String>,
    pub allowed_hosts: Option<Vec<String>>,
}

impl std::fmt::Debug for TestMailProviderRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestMailProviderRequest")
            .field("provider_type", &self.provider_type)
            .field("endpoint", &self.endpoint)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("allowed_hosts", &self.allowed_hosts)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TestMailProviderResponse {
    pub success: bool,
    pub message: String,
    pub latency_ms: Option<u64>,
}

fn is_restricted_ip(ip: IpAddr) -> bool {
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
                    .is_some_and(|v4| is_restricted_ip(IpAddr::V4(v4)))
        }
    }
}

fn normalize_list(items: Option<Vec<String>>, field: &str) -> AppResult<Vec<String>> {
    let mut values: Vec<String> = items
        .unwrap_or_default()
        .into_iter()
        .map(|item| item.trim().to_ascii_lowercase())
        .filter(|item| !item.is_empty())
        .collect();
    values.sort();
    values.dedup();
    if values.len() > MAX_LIST_ITEMS || values.iter().any(|item| item.len() > 320) {
        return Err(AppError::BadRequest(format!("{field} 数量或长度超过限制")));
    }
    Ok(values)
}

struct ValidatedOutlookTarget {
    host: String,
    pinned_address: SocketAddr,
}

async fn validate_outlook_endpoint(
    endpoint: &str,
    allowed_hosts: &[String],
) -> AppResult<ValidatedOutlookTarget> {
    if endpoint.len() > MAX_ENDPOINT_LEN {
        return Err(AppError::BadRequest("端点地址过长".to_string()));
    }
    let url = url::Url::parse(endpoint)
        .map_err(|_| AppError::BadRequest("端点不是合法 URL".to_string()))?;
    if url.scheme() != "https" {
        return Err(AppError::BadRequest("端点必须使用 HTTPS".to_string()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AppError::BadRequest(
            "端点不得包含 URL 用户信息".to_string(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| AppError::BadRequest("端点缺少主机名".to_string()))?
        .to_ascii_lowercase();
    if allowed_hosts.is_empty()
        || !allowed_hosts
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(&host))
    {
        return Err(AppError::BadRequest(
            "端点主机必须命中非空部署允许列表".to_string(),
        ));
    }
    let port = url.port_or_known_default().unwrap_or(443);
    let addresses: Vec<_> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|_| AppError::BadRequest("端点 DNS 解析失败".to_string()))?
        .collect();
    if addresses.is_empty() || addresses.iter().any(|addr| is_restricted_ip(addr.ip())) {
        return Err(AppError::BadRequest(
            "端点解析到私网、回环、链路本地或保留地址".to_string(),
        ));
    }
    Ok(ValidatedOutlookTarget {
        host,
        pinned_address: addresses[0],
    })
}

fn mail_request_url(endpoint: &str) -> AppResult<url::Url> {
    let parsed = url::Url::parse(endpoint)
        .map_err(|_| AppError::BadRequest("端点不是合法 URL".to_string()))?;
    if parsed
        .path()
        .trim_end_matches('/')
        .ends_with("/api/external/emails")
    {
        Ok(parsed)
    } else {
        parsed
            .join("api/external/emails")
            .map_err(|_| AppError::BadRequest("无法构造邮件接口地址".to_string()))
    }
}

async fn probe_outlook(
    endpoint: &str,
    api_key: &str,
    allowed_hosts: &[String],
) -> AppResult<TestMailProviderResponse> {
    let started = std::time::Instant::now();
    let target = validate_outlook_endpoint(endpoint, allowed_hosts).await?;
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::none())
        // 固定到刚刚完成全地址安全检查的目标，避免校验后再次 DNS 解析造成
        // rebinding/TOCTOU 绕过；TLS SNI 仍使用原始主机名。
        .resolve(&target.host, target.pinned_address)
        .build()
        .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))?;
    let response = client
        .get(mail_request_url(endpoint)?)
        .query(&[
            ("email", "healthcheck-invalid@example.invalid"),
            ("top", "1"),
        ])
        .header("X-API-Key", api_key.trim())
        .send()
        .await;
    let latency_ms = started.elapsed().as_millis() as u64;
    let (success, message) = match response {
        Ok(response) if response.status().is_success() => (true, "连接与鉴权测试成功".to_string()),
        Ok(response)
            if matches!(
                response.status(),
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ) =>
        {
            (false, "连接成功，但 API Key 鉴权失败".to_string())
        }
        Ok(response) if response.status() == StatusCode::TOO_MANY_REQUESTS => {
            (false, "邮件服务当前限流，请稍后重试".to_string())
        }
        Ok(response) if response.status() == StatusCode::NOT_FOUND => {
            let body = response.text().await.unwrap_or_default();
            // assast/outlookEmail 等服务在 API Key 鉴权通过但探测用虚拟账号不存在时返回 404 {"success": false, "error": "邮箱账号不存在"}
            // 这说明接口连通与鉴权均已正常通过。
            if body.contains("邮箱账号不存在")
                || (body.contains("\"error\"") && body.contains("\"success\""))
            {
                (true, "连接与鉴权测试成功".to_string())
            } else {
                (
                    false,
                    "邮件服务返回 HTTP 404（请确认接口路径是否正确）".to_string(),
                )
            }
        }
        Ok(response) => (
            false,
            format!("邮件服务返回 HTTP {}", response.status().as_u16()),
        ),
        Err(_) => (false, "无法连接邮件服务".to_string()),
    };
    Ok(TestMailProviderResponse {
        success,
        message,
        latency_ms: Some(latency_ms),
    })
}

pub async fn get_mail_provider_status(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
) -> AppResult<Json<Option<MailProviderStatusResponse>>> {
    let record = get_active_config(&state.pool).await?;
    let Some(record) = record else {
        return Ok(Json(None));
    };
    let workers_online: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM worker_nodes WHERE connected AND status IN ('在线', '忙碌')",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);
    let workers_applied: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM worker_nodes \
         WHERE connected AND status IN ('在线', '忙碌') \
           AND applied_mail_provider_version = $1 AND mail_provider_name = $2 \
           AND mail_provider_health NOT LIKE '%降级%' \
           AND mail_provider_health NOT LIKE '%异常%' \
           AND mail_provider_health NOT LIKE '%无效%' \
           AND mail_provider_health NOT LIKE '%不可用%'",
    )
    .bind(record.version)
    .bind(&record.provider_type)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);
    Ok(Json(Some(MailProviderStatusResponse {
        provider_type: record.provider_type.clone(),
        version: record.version,
        is_active: record.is_active,
        has_api_key: record.api_key_secret_ref.is_some(),
        health: if record.provider_type == "outlook_http" && record.api_key_secret_ref.is_none() {
            "未配置密钥".to_string()
        } else if record.provider_type == "manual" {
            "人工降级可用".to_string()
        } else if workers_online > 0 && workers_applied == workers_online {
            "Worker 已全部应用".to_string()
        } else {
            "等待 Worker 应用".to_string()
        },
        workers_applied,
        workers_online,
    })))
}

pub async fn get_mail_provider_config(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
) -> AppResult<Json<Option<MailProviderConfigResponse>>> {
    auth.require_super_admin()?;
    Ok(Json(get_active_config(&state.pool).await?.map(Into::into)))
}

pub async fn update_mail_provider_config(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Json(req): Json<UpdateMailProviderConfigRequest>,
) -> AppResult<Json<MailProviderConfigResponse>> {
    auth.require_super_admin()?;
    if !matches!(
        req.provider_type.as_str(),
        "manual" | "outlook_http" | "mock"
    ) {
        return Err(AppError::BadRequest("不支持的 Provider 类型".to_string()));
    }
    if req.provider_type == "mock" && !cfg!(debug_assertions) {
        return Err(AppError::BadRequest(
            "生产构建禁止启用 Mock Provider".to_string(),
        ));
    }
    let poll_interval_secs = req.poll_interval_secs.unwrap_or(5);
    let timeout_secs = req.timeout_secs.unwrap_or(60);
    if !(1..=60).contains(&poll_interval_secs) || !(10..=300).contains(&timeout_secs) {
        return Err(AppError::BadRequest(
            "轮询间隔须为 1–60 秒，超时须为 10–300 秒".to_string(),
        ));
    }
    let allowed_hosts = normalize_list(req.allowed_hosts, "允许主机")?;
    let allowed_senders = normalize_list(req.allowed_senders, "允许发件人")?;
    let previous = get_active_config(&state.pool).await?;
    let previous_secret_ref = previous
        .as_ref()
        .and_then(|record| record.api_key_secret_ref.clone());
    let supplied_key = req
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty());
    if supplied_key.is_some_and(|key| key.len() > MAX_API_KEY_LEN) {
        return Err(AppError::BadRequest("API Key 长度超过限制".to_string()));
    }
    let active_key = if let Some(key) = supplied_key {
        Some(key.to_string())
    } else if req.api_key.is_none() {
        match previous_secret_ref.as_deref() {
            Some(secret_ref) => {
                let cipher_text = get_secret_ciphertext(&state.pool, secret_ref)
                    .await?
                    .ok_or_else(|| AppError::BadRequest("API Key 密钥引用失效".to_string()))?;
                Some(
                    state
                        .cipher
                        .decrypt(&cipher_text)
                        .map_err(AppError::Internal)?,
                )
            }
            None => None,
        }
    } else {
        None
    };
    if req.provider_type == "outlook_http" {
        let key = active_key
            .as_deref()
            .ok_or_else(|| AppError::BadRequest("Outlook Provider 必须配置 API Key".to_string()))?;
        let probe = probe_outlook(&req.endpoint, key, &allowed_hosts).await?;
        if !probe.success {
            return Err(AppError::BadRequest(format!(
                "新配置健康检查失败，旧版本保持生效：{}",
                probe.message
            )));
        }
    }
    let secret_ref = if let Some(key) = supplied_key {
        let encrypted = state.cipher.encrypt(key).map_err(AppError::Internal)?;
        Some(save_secret_ciphertext(&state.pool, &encrypted, &auth.username).await?)
    } else if req.api_key.is_none() {
        previous_secret_ref
    } else {
        None
    };

    let record = save_active_config(
        &state.pool,
        UpsertMailProviderConfig {
            provider_type: req.provider_type,
            endpoint: req.endpoint,
            api_key_secret_ref: secret_ref,
            poll_interval_secs,
            timeout_secs,
            allowed_hosts,
            allowed_senders,
        },
        &auth.username,
    )
    .await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "更新邮件验证码 Provider 配置",
        &format!("version: {}", record.version),
        &format!("type: {}", record.provider_type),
    )
    .await?;
    Ok(Json(record.into()))
}

pub async fn test_mail_provider(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Json(req): Json<TestMailProviderRequest>,
) -> AppResult<Json<TestMailProviderResponse>> {
    auth.require_super_admin()?;
    if req.provider_type == "manual" {
        return Ok(Json(TestMailProviderResponse {
            success: true,
            message: "人工输入 Provider 可用".to_string(),
            latency_ms: Some(0),
        }));
    }
    if req.provider_type != "outlook_http" {
        return Ok(Json(TestMailProviderResponse {
            success: false,
            message: "只允许测试 Manual 或 Outlook Provider".to_string(),
            latency_ms: None,
        }));
    }
    let allowed_hosts = normalize_list(req.allowed_hosts, "允许主机")?;
    let api_key = match req.api_key.filter(|key| !key.trim().is_empty()) {
        Some(key) => key,
        None => {
            let record = get_active_config(&state.pool)
                .await?
                .ok_or_else(|| AppError::BadRequest("尚未保存 Provider 配置".to_string()))?;
            let secret_ref = record
                .api_key_secret_ref
                .ok_or_else(|| AppError::BadRequest("尚未配置 API Key".to_string()))?;
            let cipher_text = get_secret_ciphertext(&state.pool, &secret_ref)
                .await?
                .ok_or_else(|| AppError::BadRequest("API Key 密钥引用失效".to_string()))?;
            state
                .cipher
                .decrypt(&cipher_text)
                .map_err(AppError::Internal)?
        }
    };

    Ok(Json(
        probe_outlook(&req.endpoint, &api_key, &allowed_hosts).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restricted_ip_rules_cover_metadata_and_private_networks() {
        for value in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "::1",
        ] {
            assert!(is_restricted_ip(value.parse().unwrap()), "{value}");
        }
        assert!(!is_restricted_ip("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn list_normalization_is_bounded_and_deduplicated() {
        let result = normalize_list(
            Some(vec![
                " MAIL.EXAMPLE.COM ".to_string(),
                "mail.example.com".to_string(),
            ]),
            "允许主机",
        )
        .unwrap();
        assert_eq!(result, vec!["mail.example.com"]);
    }

    #[test]
    fn request_debug_output_redacts_api_keys() {
        let request = UpdateMailProviderConfigRequest {
            provider_type: "outlook_http".to_string(),
            endpoint: "https://mail.example.com".to_string(),
            api_key: Some("super-secret-api-key".to_string()),
            poll_interval_secs: Some(5),
            timeout_secs: Some(60),
            allowed_hosts: Some(vec!["mail.example.com".to_string()]),
            allowed_senders: None,
        };
        let debug = format!("{request:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("super-secret-api-key"));
    }
}
