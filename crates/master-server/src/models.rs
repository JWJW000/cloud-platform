//! 数据模型与接口 DTO。
//!
//! 约定（第 11 节）：结构体与字段名是英文技术标识，**状态字段的值是中文**。
//! 因此这些结构体直接 `Serialize` 出去就是管理后台需要的中文数据，
//! 前端不做任何翻译，也不存在「英文状态漏到界面」的可能。
//!
//! 状态字段用 `String` 承载而不是枚举：数据库里的值已由 CHECK 约束固定，
//! 读取路径只需原样透传；需要判断语义的地方（调度、归因）再 `parse()` 成
//! `platform_domain` 的枚举，从而在决策点获得类型安全。

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// 管理员账户。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct User {
    /// 用户编号。
    pub id: Uuid,
    /// 登录名。
    pub username: String,
    /// 中文角色。
    pub role: String,
    /// 中文状态：启用 / 已禁用。
    pub status: String,
    /// 令牌世代版本号。
    pub token_version: i64,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 最近登录时间。
    pub last_login_at: Option<DateTime<Utc>>,
}

/// 管理员会话记录。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct AdminSession {
    /// 会话编号 (jti)。
    pub id: Uuid,
    /// 用户编号。
    pub user_id: Uuid,
    /// 令牌哈希。
    pub token_hash: String,
    /// 签发时间。
    pub issued_at: DateTime<Utc>,
    /// 到期时间。
    pub expires_at: DateTime<Utc>,
    /// 撤销时间。
    pub revoked_at: Option<DateTime<Utc>>,
    /// 撤销原因。
    pub revoke_reason: Option<String>,
    /// 最近活跃时间。
    pub last_seen_at: Option<DateTime<Utc>>,
    /// User-Agent 哈希。
    pub user_agent_hash: Option<String>,
    /// IP 前缀。
    pub ip_prefix: Option<String>,
}

/// Worker 节点。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct WorkerNode {
    /// 节点编号。
    pub id: Uuid,
    /// 节点名称。
    pub name: String,
    /// 主机名。
    pub hostname: String,
    /// 操作系统（技术标识：Windows / macOS / Linux）。
    pub os: String,
    /// 系统版本。
    pub os_version: String,
    /// Agent 版本。
    pub agent_version: String,
    /// 中文 Worker 状态。
    pub status: String,
    /// 管理员设定的槽位上限。
    pub max_slots: i32,
    /// 当前可用槽位。
    pub available_slots: i32,
    /// 上传并发。
    pub upload_concurrency: i32,
    /// 当前配置版本。
    pub config_version: String,
    /// 节点已应用的配置版本。
    pub applied_config_version: String,
    /// 是否开启诊断日志。
    pub diagnostics_enabled: bool,
    /// NAS 是否健康。
    pub nas_healthy: bool,
    /// NAS 剩余空间（GB）。
    pub nas_free_gb: i64,
    /// 本机暂存剩余空间（GB）。
    pub staging_free_gb: i64,
    /// CPU 占用百分比。
    pub cpu_percent: f64,
    /// 已用内存（MB）。
    pub memory_used_mb: i64,
    /// 总内存（MB）。
    pub memory_total_mb: i64,
    /// gRPC 长连接是否在线。
    pub connected: bool,
    /// 最近心跳时间。
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    /// 审核通过时间。
    pub approved_at: Option<DateTime<Utc>>,
    /// 审核操作人。
    pub approved_by: Option<Uuid>,
    /// V5 直连注册：安装实例 UUID（唯一身份）。
    pub installation_id: Option<Uuid>,
    /// V5：CSR 公钥 SHA-256 指纹。
    pub public_key_fingerprint: Option<String>,
    /// V5 中文注册状态：待审核 / 已批准 / 已拒绝 / 已过期。
    pub registration_status: String,
    /// V5：Worker 申请槽位数。
    pub requested_slots: Option<i32>,
    /// V5：管理员批准的实际槽位数。
    pub configured_slots: Option<i32>,
    /// V5：注册申请到期时间。
    pub registration_expires_at: Option<DateTime<Utc>>,
    /// V5：首次申请来源 IP。
    pub first_seen_ip: Option<String>,
    /// V5：最近注册请求时间。
    pub last_registration_at: Option<DateTime<Utc>>,
    /// V5：拒绝时间。
    pub rejected_at: Option<DateTime<Utc>>,
    /// V5：拒绝操作人。
    pub rejected_by: Option<Uuid>,
    /// V5：拒绝原因（中文）。
    pub reject_reason: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// V5 直连注册会话（库中只存令牌哈希与 CSR 公钥，不存私钥）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RegistrationSession {
    /// 会话编号。
    pub id: Uuid,
    /// 关联节点。
    pub node_id: Uuid,
    /// 会话令牌 SHA-256（唯一）。
    pub token_hash: String,
    /// CSR PEM（仅公钥侧）。
    pub csr_pem: String,
    /// CSR 公钥指纹。
    pub csr_fingerprint: String,
    /// 服务端挑战值。
    pub challenge: String,
    /// 中文状态：待审核 / 已批准 / 已拒绝 / 已过期 / 已领取。
    pub status: String,
    /// 批准后待一次性下发的节点令牌明文（领用即清空；会话令牌本身只存哈希）。
    pub pending_node_token: Option<String>,
    /// 到期时间。
    pub expires_at: DateTime<Utc>,
    /// 防暴力查询计数。
    pub attempt_count: i32,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 最近访问时间。
    pub last_seen_at: DateTime<Utc>,
}

