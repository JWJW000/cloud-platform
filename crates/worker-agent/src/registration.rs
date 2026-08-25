//! V5 Worker 直连自动注册（实施方案 v5 阶段 4，第 6.4/6.9/6.10 节）。
//!
//! Worker 只配置一个 gRPC 地址即可：
//! - 无本地身份 → 生成安装标识/私钥/CSR → `RegisterNode` 提交「待审核」申请；
//! - 有待审核会话 → 复用会话轮询 `WatchRegistration`，不重复创建节点；
//! - 批准后一次性领取客户端证书 + Node CA + 节点令牌并原子落盘；
//! - 已拒绝 / 已禁用 → 停止高频重试，显示明确原因；
//! - 身份部分损坏 → 明确报「身份异常」，绝不静默重建身份绕过审核。
//!
//! 安全约束（第 6.4 节）：注册会话令牌只存本地身份文件（0600），
//! 任何日志都不输出私钥、注册会话令牌或节点令牌。

#![allow(deprecated)]

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use platform_proto::v1::worker_link_client::WorkerLinkClient;
use platform_proto::v1::{RegisterNodeRequest, WatchRegistrationRequest};
use rcgen::{CertificateParams, DistinguishedName, KeyPair};
use sha2::{Digest, Sha256};
use tokio_stream::StreamExt;
use tonic::transport::Channel;
use uuid::Uuid;

use crate::config::{SavedIdentity, WorkerConfig};
use crate::tls;

/// 默认注册查询退避秒数（Master 建议值，响应里也可能带 retry_after_seconds）。
const DEFAULT_RETRY_AFTER_SECONDS: u64 = 15;
/// 网络错误重连退避上限。
const MAX_BACKOFF_SECONDS: u64 = 60;

/// 一段可用的本地身份（私钥在磁盘上，这里只持有派生物）。
struct LocalKey {
    key_pem: String,
    csr_pem: String,
    fingerprint: String,
    installation_id: Uuid,
}

/// 注册入口：确保本地拥有「已批准」的完整身份。
///
/// - `existing == None`：全新安装，自动注册并等待审核（含批准后自动上线）。
/// - `existing` 为待审核：复用会话继续等待，不重复创建节点。
/// - `existing` 已批准：校验证书文件后直接返回。
/// - 身份损坏：报「身份异常」。
pub async fn ensure_registered(
    config: &WorkerConfig,
    existing: Option<SavedIdentity>,
) -> Result<SavedIdentity> {
    // 1. 已批准身份：校验证书/私钥文件齐全且匹配后直接返回
    if let Some(id) = &existing {
        if is_approved(id) {
            validate_approved_identity(config, id)?;
            tracing::info!(node_id = %id.node_id, "检测到已批准身份，直接上线");
            return Ok(id.clone());
        }
    }

    // 2. 身份异常：有待批准迹象但文件不全 → 不静默重建
    if let Some(id) = &existing {
        if !is_pending(id) {
            bail!(
                "身份异常：身份文件既不是待审核也不是已批准（node_id={}，安装标识={:?}）。\n\
                 如需重置请先由管理员在云端拒绝/禁用旧节点，再执行 `worker-agent reset-identity` 后重新 run。",
                id.node_id,
                id.installation_id,
            );
        }
    }

    // 3. 有私钥却没有身份文件：身份不完整，禁止静默重建（避免身份串号）
    if existing.is_none() && config.identity_paths().client_key_file.is_file() {
        bail!(
            "身份异常：存在本地私钥（{}）但没有身份文件，无法确认节点身份。\n\
             请先由管理员在云端拒绝/禁用旧节点，再执行 `worker-agent reset-identity` 后重新 run。",
            config.identity_paths().client_key_file.display()
        );
    }

    // 4. 取（或生成）本地密钥与安装标识
    let local = load_or_generate_key(config, existing.as_ref())?;

    // 5. 注册 / 恢复等待，直到批准、拒绝或过期
    let identity = register_and_wait(config, local, existing).await?;

    // 6. 批准后落盘证书与令牌（失败保留原身份）
    save_approved_identity(config, &identity)?;
    tracing::info!(node_id = %identity.node_id, "自动注册完成，身份已保存，准备上线");
    Ok(identity)
}

