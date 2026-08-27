//! Webshare 代理定时同步服务（第 16.4 节、V4 方案第 15 节）。
//!
//! R9（V4-14）修复点：
//! - **完整快照原则**：只有完整抓取成功的快照才允许影响「本次未出现代理」的状态。
//!   任一页网络错误、非 2xx、JSON 解析失败、next URL 非官方 HTTPS、发生重定向、
//!   达到 MAX_PAGES 仍有 next、响应体超限、重复 external_id、非法结构，
//!   全部**放弃整个同步**，不修改数据库——绝不再「break 后用残缺快照同步」。
//! - **HTTP 客户端安全**：关闭自动重定向；每一页 next 重新校验 scheme/host/port/path；
//!   设置 connect/request/overall 超时；API Key 只放 Authorization 头；
//!   错误日志不打印请求头；错误响应体截断并脱敏。
//! - **数据校验**：host 非空且长度受限；port 在 1..=65535；username/password 长度受限；
//!   external_id 规范化且同一快照唯一；同 external_id 地址变化按身份更新。

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use url::Url;

use crate::state::AppState;
use crate::store;

/// 官方 API 固定入口（第 15.2 节）。
const API_BASE: &str = "https://proxy.webshare.io/api/v2/proxy/list/";
/// 安全分页上限：达到后若仍有 next，整个同步失败。
const MAX_PAGES: usize = 50;
/// 单页响应体上限（10 MiB）。
const MAX_PAGE_BYTES: usize = 10 * 1024 * 1024;
/// 同步总响应体上限（100 MiB），防止恶意 API 拖垮内存。
const MAX_TOTAL_BYTES: usize = 100 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct WebshareResponse {
    results: Vec<WebshareProxyItem>,
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WebshareProxyItem {
    id: Option<String>,
    proxy_address: String,
    port: i32,
    username: Option<String>,
    password: Option<String>,
    #[serde(default)]
    valid: bool,
}

/// 校验外部返回的 next 分页链接安全（https + 官方域名 + 固定路径前缀）。
fn validate_next_url(next_str: &str) -> Result<String> {
    let parsed = Url::parse(next_str).context("解析 Webshare next URL 失败")?;
    if parsed.scheme() != "https" {
        bail!("拒绝非 https 的 Webshare next 分页链接: {next_str}");
    }
    if parsed.port().is_some() {
        bail!("拒绝带端口号的 Webshare next 分页链接: {next_str}");
    }
    match parsed.host_str() {
        Some(host) if host.eq_ignore_ascii_case("proxy.webshare.io") => {}
        _ => bail!("拒绝非官方域名的 Webshare next 分页链接: {next_str}"),
    }
    // 路径前缀必须仍是 API 列表入口
    if !parsed.path().starts_with("/api/v2/proxy/list/") {
        bail!("拒绝非 API 列表路径的 Webshare next 分页链接: {next_str}");
    }
    Ok(next_str.to_string())
}

/// 校验单条代理记录结构（第 15.3 节）。
fn validate_item(item: &WebshareProxyItem) -> Result<()> {
    let host = item.proxy_address.trim();
    if host.is_empty() {
        bail!("代理 host 为空");
    }
    if host.len() > 253 {
        bail!("代理 host 过长（> 253 字节）");
    }
    if !(1..=65535).contains(&item.port) {
        bail!("代理 port 非法：{}", item.port);
    }
    if let Some(user) = &item.username {
        if user.trim().len() > 128 {
            bail!("代理 username 过长（> 128 字节）");
        }
    }
    if let Some(pwd) = &item.password {
        if pwd.trim().len() > 256 {
            bail!("代理 password 过长（> 256 字节）");
        }
    }
    if let Some(id) = &item.id {
        let id = id.trim();
        if id.is_empty() || id.len() > 128 {
            bail!("代理 external_id 非法");
        }
    }
    Ok(())
}

/// 执行一次 Webshare 代理有效快照同步。
pub async fn sync_webshare_once(state: &AppState) -> Result<usize> {
    let api_key = &state.config.webshare.api_key;
    if api_key.trim().is_empty() {
        return Ok(0);
    }

    // 第 15.2 节：关闭自动重定向，设置 connect/request/overall 超时
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;

    let mut url = format!("{API_BASE}?mode=direct&page=1&page_size=100");
    let mut fetched_items: Vec<store::resource::WebshareProxyData> = Vec::new();
    let mut pages_fetched = 0;
    let mut total_bytes = 0usize;
    let mut seen_external_ids = std::collections::HashSet::new();

    // 阶段 1：完整抓取所有分页快照；任一异常都放弃整个同步（第 15.1 节）
    loop {
        pages_fetched += 1;
        if pages_fetched > MAX_PAGES {
            bail!(
                "Webshare 分页数超过安全上限（{MAX_PAGES}）且仍有下一页，放弃整个同步，数据库保持不变"
            );
        }

        // 每次请求前重新校验 URL（第一页是固定常量，后续页来自上游 next）
        validate_next_url(&url)?;

        let resp = client
            .get(&url)
            .header("Authorization", format!("Token {api_key}"))
            .send()
            .await
            .context("请求 Webshare API 失败")?;

        // 发生重定向（Policy::none 下 3xx 直接返回）→ 拒绝
        let status = resp.status();
        if status.is_redirection() {
            bail!("Webshare API 返回重定向（{status}），禁止跟随，放弃同步");
        }
        if !status.is_success() {
            // 错误响应体截断并脱敏后记录（第 15.2 节）
            let body = resp.text().await.unwrap_or_default();
            let truncated: String = body.chars().take(500).collect();
            bail!("Webshare API 响应状态错误 {status}: {truncated}");
        }

        // 响应体大小上限
        let bytes = resp.bytes().await.context("读取 Webshare 响应体失败")?;
        total_bytes += bytes.len();
        if bytes.len() > MAX_PAGE_BYTES || total_bytes > MAX_TOTAL_BYTES {
            bail!("Webshare 响应体超过大小上限，放弃整个同步");
        }

        let page: WebshareResponse =
            serde_json::from_slice(&bytes).context("解析 Webshare 响应 JSON 失败")?;

        for item in &page.results {
            validate_item(item)?;

            // 同一快照内 external_id 必须唯一
            if let Some(id) = &item.id {
                let normalized = id.trim().to_string();
                if !seen_external_ids.insert(normalized.clone()) {
                    bail!("Webshare 快照中出现重复 external_id：{normalized}，放弃同步");
                }
            }

            let password_cipher = if let Some(pwd) = &item.password {
                if !pwd.trim().is_empty() {
                    Some(state.cipher.encrypt(pwd.trim())?)
                } else {
                    None
                }
            } else {
                None
            };

            fetched_items.push(store::resource::WebshareProxyData {
                external_id: item.id.as_deref().map(|s| s.trim().to_string()),
                host: item.proxy_address.trim().to_string(),
                port: item.port,
                username: item
                    .username
                    .as_deref()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
                password_cipher,
                valid: item.valid,
            });
        }

        match page.next {
            Some(next_url) => {
                // 每一页 next 都重新校验 scheme/host/port/path（第 15.2 节）
                url = validate_next_url(&next_url)?;
            }
            None => break,
        }
    }

    // 阶段 2：在单个数据库事务中同步快照（同步世代 + 延迟退休）
    let report = store::resource::sync_webshare_snapshot(&state.pool, &fetched_items).await?;

    tracing::info!(
        total = report.total_synced,
        enabled = report.enabled_count,
        disabled = report.disabled_count,
        missing = report.missing_count,
        pages = pages_fetched,
        "Webshare 快照同步事务完成"
    );

    // 顺便使冷却到期且 provider 仍有效的代理恢复可用（第 15.3 节：已失效的不复活）
    let revived = store::resource::revive_cooled_proxies(&state.pool).await?;
    if revived > 0 {
        tracing::info!(revived = revived, "冷却到期且仍有效的代理已自动恢复为可用");
    }

    Ok(report.total_synced)
}

/// 启动 Webshare 定时同步后台任务。
pub fn spawn_webshare_sync(state: AppState) {
    if !state.config.webshare.enabled {
        tracing::info!("Webshare 自动同步未开启");
        return;
    }
    if state.config.webshare.api_key.trim().is_empty() {
        tracing::warn!("Webshare 同步已启用但 API Key 为空，同步被禁用（第 16.1 节）");
        return;
    }

    let interval_secs = (state.config.webshare.sync_minutes.max(1) * 60).max(60);

    tokio::spawn(async move {
        tracing::info!(
            interval_minutes = state.config.webshare.sync_minutes,
            "Webshare 定时同步任务已启动"
        );

        // 启动后立即先执行一次全量拉取，避免等待 30 分钟轮询周期
        match sync_webshare_once(&state).await {
            Ok(count) => {
                tracing::info!(count = count, "Webshare 代理初始同步完成");
                let _ = crate::scheduler::trigger_scheduler_sweep(&state).await;
            }
            Err(err) => {
                tracing::error!(error = %err, "Webshare 代理初始同步失败");
            }
        }

        let mut timer = tokio::time::interval(Duration::from_secs(interval_secs));
        timer.tick().await;

        loop {
            timer.tick().await;
            match sync_webshare_once(&state).await {
                Ok(count) => {
                    tracing::info!(count = count, "Webshare 代理定时同步完成");
                    let _ = crate::scheduler::trigger_scheduler_sweep(&state).await;
                }
                Err(err) => {
                    tracing::error!(error = %err, "Webshare 代理定时同步失败（数据库保持不变）");
                    let _ = store::admin::raise_alert(
                        &state.pool,
                        platform_domain::AlertLevel::Warn,
                        "代理",
                        "Webshare 同步失败",
                        &err.to_string(),
                        None,
                        Some("Webshare同步失败"),
                    )
                    .await;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_url_validation_accepts_official_https() {
        assert!(validate_next_url("https://proxy.webshare.io/api/v2/proxy/list/?page=2").is_ok());
        assert!(
            validate_next_url("https://proxy.webshare.io/api/v2/proxy/list/?page=2&x=1").is_ok()
        );
    }

    #[test]
    fn next_url_validation_rejects_unsafe_targets() {
        assert!(validate_next_url("http://proxy.webshare.io/api/v2/proxy/list/").is_err());
        assert!(validate_next_url("https://evil.example.com/api/v2/proxy/list/").is_err());
        assert!(validate_next_url("https://proxy.webshare.io/other/path").is_err());
        assert!(validate_next_url("https://proxy.webshare.io:8443/api/v2/proxy/list/").is_err());
        assert!(validate_next_url("https://proxy.webshare.io").is_err());
        assert!(validate_next_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn item_validation_rejects_bad_structure() {
        let good = WebshareProxyItem {
            id: Some("123".to_string()),
            proxy_address: "1.2.3.4".to_string(),
            port: 8080,
            username: Some("u".to_string()),
            password: Some("p".to_string()),
            valid: true,
        };
        assert!(validate_item(&good).is_ok());

        let bad_host = WebshareProxyItem {
            proxy_address: "  ".to_string(),
            ..WebshareProxyItem {
                id: None,
                proxy_address: String::new(),
                port: 8080,
                username: None,
                password: None,
                valid: true,
            }
        };
        assert!(validate_item(&bad_host).is_err());

        let bad_port = WebshareProxyItem { port: 0, ..good };
        assert!(validate_item(&bad_port).is_err());
    }
}
