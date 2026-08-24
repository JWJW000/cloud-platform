//! 调度核心（第 7.2、10.3、14 节）。
//!
//! 这一层是整个平台唯一允许「跨表推进状态」的地方。它与 [`crate::store`] 的分工是：
//! store 负责「把一行读出来/写回去」，scheduler 负责「在一个事务里让若干行同时改变」。
//! 拆分的理由很实际——领一个任务要同时动任务、会话、槽位、账号、代理五张表，
//! 任何一处单独提交都会留下一个「账号被占用但没有会话」之类无人回收的中间态。
//!
//! 模块划分：
//! - [`allocate`]：为某个节点的空闲槽位原子地凑出「槽位 + 账号 + 代理」并开会话；
//! - [`claim`]：第 7.2 节的按批次优先级领书，`FOR UPDATE SKIP LOCKED`；
//! - [`submit`]：结果上报，含第 10.3 节归因与第 14.4 / 14.5 节迟到事件判定；
//! - [`reaper`]：租约回收、离线判定、批次收尾、额度重置等周期性自愈。
//!
//! 共同的设计前提：**Master 不相信任何上报的时序**。Worker 会重连、会重放、会在
//! 网络恢复后把十分钟前的结果送上来，因此每条写入路径都带一个「这次上报还算不算数」
//! 的判定（租约编号 + 阶段版本），判定不通过的事件只留档、不改状态。

pub mod allocate;
pub mod claim;
pub mod reaper;
pub mod select_work;
pub mod submit;

pub use allocate::{
    allocate_session, AccountCredential, AllocationOutcome, ProxyCredential, SessionGrant,
    Unavailable,
};
pub use claim::{claim_next_task, BookTarget, ClaimOutcome, TaskAssignment};
pub use reaper::{reap_once, resume_protected_sessions, spawn_reaper, ReapReport};
pub use select_work::{handle_work_request, trigger_scheduler_sweep};
pub use submit::{
    accept_registration_task, accept_task, applicability, decide_task, nas_check_result,
    proxy_check_result, record_progress, record_registration_progress, session_closed,
    slot_status_report, submit_registration_result, submit_result, Applicability, FileEvidence,
    NasCheckReport, RegistrationResultReport, ReportFacts, ResultReport, SubmitOutcome,
    TaskDecision,
};
