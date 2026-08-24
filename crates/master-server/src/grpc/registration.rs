//! V5 直连注册 gRPC 处理器（实施方案 v5 第 6.5–6.8 节）。
//!
//! - `RegisterNode`：Worker 只配置一个 gRPC 地址即可提交注册申请。
//!   幂等（相同安装标识+公钥不重复建节点）、限流、CSR/指纹/私钥持有证明校验、
//!   创建「待审核」节点与短期注册会话。**待审核阶段不签发正式证书**。
//! - `WatchRegistration`：注册会话令牌 + 私钥持有证明校验，返回当前状态；
//!   批准后一次性下发客户端证书、CA 与节点令牌（防越权领取：会话已领取即失效）。
//!
//! 安全约束（第 6.8 节）：
//! - 来源 IP / 安装标识双维度限流；
//! - 全局待审核节点数量上限；
//! - nonce/challenge 签名必须通过（私钥持有证明）；
//! - 明文令牌只在响应中出现一次，库中只存哈希；日志不输出令牌与私钥。

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use platform_domain::{LogLevel, OperationSource};
use platform_proto::v1 as pb;
use tonic::Request;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::security::csr;
use crate::security::{hash_node_token, new_node_token};
use crate::state::AppState;
use crate::store;

/// 节点显示名最大长度。
const MAX_NODE_NAME: usize = 64;
/// CSR 大小上限。
const MAX_CSR_BYTES: usize = 8192;
/// 全局待审核节点数量上限（防恶意注册撑爆审核队列）。
const MAX_PENDING_NODES: usize = 500;
/// 单个注册会话查询失败次数上限（防暴力）。
const MAX_SESSION_ATTEMPTS: i32 = 50;
/// 注册申请保留天数。
const REGISTRATION_RETENTION_DAYS: i64 = 7;
/// 注册查询退避秒数。
const RETRY_AFTER_SECONDS: u32 = 15;

/// 注册限流：来源 IP 与安装标识双维度，固定窗口计数。
struct RegistrationRateLimiter {
    entries: Mutex<HashMap<String, Vec<Instant>>>,
}

const RATE_WINDOW: Duration = Duration::from_secs(10 * 60);
const MAX_REQUESTS: usize = 30;

impl RegistrationRateLimiter {
    fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn check(&self, key: &str) -> bool {
        let mut map = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        map.retain(|_, list| {
            list.retain(|&t| now.duration_since(t) < RATE_WINDOW);
            !list.is_empty()
        });
        if map.len() >= 50_000 {
            return false; // 防内存耗尽
        }
        let list = map.entry(key.to_string()).or_default();
        if list.len() >= MAX_REQUESTS {
            return false;
        }
        list.push(now);
        true
    }
}

static RATE_LIMITER: LazyLock<RegistrationRateLimiter> =
    LazyLock::new(RegistrationRateLimiter::new);

/// 解析客户端来源 IP：可信代理网段内取 X-Forwarded-For，否则取 TCP 对端。
fn client_ip(state: &AppState, request: &Request<pb::RegisterNodeRequest>) -> String {
    let peer = request.remote_addr().map(|a| a.ip());
    let trust_xff = !state.config.server.trusted_proxies.is_empty()
        && peer
            .map(|ip| {
                state.config.server.trusted_proxies.iter().any(|entry| {
                    entry
                        .parse::<ipnet::IpNet>()
                        .map(|cidr| cidr.contains(&ip))
                        .unwrap_or(false)
                        || entry
                            .parse::<std::net::IpAddr>()
                            .map(|a| a == ip)
                            .unwrap_or(false)
                })
            })
            .unwrap_or(false);
    if trust_xff {
        if let Some(xff) = request
            .metadata()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
        {
            if let Some(first) = xff.split(',').next().map(|s| s.trim()) {
                if !first.is_empty() {
                    return first.to_string();
                }
            }
        }
    }
    peer.map(|ip| ip.to_string())
        .unwrap_or_else(|| "未知".to_string())
}