/// 身份是否已批准（有节点令牌即视为已批准）。
fn is_approved(id: &SavedIdentity) -> bool {
    !id.node_token.trim().is_empty()
}

/// 身份是否处于待审核（有节点编号与注册会话，尚无令牌）。
fn is_pending(id: &SavedIdentity) -> bool {
    !id.node_id.trim().is_empty() && id.node_token.trim().is_empty()
}

/// 校验已批准身份的证书/私钥文件齐全且匹配。
fn validate_approved_identity(config: &WorkerConfig, _id: &SavedIdentity) -> Result<()> {
    let paths = config.identity_paths();
    if !paths.client_cert_file.is_file() {
        bail!(
            "身份异常：身份文件显示已批准但客户端证书缺失（{}）。\
             请先由管理员在云端拒绝/禁用旧节点，再执行 `worker-agent reset-identity` 后重新 run。",
            paths.client_cert_file.display()
        );
    }
    if !paths.client_key_file.is_file() {
        bail!(
            "身份异常：身份文件显示已批准但客户端私钥缺失（{}）。",
            paths.client_key_file.display()
        );
    }
    let cert_pem = std::fs::read_to_string(&paths.client_cert_file)
        .with_context(|| format!("读取客户端证书失败: {}", paths.client_cert_file.display()))?;
    let key_pem = std::fs::read_to_string(&paths.client_key_file)
        .with_context(|| format!("读取客户端私钥失败: {}", paths.client_key_file.display()))?;
    tls::validate_cert_key_pair(&cert_pem, &key_pem, paths.client_cert_file.as_path())?;
    tls::validate_cert_not_expired(&cert_pem, paths.client_cert_file.as_path())?;
    Ok(())
}

/// 取本地密钥；没有则生成一套并落盘私钥（0600）与安装标识。
fn load_or_generate_key(
    config: &WorkerConfig,
    existing: Option<&SavedIdentity>,
) -> Result<LocalKey> {
    let paths = config.identity_paths();

    // 已有私钥：复用（必须与身份一致，杜绝身份串号）
    if paths.client_key_file.is_file() {
        let key_pem = std::fs::read_to_string(&paths.client_key_file)
            .with_context(|| format!("读取客户端私钥失败: {}", paths.client_key_file.display()))?;
        let csr_pem = build_csr_from_key(&key_pem).with_context(|| {
            format!(
                "身份异常：本地私钥无法解析（{}）。\n\
                 请先由管理员在云端拒绝/禁用旧节点，再执行 `worker-agent reset-identity` 后重新 run。",
                paths.client_key_file.display()
            )
        })?;
        let fingerprint = csr_public_key_fingerprint(&csr_pem)?;
        let installation_id = match existing.and_then(|id| id.installation_id.as_deref()) {
            Some(raw) => Uuid::parse_str(raw).with_context(|| {
                format!("身份异常：安装标识不是合法 UUID（{raw}），请执行 reset-identity 后重试")
            })?,
            None => {
                // 上游 ensure_registered 已保证「有私钥无身份文件」被拦截为身份异常，
                // 这里只处理「有身份但缺安装标识」的旧身份补记。
                let mut id = existing
                    .cloned()
                    .context("身份异常：存在私钥但身份缺失，请执行 reset-identity 后重试")?;
                tracing::warn!("身份文件缺少安装标识，正在补记（不会改变节点身份）");
                let inst = Uuid::new_v4();
                id.installation_id = Some(inst.to_string());
                config.save_identity(&id)?;
                inst
            }
        };
        return Ok(LocalKey {
            key_pem,
            csr_pem,
            fingerprint,
            installation_id,
        });
    }

    // 全新安装：生成私钥并原子落盘（0600）
    if existing.is_some() {
        bail!(
            "身份异常：身份文件存在（待审核）但本地私钥缺失（{}）。\n\
             请先由管理员在云端拒绝/禁用旧节点，再执行 `worker-agent reset-identity` 后重新 run。",
            paths.client_key_file.display()
        );
    }
    let key = KeyPair::generate().context("生成客户端私钥失败")?;
    let key_pem = key.serialize_pem();
    write_key_file(&paths.client_key_file, key_pem.as_bytes())?;
    let csr_pem = build_csr_from_key(&key_pem)?;
    let fingerprint = csr_public_key_fingerprint(&csr_pem)?;
    let installation_id = Uuid::new_v4();
    tracing::info!(
        installation_id = %installation_id,
        "已生成本机私钥与安装标识（私钥只保存在本机）"
    );
    Ok(LocalKey {
        key_pem,
        csr_pem,
        fingerprint,
        installation_id,
    })
}

