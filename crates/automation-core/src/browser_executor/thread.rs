//! 专有 OS 线程实现的 BrowserExecutor。

use std::thread;

use async_trait::async_trait;
use platform_domain::FailureClass;
use tokio::sync::{mpsc, oneshot};

use super::{BrowserCommand, BrowserExecutor, BrowserResult};
use crate::engine::AutomationEngine;
use crate::real::RealAutomationEngine;
use crate::types::AutomationError;

type CommandEnvelope = (
    BrowserCommand,
    oneshot::Sender<Result<BrowserResult, AutomationError>>,
);

/// 基于独立 OS 线程的浏览器执行器。
pub struct ThreadBrowserExecutor {
    slot_index: u32,
    cmd_tx: mpsc::Sender<CommandEnvelope>,
}

impl ThreadBrowserExecutor {
    /// 为指定槽位启动一个专有 OS 线程。
    pub fn spawn(slot_index: u32) -> Self {
        // 有界 channel 防止重复指令积压
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<CommandEnvelope>(16);

        let thread_name = format!("browser-slot-{}", slot_index);
        thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let engine = RealAutomationEngine::new();
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("初始化浏览器 OS 线程局部运行时失败");

                runtime.block_on(async move {
                    while let Some((cmd, reply_tx)) = cmd_rx.recv().await {
                        let res = match cmd {
                            BrowserCommand::OpenSession { spec } => {
                                engine
                                    .open_session(&spec)
                                    .await
                                    .map(BrowserResult::SessionOpened)
                            }
                            BrowserCommand::DownloadBook {
                                handle,
                                spec,
                                sink,
                                cancel,
                            } => {
                                engine
                                    .download_book(&handle, &spec, &sink, &cancel)
                                    .await
                                    .map(BrowserResult::DownloadDone)
                            }
                            BrowserCommand::RegisterAccount {
                                handle,
                                spec,
                                sink,
                                ..
                            } => {
                                engine
                                    .register_account(&handle, &spec, &sink)
                                    .await
                                    .map(BrowserResult::RegistrationDone)
                            }
                            BrowserCommand::ExportCookies => {
                                // 基础支持
                                Ok(BrowserResult::Cookies(Vec::new()))
                            }
                            BrowserCommand::CloseSession { handle } => {
                                let _ = engine.close_session(&handle).await;
                                Ok(BrowserResult::Closed)
                            }
                        };

                        let _ = reply_tx.send(res);
                    }
                });
            })
            .expect("启动浏览器专属 OS 线程失败");

        Self { slot_index, cmd_tx }
    }
}

#[async_trait]
impl BrowserExecutor for ThreadBrowserExecutor {
    async fn execute(&self, command: BrowserCommand) -> Result<BrowserResult, AutomationError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send((command, reply_tx))
            .await
            .map_err(|_| {
                AutomationError::new(
                    FailureClass::Fatal,
                    format!("槽位 {} 的浏览器 OS 线程已停止", self.slot_index),
                )
            })?;

        match reply_rx.await {
            Ok(result) => result,
            Err(_) => Err(AutomationError::new(
                FailureClass::Fatal,
                format!("槽位 {} 浏览器线程在执行过程中异常退出或 Panic", self.slot_index),
            )),
        }
    }
}
