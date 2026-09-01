//! proto ⇄ 领域类型转换（第 13 节）。
//!
//! 协议里所有编号都是字符串、所有时间都是 RFC 3339 字符串，业务值则是中文。
//! 把转换集中在这里有两个好处：解析失败的中文提示只写一遍；
//! 「空字符串代表没有值」这条协议约定也只在一个地方兑现——proto3 没有 `null`，
//! Worker 发来的 `session_id` 缺失时收到的是 `""`，逐处 `if s.is_empty()` 迟早会漏。

use chrono::{DateTime, Utc};
use platform_proto::v1 as pb;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::scheduler::{SessionGrant, TaskAssignment};

/// 解析一个必填编号。
pub fn parse_uuid(raw: &str, field: &str) -> AppResult<Uuid> {
    Uuid::parse_str(raw.trim()).map_err(|_| AppError::bad(format!("{field}不是合法编号：{raw}")))
}

/// 解析一个可选编号：空字符串按「没有」处理，非法值仍然报错。
pub fn parse_optional_uuid(raw: &str, field: &str) -> AppResult<Option<Uuid>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    Ok(Some(parse_uuid(raw, field)?))
}

/// 当前时间的 RFC 3339 表示，用于每条下行消息的 `sent_at`。
pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

/// 时间转协议字符串。
pub fn to_rfc3339(time: DateTime<Utc>) -> String {
    time.to_rfc3339()
}

/// `u64` → `i64` 的安全收窄。
///
/// 协议里字节数是 `uint64`，数据库里是 `bigint`。溢出时截到上限而不是回绕：
/// 一个荒谬的大数字应该表现为「异常地大」，而不是变成负数把校验骗过去。
pub fn clamp_to_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

/// `u32` → `i32` 的安全收窄。
pub fn clamp_to_i32(value: u32) -> i32 {
    value.min(i32::MAX as u32) as i32
}

/// 把成功的会话分配组装成 `CreateSession`。
///
/// 这是明文凭据唯一被序列化的地方：`SessionGrant` 刻意不实现 `Serialize`，
/// 因此它只能经由这里进入一条已通过 mTLS 与节点凭据双重认证的 gRPC 流。
pub fn create_session_message(grant: &SessionGrant) -> pb::MasterMessage {
    let payload = pb::CreateSession {
        session_id: grant.session_id.to_string(),
        slot_index: grant.slot_index.max(0) as u32,
        task_type: grant.task_type.as_str().to_string(),
        account: grant.account.as_ref().map(|row| pb::AccountCredential {
            account_id: row.account_id.to_string(),
            email: row.email.clone(),
            password: row.password.clone(),
            nickname: row.nickname.clone(),
            daily_used: row.daily_used.max(0) as u32,
            daily_limit: row.daily_limit.max(0) as u32,
        }),
        proxy: grant.proxy.as_ref().map(|row| pb::ProxyCredential {
            proxy_id: row.proxy_id.to_string(),
            label: row.label.clone(),
            scheme: row.scheme.clone(),
            host: row.host.clone(),
            port: row.port.clamp(0, 65535) as u32,
            username: row.username.clone().unwrap_or_default(),
            password: row.password.clone().unwrap_or_default(),
        }),
        local_forward_port: grant.local_forward_port.unwrap_or(0).max(0) as u32,
        max_downloads: grant.max_downloads.max(0) as u32,
        max_duration_secs: grant.max_duration_secs.clamp(0, u32::MAX as i64) as u32,
        lease_expires_at: to_rfc3339(grant.lease_expires_at),
    };
    pb::MasterMessage::new(
        now_rfc3339(),
        pb::master_message::Payload::CreateSession(payload),
    )
}