/// 节点槽位。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct WorkerSlot {
    /// 槽位编号。
    pub id: Uuid,
    /// 所属节点。
    pub node_id: Uuid,
    /// 槽位序号。
    pub slot_index: i32,
    /// 中文槽位状态。
    pub status: String,
    /// 当前会话。
    pub session_id: Option<Uuid>,
    /// 说明文本。
    pub detail: String,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 节点证书。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct NodeCertificate {
    /// 记录编号。
    pub id: Uuid,
    /// 节点编号。
    pub node_id: Uuid,
    /// SHA-256 指纹。
    pub fingerprint: String,
    /// 签发时间。
    pub issued_at: DateTime<Utc>,
    /// 到期时间。
    pub not_after: DateTime<Utc>,
    /// 撤销时间。
    pub revoked_at: Option<DateTime<Utc>>,
    /// 撤销原因。
    pub revoke_reason: Option<String>,
}

/// 一次性注册码。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct EnrollCode {
    /// 注册码本体。
    pub code: String,
    /// 备注。
    pub note: Option<String>,
    /// 该码注册的节点默认槽位数。
    pub max_slots: i32,
    /// 过期时间。
    pub expires_at: DateTime<Utc>,
    /// 使用时间。
    pub used_at: Option<DateTime<Utc>>,
    /// 使用该码注册的节点。
    pub used_by_node: Option<Uuid>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

/// 图书主数据。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Book {
    /// 图书编号。
    pub id: Uuid,
    /// 全局序号（NAS 文件名前缀）。
    pub seq: i64,
    /// 原始书名。
    pub raw_title: String,
    /// 原始作者。
    pub raw_author: Option<String>,
    /// 原始出版社。
    pub raw_publisher: Option<String>,
    /// 原始 ISBN。
    pub raw_isbn: Option<String>,
    /// 规范化 ISBN-13。
    pub normalized_isbn: Option<String>,
    /// 去重键。
    pub dedup_key: String,
    /// 中文核验状态。
    pub verify_status: String,
    /// 合并到哪本书。
    pub merged_into: Option<Uuid>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

/// 已入库的图书文件。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct BookFile {
    /// 记录编号。
    pub id: Uuid,
    /// 所属图书。
    pub book_id: Uuid,
    /// 文件格式（技术标识）。
    pub format: String,
    /// NAS 相对路径。
    pub nas_relative_path: String,
    /// 字节数。
    pub size_bytes: i64,
    /// SHA-256。
    pub sha256: String,
    /// 中文文件状态。
    pub status: String,
    /// 入库节点。
    pub ingested_by_node: Option<Uuid>,
    /// 入库时间。
    pub ingested_at: DateTime<Utc>,
}

