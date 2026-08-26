//! Outlook 账号清单同步 API。
//!
//! 从 outlookmail 邮件服务（外部 HTTP 服务，见 `mail_provider` 配置）拉取账号清单，
//! 分两步把未注册账号落地云端：
//!
//! 1. `POST /api/accounts/outlook/preview` —— 只拉取并规范化账号清单（不写库），
//!    返回给管理端勾选「哪些要注册」；
//! 2. `POST /api/accounts/outlook/sync` —— 接收勾选的邮箱 + 云端统一注册密码，
//!    写入 `accounts`（状态「待注册」，密码 FieldCipher 加密存储、绝不下发明文），
//!    并可一键创建账号注册批次下发给 Worker。
//!
//! 安全约束（必须保持）：
//! 1. 出站请求复用 `mail_provider` 的 SSRF 校验（HTTPS-only、allowed_hosts 允许列表、
//!    DNS 解析后固定 IP、禁用重定向、1 MiB 响应上限）；
//! 2. API Key 与注册密码均经 FieldCipher 加解密，绝不进入日志、错误信息或预览响应；
//! 3. 仅超级管理员可调用。

use std::collections::HashSet;
use std::time::Duration;

use axum::extract::State;
use axum::{Json, Router};
use chrono::Utc;
use platform_domain::{AccountStatus, BatchStatus};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::auth::AuthenticatedUser;
use crate::api::mail_provider::validate_outlook_endpoint;
use crate::error::{AppError, AppResult};
use crate::models::AccountRegistrationBatch;
use crate::scheduler;
use crate::state::AppState;
use crate::store;
use crate::store::account_registration::NewAccountRegistrationBatch;
use crate::store::mail_provider::{get_active_config, get_secret_ciphertext};

/// 账号清单响应体大小上限（与邮件验证码接口一致，防无界读取）。
const MAX_RESPONSE_BYTES: usize = 1 << 20;

/// 同步请求体（第 2 步）。
#[derive(Clone, Deserialize)]
pub struct OutlookSyncRequest {
    /// 云端统一设置的注册密码（明文字段，仅 HTTPS 传输）。
    pub default_password: String,
    /// 勾选要注册的账号邮箱（来自 preview 返回清单）。
    #[serde(default)]
    pub emails: Vec<String>,
    /// 同步完成后是否为这批新账号创建注册批次。
    #[serde(default)]
    pub create_batch: bool,
    /// 注册批次名称（选填，默认自动生成时间戳批次名）。
    pub batch_name: Option<String>,
    /// 注册批次优先级。
    #[serde(default = "default_priority")]
    pub priority: i32,
    /// 创建批次后是否立即下发 Worker 注册。
    #[serde(default)]
    pub start_immediately: bool,
}

fn default_priority() -> i32 {
    10
}

impl std::fmt::Debug for OutlookSyncRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutlookSyncRequest")
            .field("default_password", &"[REDACTED]")
            .field("emails", &self.emails)
            .field("create_batch", &self.create_batch)
            .field("batch_name", &self.batch_name)
            .field("priority", &self.priority)
            .field("start_immediately", &self.start_immediately)
            .finish()
    }
}

/// 拉取阶段返回的账号（可勾选）。
#[derive(Debug, Clone, Serialize)]
pub struct OutlookPreviewAccount {
    pub email: String,
    pub nickname: String,
}

/// 拉取阶段响应包（第 1 步，不写库）。
#[derive(Debug, Clone, Serialize)]
pub struct OutlookPreviewResponse {
    /// 从 outlookmail 拉取并规范化去重后的有效账号数。
    pub fetched: usize,
    /// 缺少合法邮箱或被去重而跳过的账号数。
    pub skipped: usize,
    /// 本次拉取到的账号（供前端勾选）。
    pub accounts: Vec<OutlookPreviewAccount>,
}