/// 注册并等待审核；批准后返回完整身份（尚未落盘证书）。
async fn register_and_wait(
    config: &WorkerConfig,
    local: LocalKey,
    existing: Option<SavedIdentity>,
) -> Result<SavedIdentity> {
    let mut backoff = 1u64;

    // 有会话则先尝试恢复（避免重复注册）
    let mut pending = existing
        .filter(|id| is_pending(id) && id.registration_session.is_some())
        .map(|id| PendingSession {
            node_id: id.node_id.clone(),
            session_token: id.registration_session.clone().unwrap_or_default(),
            challenge: id.registration_challenge.clone().unwrap_or_default(),
            status: "待审核".to_string(),
        });

    loop {
        // 没有可复用会话 → 提交注册申请（幂等）
        if pending.is_none() {
            let channel = registration_channel(config).await?;
            let mut client = WorkerLinkClient::new(channel);
            let nonce = format!("reg-{}", Uuid::new_v4().simple());
            let nonce_signature = sign_hex(&local.key_pem, &nonce)?;
            let req = RegisterNodeRequest {
                installation_id: local.installation_id.to_string(),
                node_name: node_display_name(config),
                os_type: std::env::consts::OS.to_string(),
                os_version: sysinfo::System::os_version().unwrap_or_default(),
                agent_version: env!("CARGO_PKG_VERSION").to_string(),
                requested_slots: config.execution.requested_slots.clamp(1, 50),
                csr_pem: local.csr_pem.clone(),
                public_key_fingerprint: local.fingerprint.clone(),
                nonce,
                nonce_signature,
            };
            match client.register_node(req).await {
                Ok(resp) => {
                    let resp = resp.into_inner();
                    match resp.registration_status.as_str() {
                        // 已批准：Worker 应使用本地证书直接上线；无本地证书属身份异常
                        "已批准" => {
                            return Err(anyhow::anyhow!(
                                "服务器返回「已批准」但本地没有已批准身份（证书/令牌缺失）。\
                                 请先由管理员在云端拒绝/禁用旧节点，再执行 `worker-agent reset-identity` 后重新 run。"
                            ));
                        }
                        "已拒绝" | "已过期" => {
                            bail!(
                                "节点注册被拒绝（状态：{}）。请检查安装标识 {} 在云端的管理记录。",
                                resp.registration_status,
                                local.installation_id
                            );
                        }
                        _ => {}
                    }
                    if resp.registration_session.is_empty() {
                        // 服务器已有待审核会话但未返回令牌：会话令牌只在创建时下发一次。
                        // 本地没有可用的会话 = 身份/会话不一致，禁止反复注册打限流。
                        bail!(
                            "服务器已有待审核会话但未下发新令牌（node_id={}）。\
                             本地没有可用的注册会话，无法恢复等待；\
                             请先由管理员在云端处理该节点，再执行 `worker-agent reset-identity` 后重新 run。",
                            resp.node_id
                        );
                    }
                    tracing::info!(
                        node_id = %resp.node_id,
                        registration_status = %resp.registration_status,
                        "已提交直连注册申请，等待管理员审核"
                    );
                    pending = Some(PendingSession {
                        node_id: resp.node_id,
                        session_token: resp.registration_session,
                        challenge: resp.challenge,
                        status: resp.registration_status,
                    });
                    // 先落盘「待审核」身份（会话令牌明文只写本机 0600 文件）
                    persist_pending(config, &local, pending.as_ref().unwrap())?;
                }
                Err(status) => {
                    let msg = status.message();
                    if msg.contains("过于频繁") || msg.contains("上限") {
                        let wait = DEFAULT_RETRY_AFTER_SECONDS;
                        tracing::warn!(wait_secs = wait, "注册被限流：{msg}");
                        tokio::time::sleep(Duration::from_secs(wait)).await;
                        continue;
                    }
                    tracing::warn!(error = %msg, "注册请求失败，退避重试");
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF_SECONDS);
                    continue;
                }
            }
        }

        // 轮询 WatchRegistration
        let session = pending.take().unwrap();
        let event = match watch_once(config, &local, &session).await {
            Ok(ev) => ev,
            Err(err) => {
                let msg = format!("{err:#}");
                if msg.contains("限流") || msg.contains("上限") {
                    tracing::warn!(wait_secs = DEFAULT_RETRY_AFTER_SECONDS, "查询被限流");
                    tokio::time::sleep(Duration::from_secs(DEFAULT_RETRY_AFTER_SECONDS)).await;
                    pending = Some(session);
                    continue;
                }
                if msg.contains("已领取") || msg.contains("已被领取") {
                    // 会话已交付过一次：令牌明文已被服务端清空，本地若没拿到就是身份异常。
                    // 绝不静默重建身份（会绕过审核）。
                    bail!(
                        "身份异常：注册会话已被领取但本地未保存证书与令牌（节点 {}）。\n\
                         请先由管理员在云端拒绝/禁用旧节点，再执行 `worker-agent reset-identity` 后重新 run。",
                        session.node_id
                    );
                }
                // 网络/服务端瞬时错误：指数退避
                tracing::warn!(error = %err, "查询注册状态失败，退避重试");
                tokio::time::sleep(Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(MAX_BACKOFF_SECONDS);
                pending = Some(session);
                continue;
            }
        };

        match event.registration_status.as_str() {
            "待审核" => {
                tracing::info!(
                    node_id = %event.node_id,
                    expires_at = %event.expires_at,
                    "仍在等待管理员审核（建议检查时间 {}s 后再查）",
                    event.retry_after_seconds
                );
                let wait = if event.retry_after_seconds > 0 {
                    event.retry_after_seconds as u64
                } else {
                    DEFAULT_RETRY_AFTER_SECONDS
                };
                backoff = 1; // 正常等待不算故障，重置退避
                tokio::time::sleep(Duration::from_secs(wait)).await;
                pending = Some(session);
            }
            "已批准" => {
                if event.client_certificate_pem.trim().is_empty()
                    || event.ca_certificate_pem.trim().is_empty()
                    || event.node_token.trim().is_empty()
                {
                    bail!("服务器返回「已批准」但证书/令牌为空，注册状态异常，请联系管理员");
                }
                let cert_fingerprint = tls::fingerprint_der(&event.client_certificate_pem)?;
                tracing::info!(
                    node_id = %event.node_id,
                    approved_slots = event.approved_slots,
                    "管理员已批准，正在领取证书与节点令牌"
                );
                return Ok(SavedIdentity {
                    node_id: event.node_id,
                    node_token: event.node_token,
                    node_name: Some(node_display_name(config)),
                    certificate_fingerprint: Some(cert_fingerprint),
                    installation_id: Some(local.installation_id.to_string()),
                    registration_session: None, // 一次性，领取后清空
                    registration_challenge: None,
                    status: Some("已批准".to_string()),
                    client_certificate_pem: Some(event.client_certificate_pem),
                    ca_certificate_pem: Some(event.ca_certificate_pem),
                });
            }
            "已拒绝" => {
                bail!(
                    "节点注册已被管理员拒绝（原因：{}）。请联系管理员确认设备来源后重新注册。",
                    event.rejection_reason
                );
            }
            "已过期" => {
                tracing::warn!("注册申请已过期，重新发起注册");
                pending = None; // 触发重新 RegisterNode（幂等刷新）
            }
            other => {
                bail!("注册状态异常：{other}，请联系管理员");
            }
        }
    }
}

