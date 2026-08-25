//! Mock 邮件验证码适配器（仅限测试/非生产环境）。

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use automation_core::cancel::CancelToken;
use automation_core::mail_code::{MailCodeCursor, MailCodeError, MailCodeProvider, MailCodeResult};

/// 测试用 Mock Provider
#[derive(Debug, Clone)]
pub struct MockMailCodeAdapter {
    is_production: bool,
    should_fail: Arc<AtomicBool>,
    code_to_return: String,
}

impl MockMailCodeAdapter {
    pub fn new(
        is_production: bool,
        code_to_return: impl Into<String>,
    ) -> Result<Self, MailCodeError> {
        if is_production {
            return Err(MailCodeError::Unavailable(
                "MockMailCodeAdapter 严禁在生产环境中启用".to_string(),
            ));
        }
        Ok(Self {
            is_production: false,
            should_fail: Arc::new(AtomicBool::new(false)),
            code_to_return: code_to_return.into(),
        })
    }

    pub fn set_should_fail(&self, fail: bool) {
        self.should_fail.store(fail, Ordering::SeqCst);
    }
}

#[async_trait]
impl MailCodeProvider for MockMailCodeAdapter {
    fn name(&self) -> &'static str {
        "mock"
    }

    async fn prepare(
        &self,
        email: &str,
        timeout: Duration,
    ) -> Result<MailCodeCursor, MailCodeError> {
        if self.is_production {
            return Err(MailCodeError::Unavailable(
                "MockMailCodeAdapter 严禁在生产环境中启用".to_string(),
            ));
        }
        if self.should_fail.load(Ordering::SeqCst) {
            return Err(MailCodeError::Unavailable("Mock 故障模拟".to_string()));
        }
        let now = std::time::Instant::now();
        Ok(MailCodeCursor {
            email: email.to_string(),
            start_time: now,
            started_at: SystemTime::now(),
            deadline: now + timeout,
            provider_version: 0,
            prepared_by: self.name(),
            baseline_codes: HashSet::new(),
        })
    }

    async fn await_code(
        &self,
        cursor: &MailCodeCursor,
        cancel: &CancelToken,
    ) -> Result<MailCodeResult, MailCodeError> {
        if self.is_production {
            return Err(MailCodeError::Unavailable(
                "MockMailCodeAdapter 严禁在生产环境中启用".to_string(),
            ));
        }
        if self.should_fail.load(Ordering::SeqCst) {
            return Err(MailCodeError::Unavailable("Mock 提取失败模拟".to_string()));
        }

        // 模拟短延时等待邮件到达
        if !cancel.sleep(Duration::from_millis(50)).await {
            return Err(MailCodeError::Cancelled);
        }

        if std::time::Instant::now() > cursor.deadline {
            return Err(MailCodeError::Timeout);
        }

        Ok(MailCodeResult {
            code: self.code_to_return.clone(),
        })
    }

    async fn health(&self) -> Result<(), MailCodeError> {
        if self.is_production {
            return Err(MailCodeError::Unavailable(
                "MockMailCodeAdapter 严禁在生产环境中启用".to_string(),
            ));
        }
        if self.should_fail.load(Ordering::SeqCst) {
            return Err(MailCodeError::Unavailable(
                "Mock 处于模拟故障状态".to_string(),
            ));
        }
        Ok(())
    }
}