/// 本次云端新增的待注册账号。
#[derive(Debug, Clone, Serialize)]
pub struct SyncedAccount {
    pub id: Uuid,
    pub email: String,
    pub nickname: String,
}

/// 同步响应包（第 2 步）。
#[derive(Debug, Clone, Serialize)]
pub struct OutlookSyncResponse {
    /// 云端新增的待注册账号数。
    pub inserted: usize,
    /// 云端已存在而被跳过的账号数。
    pub duplicates: usize,
    /// 缺少合法邮箱而被跳过的账号数。
    pub skipped: usize,
    /// 本次新增的账号（供前端勾选创建注册批次）。
    pub accounts: Vec<SyncedAccount>,
    /// 若请求创建批次，返回批次信息。
    pub registration_batch: Option<AccountRegistrationBatch>,
}

/// outlookmail 外部接口返回的账号摘要（仅读取展示字段，刻意不接收密码）。
#[derive(Debug, Clone, Deserialize)]
struct RemoteOutlookAccount {
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    remark: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RemoteOutlookPayload {
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    accounts: Vec<RemoteOutlookAccount>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/outlook/preview",
            axum::routing::post(preview_outlook_accounts),
        )
        .route("/outlook/sync", axum::routing::post(sync_outlook_accounts))
}

/// POST /api/accounts/outlook/preview —— 拉取 outlookmail 账号清单并规范化（不写库）。
pub async fn preview_outlook_accounts(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
) -> AppResult<Json<OutlookPreviewResponse>> {
    auth.require_super_admin()?;

    // 1. 读取 Outlook 邮件服务配置（端点、允许主机、API Key 密文引用）
    let config = get_active_config(&state.pool)
        .await?
        .ok_or_else(|| AppError::bad("未配置邮件 Provider（mail_code_provider）"))?;
    if config.provider_type != "outlook_http" {
        return Err(AppError::bad("当前 Provider 类型不支持账号清单同步"));
    }
    let secret_ref = config
        .api_key_secret_ref
        .as_deref()
        .ok_or_else(|| AppError::bad("未配置 Outlook API Key"))?;
    let key_ciphertext = get_secret_ciphertext(&state.pool, secret_ref)
        .await?
        .ok_or_else(|| AppError::bad("Outlook API Key 密钥引用失效"))?;
    let api_key = state
        .cipher
        .decrypt(&key_ciphertext)
        .map_err(AppError::Internal)?;

    // 2. 拉取账号清单（复用 SSRF 安全校验 + DNS 固定客户端）
    let remote = fetch_outlook_accounts(&config.endpoint, &api_key, &config.allowed_hosts).await?;

    // 3. 规范化 + 去重（小写邮箱），得到可勾选列表
    let (accounts, skipped) = normalize_accounts(remote);

    Ok(Json(OutlookPreviewResponse {
        fetched: accounts.len(),
        skipped,
        accounts,
    }))
}

