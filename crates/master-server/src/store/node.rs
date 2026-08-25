//! Worker 节点、槽位、证书、注册码与节点配置版本。

use chrono::{DateTime, Duration, Utc};
use platform_domain::{SlotStatus, WorkerStatus};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::{EnrollCode, NodeCertificate, WorkerNode, WorkerSlot};
use crate::security::{constant_time_eq, hash_node_token};

/// 节点表全部列（含 V5/V7 直连注册与凭据模式字段），供 FromRow 查询复用。
pub const NODE_COLUMNS: &str = "id, name, hostname, os, os_version, agent_version, status, \
     max_slots, available_slots, upload_concurrency, config_version, applied_config_version, \
     diagnostics_enabled, nas_healthy, nas_free_gb, staging_free_gb, cpu_percent, \
     memory_used_mb, memory_total_mb, connected, last_heartbeat_at, approved_at, approved_by, \
     installation_id, public_key_fingerprint, registration_status, requested_slots, \
     configured_slots, registration_expires_at, first_seen_ip, last_registration_at, \
     rejected_at, rejected_by, reject_reason, credential_mode, created_at, updated_at";

const SLOT_COLUMNS: &str = "id, node_id, slot_index, status, session_id, detail, updated_at";

const CERT_COLUMNS: &str =
    "id, node_id, fingerprint, issued_at, not_after, revoked_at, revoke_reason";

const CODE_COLUMNS: &str = "code, note, max_slots, expires_at, used_at, used_by_node, created_at";

// ---------------------------------------------------------------- 注册码

/// 生成一次性注册码（第 15.1 节）。
pub async fn issue_enroll_code(
    executor: impl PgExecutor<'_>,
    code: &str,
    note: Option<&str>,
    max_slots: i32,
    valid_hours: i64,
    created_by: Option<Uuid>,
) -> AppResult<EnrollCode> {
    let expires_at = Utc::now() + Duration::hours(valid_hours.clamp(1, 24 * 30));
    let record = sqlx::query_as::<_, EnrollCode>(&format!(
        "INSERT INTO enroll_codes (code, note, max_slots, created_by, expires_at) \
         VALUES ($1, $2, $3, $4, $5) RETURNING {CODE_COLUMNS}"
    ))
    .bind(code)
    .bind(note)
    .bind(max_slots.clamp(1, 64))
    .bind(created_by)
    .bind(expires_at)
    .fetch_one(executor)
    .await?;
    Ok(record)
}

/// 列出注册码，未使用的排在前面。
pub async fn list_enroll_codes(executor: impl PgExecutor<'_>) -> AppResult<Vec<EnrollCode>> {
    let codes = sqlx::query_as::<_, EnrollCode>(&format!(
        "SELECT {CODE_COLUMNS} FROM enroll_codes \
         ORDER BY (used_at IS NOT NULL), created_at DESC LIMIT 200"
    ))
    .fetch_all(executor)
    .await?;
    Ok(codes)
}

/// 作废一个尚未使用的注册码。
pub async fn delete_enroll_code(executor: impl PgExecutor<'_>, code: &str) -> AppResult<()> {
    let affected = sqlx::query("DELETE FROM enroll_codes WHERE code = $1 AND used_at IS NULL")
        .bind(code)
        .execute(executor)
        .await?
        .rows_affected();
    if affected == 0 {
        return Err(AppError::missing("注册码不存在或已被使用"));
    }
    Ok(())
}

