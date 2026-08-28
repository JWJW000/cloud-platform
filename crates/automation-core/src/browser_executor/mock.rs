//! 测试用的 MockBrowserExecutor。

use async_trait::async_trait;
use platform_domain::FailureClass;

use super::{BrowserCommand, BrowserExecutor, BrowserResult};
use crate::types::{AutomationError, DownloadOutcome, SessionHandle};

pub struct MockBrowserExecutor {
    pub should_fail: bool,
}

impl Default for MockBrowserExecutor {
    fn default() -> Self {
        Self { should_fail: false }
    }
}

impl MockBrowserExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_failure() -> Self {
        Self { should_fail: true }
    }
}

#[async_trait]
impl BrowserExecutor for MockBrowserExecutor {
    async fn execute(&self, command: BrowserCommand) -> Result<BrowserResult, AutomationError> {
        if self.should_fail {
            return Err(AutomationError::new(
                FailureClass::SiteUnavailable,
                "Mock 浏览器执行器错误".to_string(),
            ));
        }

        match command {
            BrowserCommand::OpenSession { spec } => Ok(BrowserResult::SessionOpened(SessionHandle {
                session_id: spec.session_id,
                browser_path: spec.browser_path.unwrap_or_default(),
                profile_dir: spec.profile_dir,
            })),
            BrowserCommand::DownloadBook { spec, .. } => {
                Ok(BrowserResult::DownloadDone(DownloadOutcome {
                    staged_file: spec.staging_dir.join("book.pdf"),
                    size_bytes: 1024,
                    quota_indicator: Some((1, 10)),
                    evidence: None,
                    match_record: None,
                }))
            }
            BrowserCommand::RegisterAccount { .. } => Ok(BrowserResult::RegistrationDone(
                crate::types::RegistrationOutcome {
                    already_exists: false,
                    awaiting_verification: false,
                },
            )),
            BrowserCommand::ExportCookies => Ok(BrowserResult::Cookies(Vec::new())),
            BrowserCommand::CloseSession { .. } => Ok(BrowserResult::Closed),
        }
    }
}