/// 节点名清洗（与旧注册路径一致）。
fn node_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control() && *c != '\'' && *c != '"')
        .collect();
    let cleaned = cleaned.trim();
    let name: String = cleaned.chars().take(MAX_NODE_NAME).collect();
    if name.is_empty() {
        "未命名节点".to_string()
    } else {
        name
    }
}

/// 处理一次注册申请。
pub async fn register_node(
    state: &AppState,
    request: Request<pb::RegisterNodeRequest>,
) -> AppResult<pb::RegisterNodeResponse> {
    let req = request.get_ref();
    let ip = client_ip(state, &request);

    // 1. 输入校验
    let installation_id = Uuid::parse_str(req.installation_id.trim())
        .map_err(|_| AppError::bad("安装标识不是合法 UUID"))?;
    if req.node_name.trim().is_empty() {
        return Err(AppError::bad("节点名称不能为空"));
    }
    let os = crate::grpc::enroll::os_label(&req.os_type);
    if os == "未知" {
        return Err(AppError::bad("操作系统标识无法识别"));
    }
    let agent_version = req.agent_version.trim();
    if agent_version.is_empty() {
        return Err(AppError::bad("Agent 版本不能为空"));
    }
    let requested_slots = req.requested_slots.clamp(1, 50) as i32;
    if req.csr_pem.len() > MAX_CSR_BYTES {
        return Err(AppError::bad("CSR 大小超出合理限制"));
    }
    if req.nonce.trim().is_empty() || req.nonce.len() > 256 {
        return Err(AppError::bad("nonce 非法"));
    }

    // 2. 限流（IP + 安装标识）
    if !RATE_LIMITER.check(&format!("ip:{ip}"))
        || !RATE_LIMITER.check(&format!("inst:{installation_id}"))
    {
        return Err(AppError::too_many(
            "注册请求过于频繁，请稍后再试".to_string(),
        ));
    }

    // 3. CSR 校验：解析、公钥指纹复核、私钥持有证明（nonce 签名）
    let computed_fingerprint =
        csr::csr_public_key_fingerprint(&req.csr_pem).map_err(|e| AppError::bad(e.to_string()))?;
    let claimed = req.public_key_fingerprint.trim().to_ascii_lowercase();
    if !claimed.is_empty() && claimed != computed_fingerprint {
        return Err(AppError::bad("公钥指纹与 CSR 不符，拒绝注册"));
    }
    csr::verify_public_key_signature(&req.csr_pem, &req.nonce, &req.nonce_signature)
        .map_err(|_| AppError::bad("CSR 私钥持有证明验证失败，拒绝注册"))?;

    let mut tx = state.pool.begin().await?;

    // 4. 幂等：按安装标识查找既有节点
    if let Some(existing) =
        store::registration::find_node_by_installation(&mut *tx, installation_id).await?
    {
        // 4a. 相同安装标识 + 公钥变化 → 安全异常，不自动替换（第 6.3 节）
        if existing
            .public_key_fingerprint
            .as_deref()
            .map(|f| f != computed_fingerprint)
            .unwrap_or(true)
        {
            tx.rollback().await?;
            store::admin::log(
                &state.pool,
                OperationSource::Worker,
                LogLevel::Warn,
                &existing.name,
                "注册身份异常",
                &existing.id.to_string(),
                &format!("安装标识 {installation_id} 的公钥发生变化（IP {ip}），拒绝自动替换身份"),
            )
            .await?;
            return Err(AppError::conflict(
                "该安装标识已绑定其他公钥，疑似身份异常；请联系管理员确认重新注册",
            ));
        }
        // 4b. 已拒绝/已禁用节点不得绕过原决定（第 6.3 节）
        if existing.registration_status == "已拒绝" || existing.status == "已禁用" {
            tx.rollback().await?;
            store::admin::log(
                &state.pool,
                OperationSource::Worker,
                LogLevel::Warn,
                &existing.name,
                "注册被拒后重复申请",
                &existing.id.to_string(),
                &format!("来源 IP {ip}，原决定：{}", existing.registration_status),
            )
            .await?;
            return Err(AppError::conflict("该节点已被拒绝或禁用，无法重新注册"));
        }
        // 4c. 已批准节点重复注册（同安装+同公钥）：返回已批准状态，
        //     Worker 应使用本地已保存的证书与令牌直接 OpenLink；本地身份丢失
        //     属「身份异常」，走 reset-identity + 人工处理（第 6.9 节）。
        if existing.registration_status == "已批准" {
            tx.commit().await?;
            return Ok(pb::RegisterNodeResponse {
                node_id: existing.id.to_string(),
                registration_status: "已批准".to_string(),
                registration_session: String::new(),
                challenge: String::new(),
                expires_at: String::new(),
                retry_after_seconds: 0,
            });
        }
        // 幂等：复用既有节点，刷新注册信息并新建会话（若旧的已过期/领取）
        let active = store::registration::find_active_session(&mut *tx, existing.id).await?;
        if let Some(session) = active {
            // 已有待审核会话：返回原会话（不重复创建节点/会话）
            let expires_at = session.expires_at.to_rfc3339();
            tx.commit().await?;
            return Ok(pb::RegisterNodeResponse {
                node_id: existing.id.to_string(),
                registration_status: existing.registration_status.clone(),
                registration_session: String::new(), // 明文令牌只在创建时返回一次
                challenge: String::new(),
                expires_at,
                retry_after_seconds: RETRY_AFTER_SECONDS,
            });
        }
        // 会话已过期/已领取：刷新节点并新建会话
        store::registration::refresh_registration(
            &mut tx,
            existing.id,
            installation_id,
            &computed_fingerprint,
            requested_slots,
            Some(&ip),
            REGISTRATION_RETENTION_DAYS,
        )
        .await?;
        let (session_token, session, challenge) = create_session(
            state,
            &mut tx,
            existing.id,
            &req.csr_pem,
            &computed_fingerprint,
        )
        .await?;
        store::node::ensure_slots(&mut tx, existing.id, requested_slots).await?;
        store::node::publish_config(
            &mut tx,
            existing.id,
            &config_snapshot(state, requested_slots),
        )
        .await?;
        tx.commit().await?;
        return Ok(response(
            existing.id,
            "待审核",
            session_token,
            challenge,
            session.expires_at,
        ));
    }

    // 5. 全局待审核上限
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM worker_nodes WHERE registration_status = '待审核'",
    )
    .fetch_one(&mut *tx)
    .await?;
    if pending >= MAX_PENDING_NODES as i64 {
        tx.rollback().await?;
        return Err(AppError::too_many(
            "待审核节点数量已达上限，请联系管理员处理后再注册",
        ));
    }

    // 6. 新建节点（待审核）+ 注册会话（V5：名称只是显示名，绝不按名称合并，
    //    身份唯一性由安装标识/公钥指纹的部分唯一索引保证，见 0004/0005 迁移）
    let name = node_name(req.node_name.trim());
    let node = store::node::insert_node(
        &mut tx,
        &name,
        req.node_name.trim(),
        os,
        req.os_version.trim(),
        agent_version,
        requested_slots,
    )
    .await?;
    store::registration::refresh_registration(
        &mut tx,
        node.id,
        installation_id,
        &computed_fingerprint,
        requested_slots,
        Some(&ip),
        REGISTRATION_RETENTION_DAYS,
    )
    .await?;
    store::node::ensure_slots(&mut tx, node.id, requested_slots).await?;
    let (session_token, session, challenge) =
        create_session(state, &mut tx, node.id, &req.csr_pem, &computed_fingerprint).await?;
    store::node::publish_config(&mut tx, node.id, &config_snapshot(state, requested_slots)).await?;
    tx.commit().await?;

    store::admin::log(
        &state.pool,
        OperationSource::Worker,
        LogLevel::Info,
        &name,
        "节点注册申请",
        &node.id.to_string(),
        &format!(
            "安装标识 {installation_id}，系统 {os}，Agent {agent_version}，申请槽位 {requested_slots}，来源 IP {ip}"
        ),
    )
    .await?;
    state
        .events
        .publish("节点变更", serde_json::json!({ "节点": node.id }));

    Ok(response(
        node.id,
        "待审核",
        session_token,
        challenge,
        session.expires_at,
    ))
}