/// 下载批次。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct DownloadBatch {
    /// 批次编号。
    pub id: Uuid,
    /// 批次名称。
    pub name: String,
    /// 来源文件名。
    pub source_file: Option<String>,
    /// 中文批次状态。
    pub status: String,
    /// 优先级，越大越先执行。
    pub priority: i32,
    /// 下载格式（技术标识）。
    pub download_format: String,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 批次统计（列表页展示进度）。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct BatchProgress {
    /// 批次编号。
    pub batch_id: Uuid,
    /// 关联图书总数。
    pub total: i64,
    /// 已完成数。
    pub completed: i64,
    /// 失败数。
    pub failed: i64,
    /// 已跳过数。
    pub skipped: i64,
    /// 进行中数（含已分配、执行中、等待入库、待确认）。
    pub running: i64,
    /// 待处理数。
    pub pending: i64,
}

/// 图书任务。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct BookTask {
    /// 任务编号。
    pub id: Uuid,
    /// 图书编号。
    pub book_id: Uuid,
    /// 书名（联表取出，便于列表直接展示）。
    pub title: String,
    /// 图书序号。
    pub book_seq: i64,
    /// 文件格式（技术标识）。
    pub format: String,
    /// 中文任务状态。
    pub status: String,
    /// 已尝试次数。
    pub attempts: i32,
    /// 最大尝试次数。
    pub max_attempts: i32,
    /// 下次可领取时间。
    pub next_attempt_at: DateTime<Utc>,
    /// 中文阶段描述。
    pub stage: String,
    /// 阶段版本。
    pub stage_version: i32,
    /// 已下载字节。
    pub downloaded_bytes: i64,
    /// 总字节。
    pub total_bytes: i64,
    /// 持有租约的节点。
    pub lease_node_id: Option<Uuid>,
    /// 持有租约的会话。
    pub lease_session_id: Option<Uuid>,
    /// 持有租约的执行编号。
    pub lease_execution_id: Option<Uuid>,
    /// 租约到期时间。
    pub lease_expires_at: Option<DateTime<Utc>>,
    /// NAS 相对路径。
    pub nas_relative_path: Option<String>,
    /// 最近错误。
    pub last_error: Option<String>,
    /// 是否已请求取消。
    pub cancel_requested: bool,
    /// 期望 NAS 相对路径（R6：领取时固化）。
    pub expected_nas_relative_path: Option<String>,
    /// 期望文件名（R6）。
    pub expected_file_name: Option<String>,
    /// 期望格式（R6）。
    pub expected_format: Option<String>,
    /// 期望大小字节（R6：Worker 最后一次可信文件证据）。
    pub expected_size_bytes: Option<i64>,
    /// 期望 SHA-256（R6：Worker 最后一次可信文件证据）。
    pub expected_sha256: Option<String>,
    /// 产生证据的执行编号（R6）。
    pub evidence_execution_id: Option<Uuid>,
    /// 产生证据的节点编号（R6）。
    pub evidence_node_id: Option<Uuid>,
    /// 证据记录时间（R6）。
    pub evidence_recorded_at: Option<DateTime<Utc>>,
    /// 绑定的代理编号（一本书固定同一代理 IP）。
    pub bound_proxy_id: Option<Uuid>,
    /// 绑定的出口 IP。
    pub bound_exit_ip: Option<String>,
    /// 代理首次绑定时间。
    pub proxy_bound_at: Option<DateTime<Utc>>,
    /// 强制更换代理次数。
    pub proxy_change_count: i32,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 站点账号。响应中**不含**任何密码字段。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Account {
    /// 账号编号。
    pub id: Uuid,
    /// 邮箱。
    pub email: String,
    /// 昵称。
    pub nickname: String,
    /// 中文账号状态。
    pub status: String,
    /// 今日已用额度。
    pub daily_used: i32,
    /// 每日额度上限。
    pub daily_limit: i32,
    /// 额度归零日期。
    pub reset_date: NaiveDate,
    /// 占用该账号的会话。
    pub lease_session_id: Option<Uuid>,
    /// 最近错误。
    pub last_error: Option<String>,
    /// 注册完成时间。
    pub registered_at: Option<DateTime<Utc>>,
    /// 最近登录时间。
    pub last_login_at: Option<DateTime<Utc>>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

/// 代理。响应中**不含**密码字段。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Proxy {
    /// 代理编号。
    pub id: Uuid,
    /// 供应商。
    pub provider: String,
    /// 供应商侧编号。
    pub external_id: Option<String>,
    /// 标签。
    pub label: String,
    /// 协议（技术标识）。
    pub scheme: String,
    /// 主机。
    pub host: String,
    /// 端口。
    pub port: i32,
    /// 中文代理状态。
    pub status: String,
    /// 出口 IP。
    pub exit_ip: Option<String>,
    /// 延迟毫秒。
    pub latency_ms: Option<i32>,
    /// 成功次数。
    pub success_count: i64,
    /// 失败次数。
    pub failure_count: i64,
    /// 限流次数。
    pub throttle_count: i64,
    /// 冷却截止时间。
    pub cooldown_until: Option<DateTime<Utc>>,
    /// 占用该代理的会话。
    pub lease_session_id: Option<Uuid>,
    /// 最近检测时间。
    pub last_checked_at: Option<DateTime<Utc>>,
}