/// 消费注册码。必须在注册事务内调用。
///
/// `FOR UPDATE` + `used_at IS NULL` 的组合保证「一次性」：两个 Worker 同时用同一个码
/// 注册时，后者会看到 `used_at` 已被写入并被拒绝，而不是两个节点都注册成功。
pub async fn consume_enroll_code(
    tx: &mut sqlx::PgConnection,
    code: &str,
    node_id: Uuid,
) -> AppResult<EnrollCode> {
    let existing = sqlx::query_as::<_, EnrollCode>(&format!(
        "SELECT {CODE_COLUMNS} FROM enroll_codes WHERE code = $1 FOR UPDATE"
    ))
    .bind(code)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::Unauthorized("注册码无效".to_string()))?;

    if existing.used_at.is_some() {
        return Err(AppError::Unauthorized("注册码已被使用".to_string()));
    }
    if existing.expires_at < Utc::now() {
        return Err(AppError::Unauthorized("注册码已过期".to_string()));
    }

    sqlx::query("UPDATE enroll_codes SET used_at = now(), used_by_node = $2 WHERE code = $1")
        .bind(code)
        .bind(node_id)
        .execute(&mut *tx)
        .await?;

    Ok(existing)
}

// ---------------------------------------------------------------- 节点

/// 注册或重装一个节点。
///
/// 同名节点视为「重装」而不是冲突：重装必须先拿到一个有效的一次性注册码，
/// 因此这条路径已经过授权；沿用同一条记录可以保留历史执行记录的外键引用。
/// 重装会重置凭据散列并把状态退回待审核，避免旧凭据继续可用。
///
/// 注意：0005 起 `worker_nodes.name` 不再是 UNIQUE（V5 里名称只是显示名），
/// 所以这里不能再用 `ON CONFLICT (name)`，改为显式「按名查找 → 更新或插入」。
#[allow(clippy::too_many_arguments)]
pub async fn upsert_node(
    tx: &mut sqlx::PgConnection,
    name: &str,
    hostname: &str,
    os: &str,
    os_version: &str,
    agent_version: &str,
    max_slots: i32,
    node_token_hash: &str,
) -> AppResult<WorkerNode> {
    if let Some(existing) = find_node_by_name(&mut *tx, name).await? {
        let node = sqlx::query_as::<_, WorkerNode>(&format!(
            "UPDATE worker_nodes SET \
                 hostname = $2, os = $3, os_version = $4, agent_version = $5, \
                 max_slots = $6, node_token_hash = $7, status = $8, \
                 approved_at = NULL, approved_by = NULL, updated_at = now() \
             WHERE id = $1 RETURNING {NODE_COLUMNS}"
        ))
        .bind(existing.id)
        .bind(hostname)
        .bind(os)
        .bind(os_version)
        .bind(agent_version)
        .bind(max_slots.clamp(0, 64))
        .bind(node_token_hash)
        .bind(WorkerStatus::PendingApproval.as_str())
        .fetch_one(&mut *tx)
        .await?;
        return Ok(node);
    }
    let node = sqlx::query_as::<_, WorkerNode>(&format!(
        "INSERT INTO worker_nodes \
             (id, name, hostname, os, os_version, agent_version, status, max_slots, node_token_hash) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING {NODE_COLUMNS}"
    ))
    .bind(Uuid::new_v4())
    .bind(name)
    .bind(hostname)
    .bind(os)
    .bind(os_version)
    .bind(agent_version)
    .bind(WorkerStatus::PendingApproval.as_str())
    .bind(max_slots.clamp(0, 64))
    .bind(node_token_hash)
    .fetch_one(&mut *tx)
    .await?;
    Ok(node)
}

/// V5 直连注册专用：**无条件插入新节点**（第 6.3 节）。
///
/// 直连注册的身份是「安装标识 + 公钥指纹」，名称只是可读显示名，同名机器是常态，
/// 因此这里绝不按名称合并（否则两台不同机器会串号）。幂等性由调用方在注册
/// 处理器里按安装标识先行查重，本函数只负责落一行新记录。
#[allow(clippy::too_many_arguments)]
pub async fn insert_node(
    tx: &mut sqlx::PgConnection,
    name: &str,
    hostname: &str,
    os: &str,
    os_version: &str,
    agent_version: &str,
    max_slots: i32,
) -> AppResult<WorkerNode> {
    let node = sqlx::query_as::<_, WorkerNode>(&format!(
        "INSERT INTO worker_nodes \
             (id, name, hostname, os, os_version, agent_version, status, max_slots, node_token_hash) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, '') RETURNING {NODE_COLUMNS}"
    ))
    .bind(Uuid::new_v4())
    .bind(name)
    .bind(hostname)
    .bind(os)
    .bind(os_version)
    .bind(agent_version)
    .bind(WorkerStatus::PendingApproval.as_str())
    .bind(max_slots.clamp(0, 64))
    .fetch_one(&mut *tx)
    .await?;
    Ok(node)
}