/// 创建注册会话，返回（明文令牌，会话行，挑战值）。
async fn create_session(
    _state: &AppState,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    node_id: Uuid,
    csr_pem: &str,
    fingerprint: &str,
) -> AppResult<(String, crate::models::RegistrationSession, String)> {
    // 会话令牌：至少 256 位随机熵；库中只存哈希（第 6.4 节）
    let session_token = new_node_token();
    let token_hash = hash_node_token(&session_token);
    let challenge = csr::new_challenge();
    let session = store::registration::create_registration_session(
        tx,
        node_id,
        &token_hash,
        csr_pem,
        fingerprint,
        &challenge,
    )
    .await?;
    Ok((session_token, session, challenge))
}

fn response(
    node_id: Uuid,
    status: &str,
    session_token: String,
    challenge: String,
    expires_at: chrono::DateTime<chrono::Utc>,
) -> pb::RegisterNodeResponse {
    pb::RegisterNodeResponse {
        node_id: node_id.to_string(),
        registration_status: status.to_string(),
        registration_session: session_token,
        challenge,
        expires_at: expires_at.to_rfc3339(),
        retry_after_seconds: RETRY_AFTER_SECONDS,
    }
}

/// 审批结果查询（第 6.5 节）：会话令牌 + 私钥持有证明。
pub async fn watch_registration(
    state: &AppState,
    req: pb::WatchRegistrationRequest,
) -> AppResult<pb::RegistrationEvent> {
    let node_id =
        Uuid::parse_str(req.node_id.trim()).map_err(|_| AppError::bad("节点编号不是合法 UUID"))?;
    if req.registration_session.trim().is_empty() {
        return Err(AppError::bad("注册会话令牌不能为空"));
    }
    let token_hash = hash_node_token(req.registration_session.trim());

    // 1. 取会话（令牌哈希）
    let session = store::registration::find_session_by_token_hash(&state.pool, &token_hash)
        .await?
        .ok_or_else(|| AppError::unauthorized("注册会话不存在或已失效"))?;
    if session.node_id != node_id {
        return Err(AppError::unauthorized("注册会话与节点不匹配"));
    }
    // 「已领取」且令牌明文已清空 = 已交付过；还有明文 = 批准后待首次领取，放行到下方分支。
    if session.status == "已领取" && session.pending_node_token.is_none() {
        return Err(AppError::conflict("注册会话已被领取，请勿重复查询"));
    }
    if session.status == "已拒绝" {
        return Err(AppError::forbidden("注册已被拒绝"));
    }
    if session.expires_at < chrono::Utc::now() {
        // 会话过期：标记过期并拒绝
        let mut tx = state.pool.begin().await?;
        sqlx::query("UPDATE worker_registration_sessions SET status = '已过期' WHERE id = $1")
            .bind(session.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Err(AppError::unauthorized("注册会话已过期"));
    }
    if session.attempt_count >= MAX_SESSION_ATTEMPTS {
        return Err(AppError::too_many(
            "注册会话查询次数过多，请等待后重试".to_string(),
        ));
    }
    store::registration::bump_session_attempt(&state.pool, session.id).await?;

    // 2. 私钥持有证明：对服务端挑战值的签名
    if req.challenge.trim() != session.challenge {
        return Err(AppError::unauthorized("挑战值不匹配"));
    }
    csr::verify_public_key_signature(&session.csr_pem, &req.challenge, &req.challenge_signature)
        .map_err(|_| AppError::unauthorized("私钥持有证明验证失败"))?;

    // 3. 查询节点当前注册状态
    let node = store::node::get_node(&state.pool, node_id).await?;

    match node.registration_status.as_str() {
        "已批准" => {
            // 批准后一次性领取证书、CA 与节点令牌；会话已标记「已领取」，
            // 明文令牌只在下发这一次时存在（领取后清空，防止重复领取/越权领取）。
            let mut tx = state.pool.begin().await?;
            let cert: Option<String> = sqlx::query_scalar(
                "SELECT certificate_pem FROM node_certificates WHERE node_id = $1 \
                     ORDER BY issued_at DESC LIMIT 1",
            )
            .bind(node_id)
            .fetch_optional(&mut *tx)
            .await?;
            let cert =
                cert.ok_or_else(|| AppError::conflict("节点已批准但证书缺失，请联系管理员"))?;
            let pending: Option<String> = sqlx::query_scalar(
                "SELECT pending_node_token FROM worker_registration_sessions WHERE id = $1",
            )
            .bind(session.id)
            .fetch_optional(&mut *tx)
            .await?
            .flatten();
            let node_token =
                pending.ok_or_else(|| AppError::conflict("节点令牌已被领取，请勿重复查询"))?;
            // 清空明文令牌：一次性交付
            sqlx::query(
                "UPDATE worker_registration_sessions SET pending_node_token = NULL WHERE id = $1",
            )
            .bind(session.id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(pb::RegistrationEvent {
                node_id: node.id.to_string(),
                registration_status: "已批准".to_string(),
                approved_slots: node.configured_slots.unwrap_or(node.max_slots).max(0) as u32,
                client_certificate_pem: cert,
                ca_certificate_pem: state.ca.certificate_pem().to_string(),
                node_token,
                rejection_reason: String::new(),
                retry_after_seconds: 0,
                expires_at: String::new(),
            })
        }
        "待审核" => Ok(pb::RegistrationEvent {
            node_id: node.id.to_string(),
            registration_status: "待审核".to_string(),
            approved_slots: 0,
            client_certificate_pem: String::new(),
            ca_certificate_pem: String::new(),
            node_token: String::new(),
            rejection_reason: String::new(),
            retry_after_seconds: RETRY_AFTER_SECONDS,
            expires_at: session.expires_at.to_rfc3339(),
        }),
        "已拒绝" => Ok(pb::RegistrationEvent {
            node_id: node.id.to_string(),
            registration_status: "已拒绝".to_string(),
            approved_slots: 0,
            client_certificate_pem: String::new(),
            ca_certificate_pem: String::new(),
            node_token: String::new(),
            rejection_reason: node
                .reject_reason
                .unwrap_or_else(|| "被管理员拒绝".to_string()),
            retry_after_seconds: 0,
            expires_at: String::new(),
        }),
        "已过期" => Err(AppError::unauthorized("注册申请已过期，请重新发起注册")),
        other => Err(AppError::conflict(format!("节点注册状态异常：{other}"))),
    }
}

/// 存档一份「下发给该节点的配置长什么样」（仅注册期的版本记账）。
fn config_snapshot(state: &AppState, slots: i32) -> serde_json::Value {
    let scheduler = state.scheduler();
    serde_json::json!({
        "槽位上限": slots,
        "上传并发": 2,
        "心跳间隔秒": scheduler.heartbeat_interval_secs,
        "会话续租秒": scheduler.session_renew_secs,
        "会话时长上限秒": scheduler.session_max_duration_secs,
        "站点地址": state.config.server.site_base,
    })
}
