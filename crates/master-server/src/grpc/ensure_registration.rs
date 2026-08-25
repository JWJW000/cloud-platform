//! V7 幂等直连注册 gRPC 处理器（实施方案 v7 第 5.2–5.4 节）。
//!
//! - `EnsureRegistration`：合并注册与审批状态查询，Worker 只需要这一个 RPC 即可完成注册与证书领取；
//! - 幂等性：同一 installation_id 与同一公钥重复调用，安全返回当前状态或证书；公钥突变则拒绝；
//! - 证明校验：CSR 公钥验证签名证明（包含协议版本、安装标识、CSR SHA-256、随机数与时间戳）；
//! - 证书重复领取：已批准且存在有效证书时直接返回证书，消除单次性消费导致的不可恢复死状态。

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use platform_domain::{LogLevel, OperationSource};
use platform_proto::v1 as pb;
use platform_proto::v1::RegistrationState;
use tonic::Request;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::security::csr;
use crate::state::AppState;
use crate::store;

/// 节点显示名最大长度。
const MAX_NODE_NAME: usize = 64;
/// CSR 大小上限。
const MAX_CSR_BYTES: usize = 8192;
/// 全局待审核节点数量上限（防恶意注册撑爆审核队列）。
const MAX_PENDING_NODES: usize = 500;
/// 注册申请保留天数。
const REGISTRATION_RETENTION_DAYS: i64 = 7;
/// 建议下次轮询间隔（秒）。
const RETRY_AFTER_SECONDS: u32 = 15;
/// 请求时间允许的最大偏差（秒）。
const MAX_TIME_SKEW_SECONDS: i64 = 300;

/// 注册限流器：来源 IP 与安装标识双维度。
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
            return false;
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

/// 解析客户端来源 IP。
fn client_ip<T>(state: &AppState, request: &Request<T>) -> String {
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
            if let Some(first) = xff.split(',').next() {
                let ip_str = first.trim();
                if ip_str.parse::<std::net::IpAddr>().is_ok() {
                    return ip_str.to_string();
                }
            }
        }
    }
    peer.map(|a| a.to_string())
        .unwrap_or_else(|| "127.0.0.1".to_string())
}