/// 按编号取节点。
pub async fn get_node(executor: impl PgExecutor<'_>, id: Uuid) -> AppResult<WorkerNode> {
    sqlx::query_as::<_, WorkerNode>(&format!(
        "SELECT {NODE_COLUMNS} FROM worker_nodes WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::missing("节点不存在"))
}

/// 按名称取节点。
pub async fn find_node_by_name(
    executor: impl PgExecutor<'_>,
    name: &str,
) -> AppResult<Option<WorkerNode>> {
    let node = sqlx::query_as::<_, WorkerNode>(&format!(
        "SELECT {NODE_COLUMNS} FROM worker_nodes WHERE name = $1"
    ))
    .bind(name)
    .fetch_optional(executor)
    .await?;
    Ok(node)
}

/// 节点列表。
pub async fn list_nodes(executor: impl PgExecutor<'_>) -> AppResult<Vec<WorkerNode>> {
    let nodes = sqlx::query_as::<_, WorkerNode>(&format!(
        "SELECT {NODE_COLUMNS} FROM worker_nodes ORDER BY name"
    ))
    .fetch_all(executor)
    .await?;
    Ok(nodes)
}
/// 按注册状态过滤节点（V5：GET /api/workers?registration_status=待审核）。
pub async fn list_nodes_by_registration(
    executor: impl PgExecutor<'_>,
    registration_status: Option<&str>,
) -> AppResult<Vec<WorkerNode>> {
    let nodes = sqlx::query_as::<_, WorkerNode>(&format!(
        "SELECT {NODE_COLUMNS} FROM worker_nodes \
         WHERE ($1::text IS NULL OR registration_status = $1) ORDER BY name"
    ))
    .bind(registration_status)
    .fetch_all(executor)
    .await?;
    Ok(nodes)
}

/// 校验节点凭据。
///
/// 数据库里只有 SHA-256 散列，比较也走常量时间，因此即使日志或计时侧信道泄漏，
/// 也拿不到可用的凭据。
pub async fn authenticate_node(pool: &PgPool, node_id: Uuid, token: &str) -> AppResult<WorkerNode> {
    let stored: Option<String> =
        sqlx::query_scalar("SELECT node_token_hash FROM worker_nodes WHERE id = $1")
            .bind(node_id)
            .fetch_optional(pool)
            .await?;
    let stored = stored.ok_or_else(|| AppError::Unauthorized("节点凭据无效".to_string()))?;
    if !constant_time_eq(&stored, &hash_node_token(token)) {
        return Err(AppError::Unauthorized("节点凭据无效".to_string()));
    }
    let node = get_node(pool, node_id).await?;
    if node.status == WorkerStatus::Disabled.as_str() {
        return Err(AppError::Forbidden("节点已被禁用".to_string()));
    }
    Ok(node)
}

/// 直接改节点状态。状态迁移的合法性由调用方用 `ensure_transition` 先校验。
pub async fn set_node_status(
    executor: impl PgExecutor<'_>,
    node_id: Uuid,
    status: WorkerStatus,
) -> AppResult<WorkerNode> {
    sqlx::query_as::<_, WorkerNode>(&format!(
        "UPDATE worker_nodes SET status = $2, updated_at = now() WHERE id = $1 \
         RETURNING {NODE_COLUMNS}"
    ))
    .bind(node_id)
    .bind(status.as_str())
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::missing("节点不存在"))
}

/// 审核通过。
pub async fn approve_node(
    executor: impl PgExecutor<'_>,
    node_id: Uuid,
    approved_by: Option<Uuid>,
) -> AppResult<WorkerNode> {
    sqlx::query_as::<_, WorkerNode>(&format!(
        "UPDATE worker_nodes SET status = $2, approved_at = now(), approved_by = $3, \
             updated_at = now() \
         WHERE id = $1 AND status = $4 RETURNING {NODE_COLUMNS}"
    ))
    .bind(node_id)
    .bind(WorkerStatus::Offline.as_str())
    .bind(approved_by)
    .bind(WorkerStatus::PendingApproval.as_str())
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::conflict("节点不存在或不处于待审核状态"))
}

