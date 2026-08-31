//! Webhook 消息推送配置与每日统计通知（支持飞书机器人及多平台适配）。

use std::time::Duration;
use axum::extract::State;
use axum::Json;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use chrono::{Local, Timelike};
use reqwest::Client;
use ring::hmac;
use serde::{Deserialize, Serialize};

use crate::api::auth::AuthenticatedUser;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::store;

pub const SETTING_KEY_WEBHOOK: &str = "webhook_notification_config";

/// Webhook 平台类型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebhookPlatform {
    Feishu,
    Wechat,
    Dingtalk,
    Generic,
}

impl Default for WebhookPlatform {
    fn default() -> Self {
        Self::Feishu
    }
}

/// Webhook 推送配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub platform: WebhookPlatform,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default = "default_daily_push_time")]
    pub daily_push_time: String,
    #[serde(default = "default_title_prefix")]
    pub title_prefix: String,
    #[serde(default = "default_true")]
    pub include_system_status: bool,
    #[serde(default)]
    pub last_pushed_date: Option<String>,
}

fn default_daily_push_time() -> String {
    "20:00".to_string()
}

fn default_title_prefix() -> String {
    "「数字图书馆」每日下载日报".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for WebhookConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            platform: WebhookPlatform::Feishu,
            secret: None,
            daily_push_time: default_daily_push_time(),
            title_prefix: default_title_prefix(),
            include_system_status: true,
            last_pushed_date: None,
        }
    }
}

/// 发送请求参数。
#[derive(Debug, Deserialize)]
pub struct SendWebhookRequest {
    #[serde(default)]
    pub custom_note: Option<String>,
}

/// 发送响应。
#[derive(Debug, Serialize)]
pub struct SendWebhookResponse {
    pub success: bool,
    pub message: String,
}

/// Webhook 详情与预览响应。
#[derive(Debug, Serialize)]
pub struct WebhookDetailsResponse {
    pub config: WebhookConfig,
    pub preview_markdown: String,
}

/// 格式化字节数。
fn format_bytes(bytes: i64) -> String {
    if bytes <= 0 {
        return "0 B".to_string();
    }
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut val = bytes as f64;
    let mut idx = 0;
    while val >= 1024.0 && idx < UNITS.len() - 1 {
        val /= 1024.0;
        idx += 1;
    }
    format!("{:.2} {}", val, UNITS[idx])
}

/// 飞书签名计算（毫秒时间戳 + secret 通过 HMAC-SHA256 生成签名）。
fn sign_feishu(timestamp: i64, secret: &str) -> Option<String> {
    if secret.is_empty() {
        return None;
    }
    let string_to_sign = format!("{}\n{}", timestamp, secret);
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    let signature = hmac::sign(&key, string_to_sign.as_bytes());
    Some(BASE64.encode(signature.as_ref()))
}

/// 汇集今日统计指标并生成 Markdown 报告。
pub async fn build_daily_report_markdown(
    state: &AppState,
    title_prefix: &str,
    include_system_status: bool,
    custom_note: Option<&str>,
) -> AppResult<String> {
    let today_date = Local::now().format("%Y-%m-%d").to_string();
    let today_time = Local::now().format("%H:%M:%S").to_string();

    let today_stats = store::admin::recent_daily_stats(&state.pool, 1).await?;
    let (completed, failed, skipped, bytes_total, account_used) =
        if let Some(last) = today_stats.last() {
            (
                last.completed,
                last.failed,
                last.skipped,
                last.bytes_total,
                last.account_used,
            )
        } else {
            (0, 0, 0, 0, 0)
        };

    let mut md = String::new();
    md.push_str(&format!("### 📊 {}\n", title_prefix));
    md.push_str(&format!("> **统计日期**：{} {}\n\n", today_date, today_time));

    md.push_str("#### 📚 今日图书获取统计\n");
    md.push_str(&format!("- ✅ **成功下载**：**{}** 本\n", completed));
    md.push_str(&format!("- ❌ **下载失败**：{} 本\n", failed));
    md.push_str(&format!("- ⏭️ **跳过未收录**：{} 本\n", skipped));
    md.push_str(&format!("- 💾 **总下载流量**：{}\n", format_bytes(bytes_total)));
    md.push_str(&format!("- 🔑 **账号使用次数**：{} 次\n", account_used));

    if include_system_status {
        let nodes = store::node::list_nodes(&state.pool).await.unwrap_or_default();
        let total_workers = nodes.len();
        let online_workers = nodes.iter().filter(|n| n.connected).count();

        let available_accounts = store::resource::count_available_accounts(&state.pool)
            .await
            .unwrap_or(0);
        let total_accounts = store::resource::count_accounts(&state.pool, None)
            .await
            .unwrap_or(0);

        let task_counts = store::task::count_by_status(&state.pool)
            .await
            .unwrap_or_default();
        let mut pending_tasks = 0;
        let mut running_tasks = 0;
        for (status, count) in task_counts {
            match status.as_str() {
                "待处理" => pending_tasks += count,
                "已分配" | "执行中" | "等待入库" => running_tasks += count,
                _ => {}
            }
        }

        md.push_str("\n#### 🖥️ 集群与资源运行概况\n");
        md.push_str(&format!(
            "- ⚡ **Worker 节点**：在线 {} / 总数 {}\n",
            online_workers, total_workers
        ));
        md.push_str(&format!(
            "- 👤 **下载账号池**：可用 {} / 总数 {}\n",
            available_accounts, total_accounts
        ));
        md.push_str(&format!(
            "- ⏳ **待执行任务**：待处理 {} 本，执行中 {} 本\n",
            pending_tasks, running_tasks
        ));
    }

    if let Some(note) = custom_note {
        if !note.trim().is_empty() {
            md.push_str(&format!("\n💬 **备注信息**：{}\n", note.trim()));
        }
    }

    Ok(md)
}

