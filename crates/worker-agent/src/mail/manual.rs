//! Manual 邮件验证码适配器（人工降级）。
//!
//! Adapter 只负责一次性输入的内存通道。人工事项的可靠落库仍由 Worker → Master
//! 的 `ManualActionRequired` 事件完成，验证码本身不会写入数据库或日志。

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use automation_core::cancel::CancelToken;
use automation_core::mail_code::{MailCodeCursor, MailCodeError, MailCodeProvider, MailCodeResult};
use tokio::sync::{mpsc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualCodeRequest {
    pub action_id: String,
}

pub struct ManualCodeSubmission {
    pub action_id: String,
    pub code: String,
}

impl std::fmt::Debug for ManualCodeSubmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManualCodeSubmission")
            .field("action_id", &self.action_id)
            .field("code", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ManualMailCodeAdapter {
    request_tx: Option<mpsc::UnboundedSender<ManualCodeRequest>>,
    submission_rx: Option<Arc<Mutex<mpsc::UnboundedReceiver<ManualCodeSubmission>>>>,
}

impl ManualMailCodeAdapter {
    /// 无通道的实例用于显式声明“需要人工降级”。
    pub fn new() -> Self {
        Self::default()
    }

    /// 为一个注册尝试创建人工输入通道。通道只存在于 Worker 当前进程内。
    pub fn channel() -> (
        Self,
        mpsc::UnboundedReceiver<ManualCodeRequest>,
        mpsc::UnboundedSender<ManualCodeSubmission>,
    ) {
        let (request_tx, request_rx) = mpsc::unbounded_channel();
        let (submission_tx, submission_rx) = mpsc::unbounded_channel();
        (
            Self {
                request_tx: Some(request_tx),
                submission_rx: Some(Arc::new(Mutex::new(submission_rx))),
            },
            request_rx,
            submission_tx,
        )
    }
}

#[async_trait]
impl MailCodeProvider for ManualMailCodeAdapter {
    fn name(&self) -> &'static str {
        "manual"
    }

    async fn prepare(
        &self,
        email: &str,
        timeout: Duration,
    ) -> Result<MailCodeCursor, MailCodeError> {
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
        let (Some(request_tx), Some(submission_rx)) = (&self.request_tx, &self.submission_rx)
        else {
            return Err(MailCodeError::ManualFallbackRequired);
        };

        let action_id = uuid::Uuid::new_v4().to_string();
        request_tx
            .send(ManualCodeRequest {
                action_id: action_id.clone(),
            })
            .map_err(|_| MailCodeError::Unavailable("人工验证请求通道已关闭".to_string()))?;

        let remaining = cursor
            .deadline
            .checked_duration_since(std::time::Instant::now())
            .ok_or(MailCodeError::Timeout)?;
        let mut receiver = submission_rx.lock().await;
        let timeout = tokio::time::sleep(remaining);
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Err(MailCodeError::Cancelled),
                _ = &mut timeout => return Err(MailCodeError::Timeout),
                submission = receiver.recv() => {
                    let Some(submission) = submission else {
                        return Err(MailCodeError::Unavailable("人工验证码回传通道已关闭".to_string()));
                    };
                    if submission.action_id != action_id {
                        continue;
                    }
                    if !(4..=8).contains(&submission.code.len())
                        || !submission.code.bytes().all(|b| b.is_ascii_digit())
                    {
                        continue;
                    }
                    return Ok(MailCodeResult { code: submission.code });
                }
            }
        }
    }

    async fn health(&self) -> Result<(), MailCodeError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn manual_submission_resumes_same_waiter() {
        let (provider, mut requests, submissions) = ManualMailCodeAdapter::channel();
        let cursor = provider
            .prepare("reader@example.com", Duration::from_secs(2))
            .await
            .unwrap();
        let task =
            tokio::spawn(async move { provider.await_code(&cursor, &CancelToken::new()).await });
        let request = requests.recv().await.unwrap();
        submissions
            .send(ManualCodeSubmission {
                action_id: request.action_id,
                code: "654321".to_string(),
            })
            .unwrap();
        assert_eq!(task.await.unwrap().unwrap().code, "654321");
    }

    #[tokio::test]
    async fn cancellation_interrupts_manual_wait() {
        let (provider, mut requests, _submissions) = ManualMailCodeAdapter::channel();
        let cursor = provider
            .prepare("reader@example.com", Duration::from_secs(30))
            .await
            .unwrap();
        let cancel = CancelToken::new();
        let waiter_cancel = cancel.clone();
        let task = tokio::spawn(async move { provider.await_code(&cursor, &waiter_cancel).await });
        requests.recv().await.unwrap();
        cancel.cancel("test");
        assert_eq!(task.await.unwrap().unwrap_err(), MailCodeError::Cancelled);
    }
}