/// 调整槽位上限与上传并发。
pub async fn set_node_capacity(
    executor: impl PgExecutor<'_>,
    node_id: Uuid,
    max_slots: i32,
    upload_concurrency: i32,
) -> AppResult<WorkerNode> {
    sqlx::query_as::<_, WorkerNode>(&format!(
        "UPDATE worker_nodes SET max_slots = $2, upload_concurrency = $3, updated_at = now() \
         WHERE id = $1 RETURNING {NODE_COLUMNS}"
    ))
    .bind(node_id)
    .bind(max_slots.clamp(0, 64))
    .bind(upload_concurrency.clamp(1, 16))
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::missing("节点不存在"))
}

/// 开关诊断日志。
pub async fn set_diagnostics(
    executor: impl PgExecutor<'_>,
    node_id: Uuid,
    enabled: bool,
) -> AppResult<WorkerNode> {
    sqlx::query_as::<_, WorkerNode>(&format!(
        "UPDATE worker_nodes SET diagnostics_enabled = $2, updated_at = now() \
         WHERE id = $1 RETURNING {NODE_COLUMNS}"
    ))
    .bind(node_id)
    .bind(enabled)
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::missing("节点不存在"))
}

/// 标记 gRPC 长连接是否在线。
pub async fn set_connected(
    executor: impl PgExecutor<'_>,
    node_id: Uuid,
    connected: bool,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE worker_nodes SET connected = $2, updated_at = now(), \
             last_heartbeat_at = CASE WHEN $2 THEN now() ELSE last_heartbeat_at END \
         WHERE id = $1",
    )
    .bind(node_id)
    .bind(connected)
    .execute(executor)
    .await?;
    Ok(())
}

/// 心跳上报的机器指标（第 13.2 节）。
#[derive(Debug, Clone, Default)]
pub struct HeartbeatMetrics {
    /// NAS 是否可写。
    pub nas_healthy: bool,
    /// NAS 剩余空间（GB）。
    pub nas_free_gb: i64,
    /// 暂存剩余空间（GB）。
    pub staging_free_gb: i64,
    /// CPU 百分比。
    pub cpu_percent: f64,
    /// 已用内存（MB）。
    pub memory_used_mb: i64,
    /// 总内存（MB）。
    pub memory_total_mb: i64,
    /// Agent 版本。
    pub agent_version: String,
    /// 已应用的配置版本。
    pub applied_config_version: String,
    /// 最近一次注册任务应用的邮件 Provider 版本与健康摘要。
    pub applied_mail_provider_version: i64,
    /// 最近一次注册任务使用的 Provider 名称。
    pub mail_provider_name: String,
    /// Worker 上报的 Provider 脱敏健康摘要。
    pub mail_provider_health: String,
    /// Worker 自评状态；`None` 表示这次心跳不改状态
    /// （没上报、上报了非法值，或该值无权自评——判定见
    /// [`platform_domain::adopt_reported_worker_status`]）。
    pub reported_status: Option<WorkerStatus>,
}

/// 一次心跳之后节点的中文状态（可能与心跳前相同）。
///
/// 返回值而不是 `()`：调用方要据此决定「状态变了吗」——变了才写日志、
/// 关离线告警、推前端事件（第 3.7 节）。
pub type HeartbeatStatus = String;