/// 单次查询注册状态。
async fn watch_once(
    config: &WorkerConfig,
    local: &LocalKey,
    session: &PendingSession,
) -> Result<platform_proto::v1::RegistrationEvent> {
    let channel = registration_channel(config).await?;
    let mut client = WorkerLinkClient::new(channel);
    let challenge_signature = sign_hex(&local.key_pem, &session.challenge)?;
    let req = WatchRegistrationRequest {
        node_id: session.node_id.clone(),
        registration_session: session.session_token.clone(),
        challenge: session.challenge.clone(),
        challenge_signature,
    };
    let mut stream = client.watch_registration(req).await?.into_inner();
    let event = stream
        .next()
        .await
        .context("WatchRegistration 流未返回事件")?
        .context("读取注册状态事件失败")?;
    Ok(event)
}

/// 注册专用通道（服务端 TLS，不携带客户端证书；本地 insecure 联调例外）。
async fn registration_channel(config: &WorkerConfig) -> Result<Channel> {
    tls::enrollment_channel(config).await
}

/// 落盘待审核身份（0600）。
fn persist_pending(
    config: &WorkerConfig,
    local: &LocalKey,
    session: &PendingSession,
) -> Result<()> {
    let id = SavedIdentity {
        node_id: session.node_id.clone(),
        node_token: String::new(),
        node_name: Some(node_display_name(config)),
        certificate_fingerprint: None,
        installation_id: Some(local.installation_id.to_string()),
        registration_session: Some(session.session_token.clone()),
        registration_challenge: Some(session.challenge.clone()),
        status: Some(session.status.clone()),
        client_certificate_pem: None,
        ca_certificate_pem: None,
    };
    config.save_identity(&id)?;
    Ok(())
}