/// 处理 EnsureRegistration 请求。
pub async fn ensure_registration(
    state: &AppState,
    request: Request<pb::EnsureRegistrationRequest>,
) -> AppResult<pb::EnsureRegistrationResponse> {
    let ip = client_ip(state, &request);
    let req = request.into_inner();

    if req.protocol_version != 1 {
        return Err(AppError::bad(format!(
            "不支持的协议版本：{}（仅支持版本 1）",
            req.protocol_version
        )));
    }

    let installation_id = Uuid::parse_str(req.installation_id.trim())
        .map_err(|_| AppError::bad("安装标识 installation_id 必须是有效 UUID"))?;

    let profile = req
        .profile
        .ok_or_else(|| AppError::bad("缺少节点配置 profile"))?;

    let node_name = sanitize_node_name(&profile.node_name);
    let requested_slots = (profile.requested_slots as i32).clamp(1, 50);
    let os = profile.os_type.trim();
    let os_version = profile.os_version.trim();
    let agent_version = profile.agent_version.trim();

    let csr_pem = req.csr_pem.trim();
    if csr_pem.is_empty() || csr_pem.len() > MAX_CSR_BYTES {
        return Err(AppError::bad("CSR 内容为空或超出大小上限"));
    }

    // 从 CSR 计算公钥指纹
    let csr_fp = csr::csr_public_key_fingerprint(csr_pem)
        .map_err(|e| AppError::bad(format!("解析 CSR 公钥指纹失败：{e}")))?;

    // 校验请求时间偏差（5 分钟以内）
    let requested_at = DateTime::parse_from_rfc3339(req.requested_at.trim())
        .map_err(|_| AppError::bad("requested_at 不是合法的 RFC 3339 时间格式"))?
        .with_timezone(&Utc);
    let now = Utc::now();
    let skew = (now.timestamp() - requested_at.timestamp()).abs();
    if skew > MAX_TIME_SKEW_SECONDS {
        return Err(AppError::bad(format!(
            "请求时间偏差过大（偏差 {} 秒，允许最大 {} 秒）",
            skew, MAX_TIME_SKEW_SECONDS
        )));
    }

    // 校验私钥持有证明签名
    let proof_message = platform_proto::format_ensure_registration_proof(
        req.protocol_version,
        &req.installation_id,
        &csr_fp,
        &req.request_nonce,
        &req.requested_at,
    );
    csr::verify_public_key_signature(csr_pem, &proof_message, &req.proof_signature)
        .map_err(|e| AppError::unauthorized(format!("私钥持有证明签名校验失败：{e}")))?;

    // 限流检查
    if !RATE_LIMITER.check(&format!("ip:{ip}"))
        || !RATE_LIMITER.check(&format!("inst:{installation_id}"))
    {
        return Err(AppError::too_many("注册请求过于频繁，请稍后重试"));
    }

    let expires_at = Utc::now() + chrono::Duration::days(REGISTRATION_RETENTION_DAYS);

    // 查询是否已有此 installation_id
    let existing_node =
        store::registration::find_node_by_installation(&state.pool, installation_id).await?;

    let node = match existing_node {
        Some(mut existing) => {
            // 校验公钥指纹是否一致（防身份突变）
            if let Some(ref saved_fp) = existing.public_key_fingerprint {
                if saved_fp != &csr_fp {
                    tracing::warn!(
                        installation_id = %installation_id,
                        old_fp = %saved_fp,
                        new_fp = %csr_fp,
                        ip = %ip,
                        "节点公钥发生突变，拒绝自动替换"
                    );
                    return Err(AppError::conflict(format!(
                        "安装标识 {} 的公钥发生变化（IP {}），拒绝自动替换身份",
                        installation_id, ip
                    )));
                }
            }

            // 更新最近请求记录
            let mut tx = state.pool.begin().await?;
            store::registration::refresh_registration(
                &mut tx,
                existing.id,
                installation_id,
                &csr_fp,
                requested_slots,
                Some(&ip),
                REGISTRATION_RETENTION_DAYS,
            )
            .await?;
            store::registration_request::upsert_registration_request(
                &mut *tx,
                existing.id,
                installation_id,
                csr_pem,
                &csr_fp,
                Some(&ip),
                requested_slots,
                expires_at,
            )
            .await?;
            tx.commit().await?;

            existing = store::node::get_node(&state.pool, existing.id).await?;
            existing
        }
        None => {
            // 新节点：检查全局待审核上限
            let pending_count = store::node::count_pending_nodes(&state.pool).await?;
            if pending_count >= MAX_PENDING_NODES as i64 {
                return Err(AppError::too_many(
                    "云端待审核节点队列已满，请联系管理员处理后再试",
                ));
            }

            let mut tx = state.pool.begin().await?;

            // 插入新节点
            let new_node = store::node::insert_node(
                &mut tx,
                &node_name,
                &node_name,
                os,
                os_version,
                agent_version,
                requested_slots,
            )
            .await?;

            // 补充直连注册字段并设置 credential_mode 为 certificate_only
            sqlx::query(
                "UPDATE worker_nodes SET \
                     installation_id = $2, \
                     public_key_fingerprint = $3, \
                     requested_slots = $4, \
                     first_seen_ip = $5, \
                     last_registration_at = now(), \
                     registration_expires_at = $6, \
                     credential_mode = 'certificate_only', \
                     updated_at = now() \
                 WHERE id = $1",
            )
            .bind(new_node.id)
            .bind(installation_id)
            .bind(&csr_fp)
            .bind(requested_slots)
            .bind(&ip)
            .bind(expires_at)
            .execute(&mut *tx)
            .await?;

            // 插入 registration_requests 表
            store::registration_request::upsert_registration_request(
                &mut *tx,
                new_node.id,
                installation_id,
                csr_pem,
                &csr_fp,
                Some(&ip),
                requested_slots,
                expires_at,
            )
            .await?;

            // 审计日志
            store::admin::log(
                &mut *tx,
                OperationSource::Worker,
                LogLevel::Info,
                &node_name,
                "节点提交直连注册申请",
                &new_node.id.to_string(),
                &format!(
                    "安装标识 {}，系统 {}，Agent {}，申请槽位 {}，来源 IP {}",
                    installation_id, os, agent_version, requested_slots, ip
                ),
            )
            .await?;

            tx.commit().await?;

            // 发布事件
            state
                .events
                .publish("节点变更", serde_json::json!({ "节点": new_node.id }));

            let inserted = store::node::get_node(&state.pool, new_node.id).await?;
            inserted
        }
    };

    // 处理可选长轮询 wait_seconds
    let wait_secs = req.wait_seconds.min(30);
    let mut current_node = node;

    if wait_secs > 0 && current_node.registration_status == "待审核" {
        let deadline = Instant::now() + Duration::from_secs(wait_secs as u64);
        while Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(800)).await;
            if let Ok(updated) = store::node::get_node(&state.pool, current_node.id).await {
                if updated.registration_status != "待审核" {
                    current_node = updated;
                    break;
                }
            }
        }
    }

    // 根据当前状态返回相应响应
    build_response(state, &current_node, csr_pem).await
}