/// 写入一次心跳，返回心跳生效后的节点状态。
///
/// 状态的**合法性**在调用方判（`metrics.reported_status` 已经是解析并鉴权过的值），
/// 状态的**归属**在这条 SQL 里判：`CASE WHEN status IN (待审核, 维护中, 已禁用, 已暂停)`
/// 读的是本语句已加锁的当前值，因此「管理员刚点了维护中」与「节点刚发来心跳」
/// 这两个并发写不会互相覆盖。若改成先查再改，后到的心跳会把管理员的决定冲掉。
pub async fn apply_heartbeat(
    executor: impl PgExecutor<'_>,
    node_id: Uuid,
    metrics: &HeartbeatMetrics,
) -> AppResult<HeartbeatStatus> {
    let status: Option<String> = sqlx::query_scalar(
        "UPDATE worker_nodes SET \
             nas_healthy = $2, nas_free_gb = $3, staging_free_gb = $4, cpu_percent = $5, \
             memory_used_mb = $6, memory_total_mb = $7, \
             agent_version = COALESCE(NULLIF($8, ''), agent_version), \
             applied_config_version = COALESCE(NULLIF($9, ''), applied_config_version), \
             applied_mail_provider_version = CASE WHEN $10 > 0 THEN $10 ELSE applied_mail_provider_version END, \
             mail_provider_name = COALESCE(NULLIF($11, ''), mail_provider_name), \
             mail_provider_health = COALESCE(NULLIF($12, ''), mail_provider_health), \
             status = CASE \
                 WHEN $13::text = '' THEN status \
                 WHEN status IN ($14, $15, $16, $17) THEN status \
                 ELSE $13::text END, \
             connected = TRUE, last_heartbeat_at = now(), updated_at = now() \
         WHERE id = $1 \
         RETURNING status",
    )
    .bind(node_id)
    .bind(metrics.nas_healthy)
    .bind(metrics.nas_free_gb)
    .bind(metrics.staging_free_gb)
    .bind(metrics.cpu_percent)
    .bind(metrics.memory_used_mb)
    .bind(metrics.memory_total_mb)
    .bind(&metrics.agent_version)
    .bind(&metrics.applied_config_version)
    .bind(metrics.applied_mail_provider_version)
    .bind(&metrics.mail_provider_name)
    .bind(&metrics.mail_provider_health)
    .bind(metrics.reported_status.map(|s| s.as_str()).unwrap_or(""))
    .bind(WorkerStatus::PendingApproval.as_str())
    .bind(WorkerStatus::Maintenance.as_str())
    .bind(WorkerStatus::Disabled.as_str())
    .bind(WorkerStatus::Paused.as_str())
    .fetch_optional(executor)
    .await?;

    status.ok_or_else(|| AppError::missing("节点不存在"))
}

/// 记录一次节点上线自报（第 14.1 节重连对账）。
///
/// `NodeOnline` 里的 `applied_config_version` 是 Worker 唯一一次主动告知
/// 「我现在跑的是哪一版运行配置」的机会。以前这个字段被直接丢掉，于是节点行里的
/// `applied_config_version` 永远是空的，后台也就无法判断下发的配置到底生效了没有
/// （第 3.3 节要求配置生效可核对）。
///
/// 空字符串一律按「没上报」处理，不覆盖库里已有的值；状态不在这里改，
/// 那是调用方按 [`WorkerStatus::is_admin_governed`] 判定后的事。
pub async fn record_node_online(
    executor: impl PgExecutor<'_>,
    node_id: Uuid,
    agent_version: &str,
    os: &str,
    os_version: &str,
    applied_config_version: &str,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE worker_nodes SET \
             agent_version = COALESCE(NULLIF($2, ''), agent_version), \
             os = COALESCE(NULLIF($3, ''), os), \
             os_version = COALESCE(NULLIF($4, ''), os_version), \
             applied_config_version = COALESCE(NULLIF($5, ''), applied_config_version), \
             connected = TRUE, last_heartbeat_at = now(), updated_at = now() \
         WHERE id = $1",
    )
    .bind(node_id)
    .bind(agent_version.trim())
    .bind(os.trim())
    .bind(os_version.trim())
    .bind(applied_config_version.trim())
    .execute(executor)
    .await?;
    Ok(())
}

