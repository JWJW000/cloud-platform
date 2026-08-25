//! 邮件验证码 Provider 数据库存储层。

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

/// 邮件验证码 Provider 类型
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MailProviderType {
    /// 人工录入
    #[default]
    Manual,
    /// Outlook HTTP 自动化提取
    OutlookHttp,
    /// 模拟测试
    Mock,
}

/// 邮件验证码 Provider 配置模型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailProviderConfigRecord {
    /// 唯一主键版本
    pub id: i64,
    /// Provider 类型
    pub provider_type: String,
    /// 端点地址
    pub endpoint: String,
    /// API Key 密文引用（不存明文）
    pub api_key_secret_ref: Option<String>,
    /// 轮询间隔秒数
    pub poll_interval_secs: i32,
    /// 超时秒数
    pub timeout_secs: i32,
    /// 允许的白名单域名列表
    pub allowed_hosts: Vec<String>,
    /// 允许的验证码邮件发件人；非空时 Worker 严格匹配。
    #[serde(default)]
    pub allowed_senders: Vec<String>,
    /// 配置版本号
    pub version: i64,
    /// 是否激活
    pub is_active: bool,
    /// 最后更新人
    pub updated_by: String,
    /// 更新时间
    pub updated_at: DateTime<Utc>,
}

/// 写入或更新 Provider 配置参数
#[derive(Debug, Clone, Deserialize)]
pub struct UpsertMailProviderConfig {
    /// Provider 类型
    pub provider_type: String,
    /// 端点地址
    pub endpoint: String,
    /// API Key 密文引用
    pub api_key_secret_ref: Option<String>,
    /// 轮询间隔秒数
    pub poll_interval_secs: i32,
    /// 超时秒数
    pub timeout_secs: i32,
    /// 允许的白名单域名列表
    pub allowed_hosts: Vec<String>,
    /// 允许发送验证码邮件的发件人列表；空列表表示不额外限制。
    pub allowed_senders: Vec<String>,
}

/// 将已经由 FieldCipher 加密的密钥写入独立密钥表，返回不可猜测引用。
pub async fn save_secret_ciphertext(
    pool: &PgPool,
    cipher_text: &str,
    operator: &str,
) -> Result<String> {
    let secret_ref = format!("mailsec_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO mail_provider_secrets (secret_ref, cipher_text, created_by) VALUES ($1, $2, $3)",
    )
    .bind(&secret_ref)
    .bind(cipher_text)
    .bind(operator)
    .execute(pool)
    .await
    .context("保存邮件 Provider 密钥失败")?;
    Ok(secret_ref)
}

/// 读取密文供任务分配时在 Master 内存中解密。调用方不得记录返回值。
pub async fn get_secret_ciphertext(pool: &PgPool, secret_ref: &str) -> Result<Option<String>> {
    sqlx::query_scalar("SELECT cipher_text FROM mail_provider_secrets WHERE secret_ref = $1")
        .bind(secret_ref)
        .fetch_optional(pool)
        .await
        .context("读取邮件 Provider 密钥失败")
}

/// 获取当前激活的 Provider 配置
pub async fn get_active_config(pool: &PgPool) -> Result<Option<MailProviderConfigRecord>> {
    let row = sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT value FROM settings WHERE key = 'mail_code_provider'",
    )
    .fetch_optional(pool)
    .await
    .context("查询 mail_code_provider 系统设置失败")?;

    if let Some(val) = row {
        if let Ok(cfg) = serde_json::from_value::<MailProviderConfigRecord>(val) {
            return Ok(Some(cfg));
        }
    }

    Ok(None)
}

/// 保存新的 Provider 配置（版本单调递增，Secret 绝不进入明文普通字段）
pub async fn save_active_config(
    pool: &PgPool,
    cfg: UpsertMailProviderConfig,
    operator: &str,
) -> Result<MailProviderConfigRecord> {
    let mut tx = pool.begin().await?;

    // 首次写入时 settings 行还不存在，单靠 FOR UPDATE 无法锁住“空行”。
    // 事务级 advisory lock 保证并发发布也只能得到严格递增版本。
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('mail_code_provider'))")
        .execute(&mut *tx)
        .await?;

    let existing = sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT value FROM settings WHERE key = 'mail_code_provider' FOR UPDATE",
    )
    .fetch_optional(&mut *tx)
    .await?;

    let next_version = if let Some(val) = existing {
        if let Ok(old) = serde_json::from_value::<MailProviderConfigRecord>(val) {
            old.version + 1
        } else {
            1
        }
    } else {
        1
    };

    let record = MailProviderConfigRecord {
        id: next_version,
        provider_type: cfg.provider_type,
        endpoint: cfg.endpoint,
        api_key_secret_ref: cfg.api_key_secret_ref,
        poll_interval_secs: cfg.poll_interval_secs,
        timeout_secs: cfg.timeout_secs,
        allowed_hosts: cfg.allowed_hosts,
        allowed_senders: cfg.allowed_senders,
        version: next_version,
        is_active: true,
        updated_by: operator.to_string(),
        updated_at: Utc::now(),
    };

    let json_val = serde_json::to_value(&record)?;

    sqlx::query(
        r#"
        INSERT INTO settings (key, value, updated_at)
        VALUES ('mail_code_provider', $1, NOW())
        ON CONFLICT (key) DO UPDATE
        SET value = $1, updated_at = NOW()
        "#,
    )
    .bind(json_val)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(record)
}