/// POST /api/accounts/outlook/sync —— 把勾选的账号写入云端（待注册），可选创建注册批次。
pub async fn sync_outlook_accounts(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Json(req): Json<OutlookSyncRequest>,
) -> AppResult<Json<OutlookSyncResponse>> {
    auth.require_super_admin()?;

    let password = req.default_password.trim();
    if password.len() < 6 || password.len() > 64 {
        return Err(AppError::bad("注册密码长度须为 6–64 字符"));
    }

    // 勾选邮箱规范化 + 去重（小写、含 @、非空）
    let mut seen = HashSet::new();
    let mut skipped = 0usize;
    let mut pending: Vec<(String, String)> = Vec::new(); // (email, nickname)
    for raw_email in &req.emails {
        let email = raw_email.trim().to_ascii_lowercase();
        if email.is_empty() || !email.contains('@') || !seen.insert(email.clone()) {
            skipped += 1;
            continue;
        }
        let nickname = email.split('@').next().unwrap_or(&email).to_string();
        pending.push((email, nickname));
    }

    // 事务：新增待注册账号（云端统一密码）+ 可选创建注册批次
    let password_cipher = state.cipher.encrypt(password).map_err(AppError::Internal)?;
    let mut tx = state.pool.begin().await?;

    let mut inserted: Vec<SyncedAccount> = Vec::new();
    let mut duplicates = 0usize;
    for (email, nickname) in &pending {
        // ON CONFLICT (email) DO NOTHING 保证并发安全，重复邮箱跳过而非报错
        let row = sqlx::query_as::<_, (Uuid, String, String)>(
            "INSERT INTO accounts (id, email, password_cipher, nickname, status, daily_limit) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (email) DO NOTHING \
             RETURNING id, email, nickname",
        )
        .bind(Uuid::new_v4())
        .bind(email)
        .bind(&password_cipher)
        .bind(nickname)
        .bind(AccountStatus::PendingRegistration.as_str())
        .bind(10_i32)
        .fetch_optional(&mut *tx)
        .await?;
        match row {
            Some((id, email, nickname)) => inserted.push(SyncedAccount {
                id,
                email,
                nickname,
            }),
            None => duplicates += 1,
        }
    }

    let mut created_batch = None;
    if req.create_batch && !inserted.is_empty() {
        let batch_name = req
            .batch_name
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| format!("Outlook同步-{}", Utc::now().format("%Y%m%d%H%M%S")));

        let batch = store::account_registration::create_batch(
            &mut *tx,
            &NewAccountRegistrationBatch {
                name: batch_name,
                source_file: Some("outlookmail".to_string()),
                priority: req.priority,
                created_by: Some(auth.id),
            },
        )
        .await?;

        for account in &inserted {
            store::account_registration::create_task(&mut *tx, batch.id, account.id, req.priority)
                .await?;
        }

        if req.start_immediately {
            store::account_registration::update_batch_status(
                &mut *tx,
                batch.id,
                BatchStatus::NotStarted,
                BatchStatus::Running,
            )
            .await?;
        }

        created_batch = Some(batch);
    }

    tx.commit().await?;

    // 5. 立即触发调度器扫单，让 Worker 认领注册任务
    if req.start_immediately && created_batch.is_some() {
        let _ = scheduler::trigger_scheduler_sweep(&state).await;
    }

    state.events.publish(
        "账号变更",
        serde_json::json!({
            "来源": "outlookmail",
            "新增": inserted.len(),
            "重复": duplicates,
        }),
    );
    if let Some(batch) = &created_batch {
        state.events.publish(
            "账号注册批次变更",
            serde_json::json!({ "批次": batch.id, "动作": "创建" }),
        );
    }

    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Admin,
        platform_domain::LogLevel::Info,
        &auth.username,
        "Outlook 账号同步注册",
        &format!(
            "新增 {} / 重复 {} / 跳过 {}",
            inserted.len(),
            duplicates,
            skipped
        ),
        &format!(
            "勾选 {}，创建批次：{}",
            pending.len(),
            created_batch.is_some()
        ),
    )
    .await?;

    Ok(Json(OutlookSyncResponse {
        inserted: inserted.len(),
        duplicates,
        skipped,
        accounts: inserted,
        registration_batch: created_batch,
    }))
}

/// 规范化 + 去重（小写邮箱），返回 (email, nickname) 列表与跳过数。
fn normalize_accounts(remote: Vec<RemoteOutlookAccount>) -> (Vec<OutlookPreviewAccount>, usize) {
    let mut seen = HashSet::new();
    let mut skipped = 0usize;
    let mut accounts = Vec::new();
    for acc in remote {
        let Some(raw_email) = acc.email else {
            skipped += 1;
            continue;
        };
        let email = raw_email.trim().to_ascii_lowercase();
        if email.is_empty() || !email.contains('@') || !seen.insert(email.clone()) {
            skipped += 1;
            continue;
        }
        let nickname = acc
            .remark
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| email.split('@').next().unwrap_or(&email).to_string());
        accounts.push(OutlookPreviewAccount { email, nickname });
    }
    (accounts, skipped)
}

