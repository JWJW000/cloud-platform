//! 邮件验证码 Router 与版本化热切换。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use automation_core::cancel::CancelToken;
use automation_core::mail_code::{MailCodeCursor, MailCodeError, MailCodeProvider, MailCodeResult};
use tokio::sync::RwLock;
use tracing::{info, warn};

use super::manual::ManualMailCodeAdapter;
use super::mock::MockMailCodeAdapter;
use super::outlook_http::{OutlookConfig, OutlookHttpMailCodeAdapter};

/// 人工输入必须拥有完整的独立窗口，不能被自动 Provider 已消耗的轮询时间挤占。
const MANUAL_INPUT_TIMEOUT: Duration = Duration::from_secs(10 * 60);

struct AutomaticWithManualFallback {
    primary: Arc<dyn MailCodeProvider>,
    manual: Arc<dyn MailCodeProvider>,
}

#[async_trait]
impl MailCodeProvider for AutomaticWithManualFallback {
    fn name(&self) -> &'static str {
        "automatic_with_manual_fallback"
    }

    async fn prepare(
        &self,
        email: &str,
        timeout: Duration,
    ) -> Result<MailCodeCursor, MailCodeError> {
        match self.primary.prepare(email, timeout).await {
            Ok(cursor) => Ok(cursor),
            Err(MailCodeError::Cancelled) => Err(MailCodeError::Cancelled),
            Err(_) => self.manual.prepare(email, MANUAL_INPUT_TIMEOUT).await,
        }
    }

    async fn await_code(
        &self,
        cursor: &MailCodeCursor,
        cancel: &CancelToken,
    ) -> Result<MailCodeResult, MailCodeError> {
        if cursor.prepared_by == self.manual.name() {
            return self.manual.await_code(cursor, cancel).await;
        }
        match self.primary.await_code(cursor, cancel).await {
            Ok(code) => Ok(code),
            Err(MailCodeError::Cancelled) => Err(MailCodeError::Cancelled),
            Err(_) => {
                let manual_cursor = self
                    .manual
                    .prepare(&cursor.email, MANUAL_INPUT_TIMEOUT)
                    .await?;
                self.manual.await_code(&manual_cursor, cancel).await
            }
        }
    }

    async fn health(&self) -> Result<(), MailCodeError> {
        self.primary.health().await
    }
}

/// Provider 配置类型
#[derive(Debug, Clone)]
pub enum MailCodeProviderConfig {
    Manual,
    Mock { is_production: bool, code: String },
    OutlookHttp(OutlookConfig),
}

/// Router 保留仍可能被运行中任务引用的版本。游标携带 prepare 时的版本，
/// 因此热切换不会让同一次注册在中途换 Provider。
struct RouterState {
    current_version: u64,
    providers: BTreeMap<u64, Arc<dyn MailCodeProvider>>,
}

/// 支持热切换的 MailCodeRouter
#[derive(Clone)]
pub struct MailCodeRouter {
    state: Arc<RwLock<RouterState>>,
    version_counter: Arc<AtomicU64>,
}

impl MailCodeRouter {
    /// 构建任务级不可变快照。Master 下发的版本直接成为游标版本；后续全局配置
    /// 更新不会改变这个实例。自动 Provider 失败时转入同一次尝试的人工通道。
    pub fn new_attempt(
        version: u64,
        primary: Arc<dyn MailCodeProvider>,
        manual: Arc<dyn MailCodeProvider>,
    ) -> Self {
        let provider: Arc<dyn MailCodeProvider> = if primary.name() == "manual" {
            manual
        } else {
            Arc::new(AutomaticWithManualFallback { primary, manual })
        };
        let version = version.max(1);
        let mut providers = BTreeMap::new();
        providers.insert(version, provider);
        Self {
            state: Arc::new(RwLock::new(RouterState {
                current_version: version,
                providers,
            })),
            version_counter: Arc::new(AtomicU64::new(version)),
        }
    }

