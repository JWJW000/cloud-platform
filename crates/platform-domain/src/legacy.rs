//! 第 11.9 节：旧 SQLite 英文状态 → 新中文状态的显式映射。
//!
//! 关键约束：**未知值必须阻止迁移**，不允许默认映射为某个正常状态。
//! 因此这里返回 `Result`，错误里带上具体记录标识，供迁移脚本打印后中断。

use crate::enums::{AccountStatus, TaskStatus};

/// 迁移遇到无法映射的旧值。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("记录 {record} 的{field}存在未知旧值 `{value}`，迁移已中止，请人工确认后再重跑")]
pub struct LegacyMigrationError {
    /// 出错记录的定位信息，例如 `accounts.id=42`。
    pub record: String,
    /// 字段业务名，例如「账号状态」。
    pub field: &'static str,
    /// 无法映射的旧值。
    pub value: String,
}

/// 旧账号状态映射。`record` 用于错误定位，例如 `accounts.id=42`。
pub fn migrate_account_status(
    legacy: &str,
    record: &str,
) -> Result<AccountStatus, LegacyMigrationError> {
    let mapped = match legacy.trim() {
        "pending_registration" => AccountStatus::PendingRegistration,
        "registered" => AccountStatus::Registered,
        "verification_pending" => AccountStatus::VerificationPending,
        "login_failed" => AccountStatus::LoginFailed,
        "exhausted_today" => AccountStatus::ExhaustedToday,
        "disabled" => AccountStatus::Disabled,
        // 已经是中文值时原样接受，便于迁移脚本重跑（幂等）
        other => {
            return other.parse().map_err(|_| LegacyMigrationError {
                record: record.to_string(),
                field: AccountStatus::TYPE_NAME,
                value: other.to_string(),
            });
        }
    };
    Ok(mapped)
}

/// 旧任务状态映射。
pub fn migrate_task_status(legacy: &str, record: &str) -> Result<TaskStatus, LegacyMigrationError> {
    let mapped = match legacy.trim() {
        "pending" => TaskStatus::Pending,
        "claimed" => TaskStatus::Claimed,
        "running" => TaskStatus::Running,
        "succeeded" => TaskStatus::Completed,
        "failed" => TaskStatus::Failed,
        "skipped" => TaskStatus::Skipped,
        other => {
            return other.parse().map_err(|_| LegacyMigrationError {
                record: record.to_string(),
                field: TaskStatus::TYPE_NAME,
                value: other.to_string(),
            });
        }
    };
    Ok(mapped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_documented_legacy_value() {
        let account_cases = [
            ("pending_registration", AccountStatus::PendingRegistration),
            ("registered", AccountStatus::Registered),
            ("verification_pending", AccountStatus::VerificationPending),
            ("login_failed", AccountStatus::LoginFailed),
            ("exhausted_today", AccountStatus::ExhaustedToday),
            ("disabled", AccountStatus::Disabled),
        ];
        for (legacy, expected) in account_cases {
            assert_eq!(
                migrate_account_status(legacy, "accounts.id=1").unwrap(),
                expected
            );
        }

        let task_cases = [
            ("pending", TaskStatus::Pending),
            ("claimed", TaskStatus::Claimed),
            ("running", TaskStatus::Running),
            ("succeeded", TaskStatus::Completed),
            ("failed", TaskStatus::Failed),
            ("skipped", TaskStatus::Skipped),
        ];
        for (legacy, expected) in task_cases {
            assert_eq!(migrate_task_status(legacy, "tasks.id=1").unwrap(), expected);
        }
    }

    #[test]
    fn unknown_value_aborts_with_record_id() {
        let err = migrate_task_status("weird_state", "download_tasks.id=99").unwrap_err();
        assert_eq!(err.record, "download_tasks.id=99");
        assert_eq!(err.value, "weird_state");
        assert!(err.to_string().contains("迁移已中止"));
    }

    #[test]
    fn rerun_is_idempotent_for_already_migrated_rows() {
        assert_eq!(
            migrate_account_status("已注册", "accounts.id=7").unwrap(),
            AccountStatus::Registered
        );
        assert_eq!(
            migrate_task_status("已完成", "tasks.id=7").unwrap(),
            TaskStatus::Completed
        );
    }
}