/// 把成功的任务分配组装成 `AssignTask`。
pub fn assign_task_message(assignment: &TaskAssignment) -> pb::MasterMessage {
    let book = &assignment.book;
    let payload = pb::AssignTask {
        session_id: assignment.session_id.to_string(),
        execution_id: assignment.execution_id.to_string(),
        task_id: assignment.task_id.to_string(),
        task_type: assignment.task_type.as_str().to_string(),
        book: Some(pb::BookTarget {
            book_id: book.book_id.to_string(),
            book_seq: book.book_seq,
            title: book.title.clone(),
            author: book.author.clone().unwrap_or_default(),
            publisher: book.publisher.clone().unwrap_or_default(),
            isbn: book.isbn.clone().unwrap_or_default(),
            format: book.format.clone(),
        }),
        nas_relative_path: assignment.nas_relative_path.clone(),
        final_file_name: assignment.final_file_name.clone(),
        uploading_file_name: assignment.uploading_file_name.clone(),
        attempt: assignment.attempt.max(0) as u32,
        lease_expires_at: to_rfc3339(assignment.lease_expires_at),
        stall_timeout_secs: assignment.stall_timeout_secs,
        // 领取事务 `RETURNING stage_version` 的真实值（第 6.1 节）。
        // 这条链路是「结果能否落地」的关键：少了它，Worker 只能猜一个数字，
        // 而 Master 提交时要求上报版本与库内完全一致，于是连成功结果都会被判为过期。
        stage_version: assignment.stage_version.max(0) as u32,
    };
    pb::MasterMessage::new(
        now_rfc3339(),
        pb::master_message::Payload::AssignTask(payload),
    )
}

/// 「暂时没有任务」的下行消息。
pub fn no_task_message(
    session_id: Option<Uuid>,
    reason: &str,
    retry_after_secs: u32,
) -> pb::MasterMessage {
    pb::MasterMessage::new(
        now_rfc3339(),
        pb::master_message::Payload::NoTask(pb::NoTaskAvailable {
            session_id: session_id.map(|id| id.to_string()).unwrap_or_default(),
            reason: reason.to_string(),
            retry_after_secs,
        }),
    )
}

/// 「该结束会话了」的下行消息。
pub fn end_session_message(
    session_id: Uuid,
    reason: &str,
    finish_current_task: bool,
) -> pb::MasterMessage {
    pb::MasterMessage::new(
        now_rfc3339(),
        pb::master_message::Payload::EndSession(pb::EndSession {
            session_id: session_id.to_string(),
            reason: reason.to_string(),
            finish_current_task,
        }),
    )
}

/// 事件确认：Worker 收到后即可从本地 outbox 删除该事件。
pub fn ack(event_id: &str, accepted: bool, detail: &str) -> pb::MasterMessage {
    pb::MasterMessage::new(
        now_rfc3339(),
        pb::master_message::Payload::EventAck(pb::EventAck {
            event_id: event_id.to_string(),
            accepted,
            detail: detail.to_string(),
        }),
    )
}

/// 让在线 Worker 去核查 NAS 上是否已存在目标文件（第 14.4 节、V3 方案第 14 节）。
pub fn verify_nas_file_message(
    task_id: Uuid,
    nas_relative_path: &str,
    expected_sha256: &str,
    expected_size_bytes: i64,
    expected_format: &str,
    expected_file_name: &str,
) -> pb::MasterMessage {
    pb::MasterMessage::new(
        now_rfc3339(),
        pb::master_message::Payload::VerifyNasFile(pb::VerifyNasFile {
            task_id: task_id.to_string(),
            nas_relative_path: nas_relative_path.to_string(),
            expected_sha256: expected_sha256.to_string(),
            expected_size_bytes: expected_size_bytes.max(0) as u64,
            expected_format: expected_format.to_string(),
            expected_file_name: expected_file_name.to_string(),
        }),
    )
}

/// 取消一个正在执行的任务（V4 精确取消：携带 node_id 与执行世代）。
pub fn cancel_task_message(
    node_id: Option<Uuid>,
    session_id: Option<Uuid>,
    task_id: Uuid,
    execution_id: Option<Uuid>,
    stage_version: i32,
    reason: &str,
) -> pb::MasterMessage {
    pb::MasterMessage::new(
        now_rfc3339(),
        pb::master_message::Payload::CancelTask(pb::CancelTask {
            node_id: node_id.map(|id| id.to_string()).unwrap_or_default(),
            session_id: session_id.map(|id| id.to_string()).unwrap_or_default(),
            task_id: task_id.to_string(),
            execution_id: execution_id.map(|id| id.to_string()).unwrap_or_default(),
            stage_version: stage_version.max(0) as u32,
            reason: reason.to_string(),
        }),
    )
}