    pub fn new_manual() -> Self {
        let provider = Arc::new(ManualMailCodeAdapter::new());
        let mut providers: BTreeMap<u64, Arc<dyn MailCodeProvider>> = BTreeMap::new();
        providers.insert(1, provider);
        Self {
            state: Arc::new(RwLock::new(RouterState {
                current_version: 1,
                providers,
            })),
            version_counter: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn new_with_config(config: MailCodeProviderConfig) -> Result<Self, MailCodeError> {
        let provider: Arc<dyn MailCodeProvider> = match config {
            MailCodeProviderConfig::Manual => Arc::new(ManualMailCodeAdapter::new()),
            MailCodeProviderConfig::Mock {
                is_production,
                code,
            } => Arc::new(MockMailCodeAdapter::new(is_production, code)?),
            MailCodeProviderConfig::OutlookHttp(cfg) => {
                Arc::new(OutlookHttpMailCodeAdapter::new(cfg)?)
            }
        };

        let mut providers = BTreeMap::new();
        providers.insert(1, provider);
        Ok(Self {
            state: Arc::new(RwLock::new(RouterState {
                current_version: 1,
                providers,
            })),
            version_counter: Arc::new(AtomicU64::new(1)),
        })
    }

    /// 获取当前生效的 Provider 快照（用于保证正在执行的任务锁定开始时的配置）
    pub async fn snapshot(&self) -> (u64, Arc<dyn MailCodeProvider>) {
        let guard = self.state.read().await;
        let version = guard.current_version;
        let provider = guard
            .providers
            .get(&version)
            .expect("current provider version must exist")
            .clone();
        (version, provider)
    }

    /// 热切换 Provider
    pub async fn update_config(
        &self,
        config: MailCodeProviderConfig,
    ) -> Result<u64, MailCodeError> {
        let new_provider: Arc<dyn MailCodeProvider> = match config {
            MailCodeProviderConfig::Manual => Arc::new(ManualMailCodeAdapter::new()),
            MailCodeProviderConfig::Mock {
                is_production,
                code,
            } => Arc::new(MockMailCodeAdapter::new(is_production, code)?),
            MailCodeProviderConfig::OutlookHttp(cfg) => {
                Arc::new(OutlookHttpMailCodeAdapter::new(cfg)?)
            }
        };

        // 先验证再发布。失败时不改变 current_version，上一健康版本继续服务。
        if new_provider.name() != "manual" {
            if let Err(err) = new_provider.health().await {
                warn!(
                    "新 Provider [{}] 健康检查未通过: {err}",
                    new_provider.name()
                );
                return Err(err);
            }
        }

        let new_version = self.version_counter.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut guard = self.state.write().await;
            guard.providers.insert(new_version, new_provider.clone());
            guard.current_version = new_version;
        }

        info!(
            version = new_version,
            provider = new_provider.name(),
            "MailCodeRouter 已热切换到新 Provider 配置"
        );
        Ok(new_version)
    }
}

#[async_trait]
impl MailCodeProvider for MailCodeRouter {
    fn name(&self) -> &'static str {
        "router"
    }

    async fn prepare(
        &self,
        email: &str,
        timeout: Duration,
    ) -> Result<MailCodeCursor, MailCodeError> {
        let (version, provider) = self.snapshot().await;
        let mut cursor = provider.prepare(email, timeout).await?;
        cursor.provider_version = version;
        Ok(cursor)
    }

    async fn await_code(
        &self,
        cursor: &MailCodeCursor,
        cancel: &CancelToken,
    ) -> Result<MailCodeResult, MailCodeError> {
        let provider = {
            let guard = self.state.read().await;
            guard.providers.get(&cursor.provider_version).cloned()
        }
        .ok_or_else(|| {
            MailCodeError::Unavailable(format!(
                "邮件 Provider 配置版本 {} 已不可用",
                cursor.provider_version
            ))
        })?;
        provider.await_code(cursor, cancel).await
    }

    async fn health(&self) -> Result<(), MailCodeError> {
        let (_, provider) = self.snapshot().await;
        provider.health().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn running_attempt_keeps_prepare_snapshot_after_hot_switch() {
        let router = MailCodeRouter::new_with_config(MailCodeProviderConfig::Mock {
            is_production: false,
            code: "111111".to_string(),
        })
        .unwrap();
        let cursor = router
            .prepare("reader@example.com", Duration::from_secs(2))
            .await
            .unwrap();

        router
            .update_config(MailCodeProviderConfig::Mock {
                is_production: false,
                code: "222222".to_string(),
            })
            .await
            .unwrap();

        let result = router
            .await_code(&cursor, &CancelToken::new())
            .await
            .unwrap();
        assert_eq!(result.code, "111111");
    }

    #[tokio::test]
    async fn invalid_update_keeps_previous_version() {
        let router = MailCodeRouter::new_manual();
        let before = router.snapshot().await.0;
        let err = router
            .update_config(MailCodeProviderConfig::Mock {
                is_production: true,
                code: "123456".to_string(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MailCodeError::Unavailable(_)));
        assert_eq!(router.snapshot().await.0, before);
    }
}