/// 拉取 outlookmail 账号清单。
async fn fetch_outlook_accounts(
    endpoint: &str,
    api_key: &str,
    allowed_hosts: &[String],
) -> AppResult<Vec<RemoteOutlookAccount>> {
    let target = validate_outlook_endpoint(endpoint, allowed_hosts).await?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        // 固定到刚刚完成全地址安全检查的目标，避免校验后再次 DNS 解析造成
        // rebinding/TOCTOU 绕过；TLS SNI 仍使用原始主机名。
        .resolve(&target.host, target.pinned_address)
        .build()
        .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))?;

    let accounts_url = url::Url::parse(endpoint)
        .map_err(|_| AppError::bad("端点不是合法 URL"))?
        .join("api/external/accounts")
        .map_err(|_| AppError::bad("无法构造账号清单接口地址"))?;

    let response = client
        .get(accounts_url)
        .header("X-API-Key", api_key.trim())
        .send()
        .await
        .map_err(|_| AppError::bad("无法连接 Outlook 账号服务"))?;

    let status = response.status();
    if status.is_success() {
        let bytes = response
            .bytes()
            .await
            .map_err(|_| AppError::bad("读取 Outlook 账号清单失败"))?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(AppError::bad("Outlook 账号清单响应过大"));
        }
        let payload: RemoteOutlookPayload = serde_json::from_slice(&bytes)
            .map_err(|error| AppError::internal(format!("解析 Outlook 账号清单失败：{error}")))?;
        if payload.success == Some(false) {
            return Err(AppError::bad(
                payload
                    .error
                    .unwrap_or_else(|| "Outlook 服务拒绝返回账号清单".to_string()),
            ));
        }
        Ok(payload.accounts)
    } else if matches!(
        status,
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
    ) {
        Err(AppError::bad("Outlook API Key 鉴权失败"))
    } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        Err(AppError::bad("Outlook 服务当前限流，请稍后重试"))
    } else {
        Err(AppError::bad(format!(
            "Outlook 服务返回 HTTP {}",
            status.as_u16()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_normalizes_emails_and_skips_invalid() {
        let remote = vec![
            RemoteOutlookAccount {
                email: Some("  Alice@Example.COM  ".to_string()),
                remark: Some("Alice".to_string()),
            },
            RemoteOutlookAccount {
                email: Some("alice@example.com".to_string()),
                remark: None,
            },
            RemoteOutlookAccount {
                email: Some("bob@example.com".to_string()),
                remark: Some("   ".to_string()),
            },
            RemoteOutlookAccount {
                email: Some("not-an-email".to_string()),
                remark: None,
            },
            RemoteOutlookAccount {
                email: None,
                remark: None,
            },
        ];

        let (accounts, skipped) = normalize_accounts(remote);

        assert_eq!(accounts.len(), 2, "只有 alice 与 bob 两个有效唯一账号");
        assert_eq!(accounts[0].email, "alice@example.com");
        assert_eq!(accounts[0].nickname, "Alice");
        assert_eq!(accounts[1].email, "bob@example.com");
        assert_eq!(accounts[1].nickname, "bob", "备注全空白时退回邮箱本地部分");
        assert_eq!(skipped, 3, "重复、非法邮箱与无邮箱各计一次跳过");
    }

    #[test]
    fn sync_request_debug_redacts_password() {
        let request = OutlookSyncRequest {
            default_password: "super-secret-password".to_string(),
            emails: vec!["alice@example.com".to_string()],
            create_batch: true,
            batch_name: None,
            priority: 10,
            start_immediately: false,
        };
        let debug = format!("{request:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("super-secret-password"));
    }
}