/// 执行会话。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ExecutionSession {
    /// 会话编号。
    pub id: Uuid,
    /// 节点编号。
    pub node_id: Uuid,
    /// 槽位序号。
    pub slot_index: i32,
    /// 账号编号。
    pub account_id: Option<Uuid>,
    /// 代理编号。
    pub proxy_id: Option<Uuid>,
    /// 中文任务类型。
    pub task_type: String,
    /// 中文会话状态。
    pub status: String,
    /// 本机固定转发端口。
    pub local_forward_port: Option<i32>,
    /// 会话内已完成数量。
    pub completed_count: i32,
    /// 租约到期时间。
    pub lease_expires_at: DateTime<Utc>,
    /// 断线保护截止时间。
    pub protected_until: Option<DateTime<Utc>>,
    /// 开始时间。
    pub started_at: DateTime<Utc>,
    /// 结束时间。
    pub ended_at: Option<DateTime<Utc>>,
    /// 结束原因。
    pub end_reason: Option<String>,
}

/// 任务执行记录。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct TaskExecution {
    /// 执行编号。
    pub id: Uuid,
    /// 图书任务编号（兼容保留，账号注册执行时为 None）。
    pub task_id: Option<Uuid>,
    /// 账号注册任务编号。
    pub account_registration_task_id: Option<Uuid>,
    /// 会话编号。
    pub session_id: Option<Uuid>,
    /// 节点编号。
    pub node_id: Option<Uuid>,
    /// 槽位序号。
    pub slot_index: Option<i32>,
    /// 账号编号。
    pub account_id: Option<Uuid>,
    /// 代理编号。
    pub proxy_id: Option<Uuid>,
    /// 中文任务类型。
    pub task_type: String,
    /// 第几次尝试。
    pub attempt: i32,
    /// 阶段版本。
    pub stage_version: i32,
    /// 中文执行结果。
    pub result: Option<String>,
    /// 错误文本。
    pub error: Option<String>,
    /// 耗时毫秒。
    pub duration_ms: Option<i64>,
    /// 开始时间。
    pub started_at: DateTime<Utc>,
    /// 结束时间。
    pub finished_at: Option<DateTime<Utc>>,
}

/// 操作日志。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct OperationLog {
    /// 日志编号。
    pub id: Uuid,
    /// 中文来源。
    pub source: String,
    /// 中文级别。
    pub level: String,
    /// 操作者。
    pub actor: String,
    /// 动作。
    pub action: String,
    /// 目标。
    pub target: String,
    /// 详情。
    pub detail: String,
    /// 时间。
    pub created_at: DateTime<Utc>,
}

/// 告警。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Alert {
    /// 告警编号。
    pub id: Uuid,
    /// 中文级别。
    pub level: String,
    /// 分类。
    pub category: String,
    /// 标题。
    pub title: String,
    /// 详情。
    pub detail: String,
    /// 相关节点。
    pub node_id: Option<Uuid>,
    /// 解决时间。
    pub resolved_at: Option<DateTime<Utc>>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

/// 每日统计。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct DailyStat {
    /// 统计日期。
    pub stat_date: NaiveDate,
    /// 完成数。
    pub completed: i64,
    /// 失败数。
    pub failed: i64,
    /// 跳过数。
    pub skipped: i64,
    /// 累计字节。
    pub bytes_total: i64,
    /// 账号消耗额度。
    pub account_used: i64,
}