/// 下发账号注册任务所需的完整快照。
pub struct RegistrationTaskAssignment {
    /// 承载本次注册执行的 Worker 会话。
    pub session_id: Uuid,
    /// 本次租约执行世代，用于拒绝旧结果。
    pub execution_id: Uuid,
    /// 待执行的账号注册任务编号。
    pub registration_task_id: Uuid,
    /// 当前重试次数。
    pub attempt: i32,
    /// 注册任务的阶段版本。
    pub stage_version: i32,
    /// Worker 必须续租或结束执行的时间。
    pub lease_expires_at: DateTime<Utc>,
    /// 此注册流程是否需要邮件验证码。
    pub needs_mail_code: bool,
    /// 固定到本次任务的邮件 Provider 配置快照。
    pub mail_provider: Option<pb::MailProviderLease>,
}

/// 下发账号注册任务。
pub fn assign_registration_task_message(
    assignment: RegistrationTaskAssignment,
) -> pb::MasterMessage {
    pb::MasterMessage::new(
        now_rfc3339(),
        pb::master_message::Payload::AssignRegistrationTask(pb::AssignRegistrationTask {
            session_id: assignment.session_id.to_string(),
            execution_id: assignment.execution_id.to_string(),
            registration_task_id: assignment.registration_task_id.to_string(),
            attempt: assignment.attempt.max(1) as u32,
            stage_version: assignment.stage_version.max(0) as u32,
            lease_expires_at: to_rfc3339(assignment.lease_expires_at),
            needs_mail_code: assignment.needs_mail_code,
            mail_provider: assignment.mail_provider,
        }),
    )
}

/// 下发人工确认输入（如验证码）。
pub fn continue_manual_action_message(
    action_id: Uuid,
    execution_id: Uuid,
    action_type: &str,
    action_payload: &str,
) -> pb::MasterMessage {
    pb::MasterMessage::new(
        now_rfc3339(),
        pb::master_message::Payload::ContinueManualAction(pb::ContinueManualAction {
            action_id: action_id.to_string(),
            execution_id: execution_id.to_string(),
            action_type: action_type.to_string(),
            action_payload: action_payload.to_string(),
        }),
    )
}

/// 取消账号注册任务。
pub fn cancel_registration_task_message(
    node_id: Option<Uuid>,
    session_id: Option<Uuid>,
    registration_task_id: Uuid,
    execution_id: Option<Uuid>,
    stage_version: i32,
    reason: &str,
) -> pb::MasterMessage {
    pb::MasterMessage::new(
        now_rfc3339(),
        pb::master_message::Payload::CancelRegistrationTask(pb::CancelRegistrationTask {
            node_id: node_id.map(|id| id.to_string()).unwrap_or_default(),
            session_id: session_id.map(|id| id.to_string()).unwrap_or_default(),
            registration_task_id: registration_task_id.to_string(),
            execution_id: execution_id.map(|id| id.to_string()).unwrap_or_default(),
            stage_version: stage_version.max(0) as u32,
            reason: reason.to_string(),
        }),
    )
}