/// 批准后落盘证书、Node CA 与完整身份（私钥已在注册期落盘）。
fn save_approved_identity(config: &WorkerConfig, id: &SavedIdentity) -> Result<()> {
    let cert_pem = id
        .client_certificate_pem
        .as_deref()
        .context("已批准身份缺少客户端证书 PEM")?;
    let ca_pem = id
        .ca_certificate_pem
        .as_deref()
        .context("已批准身份缺少 Node CA PEM")?;
    let paths = config.identity_paths();

    // 落盘前校验（复用 V4 的校验链）
    let key_pem = std::fs::read_to_string(&paths.client_key_file)
        .with_context(|| format!("读取客户端私钥失败: {}", paths.client_key_file.display()))?;
    tls::validate_cert_key_pair(cert_pem, &key_pem, paths.client_cert_file.as_path())?;
    tls::validate_chain_signed_by(cert_pem, ca_pem)?;
    tls::validate_cert_not_expired(cert_pem, paths.client_cert_file.as_path())?;

    write_file_atomic(&paths.client_cert_file, cert_pem.as_bytes())?;
    write_file_atomic(&paths.node_ca_file, ca_pem.as_bytes())?;

    // 身份文件为提交点：证书文件先就位，最后写身份（不含 PEM 副本，避免冗余）
    let mut final_id = id.clone();
    final_id.client_certificate_pem = None;
    final_id.ca_certificate_pem = None;
    config.save_identity(&final_id)?;
    Ok(())
}

/// 节点显示名：优先取系统主机名。
fn node_display_name(config: &WorkerConfig) -> String {
    sysinfo::System::host_name()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| config.storage.nas_mount.to_string_lossy().into_owned())
}

/// 从已有私钥构造 CSR（同一公钥 → 指纹稳定）。
fn build_csr_from_key(key_pem: &str) -> Result<String> {
    let key = KeyPair::from_pem(key_pem).context("解析本地私钥失败")?;
    let mut params = CertificateParams::new(Vec::<String>::new()).context("构造 CSR 参数失败")?;
    let mut dn = DistinguishedName::new();
    dn.push(
        rcgen::DnType::CommonName,
        format!(
            "worker-{}",
            sysinfo::System::host_name().unwrap_or_else(|| "node".into())
        ),
    );
    params.distinguished_name = dn;
    let csr = params.serialize_request(&key).context("生成 CSR 失败")?;
    csr.pem().context("序列化 CSR PEM 失败")
}

/// 计算 CSR 公钥指纹（与 Master 侧算法一致：SPKI 公钥的 SHA-256，小写 hex）。
fn csr_public_key_fingerprint(csr_pem: &str) -> Result<String> {
    let public_key = csr_public_key(csr_pem)?;
    let mut hasher = Sha256::new();
    hasher.update(&public_key);
    Ok(hex::encode(hasher.finalize()))
}

