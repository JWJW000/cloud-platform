//! 引擎接口。

use std::path::Path;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::cancel::CancelToken;
use crate::types::{
    AutomationError, AutomationEvent, DownloadOutcome, DownloadSpec, RegistrationOutcome,
    RegistrationSpec, SessionHandle, SessionSpec,
};

/// 事件出口：引擎把过程事件推给 Worker，Worker 负责节流后上报 Master。
///
/// 事件丢弃是可接受的（进度事件本身允许节流），因此使用 `try_send` 而不是阻塞发送。
#[derive(Debug, Clone)]
pub struct EventSink {
    sender: mpsc::Sender<AutomationEvent>,
}

impl EventSink {
    /// 由一个 mpsc 发送端构造。
    pub fn new(sender: mpsc::Sender<AutomationEvent>) -> Self {
        Self { sender }
    }

    /// 推送一个事件；通道已满或已关闭时静默丢弃。
    pub fn emit(&self, event: AutomationEvent) {
        let _ = self.sender.try_send(event);
    }

    /// 推送一个中文阶段变化。
    pub fn stage(&self, stage: impl Into<String>) {
        self.emit(AutomationEvent::Stage {
            stage: stage.into(),
        });
    }

    /// 推送下载进度。
    pub fn progress(&self, downloaded_bytes: u64, total_bytes: u64) {
        self.emit(AutomationEvent::Progress {
            downloaded_bytes,
            total_bytes,
        });
    }

    /// 推送一条中文级别日志。
    pub fn log(&self, level: impl Into<String>, message: impl Into<String>) {
        self.emit(AutomationEvent::Log {
            level: level.into(),
            message: message.into(),
        });
    }

    /// 创建一个丢弃所有事件的出口（测试与「不关心进度」的调用方使用）。
    pub fn discarding() -> Self {
        let (sender, receiver) = mpsc::channel(1);
        // 立即丢弃接收端：`emit` 使用 try_send，通道关闭时静默丢弃事件。
        drop(receiver);
        Self::new(sender)
    }
}

/// 浏览器自动化引擎。
///
/// 实现者只负责「站点怎么操作」；「做哪一本书」「结果算什么状态」由平台决定。
#[async_trait]
pub trait AutomationEngine: Send + Sync {
    /// 引擎名称，写入执行记录便于排查。
    fn name(&self) -> &'static str;

    /// 打开一个执行会话：启动浏览器、应用固定代理、登录账号。
    async fn open_session(&self, spec: &SessionSpec) -> Result<SessionHandle, AutomationError>;

    /// 把浏览器的下载目录切换到**本任务独占**的目录（第 8.2 节）。
    ///
    /// 这一步必须在点击下载**之前**完成。否则文件落到公共 staging 根目录，
    /// 任务只能靠「目录里最新的那个文件」去猜归属——多槽位并发时这个猜测一定会错。
    ///
    /// 调用方负责保证目录已存在且只属于当前任务。
    async fn set_task_download_dir(
        &self,
        session: &SessionHandle,
        dir: &Path,
    ) -> Result<(), AutomationError>;

    /// 在会话内下载一本书。
    ///
    /// `cancel` 必须在所有等待点被检查（第 10.1 节）：等页面、等搜索结果、
    /// 等下载完成、等文件大小稳定。忽略它就等于「取消命令只记录日志但不停止执行」，
    /// 而这是 V2 第 18 节明确禁止的。
    async fn download_book(
        &self,
        session: &SessionHandle,
        spec: &DownloadSpec,
        events: &EventSink,
        cancel: &CancelToken,
    ) -> Result<DownloadOutcome, AutomationError>;

    /// 在会话内注册一个账号。
    async fn register_account(
        &self,
        session: &SessionHandle,
        spec: &RegistrationSpec,
        events: &EventSink,
    ) -> Result<RegistrationOutcome, AutomationError>;

    /// 在仍打开的注册页上提交邮箱验证码（人工降级后浏览器必须还在）。
    async fn submit_verification_code(
        &self,
        session: &SessionHandle,
        code: &str,
        events: &EventSink,
        cancel: &CancelToken,
    ) -> Result<RegistrationOutcome, AutomationError>;

    /// 读取站点配额指示器（`.caret-scroll__title`，形如 `7/10`）。
    ///
    /// 返回 `None` 表示读不到指示器，此时**不得**把账号标记为额度耗尽。
    async fn read_quota_indicator(
        &self,
        session: &SessionHandle,
    ) -> Result<Option<(u32, u32)>, AutomationError>;

    /// 结束会话：关闭浏览器并清理 Profile。
    async fn close_session(&self, session: &SessionHandle) -> Result<(), AutomationError>;
}