/// 导入图书的一行（第 16.3 节：CSV 或表格粘贴）。
#[derive(Debug, Clone, Deserialize)]
pub struct ImportRow {
    /// 书名，必填。
    pub title: String,
    /// 作者。
    #[serde(default)]
    pub author: Option<String>,
    /// 出版社。
    #[serde(default)]
    pub publisher: Option<String>,
    /// ISBN。
    #[serde(default)]
    pub isbn: Option<String>,
}

/// 导入结果摘要，直接回给管理员看。
#[derive(Debug, Clone, Default, Serialize)]
pub struct ImportSummary {
    /// 批次编号。
    pub batch_id: Option<Uuid>,
    /// 提交的总行数。
    pub total_rows: usize,
    /// 新建图书数。
    pub new_books: usize,
    /// 命中已有图书（全局去重生效）的行数。
    pub deduplicated: usize,
    /// 因文件已存在而直接判为已完成的任务数。
    pub already_ingested: usize,
    /// 仅按书名归并、需要人工确认的图书数。
    pub needs_confirm: usize,
    /// 无效行（书名为空）的行号与原因。
    pub invalid_rows: Vec<InvalidRow>,
}

/// 无效导入行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvalidRow {
    /// 行号，从 1 开始。
    pub line: usize,
    /// 原始内容。
    #[serde(default)]
    pub raw: String,
    /// 中文原因。
    pub reason: String,
}