/// 单独更新 NAS 健康状况（第 14.4 节 NAS 核验结果，不带其他机器指标）。
///
/// 与 [`apply_heartbeat`] 分开是因为核验是一次专门的探测，不该顺手把
/// CPU、内存这些字段覆盖成默认值，也不该刷新 `last_heartbeat_at`——
/// 否则一个已经失联的节点会因为一条迟到的核验结果被判成在线。
pub async fn set_nas_health(
    executor: impl PgExecutor<'_>,
    node_id: Uuid,
    nas_healthy: bool,
    nas_free_gb: i64,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE worker_nodes SET nas_healthy = $2, \
             nas_free_gb = GREATEST($3, 0), updated_at = now() \
         WHERE id = $1",
    )
    .bind(node_id)
    .bind(nas_healthy)
    .bind(nas_free_gb)
    .execute(executor)
    .await?;
    Ok(())
}

/// 把心跳超时的节点标记为离线，返回受影响的节点编号。
///
/// 只处理「本应在工作」的状态，`待审核`/`已禁用`/`维护中` 不参与，
/// 否则管理员刚设成维护中就会被巡检改回离线。
pub async fn mark_stale_nodes_offline(
    executor: impl PgExecutor<'_>,
    timeout_secs: i64,
) -> AppResult<Vec<Uuid>> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE worker_nodes SET status = $1, connected = FALSE, available_slots = 0, \
             updated_at = now() \
         WHERE status IN ($2, $3, $4, $5) \
           AND (last_heartbeat_at IS NULL OR last_heartbeat_at < now() - ($6 || ' seconds')::interval) \
         RETURNING id",
    )
    .bind(WorkerStatus::Offline.as_str())
    .bind(WorkerStatus::Online.as_str())
    .bind(WorkerStatus::Busy.as_str())
    .bind(WorkerStatus::StorageError.as_str())
    .bind(WorkerStatus::Paused.as_str())
    .bind(timeout_secs.max(1).to_string())
    .fetch_all(executor)
    .await?;
    Ok(ids)
}

// ---------------------------------------------------------------- 槽位

