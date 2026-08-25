use crate::cancel::CancelToken;
use async_trait::async_trait;
use std::collections::HashSet;
use std::time::{Duration, Instant, SystemTime};

/// 邮件验证码游标：记录准备阶段的邮箱、起始时间点和截止时间
#[derive(Clone)]
pub struct MailCodeCursor {
    pub email: String,
    pub start_time: Instant,
    /// 提交注册表单前的墙上时钟。Provider 可据此排除历史邮件。
    pub started_at: SystemTime,
    pub deadline: Instant,
    /// Router 在 prepare 时固定的配置版本；await_code 必须使用同一版本。
    pub provider_version: u64,
    /// 实际完成 prepare 的 Adapter，用于自动 Provider 失败时固定人工降级路径。
    pub prepared_by: &'static str,
    /// prepare 阶段看到的验证码基线。用于兼容没有邮件时间戳的旧 Outlook API。
    pub baseline_codes: HashSet<String>,
}

impl std::fmt::Debug for MailCodeCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MailCodeCursor")
            .field("email", &"[REDACTED]")
            .field("start_time", &self.start_time)
            .field("started_at", &self.started_at)
            .field("deadline", &self.deadline)
            .field("provider_version", &self.provider_version)
            .field("prepared_by", &self.prepared_by)
            .field("baseline_code_count", &self.baseline_codes.len())
            .finish()
    }
}

/// 邮件验证码提取结果
#[derive(Clone, PartialEq, Eq)]
pub struct MailCodeResult {
    pub code: String,
}

impl std::fmt::Debug for MailCodeResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MailCodeResult")
            .field("code", &"[REDACTED]")
            .finish()
    }
}

/// 邮件验证码提取错误分类
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum MailCodeError {
    #[error("邮件服务认证失败")]
    AuthFailed,
    #[error("邮件服务被限流")]
    RateLimited,
    #[error("等待邮件验证码超时")]
    Timeout,
    #[error("邮件服务不可用：{0}")]
    Unavailable(String),
    #[error("邮件服务网络异常：{0}")]
    Network(String),
    #[error("需要人工降级")]
    ManualFallbackRequired,
    #[error("任务已取消")]
    Cancelled,
}

/// 邮件验证码 Provider 接口（Seam）
#[async_trait]
pub trait MailCodeProvider: Send + Sync {
    /// Provider 名称（如 "outlook_http", "manual", "mock"）
    fn name(&self) -> &'static str;

    /// 在提交注册表单前准备游标（记录开始时间戳）
    async fn prepare(
        &self,
        email: &str,
        timeout: Duration,
    ) -> Result<MailCodeCursor, MailCodeError>;

    /// 轮询或等待验证码到达并提取
    async fn await_code(
        &self,
        cursor: &MailCodeCursor,
        cancel: &CancelToken,
    ) -> Result<MailCodeResult, MailCodeError>;

    /// 健康检查
    async fn health(&self) -> Result<(), MailCodeError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_never_contains_email_or_codes() {
        let now = Instant::now();
        let cursor = MailCodeCursor {
            email: "secret-reader@example.com".to_string(),
            start_time: now,
            started_at: SystemTime::now(),
            deadline: now + Duration::from_secs(10),
            provider_version: 7,
            prepared_by: "test",
            baseline_codes: HashSet::from(["112233".to_string()]),
        };
        let result = MailCodeResult {
            code: "445566".to_string(),
        };
        let debug = format!("{cursor:?} {result:?}");
        assert!(!debug.contains("secret-reader"));
        assert!(!debug.contains("112233"));
        assert!(!debug.contains("445566"));
    }
}