/// 构造响应
async fn build_response(
    state: &AppState,
    node: &crate::models::WorkerNode,
    csr_pem: &str,
) -> AppResult<pb::EnsureRegistrationResponse> {
    match node.registration_status.as_str() {
        "待审核" => Ok(pb::EnsureRegistrationResponse {
            node_id: node.id.to_string(),
            state: RegistrationState::Pending.into(),
            approved_slots: 0,
            client_certificate_pem: String::new(),
            rejection_reason: String::new(),
            retry_after_seconds: RETRY_AFTER_SECONDS,
            registration_expires_at: node
                .registration_expires_at
                .map(|t| t.to_rfc3339())
                .unwrap_or_default(),
        }),
        "已拒绝" => Ok(pb::EnsureRegistrationResponse {
            node_id: node.id.to_string(),
            state: RegistrationState::Rejected.into(),
            approved_slots: 0,
            client_certificate_pem: String::new(),
            rejection_reason: node
                .reject_reason
                .clone()
                .unwrap_or_else(|| "管理员拒绝了该节点的注册申请".to_string()),
            retry_after_seconds: 0,
            registration_expires_at: String::new(),
        }),
        "已过期" => Ok(pb::EnsureRegistrationResponse {
            node_id: node.id.to_string(),
            state: RegistrationState::Expired.into(),
            approved_slots: 0,
            client_certificate_pem: String::new(),
            rejection_reason: "注册申请已过期".to_string(),
            retry_after_seconds: 0,
            registration_expires_at: String::new(),
        }),
        "已批准" => {
            // 幂等获取或签发有效证书
            let cert_pem = get_or_issue_active_cert(state, node, csr_pem).await?;
            let approved_slots = node.configured_slots.unwrap_or(node.max_slots).max(1) as u32;

            Ok(pb::EnsureRegistrationResponse {
                node_id: node.id.to_string(),
                state: RegistrationState::Approved.into(),
                approved_slots,
                client_certificate_pem: cert_pem,
                rejection_reason: String::new(),
                retry_after_seconds: 0,
                registration_expires_at: String::new(),
            })
        }
        _ => Err(AppError::internal(format!(
            "未知注册状态：{}",
            node.registration_status
        ))),
    }
}

/// 获取现有有效证书或签发新证书
async fn get_or_issue_active_cert(
    state: &AppState,
    node: &crate::models::WorkerNode,
    csr_pem: &str,
) -> AppResult<String> {
    if let Some((_fp, cert_pem, _not_after)) =
        store::node::find_active_certificate(&state.pool, node.id).await?
    {
        if !cert_pem.trim().is_empty() {
            return Ok(cert_pem);
        }
    }

    // 优先从 worker_registration_requests 获取 CSR（如果入参为空）
    let csr_to_sign = if !csr_pem.trim().is_empty() {
        csr_pem.to_string()
    } else if let Ok(Some(req)) =
        store::registration_request::find_request_by_node_id(&state.pool, node.id).await
    {
        req.csr_pem
    } else {
        return Err(AppError::bad("缺少 CSR 无法签发证书"));
    };

    // 签发新证书
    let issued = state
        .ca
        .sign_csr(&csr_to_sign, &node.id.to_string())
        .map_err(|e| AppError::internal(format!("签发客户端证书失败：{e}")))?;

    store::node::record_certificate(
        &state.pool,
        node.id,
        &issued.fingerprint,
        &issued.certificate_pem,
        issued.not_after,
    )
    .await?;

    Ok(issued.certificate_pem)
}

fn sanitize_node_name(raw: &str) -> String {
    let trimmed = raw.trim();
    let safe: String = trimmed
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .take(MAX_NODE_NAME)
        .collect();
    if safe.is_empty() {
        "worker-node".to_string()
    } else {
        safe
    }
}