/// 按节点的 `max_slots` 对齐槽位表。
///
/// 缩容不会删除槽位记录，只把超出上限的置为 `已停用`：正在执行的会话仍然通过
/// `session_id` 指向该槽位，删除会破坏正在跑的任务的可观测性。
pub async fn ensure_slots(
    tx: &mut sqlx::PgConnection,
    node_id: Uuid,
    max_slots: i32,
) -> AppResult<()> {
    let max_slots = max_slots.clamp(0, 64);
    if max_slots > 0 {
        sqlx::query(
            "INSERT INTO worker_slots (id, node_id, slot_index, status) \
             SELECT gen_random_uuid(), $1, i, $2 FROM generate_series(0, $3 - 1) AS i \
             ON CONFLICT (node_id, slot_index) DO NOTHING",
        )
        .bind(node_id)
        .bind(SlotStatus::Idle.as_str())
        .bind(max_slots)
        .execute(&mut *tx)
        .await?;

        // 扩容时把之前停用、且当前没有会话的槽位放回空闲
        sqlx::query(
            "UPDATE worker_slots SET status = $2, detail = '', updated_at = now() \
             WHERE node_id = $1 AND slot_index < $3 AND status = $4 AND session_id IS NULL",
        )
        .bind(node_id)
        .bind(SlotStatus::Idle.as_str())
        .bind(max_slots)
        .bind(SlotStatus::Deactivated.as_str())
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query(
        "UPDATE worker_slots SET status = $2, detail = '超出槽位上限', updated_at = now() \
         WHERE node_id = $1 AND slot_index >= $3 AND session_id IS NULL AND status <> $2",
    )
    .bind(node_id)
    .bind(SlotStatus::Deactivated.as_str())
    .bind(max_slots)
    .execute(&mut *tx)
    .await?;

    Ok(())
}

/// 某节点的槽位。
pub async fn list_slots(
    executor: impl PgExecutor<'_>,
    node_id: Uuid,
) -> AppResult<Vec<WorkerSlot>> {
    let slots = sqlx::query_as::<_, WorkerSlot>(&format!(
        "SELECT {SLOT_COLUMNS} FROM worker_slots WHERE node_id = $1 ORDER BY slot_index"
    ))
    .bind(node_id)
    .fetch_all(executor)
    .await?;
    Ok(slots)
}

/// 全部槽位，总览页用。
pub async fn list_all_slots(executor: impl PgExecutor<'_>) -> AppResult<Vec<WorkerSlot>> {
    let slots = sqlx::query_as::<_, WorkerSlot>(&format!(
        "SELECT {SLOT_COLUMNS} FROM worker_slots ORDER BY node_id, slot_index"
    ))
    .fetch_all(executor)
    .await?;
    Ok(slots)
}

/// 更新槽位状态。
pub async fn set_slot(
    executor: impl PgExecutor<'_>,
    node_id: Uuid,
    slot_index: i32,
    status: SlotStatus,
    session_id: Option<Uuid>,
    detail: &str,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE worker_slots SET status = $3, session_id = $4, detail = $5, updated_at = now() \
         WHERE node_id = $1 AND slot_index = $2",
    )
    .bind(node_id)
    .bind(slot_index)
    .bind(status.as_str())
    .bind(session_id)
    .bind(detail)
    .execute(executor)
    .await?;
    Ok(())
}

/// 按节点当前空闲槽位数刷新 `available_slots`，返回刷新后的值。
///
/// 可用槽位是派生数据，让它由一条 SQL 从槽位表算出来，就不会出现
/// 「计数器加减漏了一次」这种只能靠重启修复的漂移。
pub async fn refresh_available_slots(
    executor: impl PgExecutor<'_>,
    node_id: Uuid,
) -> AppResult<i32> {
    let available: i32 = sqlx::query_scalar(
        "UPDATE worker_nodes n SET available_slots = sub.free, updated_at = now() \
         FROM (SELECT count(*)::int AS free FROM worker_slots \
               WHERE node_id = $1 AND status = $2) AS sub \
         WHERE n.id = $1 RETURNING n.available_slots",
    )
    .bind(node_id)
    .bind(SlotStatus::Idle.as_str())
    .fetch_optional(executor)
    .await?
    .unwrap_or(0);
    Ok(available)
}

// ---------------------------------------------------------------- 证书

/// 记录一次证书签发。
pub async fn record_certificate(
    executor: impl PgExecutor<'_>,
    node_id: Uuid,
    fingerprint: &str,
    certificate_pem: &str,
    not_after: DateTime<Utc>,
) -> AppResult<NodeCertificate> {
    let cert = sqlx::query_as::<_, NodeCertificate>(&format!(
        "INSERT INTO node_certificates (id, node_id, fingerprint, certificate_pem, not_after) \
         VALUES ($1, $2, $3, $4, $5) RETURNING {CERT_COLUMNS}"
    ))
    .bind(Uuid::new_v4())
    .bind(node_id)
    .bind(fingerprint)
    .bind(certificate_pem)
    .bind(not_after)
    .fetch_one(executor)
    .await?;
    Ok(cert)
}

/// 查找节点当前最有效的一张证书（未撤销且未过期），返回 (fingerprint, certificate_pem, not_after)。
pub async fn find_active_certificate(
    executor: impl PgExecutor<'_>,
    node_id: Uuid,
) -> AppResult<Option<(String, String, DateTime<Utc>)>> {
    let row: Option<(String, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT fingerprint, certificate_pem, not_after FROM node_certificates \
         WHERE node_id = $1 AND revoked_at IS NULL AND not_after > now() \
         ORDER BY issued_at DESC LIMIT 1",
    )
    .bind(node_id)
    .fetch_optional(executor)
    .await?;
    Ok(row)
}

