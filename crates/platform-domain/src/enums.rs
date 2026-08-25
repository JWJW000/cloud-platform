//! 第 11 节「中文状态与枚举字典」的唯一实现来源。
//!
//! 每个枚举都提供：
//! - `as_str()`：中文持久化值；
//! - `Display`：同上，便于日志与错误信息；
//! - `FromStr`：严格解析，未知值返回错误而不是回落到某个「正常」状态；
//! - `Serialize`/`Deserialize`：直接读写中文字符串；
//! - `ALL`：全部取值，供数据库 CHECK 约束与接口文档生成使用。
//!
//! 严格解析是刻意的：设计方案第 11.9 节要求迁移遇到未知值必须中断并输出记录，
//! 静默回落会把脏数据伪装成正常状态。

use std::fmt;
use std::str::FromStr;

/// 中文枚举值解析失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("未知的{type_name}取值：`{value}`，合法取值为 {allowed}")]
pub struct EnumParseError {
    /// 枚举的业务名称，例如「账号状态」。
    pub type_name: &'static str,
    /// 解析失败的原始输入。
    pub value: String,
    /// 合法取值列表，以 `、` 连接。
    pub allowed: String,
}

/// 生成一个「中文值 <-> Rust 变体」双向映射的枚举。
macro_rules! chinese_enum {
    (
        $(#[$outer:meta])*
        $name:ident ($type_name:literal) {
            $( $(#[$inner:meta])* $variant:ident => $value:literal ),+ $(,)?
        }
    ) => {
        $(#[$outer])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name {
            $( $(#[$inner])* $variant, )+
        }

        impl $name {
            /// 业务名称，用于错误信息与接口文档。
            pub const TYPE_NAME: &'static str = $type_name;

            /// 全部合法取值，顺序与声明顺序一致（界面下拉框可直接使用）。
            pub const ALL: &'static [Self] = &[ $( Self::$variant, )+ ];

            /// 中文持久化值。
            pub const fn as_str(self) -> &'static str {
                match self {
                    $( Self::$variant => $value, )+
                }
            }

            /// 全部中文取值，供数据库 CHECK 约束生成。
            pub fn all_values() -> Vec<&'static str> {
                Self::ALL.iter().map(|item| item.as_str()).collect()
            }

            /// 生成 PostgreSQL `CHECK (col IN (...))` 的取值片段。
            pub fn sql_in_list() -> String {
                Self::all_values()
                    .into_iter()
                    .map(|value| format!("'{value}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = EnumParseError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $( $value => Ok(Self::$variant), )+
                    other => Err(EnumParseError {
                        type_name: $type_name,
                        value: other.to_string(),
                        allowed: Self::all_values().join("、"),
                    }),
                }
            }
        }

        impl TryFrom<String> for $name {
            // 使用完整类型名而非 `Self::Error`：部分枚举自身带有 `Error` 变体，
            // 关联项与变体同名会导致解析歧义。
            type Error = $crate::enums::EnumParseError;

            fn try_from(value: String) -> Result<Self, $crate::enums::EnumParseError> {
                value.parse()
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.as_str().to_string()
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                raw.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

chinese_enum! {
    /// 第 11.1 节：账号状态。
    AccountStatus("账号状态") {
        /// 已导入，尚未注册。
        PendingRegistration => "待注册",
        /// 可用于登录和下载。
        Registered => "已注册",
        /// 等待邮箱验证码或确认。
        VerificationPending => "待验证",
        /// 凭据错误或账号不可登录。
        LoginFailed => "登录失败",
        /// 当日账号额度已用完。
        ExhaustedToday => "今日额度耗尽",
        /// 管理员或系统永久停用。
        Disabled => "已禁用",
    }
}

chinese_enum! {
    /// 第 11.2 节：图书任务状态。
    TaskStatus("图书任务状态") {
        /// 可被调度。
        Pending => "待处理",
        /// 已创建租约，Worker 尚未开始。
        Claimed => "已分配",
        /// 正在搜索或下载。
        Running => "执行中",
        /// 已下载到本机，等待写入 NAS。
        AwaitingIngest => "等待入库",
        /// Worker 失联或结果不确定，需核查 NAS。
        NeedsConfirm => "待确认",
        /// NAS 文件已校验并提交。
        Completed => "已完成",
        /// 达到重试上限或不可恢复错误。
        Failed => "失败",
        /// 站点未收录或无目标格式。
        Skipped => "已跳过",
        /// 管理员取消。
        Cancelled => "已取消",
    }
}

chinese_enum! {
    /// 第 11.3 节：批次状态。
    BatchStatus("批次状态") {
        /// 已导入但尚未执行。
        NotStarted => "待开始",
        /// 可被调度器选择。
        Running => "执行中",
        /// 暂停分配新任务。
        Paused => "已暂停",
        /// 批次内任务均为终态。
        Completed => "已完成",
        /// 管理员取消该批次。
        Cancelled => "已取消",
    }
}

chinese_enum! {
    /// 第 11.4 节：Worker 状态。
    WorkerStatus("Worker状态") {
        /// 首次接入，等待管理员批准。
        PendingApproval => "待审核",
        /// 已连接且可以领取任务。
        Online => "在线",
        /// 无空闲槽位。
        Busy => "忙碌",
        /// 管理员暂停领取任务。
        Paused => "已暂停",
        /// NAS 或本机暂存空间异常。
        StorageError => "存储异常",
        /// 正在诊断或升级。
        Maintenance => "维护中",
        /// gRPC 连接中断。
        Offline => "离线",
        /// 节点证书已撤销或管理员停用。
        Disabled => "已禁用",
    }
}

chinese_enum! {
    /// 第 11.5 节：槽位状态。
    SlotStatus("槽位状态") {
        /// 可创建会话。
        Idle => "空闲",
        /// Master 已分配资源。
        Reserved => "已预留",
        /// 正在启动代理和浏览器。
        Starting => "启动中",
        /// 正在执行图书任务。
        Running => "执行中",
        /// 正在上传、清理或结束会话。
        Finishing => "收尾中",
        /// 浏览器或本机资源异常。
        Error => "异常",
        /// 管理员关闭该槽位。
        Deactivated => "已停用",
    }
}

chinese_enum! {
    /// 第 11.6 节：执行会话状态。
    SessionStatus("执行会话状态") {
        /// 已分配账号和代理，Worker 正在准备。
        Creating => "创建中",
        /// 可以连续领取图书任务。
        Running => "运行中",
        /// Worker 失联，暂不释放资源。
        Protected => "断线保护",
        /// 当前任务完成后退出。
        Draining => "正在结束",
        /// 正常结束。
        Ended => "已结束",
        /// 异常结束。
        Failed => "失败",
    }
}

chinese_enum! {
    /// 第 11.7 节：代理状态。
    ProxyStatus("代理状态") {
        /// 健康且未占用。
        Available => "可用",
        /// 已绑定执行会话。
        Occupied => "已占用",
        /// 疑似 IP 限流，暂不分配。
        CoolingDown => "冷却中",
        /// 连通性或出口检查失败。
        Error => "异常",
        /// 管理员停用。
        Disabled => "已停用",
    }
}

chinese_enum! {
    /// 第 11.8 节：任务类型。
    TaskType("任务类型") {
        /// 账号注册。
        AccountRegister => "账号注册",
        /// 图书下载。
        BookDownload => "图书下载",
        /// NAS 文件核验。
        NasVerify => "NAS核验",
        /// 代理连通性检测。
        ProxyCheck => "代理检测",
    }
}

chinese_enum! {
    /// 第 11.8 节：执行结果。
    ExecutionResult("执行结果") {
        /// 成功。
        Success => "成功",
        /// 可重试失败。
        RetryableFailure => "可重试失败",
        /// 不可重试失败。
        FatalFailure => "不可重试失败",
        /// 跳过。
        Skipped => "跳过",
        /// 取消。
        Cancelled => "取消",
        /// 结果不确定，需要核验。
        Uncertain => "结果不确定",
    }
}

chinese_enum! {
    /// 第 11.8 节：日志级别。
    LogLevel("日志级别") {
        /// 调试。
        Debug => "调试",
        /// 信息。
        Info => "信息",
        /// 警告。
        Warn => "警告",
        /// 错误。
        Error => "错误",
    }
}

chinese_enum! {
    /// 第 11.8 节：操作来源。
    OperationSource("操作来源") {
        /// 管理员在管理后台操作。
        Admin => "管理员",
        /// Master 调度器自动操作。
        Scheduler => "调度器",
        /// Worker 上报触发。
        Worker => "工作节点",
        /// Master 定时系统任务。
        SystemJob => "系统任务",
    }
}

chinese_enum! {
    /// 第 8.2 节：图书去重核验状态。
    VerifyStatus("图书核验状态") {
        /// 依据强唯一键（ISBN）自动确认。
        Confirmed => "已确认",
        /// 仅按书名归并，需人工确认是否为同一本书。
        NeedsConfirm => "待确认",
        /// 管理员已手工合并。
        Merged => "已合并",
    }
}

chinese_enum! {
    /// 告警级别，用于第 17 节告警。
    AlertLevel("告警级别") {
        /// 提示。
        Notice => "提示",
        /// 警告。
        Warn => "警告",
        /// 严重。
        Critical => "严重",
    }
}

chinese_enum! {
    /// 账号注册任务状态（V6 方案）。
    AccountRegistrationTaskStatus("账号注册任务状态") {
        /// 可被调度。
        Pending => "待处理",
        /// 已分配执行租约。
        Claimed => "已分配",
        /// 正在执行注册。
        Running => "执行中",
        /// 等待人工验证码或确认。
        AwaitingManualConfirm => "等待人工确认",
        /// 正在重试。
        Retrying => "正在重试",
        /// 注册成功完成。
        Completed => "已完成",
        /// 注册失败。
        Failed => "失败",
        /// 管理员取消。
        Cancelled => "已取消",
    }
}

chinese_enum! {
    /// 导入任务类型（V6 方案）。
    ImportType("导入类型") {
        /// 图书 CSV 导入。
        Books => "图书",
        /// 账号文件导入。
        Accounts => "账号",
    }
}

chinese_enum! {
    /// 导入任务状态（V6 方案）。
    ImportStatus("导入状态") {
        /// 正在预检解析中。
        Previewing => "预检中",
        /// 预检完成，等待管理员确认提交。
        PendingConfirm => "待确认",
        /// 已提交并完成创建。
        Committed => "已提交",
        /// 超时未确认已过期。
        Expired => "已过期",
        /// 预检或提交失败。
        Failed => "失败",
    }
}

chinese_enum! {
    /// 账号导入模式（V6 方案）。
    AccountImportMode("账号导入模式") {
        /// 待注册账号。
        PendingRegistration => "待注册",
        /// 已注册账号。
        Registered => "已注册",
    }
}

chinese_enum! {
    /// 人工确认类型（V6 方案）。
    ManualActionType("人工确认类型") {
        /// 邮箱验证码。
        MailCode => "邮箱验证码",
        /// 图片验证码。
        ImageCaptcha => "图片验证码",
        /// 人工确认。
        ManualConfirm => "人工确认",
        /// 风控处理。
        RiskControl => "风控",
    }
}

chinese_enum! {
    /// 人工确认状态（V6 方案）。
    ManualActionStatus("人工确认状态") {
        /// 待处理。
        Pending => "待处理",
        /// 已解决（输入验证码并继续）。
        Resolved => "已解决",
        /// 已过期。
        Expired => "已过期",
        /// 已取消。
        Cancelled => "已取消",
    }
}

chinese_enum! {
    /// 远程命令状态（V6 方案）。
    WorkerCommandStatus("节点命令状态") {
        /// 待下发。
        Pending => "待下发",
        /// 已下发。
        Sent => "已下发",
        /// 已接收。
        Accepted => "已接收",
        /// 执行中。
        Running => "执行中",
        /// 已完成。
        Completed => "已完成",
        /// 失败。
        Failed => "失败",
        /// 已过期。
        Expired => "已过期",
        /// 已取消。
        Cancelled => "已取消",
    }
}

impl TaskStatus {
    /// 是否为终态：终态任务不再被调度，也不允许被回退。
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Skipped | Self::Cancelled
        )
    }

    /// 是否占用调度资源（持有租约或正在执行）。
    pub const fn is_active(self) -> bool {
        matches!(
            self,
            Self::Claimed | Self::Running | Self::AwaitingIngest | Self::NeedsConfirm
        )
    }
}

impl AccountRegistrationTaskStatus {
    /// 是否为终态。
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// 是否占用调度资源。
    pub const fn is_active(self) -> bool {
        matches!(
            self,
            Self::Claimed | Self::Running | Self::AwaitingManualConfirm | Self::Retrying
        )
    }
}

impl BatchStatus {
    /// 调度器是否可以从该批次取任务（第 7.2 节：只选择「执行中」的批次）。
    pub const fn is_schedulable(self) -> bool {
        matches!(self, Self::Running)
    }
}

impl WorkerStatus {
    /// 节点是否允许领取新任务。
    pub const fn can_accept_work(self) -> bool {
        matches!(self, Self::Online)
    }
}

chinese_enum! {
    /// 图书馆总库：作品类型。
    WorkType("作品类型") {
        Book => "整书",
        Chapter => "章节",
        Paper => "论文",
        Collection => "合集",
        Other => "其他",
    }
}

chinese_enum! {
    /// 图书馆总库：规范书目消歧与合并状态。
    ResolutionStatus("消歧状态") {
        Confirmed => "已确认",
        Ambiguous => "待消歧",
        Merged => "已合并",
        Split => "已拆分",
        Ignored => "已忽略",
    }
}

chinese_enum! {
    /// 图书馆总库：全局获取状态（设计方案第 6.1 节）。
    AcquisitionStatus("获取状态") {
        Pending => "待下载",
        Queued => "排队中",
        Claimed => "已领取",
        Downloading => "下载中",
        Verifying => "校验中",
        Acquired => "已下载",
        RetryableFailure => "暂时失败",
        SourceInvalid => "来源无效",
        NeedsConfirm => "人工确认",
        Excluded => "暂不获取",
    }
}

chinese_enum! {
    /// 图书馆总库：数据导入运行状态。
    ImportRunStatus("导入运行状态") {
        Preparing => "准备中",
        Running => "运行中",
        Paused => "已暂停",
        Completed => "已完成",
        PartiallyFailed => "部分失败",
        Failed => "失败",
    }
}

chinese_enum! {
    /// 图书馆总库：馆藏文件校验状态。
    CatalogFileVerifyStatus("馆藏文件状态") {
        Pending => "待校验",
        Valid => "有效",
        Corrupt => "损坏",
        Missing => "丢失",
    }
}

chinese_enum! {
    /// 图书馆总库：存储后端。
    StorageBackend("存储后端") {
        Nas => "NAS",
        S3 => "S3",
        Oss => "OSS",
        Local => "Local",
    }
}

chinese_enum! {
    /// 图书馆总库：贡献者角色。
    ContributorRole("贡献者角色") {
        Author => "作者",
        Translator => "译者",
        Editor => "编者",
        Other => "其他",
    }
}

chinese_enum! {
    /// 图书馆总库：主题分类类型。
    SubjectType("主题类型") {
        Clc => "中图分类号",
        Subject => "主题词",
        Keyword => "关键词",
        Category => "分类",
    }
}

chinese_enum! {
    /// 图书馆总库：标识符类型。
    IdentifierType("标识符类型") {
        Isbn13 => "isbn13",
        Isbn10 => "isbn10",
        Doi => "doi",
        ExternalId => "external_id",
        DamsCode => "dams_code",
        Custom => "custom",
    }
}

chinese_enum! {
    /// 图书馆总库：来源文件候选状态。
    SourceAssetStatus("来源资产状态") {
        Available => "可用",
        Unavailable => "不可用",
        Corrupted => "已损坏",
        Unknown => "未知",
    }
}

impl Default for WorkType {
    fn default() -> Self {
        Self::Book
    }
}

impl Default for ResolutionStatus {
    fn default() -> Self {
        Self::Confirmed
    }
}

impl Default for AcquisitionStatus {
    fn default() -> Self {
        Self::Pending
    }
}

impl AcquisitionStatus {
    /// 是否为终态。
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Acquired | Self::SourceInvalid | Self::Excluded
        )
    }

    /// 是否占用调度资源。
    pub const fn is_active(self) -> bool {
        matches!(
            self,
            Self::Claimed | Self::Downloading | Self::Verifying
        )
    }

    /// 是否可被全局调度池领取。
    pub const fn is_claimable(self) -> bool {
        matches!(
            self,
            Self::Pending | Self::Queued | Self::RetryableFailure
        )
    }
}

impl SessionStatus {
    /// 会话是否仍然持有账号与代理租约。
    pub const fn holds_lease(self) -> bool {
        matches!(
            self,
            Self::Creating | Self::Running | Self::Protected | Self::Draining
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_chinese_values() {
        assert_eq!(AccountStatus::Registered.as_str(), "已注册");
        assert_eq!(TaskStatus::AwaitingIngest.as_str(), "等待入库");
        assert_eq!(WorkerStatus::StorageError.to_string(), "存储异常");
    }

    #[test]
    fn parses_chinese_values() {
        assert_eq!(
            "今日额度耗尽".parse::<AccountStatus>().unwrap(),
            AccountStatus::ExhaustedToday
        );
        assert_eq!(
            "结果不确定".parse::<ExecutionResult>().unwrap(),
            ExecutionResult::Uncertain
        );
    }

    #[test]
    fn rejects_english_and_unknown_values() {
        // 设计方案禁止持久化英文状态：解析必须失败，而不是回落到默认值。
        let err = "registered".parse::<AccountStatus>().unwrap_err();
        assert_eq!(err.type_name, "账号状态");
        assert!(err.allowed.contains("已注册"));
        assert!("莫名状态".parse::<TaskStatus>().is_err());
    }

    #[test]
    fn json_round_trip_uses_chinese() {
        let json = serde_json::to_string(&TaskStatus::Completed).unwrap();
        assert_eq!(json, "\"已完成\"");
        let back: TaskStatus = serde_json::from_str("\"待确认\"").unwrap();
        assert_eq!(back, TaskStatus::NeedsConfirm);
        assert!(serde_json::from_str::<TaskStatus>("\"succeeded\"").is_err());
    }

    #[test]
    fn sql_in_list_covers_every_variant() {
        let list = SlotStatus::sql_in_list();
        for status in SlotStatus::ALL {
            assert!(list.contains(status.as_str()), "缺少 {status}");
        }
    }

    #[test]
    fn terminal_and_active_classification() {
        assert!(TaskStatus::Cancelled.is_terminal());
        assert!(!TaskStatus::AwaitingIngest.is_terminal());
        assert!(TaskStatus::NeedsConfirm.is_active());
        assert!(!TaskStatus::Pending.is_active());
    }
}
