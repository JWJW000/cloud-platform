//! 协议技术枚举 → 中文展示值的唯一映射（V4 方案第 10.1 节）。
//!
//! 业务阶段与裁决一律用 protobuf 枚举表达（禁止自由字符串跨组件比较），
//! 需要展示为中文时统一走这里的映射，杜绝「Master 写一种、Worker 写另一种」
//! 的常量漂移（V4-03）。

use crate::v1::{ExecutionStage, ReconcileAction};

impl ExecutionStage {
    /// 技术枚举对应的中文展示值。
    pub fn display_name(self) -> &'static str {
        match self {
            ExecutionStage::Accepted => "已接受",
            ExecutionStage::Searching => "搜索中",
            ExecutionStage::Downloading => "下载中",
            ExecutionStage::LocalFileReady => "本地文件完成",
            ExecutionStage::NasUploading => "NAS 上传中",
            ExecutionStage::NasCommitted => "NAS 已原子落盘",
            ExecutionStage::ResultPending => "结果待上报",
            ExecutionStage::Unspecified => "未知阶段",
        }
    }

    /// 从 protobuf 整数值安全转换；非法值返回 [`ExecutionStage::Unspecified`]。
    pub fn from_i32_safe(value: i32) -> Self {
        match ExecutionStage::try_from(value) {
            Ok(stage) => stage,
            Err(_) => ExecutionStage::Unspecified,
        }
    }
}

impl ReconcileAction {
    /// 技术枚举对应的中文展示值。
    pub fn display_name(self) -> &'static str {
        match self {
            ReconcileAction::StopAndRetry => "停止并重试",
            ReconcileAction::ResumeIngest => "继续入库",
            ReconcileAction::VerifyNas => "核验NAS",
            ReconcileAction::ReplayResult => "重放结果",
            ReconcileAction::CleanupOnly => "停止并清理",
            ReconcileAction::Unspecified => "未知裁决",
        }
    }

    /// 从 protobuf 整数值安全转换；非法值返回 [`ReconcileAction::Unspecified`]。
    pub fn from_i32_safe(value: i32) -> Self {
        match ReconcileAction::try_from(value) {
            Ok(action) => action,
            Err(_) => ReconcileAction::Unspecified,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::v1::{ExecutionStage, ReconcileAction};

    #[test]
    fn stage_display_is_exhaustive_and_chinese() {
        assert_eq!(ExecutionStage::Accepted.display_name(), "已接受");
        assert_eq!(ExecutionStage::Searching.display_name(), "搜索中");
        assert_eq!(ExecutionStage::Downloading.display_name(), "下载中");
        assert_eq!(
            ExecutionStage::LocalFileReady.display_name(),
            "本地文件完成"
        );
        assert_eq!(ExecutionStage::NasUploading.display_name(), "NAS 上传中");
        assert_eq!(
            ExecutionStage::NasCommitted.display_name(),
            "NAS 已原子落盘"
        );
        assert_eq!(ExecutionStage::ResultPending.display_name(), "结果待上报");
    }

    #[test]
    fn action_display_is_exhaustive_and_chinese() {
        assert_eq!(ReconcileAction::StopAndRetry.display_name(), "停止并重试");
        assert_eq!(ReconcileAction::ResumeIngest.display_name(), "继续入库");
        assert_eq!(ReconcileAction::VerifyNas.display_name(), "核验NAS");
        assert_eq!(ReconcileAction::ReplayResult.display_name(), "重放结果");
        assert_eq!(ReconcileAction::CleanupOnly.display_name(), "停止并清理");
    }

    #[test]
    fn invalid_i32_maps_to_unspecified() {
        assert_eq!(
            ExecutionStage::from_i32_safe(999),
            ExecutionStage::Unspecified
        );
        assert_eq!(
            ReconcileAction::from_i32_safe(-1),
            ReconcileAction::Unspecified
        );
    }

    #[test]
    fn round_trip_through_protobuf_value() {
        assert_eq!(
            ExecutionStage::try_from(ExecutionStage::NasCommitted as i32).unwrap(),
            ExecutionStage::NasCommitted
        );
        assert_eq!(
            ReconcileAction::try_from(ReconcileAction::ResumeIngest as i32).unwrap(),
            ReconcileAction::ResumeIngest
        );
    }
}