/// 某节点的证书历史。响应里不含证书 PEM 本体，避免无谓地把它散播出去。
pub async fn list_certificates(
    executor: impl PgExecutor<'_>,
    node_id: Uuid,
) -> AppResult<Vec<NodeCertificate>> {
    let certs = sqlx::query_as::<_, NodeCertificate>(&format!(
        "SELECT {CERT_COLUMNS} FROM node_certificates WHERE node_id = $1 ORDER BY issued_at DESC"
    ))
    .bind(node_id)
    .fetch_all(executor)
    .await?;
    Ok(certs)
}

/// 统计当前处于「待审核」状态的节点数量。
pub async fn count_pending_nodes(executor: impl PgExecutor<'_>) -> AppResult<i64> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM worker_nodes WHERE registration_status = '待审核'",
    )
    .fetch_one(executor)
    .await?;
    Ok(count)
}

/// 撤销证书。撤销后该指纹立即不被接受。
pub async fn revoke_certificate(
    executor: impl PgExecutor<'_>,
    fingerprint: &str,
    reason: &str,
) -> AppResult<()> {
    let affected = sqlx::query(
        "UPDATE node_certificates SET revoked_at = now(), revoke_reason = $2 \
         WHERE fingerprint = $1 AND revoked_at IS NULL",
    )
    .bind(fingerprint)
    .bind(reason)
    .execute(executor)
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(AppError::missing("证书不存在或已撤销"));
    }
    Ok(())
}

/// 指纹是否仍然有效（未撤销且未过期），并返回它属于哪个节点。
pub async fn fingerprint_owner(
    executor: impl PgExecutor<'_>,
    fingerprint: &str,
) -> AppResult<Option<Uuid>> {
    let node_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT node_id FROM node_certificates \
         WHERE fingerprint = $1 AND revoked_at IS NULL AND not_after > now()",
    )
    .bind(fingerprint)
    .fetch_optional(executor)
    .await?;
    Ok(node_id)
}

// ---------------------------------------------------------------- 节点配置

/// 保存一份节点配置并把版本号加一，返回新版本号。
///
/// 版本号用「当前最大版本 + 1」而不是时间戳：Worker 用它做幂等比较，
/// 单调递增的整数比时间戳更容易在日志里看懂谁比谁新。
pub async fn publish_config(
    tx: &mut sqlx::PgConnection,
    node_id: Uuid,
    payload: &serde_json::Value,
) -> AppResult<String> {
    let current: String =
        sqlx::query_scalar("SELECT config_version FROM worker_nodes WHERE id = $1 FOR UPDATE")
            .bind(node_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| AppError::missing("节点不存在"))?;
    let next = current.parse::<u64>().unwrap_or(0) + 1;
    let next = next.to_string();

    sqlx::query(
        "INSERT INTO node_config_versions (id, node_id, version, payload) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (node_id, version) DO UPDATE SET payload = $4",
    )
    .bind(Uuid::new_v4())
    .bind(node_id)
    .bind(&next)
    .bind(payload)
    .execute(&mut *tx)
    .await?;

    sqlx::query("UPDATE worker_nodes SET config_version = $2, updated_at = now() WHERE id = $1")
        .bind(node_id)
        .bind(&next)
        .execute(&mut *tx)
        .await?;

    Ok(next)
}

/// 取节点当前应下发的配置。
pub async fn current_config(
    executor: impl PgExecutor<'_>,
    node_id: Uuid,
) -> AppResult<Option<(String, serde_json::Value)>> {
    let row: Option<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT v.version, v.payload FROM node_config_versions v \
         JOIN worker_nodes n ON n.id = v.node_id AND n.config_version = v.version \
         WHERE v.node_id = $1",
    )
    .bind(node_id)
    .fetch_optional(executor)
    .await?;
    Ok(row)
}
