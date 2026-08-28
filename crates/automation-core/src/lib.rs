//! `automation-core`：与任务来源解耦的浏览器自动化核心（设计方案第 5.1 节）。
//!
//! 本 crate **不决定任务来源，也不决定任务最终状态**：它只接收一个已经准备好的
//! 会话规格（账号 + 本机固定代理端口 + Profile 目录），执行一次具体的自动化动作，
//! 并返回结构化结果与失败分类。任务领取、租约、状态机与 NAS 入库都由
//! `master-server` 与 `worker-agent` 负责。
//!
//! 这样 Worker 侧可以在不改动业务规则的前提下替换执行引擎：
//! - [`SimulatedEngine`]：不启动浏览器，用于平台自身的端到端验证与压力测试；
//! - [`RealAutomationEngine`]：接入 `rust_drission`，启动 Chromium 执行真实站点自动化流程。

pub mod browser;
pub mod cancel;
pub mod engine;
pub mod http_download;
pub mod mail_code;
pub mod matching;
pub mod real;
pub mod simulated;
pub mod site;
pub mod types;
pub mod verify;

pub use cancel::CancelToken;
pub use engine::{AutomationEngine, EventSink};
pub use mail_code::{MailCodeCursor, MailCodeError, MailCodeProvider, MailCodeResult};
pub use matching::{select_candidate, CandidateBook, MatchBasis, MatchOutcome};
pub use real::RealAutomationEngine;
pub use simulated::{SimulatedEngine, SimulationScript};
pub use types::{
    AccountCredential, AutomationError, AutomationEvent, BookTarget, DownloadOutcome, DownloadSpec,
    RegistrationOutcome, RegistrationSpec, SessionHandle, SessionSpec,
};
pub use verify::{verify_downloaded_file, FileEvidence, VerifyError};
