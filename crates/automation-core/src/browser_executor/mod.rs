//! 浏览器命令执行隔离深模块（方案第 5.2 节）。
//!
//! 每个槽位分配一个专有 OS 线程，所有与 `ChromiumPage` 相关的同步创建、
//! 导航、DOM 查询、CDP 交互、Cookie 导出以及关闭操作完全隔离在该线程中。
//!
//! Tokio 控制面协程只通过有界通道投递命令并异步等待结果，从而彻底解除
//! 同步操作对 Tokio 工作线程的阻塞，保障 gRPC 心跳和网络 I/O 稳定。

use async_trait::async_trait;
use rust_drission::Cookie;

use crate::cancel::CancelToken;
use crate::engine::EventSink;
use crate::types::{
    AutomationError, DownloadOutcome, DownloadSpec, RegistrationOutcome, RegistrationSpec,
    SessionHandle, SessionSpec,
};

pub mod mock;
pub mod thread;

pub use mock::MockBrowserExecutor;
pub use thread::ThreadBrowserExecutor;

/// 浏览器执行命令。
pub enum BrowserCommand {
    /// 打开并初始化浏览器会话。
    OpenSession { spec: SessionSpec },
    /// 在已打开会话中执行一本书的下载。
    DownloadBook {
        handle: SessionHandle,
        spec: DownloadSpec,
        sink: EventSink,
        cancel: CancelToken,
    },
    /// 在已打开会话中执行账号注册。
    RegisterAccount {
        handle: SessionHandle,
        spec: RegistrationSpec,
        sink: EventSink,
        cancel: CancelToken,
    },
    /// 导出当前 Cookie。
    ExportCookies,
    /// 关闭指定会话并退出浏览器。
    CloseSession { handle: SessionHandle },
    /// 关闭本槽位持有的全部浏览器会话。
    CloseAllSessions,
}

/// 浏览器执行结果。
pub enum BrowserResult {
    SessionOpened(SessionHandle),
    DownloadDone(Box<DownloadOutcome>),
    RegistrationDone(RegistrationOutcome),
    Cookies(Vec<Cookie>),
    Closed,
}

/// 浏览器执行器接口。
#[async_trait]
pub trait BrowserExecutor: Send + Sync {
    /// 异步提交命令到专属 OS 线程并等待结果。
    async fn execute(&self, command: BrowserCommand) -> Result<BrowserResult, AutomationError>;
}
