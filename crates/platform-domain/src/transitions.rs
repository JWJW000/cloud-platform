//! 状态机约束。
//!
//! 第 14.5 节要求：重复或乱序的 gRPC 事件只能做审计，**不得回退任务状态**。
//! 因此终态一律不可离开，且任何状态更新都必须先通过这里的校验。

use crate::enums::{BatchStatus, SessionStatus, SlotStatus, TaskStatus, WorkerStatus};

/// 非法状态迁移。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("不允许的{type_name}迁移：{from} → {to}")]
pub struct TransitionError {
    /// 枚举业务名。
    pub type_name: &'static str,
    /// 原状态。
    pub from: &'static str,
    /// 目标状态。
    pub to: &'static str,
}

impl TaskStatus {
    /// 图书任务允许的迁移。
    pub const fn can_transition_to(self, to: Self) -> bool {
        use TaskStatus::*;
        match (self, to) {
            // 终态不可离开：迟到事件只写审计
            (Completed | Failed | Skipped | Cancelled, _) => false,
            // 管理员随时可取消未终态任务
            (_, Cancelled) => true,
            (Pending, Claimed) => true,
            (Claimed, Running | Pending | NeedsConfirm) => true,
            (Running, AwaitingIngest | NeedsConfirm | Completed | Failed | Skipped | Pending) => {
                true
            }
            (AwaitingIngest, Completed | NeedsConfirm | Failed) => true,
            // 核验后可以补记完成，也可以重新排队
            (NeedsConfirm, Completed | Pending | Failed) => true,
            _ => false,
        }
    }

    /// 校验迁移，非法时返回错误。
    pub fn ensure_transition(self, to: Self) -> Result<(), TransitionError> {
        if self.can_transition_to(to) {
            Ok(())
        } else {
            Err(TransitionError {
                type_name: Self::TYPE_NAME,
                from: self.as_str(),
                to: to.as_str(),
            })
        }
    }
}

impl BatchStatus {
    /// 批次允许的迁移（第 16.3 节：开始、暂停、恢复、取消）。
    pub const fn can_transition_to(self, to: Self) -> bool {
        use BatchStatus::*;
        match (self, to) {
            (Completed | Cancelled, _) => false,
            (NotStarted, Running | Cancelled) => true,
            (Running, Paused | Completed | Cancelled) => true,
            (Paused, Running | Cancelled | Completed) => true,
            _ => false,
        }
    }

    /// 校验迁移。
    pub fn ensure_transition(self, to: Self) -> Result<(), TransitionError> {
        if self.can_transition_to(to) {
            Ok(())
        } else {
            Err(TransitionError {
                type_name: Self::TYPE_NAME,
                from: self.as_str(),
                to: to.as_str(),
            })
        }
    }
}

impl SessionStatus {
    /// 执行会话允许的迁移（第 6.2 节生命周期）。
    pub const fn can_transition_to(self, to: Self) -> bool {
        use SessionStatus::*;
        match (self, to) {
            (Ended | Failed, _) => false,
            (Creating, Running | Failed | Ended | Protected) => true,
            (Running, Draining | Protected | Ended | Failed) => true,
            // 断线保护到期后异常结束；重连成功则回到运行中
            (Protected, Running | Draining | Ended | Failed) => true,
            (Draining, Ended | Failed) => true,
            _ => false,
        }
    }

    /// 校验迁移。
    pub fn ensure_transition(self, to: Self) -> Result<(), TransitionError> {
        if self.can_transition_to(to) {
            Ok(())
        } else {
            Err(TransitionError {
                type_name: Self::TYPE_NAME,
                from: self.as_str(),
                to: to.as_str(),
            })
        }
    }
}

impl SlotStatus {
    /// 槽位允许的迁移（第 11.5 节）。
    pub const fn can_transition_to(self, to: Self) -> bool {
        use SlotStatus::*;
        match (self, to) {
            // 管理员停用与恢复
            (Deactivated, Idle) => true,
            (Deactivated, _) => false,
            (_, Deactivated) => true,
            (_, Error) => true,
            (Error, Idle) => true,
            (Idle, Reserved) => true,
            (Reserved, Starting | Idle) => true,
            (Starting, Running | Finishing | Idle) => true,
            (Running, Finishing | Running) => true,
            (Finishing, Idle) => true,
            _ => false,
        }
    }

    /// 校验迁移。
    pub fn ensure_transition(self, to: Self) -> Result<(), TransitionError> {
        if self.can_transition_to(to) {
            Ok(())
        } else {
            Err(TransitionError {
                type_name: Self::TYPE_NAME,
                from: self.as_str(),
                to: to.as_str(),
            })
        }
    }
}

impl WorkerStatus {
    /// 该状态是否由管理员或云端决定，Worker 的自评不得把它解除。
    ///
    /// 管理员刚把节点设成 `维护中`、`已禁用` 或 `已暂停`，节点的下一个心跳不能把它改回
    /// `在线`——尤其是 Worker 进程重启后本地暂停标记已经丢了，它会诚实地报「我在线」，
    /// 而那恰恰是最需要拦住的一次上报。`待审核` 同理：批准是管理员动作，
    /// 节点自己说「我在线」不构成批准。
    pub const fn is_admin_governed(self) -> bool {
        matches!(
            self,
            Self::PendingApproval | Self::Maintenance | Self::Disabled | Self::Paused
        )
    }