/// 执行向 Webhook URL 发送消息。
pub async fn send_to_webhook(
    client: &Client,
    config: &WebhookConfig,
    title: &str,
    markdown_content: &str,
) -> Result<(), String> {
    let url = config.url.trim();
    if url.is_empty() {
        return Err("Webhook URL 为空".to_string());
    }

    match config.platform {
        WebhookPlatform::Feishu => {
            let timestamp = chrono::Utc::now().timestamp();
            let mut payload = serde_json::json!({
                "msg_type": "interactive",
                "card": {
                    "config": {
                        "wide_screen_mode": true
                    },
                    "header": {
                        "template": "blue",
                        "title": {
                            "content": title,
                            "tag": "plain_text"
                        }
                    },
                    "elements": [
                        {
                            "tag": "markdown",
                            "content": markdown_content
                        },
                        {
                            "tag": "hr"
                        },
                        {
                            "tag": "note",
                            "elements": [
                                {
                                    "tag": "plain_text",
                                    "content": format!("推送时间: {}", Local::now().format("%Y-%m-%d %H:%M:%S"))
                                }
                            ]
                        }
                    ]
                }
            });

            if let Some(secret) = &config.secret {
                if !secret.trim().is_empty() {
                    if let Some(sign) = sign_feishu(timestamp, secret.trim()) {
                        payload["timestamp"] = serde_json::json!(timestamp.to_string());
                        payload["sign"] = serde_json::json!(sign);
                    }
                }
            }

            let resp = client
                .post(url)
                .timeout(Duration::from_secs(10))
                .json(&payload)
                .send()
                .await
                .map_err(|e| format!("请求飞书 Webhook 失败: {e}"))?;

            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                return Err(format!("飞书 Webhook 返回 HTTP {status}: {text}"));
            }

            // 飞书机器人可能会返回 200 但 code != 0
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(code) = val.get("code").and_then(|c| c.as_i64()) {
                    if code != 0 {
                        let msg = val.get("msg").and_then(|m| m.as_str()).unwrap_or("未知错误");
                        return Err(format!("飞书接口错误 (code={code}): {msg}"));
                    }
                }
            }
            Ok(())
        }
        WebhookPlatform::Wechat => {
            let payload = serde_json::json!({
                "msgtype": "markdown",
                "markdown": {
                    "content": markdown_content
                }
            });
            let resp = client
                .post(url)
                .timeout(Duration::from_secs(10))
                .json(&payload)
                .send()
                .await
                .map_err(|e| format!("请求企业微信 Webhook 失败: {e}"))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(format!("企业微信 Webhook 返回 HTTP {status}: {text}"));
            }
            Ok(())
        }
        WebhookPlatform::Dingtalk => {
            let payload = serde_json::json!({
                "msgtype": "markdown",
                "markdown": {
                    "title": title,
                    "text": markdown_content
                }
            });
            let resp = client
                .post(url)
                .timeout(Duration::from_secs(10))
                .json(&payload)
                .send()
                .await
                .map_err(|e| format!("请求钉钉 Webhook 失败: {e}"))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(format!("钉钉 Webhook 返回 HTTP {status}: {text}"));
            }
            Ok(())
        }
        WebhookPlatform::Generic => {
            let payload = serde_json::json!({
                "title": title,
                "markdown": markdown_content,
                "timestamp": chrono::Utc::now().to_rfc3339()
            });
            let resp = client
                .post(url)
                .timeout(Duration::from_secs(10))
                .json(&payload)
                .send()
                .await
                .map_err(|e| format!("请求自定义 Webhook 失败: {e}"))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(format!("自定义 Webhook 返回 HTTP {status}: {text}"));
            }
            Ok(())
        }
    }
}

/// 读取当前 Webhook 配置。
pub async fn get_webhook_config(pool: &sqlx::PgPool) -> WebhookConfig {
    if let Ok(Some(val)) = store::admin::get_setting(pool, SETTING_KEY_WEBHOOK).await {
        if let Ok(config) = serde_json::from_value::<WebhookConfig>(val) {
            return config;
        }
    }
    WebhookConfig::default()
}

/// GET /api/settings/webhook
pub async fn get_webhook_endpoint(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
) -> AppResult<Json<WebhookDetailsResponse>> {
    auth.require_super_admin()?;
    let config = get_webhook_config(&state.pool).await;
    let preview_markdown = build_daily_report_markdown(
        &state,
        &config.title_prefix,
        config.include_system_status,
        Some("（此为手动预览样例）"),
    )
    .await?;

    Ok(Json(WebhookDetailsResponse {
        config,
        preview_markdown,
    }))
}