/// 组装下发给节点的运行参数（第 16.1 节）。
///
/// 节奏参数一律由 Master 下发，Worker 本地配置只保留「连到哪、证书在哪」：
/// 心跳与续租间隔必须两端一致，让它们从两个配置文件里各读一份迟早会漂移。
pub fn node_config_message(
    node: &crate::models::WorkerNode,
    config: &crate::config::MasterConfig,
    search: &crate::download_search::DownloadSearchOptions,
    layout_root: &str,
    max_session_duration_secs: u32,
) -> pb::MasterMessage {
    let scheduler = &config.scheduler;
    let payload = pb::NodeConfig {
        node_id: node.id.to_string(),
        node_name: node.name.clone(),
        node_status: node.status.clone(),
        max_slots: node.max_slots.max(0) as u32,
        upload_concurrency: node.upload_concurrency.max(1) as u32,
        heartbeat_interval_secs: scheduler.heartbeat_interval_secs.min(u32::MAX as u64) as u32,
        session_renew_secs: scheduler.session_renew_secs.min(u32::MAX as u64) as u32,
        progress_min_interval_secs: scheduler.progress_min_interval_secs.min(u32::MAX as u64)
            as u32,
        progress_min_bytes: scheduler.progress_min_bytes,
        max_session_duration_secs,
        stall_timeout_secs: scheduler.stall_timeout_secs.min(u32::MAX as u64) as u32,
        nas_relative_root: layout_root.to_string(),
        minimum_free_gb: config.nas.free_space_alert_gb.max(0) as u64,
        site_base: config.server.site_base.clone(),
        download_format: "pdf".to_string(),
        config_version: node.config_version.clone(),
        min_agent_version: String::new(),
        diagnostics_enabled: node.diagnostics_enabled,
        minimum_file_bytes: config.nas.minimum_file_bytes,
        search_order: search.order.clone(),
        search_extensions: search.extensions.clone(),
    };
    pb::MasterMessage::new(
        now_rfc3339(),
        pb::master_message::Payload::NodeConfig(payload),
    )
}

/// 领域错误 → gRPC 状态码。
///
/// 映射刻意保守：只有明确属于「请求本身不对」的错误才回客户端错误码，
/// 其余一律 `internal`。把内部细节塞进 gRPC 状态消息会让它出现在 Worker 日志里。
pub fn to_status(error: AppError) -> tonic::Status {
    let message = error.to_string();
    match error {
        AppError::BadRequest(_) => tonic::Status::invalid_argument(message),
        AppError::Unauthorized(_) => tonic::Status::unauthenticated(message),
        AppError::Forbidden(_) => tonic::Status::permission_denied(message),
        AppError::NotFound(_) => tonic::Status::not_found(message),
        AppError::Conflict(_) => tonic::Status::failed_precondition(message),
        AppError::TooManyRequests(_) => tonic::Status::resource_exhausted(message),
        AppError::Database(error) => {
            tracing::error!(%error, "gRPC 请求的数据库操作失败");
            tonic::Status::internal("服务器内部错误")
        }
        AppError::Internal(error) => {
            tracing::error!(%error, "gRPC 请求处理失败");
            tonic::Status::internal("服务器内部错误")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_optional_uuid_is_none_not_an_error() {
        // proto3 没有 null，缺失的编号到达时是空字符串
        assert_eq!(parse_optional_uuid("", "会话").unwrap(), None);
        assert_eq!(parse_optional_uuid("   ", "会话").unwrap(), None);
    }

    #[test]
    fn malformed_uuid_is_reported_in_chinese() {
        let error = parse_uuid("不是编号", "任务").unwrap_err();
        assert!(error.to_string().contains("任务不是合法编号"));
    }

    #[test]
    fn optional_uuid_still_rejects_garbage() {
        assert!(parse_optional_uuid("abc", "会话").is_err());
    }

    #[test]
    fn oversized_byte_counts_saturate_instead_of_wrapping() {
        assert_eq!(clamp_to_i64(u64::MAX), i64::MAX);
        assert_eq!(clamp_to_i64(1024), 1024);
        assert_eq!(clamp_to_i32(u32::MAX), i32::MAX);
    }

    #[test]
    fn internal_errors_do_not_leak_details() {
        let status = to_status(AppError::Internal(anyhow::anyhow!("连接串里有密码")));
        assert_eq!(status.message(), "服务器内部错误");
        assert_eq!(status.code(), tonic::Code::Internal);
    }

    #[test]
    fn client_errors_keep_their_chinese_message() {
        let status = to_status(AppError::bad("槽位序号超出范围"));
        assert_eq!(status.message(), "槽位序号超出范围");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }
}