    /// Worker 心跳允许自评的状态。
    ///
    /// 只有这四个：一切正常（`在线`）、槽位全占满（`忙碌`）、存储坏了（`存储异常`），
    /// 以及照原样回报云端下发的暂停（`已暂停`）。
    /// `离线` 不在其中——收到心跳这件事本身就证明它没离线，离线只能由 Master 的
    /// 心跳超时巡检判定；`待审核`/`维护中`/`已禁用` 属于管理员治理范围。
    pub const fn is_self_assessable(self) -> bool {
        matches!(
            self,
            Self::Online | Self::Busy | Self::Paused | Self::StorageError
        )
    }
}

/// 对 Worker 自评状态的处理结论（第 3.7 节）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusAdoption {
    /// 采纳该自评状态，写回节点行。
    Adopt(WorkerStatus),
    /// 与当前状态一致，无需写库。
    Unchanged,
    /// 当前状态由管理员或云端决定，自评一律忽略（不是错误，不必告警）。
    AdminGoverned(WorkerStatus),
    /// 自评值非法或越权，保留原状态并按中文原因告警。
    Rejected(String),
}

/// 决定是否采纳 Worker 心跳里的自评状态。
///
/// V2 第 3.7 节记录的现象是「已审核节点可能一直停留在离线」：巡检把一个卡顿过的节点
/// 判成 `离线` 之后，即使它一直在发心跳，也没有任何一条路径把它改回 `在线`——
/// 因为心跳处理完全忽略了 `node_status`。这个函数就是那条缺失的判断。
///
/// 非法值一律 [`StatusAdoption::Rejected`] 而**不是**回落到 `在线`：
/// 「不得把非法中文状态默认成正常状态」，把解析不出来的字符串当成健康，
/// 等于把一台状态未知的机器放回派活池。
pub fn adopt_reported_worker_status(current: WorkerStatus, reported: &str) -> StatusAdoption {
    let reported = reported.trim();
    if reported.is_empty() {
        // 老版本 Agent 不报自评状态。这不是错误，只是没有信息可用。
        return StatusAdoption::Unchanged;
    }
    let Ok(reported) = reported.parse::<WorkerStatus>() else {
        return StatusAdoption::Rejected(format!("无法识别的节点自评状态「{reported}」"));
    };
    if !reported.is_self_assessable() {
        return StatusAdoption::Rejected(format!("节点无权自评为「{reported}」"));
    }
    if current.is_admin_governed() {
        return StatusAdoption::AdminGoverned(current);
    }
    if current == reported {
        return StatusAdoption::Unchanged;
    }
    StatusAdoption::Adopt(reported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_task_states_never_revert() {
        for terminal in [
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Skipped,
            TaskStatus::Cancelled,
        ] {
            for target in TaskStatus::ALL {
                assert!(
                    !terminal.can_transition_to(*target),
                    "{terminal} 不应能迁移到 {target}"
                );
            }
        }
    }

    #[test]
    fn happy_path_download_flow() {
        let flow = [
            TaskStatus::Pending,
            TaskStatus::Claimed,
            TaskStatus::Running,
            TaskStatus::AwaitingIngest,
            TaskStatus::Completed,
        ];
        for window in flow.windows(2) {
            window[0].ensure_transition(window[1]).unwrap();
        }
    }

    #[test]
    fn needs_confirm_can_be_backfilled_or_requeued() {
        TaskStatus::NeedsConfirm
            .ensure_transition(TaskStatus::Completed)
            .unwrap();
        TaskStatus::NeedsConfirm
            .ensure_transition(TaskStatus::Pending)
            .unwrap();
    }

    #[test]
    fn transition_error_message_is_chinese() {
        let err = TaskStatus::Completed
            .ensure_transition(TaskStatus::Pending)
            .unwrap_err();
        assert_eq!(err.to_string(), "不允许的图书任务状态迁移：已完成 → 待处理");
    }

    #[test]
    fn batch_pause_resume_cancel() {
        BatchStatus::NotStarted
            .ensure_transition(BatchStatus::Running)
            .unwrap();
        BatchStatus::Running
            .ensure_transition(BatchStatus::Paused)
            .unwrap();
        BatchStatus::Paused
            .ensure_transition(BatchStatus::Running)
            .unwrap();
        assert!(BatchStatus::Cancelled
            .ensure_transition(BatchStatus::Running)
            .is_err());
    }

    #[test]
    fn session_protection_can_recover() {
        SessionStatus::Running
            .ensure_transition(SessionStatus::Protected)
            .unwrap();
        SessionStatus::Protected
            .ensure_transition(SessionStatus::Running)
            .unwrap();
        assert!(SessionStatus::Ended
            .ensure_transition(SessionStatus::Running)
            .is_err());
    }

    #[test]
    fn slot_lifecycle() {
        let flow = [
            SlotStatus::Idle,
            SlotStatus::Reserved,
            SlotStatus::Starting,
            SlotStatus::Running,
            SlotStatus::Finishing,
            SlotStatus::Idle,
        ];
        for window in flow.windows(2) {
            window[0].ensure_transition(window[1]).unwrap();
        }
        assert!(SlotStatus::Deactivated
            .ensure_transition(SlotStatus::Running)
            .is_err());
    }

    #[test]
    fn a_heartbeating_node_can_climb_back_out_of_offline() {
        // 第 3.7 节的核心场景：巡检误判离线后，心跳必须能把它拉回在线
        assert_eq!(
            adopt_reported_worker_status(WorkerStatus::Offline, "在线"),
            StatusAdoption::Adopt(WorkerStatus::Online)
        );
        assert_eq!(
            adopt_reported_worker_status(WorkerStatus::Online, "存储异常"),
            StatusAdoption::Adopt(WorkerStatus::StorageError)
        );
        assert_eq!(
            adopt_reported_worker_status(WorkerStatus::StorageError, "在线"),
            StatusAdoption::Adopt(WorkerStatus::Online)
        );
    }

    #[test]
    fn an_unparseable_self_assessment_never_becomes_a_healthy_status() {
        // 「不得把非法中文状态默认成正常状态」
        for bogus in ["online", "正常", "已上线", "??"] {
            match adopt_reported_worker_status(WorkerStatus::Offline, bogus) {
                StatusAdoption::Rejected(reason) => {
                    assert!(
                        reason.contains("自评状态") || reason.contains("无权"),
                        "{reason}"
                    )
                }
                StatusAdoption::Unchanged => {}
                other => panic!("非法值 {bogus} 不应被采纳：{other:?}"),
            }
        }
    }

    #[test]
    fn a_node_cannot_self_assess_into_admin_territory() {
        for forbidden in ["待审核", "维护中", "已禁用", "离线"] {
            let decision = adopt_reported_worker_status(WorkerStatus::Online, forbidden);
            assert!(
                matches!(decision, StatusAdoption::Rejected(_)),
                "{forbidden} 不该被节点自评：{decision:?}"
            );
        }
    }

    #[test]
    fn admin_governed_states_survive_every_heartbeat() {
        for governed in [
            WorkerStatus::PendingApproval,
            WorkerStatus::Maintenance,
            WorkerStatus::Disabled,
            WorkerStatus::Paused,
        ] {
            assert!(governed.is_admin_governed());
            assert_eq!(
                adopt_reported_worker_status(governed, "在线"),
                StatusAdoption::AdminGoverned(governed),
                "管理员设定的 {governed} 不能被心跳改写"
            );
        }
    }

    #[test]
    fn a_restarted_worker_cannot_un_pause_itself() {
        // Worker 重启后本地暂停标记丢了，它会诚实地报「在线」。
        // 这一次上报必须被拦住，否则管理员的暂停会被一次进程重启悄悄解除。
        assert_eq!(
            adopt_reported_worker_status(WorkerStatus::Paused, "在线"),
            StatusAdoption::AdminGoverned(WorkerStatus::Paused)
        );
        // 而照原样回报云端下发的暂停不是错误，不该产生告警
        assert_eq!(
            adopt_reported_worker_status(WorkerStatus::Paused, "已暂停"),
            StatusAdoption::AdminGoverned(WorkerStatus::Paused)
        );
    }

    #[test]
    fn an_unchanged_status_costs_no_write() {
        assert_eq!(
            adopt_reported_worker_status(WorkerStatus::Online, "在线"),
            StatusAdoption::Unchanged
        );
        // 旧版 Agent 不报自评状态时也不写库
        assert_eq!(
            adopt_reported_worker_status(WorkerStatus::Busy, ""),
            StatusAdoption::Unchanged
        );
    }

    #[test]
    fn only_online_and_busy_and_storage_error_are_ever_adopted() {
        // 心跳能改写的目标状态只有这三个：其余要么越权，要么由 Master 判定
        for status in WorkerStatus::ALL {
            let decision = adopt_reported_worker_status(WorkerStatus::Offline, status.as_str());
            match status {
                WorkerStatus::Online | WorkerStatus::Busy | WorkerStatus::StorageError => {
                    assert_eq!(decision, StatusAdoption::Adopt(*status));
                }
                // 离线 → 已暂停：暂停是云端下发的，节点自己进不去
                WorkerStatus::Paused => assert_eq!(decision, StatusAdoption::Adopt(*status)),
                WorkerStatus::Offline => assert!(matches!(decision, StatusAdoption::Rejected(_))),
                _ => assert!(
                    matches!(decision, StatusAdoption::Rejected(_)),
                    "{status} 不该被采纳：{decision:?}"
                ),
            }
        }
        // 只有「在线」能领活，因此它必须在可自评集合里，否则节点永远回不到可派活状态
        assert!(WorkerStatus::Online.is_self_assessable());
        assert!(WorkerStatus::Online.can_accept_work());
    }
}