fn csr_public_key(csr_pem: &str) -> Result<Vec<u8>> {
    use x509_parser::certification_request::X509CertificationRequest;
    use x509_parser::pem::parse_x509_pem;
    use x509_parser::prelude::FromDer;
    let (_, pem) = parse_x509_pem(csr_pem.as_bytes()).context("CSR 不是合法 PEM")?;
    let (_, csr) = X509CertificationRequest::from_der(&pem.contents).context("CSR DER 解析失败")?;
    Ok(csr
        .certification_request_info
        .subject_pki
        .subject_public_key
        .data
        .to_vec())
}

/// 用私钥对消息做 ECDSA P-256 签名（ASN.1 DER，hex 编码）——私钥持有证明。
fn sign_hex(key_pem: &str, message: &str) -> Result<String> {
    let key = KeyPair::from_pem(key_pem).context("解析本地私钥失败")?;
    let der = key.serialize_der();
    let kp = ring::signature::EcdsaKeyPair::from_pkcs8(
        &ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING,
        &der,
        &ring::rand::SystemRandom::new(),
    )
    .map_err(|e| anyhow::anyhow!("构造签名器失败：{e}"))?;
    let sig = kp
        .sign(&ring::rand::SystemRandom::new(), message.as_bytes())
        .map_err(|e| anyhow::anyhow!("私钥签名失败：{e}"))?;
    Ok(hex::encode(sig))
}

/// 写文件并 fsync（0600 权限，Unix）。
fn write_file_atomic(path: &Path, content: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_file_name(format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        Uuid::new_v4().simple()
    ));
    write_key_file(&temp, content)?;
    std::fs::rename(&temp, path).with_context(|| format!("切换文件失败: {}", path.display()))?;
    Ok(())
}

/// 写私钥/证书文件（0600，Unix）。
fn write_key_file(path: &Path, content: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("创建文件失败: {}", path.display()))?;
        file.write_all(content)?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, content)
            .with_context(|| format!("写入文件失败: {}", path.display()))?;
    }
    Ok(())
}

/// 待审核会话的轻量描述。
struct PendingSession {
    node_id: String,
    session_token: String,
    challenge: String,
    status: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_matches_master_algorithm() {
        // 同一把 key 生成的 CSR 指纹必须稳定
        let key = KeyPair::generate().unwrap();
        let csr1 = build_csr_from_key(&key.serialize_pem()).unwrap();
        let csr2 = build_csr_from_key(&key.serialize_pem()).unwrap();
        let fp1 = csr_public_key_fingerprint(&csr1).unwrap();
        let fp2 = csr_public_key_fingerprint(&csr2).unwrap();
        assert_eq!(fp1.len(), 64);
        assert_eq!(fp1, fp2, "同一私钥的 CSR 指纹必须稳定（重启不换身份）");
    }

    #[test]
    fn sign_hex_roundtrip() {
        let key = KeyPair::generate().unwrap();
        let pem = key.serialize_pem();
        let sig = sign_hex(&pem, "challenge-abc").unwrap();
        // P-256 ASN.1 DER 签名 70-72 字节（r/s 各 32-33 字节 + 序列头，视前导零而定）
        let bytes = hex::decode(&sig).unwrap();
        assert!(
            (70..=72).contains(&bytes.len()),
            "P-256 ASN.1 签名应为 70-72 字节，实际 {}",
            bytes.len()
        );
        // 换一个消息签名必须不同（防重放语义）
        let sig2 = sign_hex(&pem, "challenge-abc-2").unwrap();
        assert_ne!(sig, sig2);
    }

    #[test]
    fn pending_vs_approved_classification() {
        let pending = SavedIdentity {
            node_id: "n1".to_string(),
            node_token: String::new(),
            node_name: None,
            certificate_fingerprint: None,
            installation_id: None,
            registration_session: Some("s".to_string()),
            registration_challenge: Some("c".to_string()),
            status: Some("待审核".to_string()),
            client_certificate_pem: None,
            ca_certificate_pem: None,
        };
        assert!(is_pending(&pending));
        assert!(!is_approved(&pending));

        let approved = SavedIdentity {
            node_token: "t".to_string(),
            ..pending.clone()
        };
        assert!(is_approved(&approved));
        assert!(!is_pending(&approved));
    }
}