/// PUT /api/settings/webhook
pub async fn update_webhook_endpoint(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Json(mut config): Json<WebhookConfig>,
) -> AppResult<Json<WebhookConfig>> {
    auth.require_super_admin()?;

    // 格式化与校验 daily_push_time (HH:mm)
    let parts: Vec<&str> = config.daily_push_time.trim().split(':').collect();
    if parts.len() != 2
        || parts[0].parse::<u32>().map_or(true, |h| h > 23)
        || parts[1].parse::<u32>().map_or(true, |m| m > 59)
    {
        return Err(AppError::bad("定时推送时间格式必须为 HH:mm，如 20:00"));
    }
    config.daily_push_time = format!(
        "{:02}:{:02}",
        parts[0].parse::<u32>().unwrap(),
        parts[1].parse::<u32>().unwrap()
    );

    if config.title_prefix.trim().is_empty() {
        config.title_prefix = default_title_prefix();
    }

    let val = serde_json::to_value(&config).map_err(|e| AppError::internal(e.to_string()))?;
    store::admin::put_setting(&state.pool, SETTING_KEY_WEBHOOK, &val).await?;

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "更新 Webhook 推送配置",
        "webhook",
        &format!(
            "启用: {}, 平台: {:?}, 定时时间: {}",
            config.enabled, config.platform, config.daily_push_time
        ),
    )
    .await?;

    Ok(Json(config))
}

/// POST /api/settings/webhook/send
pub async fn manual_send_webhook(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Json(req): Json<SendWebhookRequest>,
) -> AppResult<Json<SendWebhookResponse>> {
    auth.require_super_admin()?;
    let config = get_webhook_config(&state.pool).await;
    if config.url.trim().is_empty() {
        return Err(AppError::bad("尚未配置 Webhook 推送地址"));
    }

    let md = build_daily_report_markdown(
        &state,
        &config.title_prefix,
        config.include_system_status,
        req.custom_note.as_deref(),
    )
    .await?;

    let client = Client::new();
    match send_to_webhook(&client, &config, &config.title_prefix, &md).await {
        Ok(_) => {
            store::admin::log(
                &state.pool,
                platform_domain::OperationSource::Admin,
                platform_domain::LogLevel::Info,
                &auth.username,
                "手动触发 Webhook 推送",
                "webhook",
                "成功推送今日下载报告",
            )
            .await?;

            Ok(Json(SendWebhookResponse {
                success: true,
                message: "消息已成功推送至机器人群组".to_string(),
            }))
        }
        Err(err) => {
            store::admin::log(
                &state.pool,
                platform_domain::OperationSource::Admin,
                platform_domain::LogLevel::Warn,
                &auth.username,
                "手动触发 Webhook 推送失败",
                "webhook",
                &err,
            )
            .await?;

            Err(AppError::bad(format!("推送失败: {err}")))
        }
    }
}

/// 检查并执行每日定时推送。
pub async fn check_and_trigger_daily_webhook_push(state: &AppState) -> AppResult<()> {
    let config = get_webhook_config(&state.pool).await;
    if !config.enabled || config.url.trim().is_empty() {
        return Ok(());
    }

    let now = Local::now();
    let today_str = now.format("%Y-%m-%d").to_string();

    // 如果今天已经推送过了，跳过
    if let Some(last) = &config.last_pushed_date {
        if last == &today_str {
            return Ok(());
        }
    }

    // 检查时间是否到达
    let parts: Vec<&str> = config.daily_push_time.split(':').collect();
    if parts.len() != 2 {
        return Ok(());
    }
    let target_hour: u32 = parts[0].parse().unwrap_or(20);
    let target_min: u32 = parts[1].parse().unwrap_or(0);

    let current_hour = now.hour();
    let current_min = now.minute();

    // 如果当前小时和分钟大于等于设定时间
    if (current_hour > target_hour) || (current_hour == target_hour && current_min >= target_min) {
        let md = build_daily_report_markdown(
            state,
            &config.title_prefix,
            config.include_system_status,
            None,
        )
        .await?;

        let client = Client::new();
        match send_to_webhook(&client, &config, &config.title_prefix, &md).await {
            Ok(_) => {
                let mut updated_config = config;
                updated_config.last_pushed_date = Some(today_str);
                let val = serde_json::to_value(&updated_config)
                    .map_err(|e| AppError::internal(e.to_string()))?;
                store::admin::put_setting(&state.pool, SETTING_KEY_WEBHOOK, &val).await?;

                store::admin::log(
                    &state.pool,
                    platform_domain::OperationSource::SystemJob,
                    platform_domain::LogLevel::Info,
                    "系统定时调度",
                    "定时推送 Webhook 日报",
                    "webhook",
                    "今日下载日报已自动推送成功",
                )
                .await?;
            }
            Err(err) => {
                tracing::warn!(error = %err, "定时推送 Webhook 日报失败");
            }
        }
    }

    Ok(())
}