/// 导入任务记录。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ImportJob {
    /// 导入编号。
    pub id: Uuid,
    /// 导入类型：图书 / 账号。
    pub import_type: String,
    /// 导入状态：预检中 / 待确认 / 已提交 / 已过期 / 失败。
    pub status: String,
    /// 原始文件名。
    pub original_file_name: String,
    /// 文件 SHA-256。
    pub file_sha256: String,
    /// 暂存路径。
    pub temp_path: Option<String>,
    /// 令牌哈希。
    pub token_hash: String,
    /// 创建人。
    pub created_by: Option<Uuid>,
    /// 过期时间。
    pub expires_at: DateTime<Utc>,
    /// 提交时间。
    pub committed_at: Option<DateTime<Utc>>,
    /// 提交生成的资源编号（批次编号）。
    pub committed_resource_id: Option<Uuid>,
    /// 统计摘要。
    pub summary: serde_json::Value,
    /// 加密暂存载荷（账号密码等）。
    #[serde(skip)]
    pub payload_encrypted: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 账号注册批次。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct AccountRegistrationBatch {
    /// 批次编号。
    pub id: Uuid,
    /// 批次名称。
    pub name: String,
    /// 来源文件。
    pub source_file: Option<String>,
    /// 中文状态：待开始 / 执行中 / 已暂停 / 已完成 / 已取消。
    pub status: String,
    /// 优先级。
    pub priority: i32,
    /// 创建人。
    pub created_by: Option<Uuid>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 账号注册批次统计。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct AccountRegistrationBatchProgress {
    /// 批次编号。
    pub batch_id: Uuid,
    /// 任务总数。
    pub total: i64,
    /// 已完成数。
    pub completed: i64,
    /// 失败数。
    pub failed: i64,
    /// 执行中数。
    pub running: i64,
    /// 等待人工确认数。
    pub awaiting_confirm: i64,
    /// 待处理数。
    pub pending: i64,
}

/// 账号注册任务。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct AccountRegistrationTask {
    /// 任务编号。
    pub id: Uuid,
    /// 所属批次。
    pub batch_id: Uuid,
    /// 待注册账号编号。
    pub account_id: Uuid,
    /// 待注册账号邮箱（联表取出便于展示）。
    pub email: String,
    /// 待注册账号昵称。
    pub nickname: String,
    /// 中文状态：待处理 / 已分配 / 执行中 / 等待人工确认 / 正在重试 / 已完成 / 失败 / 已取消。
    pub status: String,
    /// 优先级。
    pub priority: i32,
    /// 已尝试次数。
    pub attempts: i32,
    /// 最大尝试次数。
    pub max_attempts: i32,
    /// 下次尝试时间。
    pub next_attempt_at: DateTime<Utc>,
    /// 持有租约的节点。
    pub lease_node_id: Option<Uuid>,
    /// 持有租约的会话。
    pub lease_session_id: Option<Uuid>,
    /// 持有租约的执行编号。
    pub lease_execution_id: Option<Uuid>,
    /// 租约到期时间。
    pub lease_expires_at: Option<DateTime<Utc>>,
    /// 阶段描述。
    pub stage: String,
    /// 阶段版本。
    pub stage_version: i32,
    /// 最近错误。
    pub last_error: Option<String>,
    /// 是否已请求取消。
    pub cancel_requested: bool,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 待确认事项（人工确认流程）。
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ManualAction {
    /// 事项编号。
    pub id: Uuid,
    /// 中文任务类型。
    pub task_type: String,
    /// 账号注册任务编号。
    pub registration_task_id: Option<Uuid>,
    /// 图书任务编号。
    pub book_task_id: Option<Uuid>,
    /// 执行编号。
    pub execution_id: Option<Uuid>,
    /// 执行节点。
    pub node_id: Option<Uuid>,
    /// 执行会话。
    pub session_id: Option<Uuid>,
    /// 确认类型：邮箱验证码 / 图片验证码 / 人工确认 / 风控。
    pub action_type: String,
    /// 说明提示。
    pub prompt: String,
    /// 中文状态：待处理 / 已解决 / 已过期 / 已取消。
    pub status: String,
    /// 证据或截图 URL。
    pub artifact_url: Option<String>,
    /// 输入内容（验证码等，API 响应脱敏）。
    #[serde(skip_serializing)]
    pub input_code: Option<String>,
    /// 过期时间。
    pub expires_at: DateTime<Utc>,
    /// 解决时间。
    pub resolved_at: Option<DateTime<Utc>>,
    /// 解决人。
    pub resolved_by: Option<Uuid>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 图书导入预检预览响应。
#[derive(Debug, Clone, Serialize)]
pub struct BookImportPreview {
    /// 导入令牌。
    pub import_token: String,
    /// 原始文件名。
    pub file_name: String,
    /// 文件 SHA-256。
    pub file_sha256: String,
    /// 总行数。
    pub total_rows: usize,
    /// 有效行数。
    pub valid_rows: usize,
    /// 文件内重复数。
    pub duplicate_in_file: usize,
    /// 库内已有数。
    pub duplicate_in_library: usize,
    /// 已入库数。
    pub already_ingested: usize,
    /// 错误行数。
    pub error_rows: usize,
    /// 警告信息列表。
    pub warnings: Vec<String>,
    /// 预览行列表。
    pub preview: Vec<BookPreviewRow>,
}

/// 图书预检预览单行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookPreviewRow {
    /// 行号。
    pub line: usize,
    /// 书名。
    pub title: String,
    /// 作者。
    pub author: Option<String>,
    /// 出版社。
    pub publisher: Option<String>,
    /// ISBN。
    pub isbn: Option<String>,
    /// 状态描述。
    pub status: String,
    /// 原因说明。
    pub reason: Option<String>,
}

/// 账号导入预检预览响应。
#[derive(Debug, Clone, Serialize)]
pub struct AccountImportPreview {
    /// 导入令牌。
    pub import_token: String,
    /// 文件名。
    pub file_name: String,
    /// 文件 SHA-256。
    pub file_sha256: String,
    /// 总行数。
    pub total_rows: usize,
    /// 有效行数。
    pub valid_rows: usize,
    /// 文件内重复数。
    pub duplicate_in_file: usize,
    /// 库内已有数。
    pub duplicate_in_library: usize,
    /// 错误行数。
    pub error_rows: usize,
    /// 警告列表。
    pub warnings: Vec<String>,
    /// 预览行列表。
    pub preview: Vec<AccountPreviewRow>,
}

/// 账号预检预览单行（不泄露明文密码）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountPreviewRow {
    /// 行号。
    pub line: usize,
    /// 脱敏邮箱。
    pub email_masked: String,
    /// 昵称。
    pub nickname: String,
    /// 是否已提供密码。
    pub password_provided: bool,
    /// 状态描述。
    pub status: String,
    /// 原因说明。
    pub reason: Option<String>,
}
