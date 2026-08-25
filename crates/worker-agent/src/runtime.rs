//! Worker 运行时深模块与运行状态机（V7 实施方案第 4.3 节、4.4 节、第 10 节）。
//!
//! 对外只暴露 `WorkerRuntime::run(config)`，内部管理注册、审批等待、证书落盘、
//! mTLS 正式连接、现场对账与退避重连。

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use rand::Rng;
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};
use uuid::Uuid;

use crate::bus::OutboundEventBus;
use crate::config::{SavedIdentity, WorkerConfig};
use crate::credential_store::{CredentialStore, LocalCredentialState};
use crate::dynamic::ConfigState;
use crate::master_port::{
    ClientCredential, ConnectError, EnsureRegistrationRequestDto, MasterPort, RegistrationOutcome,
};
use crate::outbox::LocalStore;
use crate::slot::SlotManager;
use crate::storage::NasProbeManager;

/// 状态机运行阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimePhase {
    /// 加载本地凭据。
    LoadingLocal,
    /// 注册申请/等待审批中。
    RegistrationPending,
    /// 证书已原子落盘。
    CredentialSaved,
    /// 建立 mTLS 正式连接。
    ConnectingMtls,
    /// 现场对账中。
    Reconciling,
    /// 在线运行。
    Online,
    /// 连接断开。
    Disconnected,
    /// 故障退避中。
    BackingOff,
    /// 身份异常停止。
    StoppedIdentityError,
}

impl RuntimePhase {
    /// 结构化日志标识。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LoadingLocal => "loading_local",
            Self::RegistrationPending => "registration_pending",
            Self::CredentialSaved => "credential_saved",
            Self::ConnectingMtls => "connecting_mtls",
            Self::Reconciling => "reconciling",
            Self::Online => "online",
            Self::Disconnected => "disconnected",
            Self::BackingOff => "backing_off",
            Self::StoppedIdentityError => "stopped_identity_error",
        }
    }
}

/// Worker 唯一运行时结构。
pub struct WorkerRuntime<P, S> {
    master: P,
    credentials: S,
}

impl<P: MasterPort, S: CredentialStore> WorkerRuntime<P, S> {
    /// 构造 Worker 运行时。
    pub fn new(master: P, credentials: S) -> Self {
        Self {
            master,
            credentials,
        }
    }

    /// 启动 Worker Agent 主运行循环。
    pub async fn run(self, config: WorkerConfig) -> Result<()> {
        tracing::info!(
            phase = RuntimePhase::LoadingLocal.as_str(),
            "正在加载本地凭据..."
        );

        // 1. 本地凭据加载与推导
        let local_state = match self.credentials.load_state() {
            Ok(LocalCredentialState::Uninitialized) => {
                tracing::info!("未检测到本地凭据，正在初始化全新身份私钥与安装标识...");
                self.credentials.initialize_fresh().map_err(to_anyhow)?
            }
            Ok(state) => state,
            Err(ConnectError::LocalCredentialCorrupt(reason)) => {
                tracing::error!(
                    phase = RuntimePhase::StoppedIdentityError.as_str(),
                    reason = %reason,
                    "本地身份凭据损坏或不匹配，禁止静默覆盖"
                );
                bail!("本地凭据损坏：{reason}");
            }
            Err(e) => return Err(to_anyhow(e)),
        };

        // 2. 注册与证书获取（若缺少证书）
        let credential = match local_state {
            LocalCredentialState::Ready { credential } => {
                tracing::info!(
                    node_id = %credential.node_id,
                    installation_id = %credential.installation_id,
                    "本地已存在有效证书与私钥，直接建立正式链路"
                );
                credential
            }
            LocalCredentialState::PendingRegistration {
                installation_id,
                key_pem: _,
                csr_pem: _,
                node_id: _,
            } => {
                self.ensure_registration_loop(&config, &installation_id)
                    .await?
            }
            LocalCredentialState::Uninitialized => unreachable!(),
        };

        // 3. 正式链路连接、对账与业务消息循环
        self.run_online_loop(config, credential).await
    }

    /// 确保注册并等待审批通过领取证书。
    async fn ensure_registration_loop(
        &self,
        config: &WorkerConfig,
        installation_id: &str,
    ) -> Result<ClientCredential> {
        let node_name = sysinfo::System::host_name().unwrap_or_else(|| "worker-node".to_string());
        let os_type = std::env::consts::OS.to_string();
        let os_version = sysinfo::System::os_version().unwrap_or_default();
        let agent_version = env!("CARGO_PKG_VERSION").to_string();

        let mut backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(30);

        loop {
            let csr_pem = self.credentials.csr_pem().map_err(to_anyhow)?;
            let csr_fp = crate::tls::fingerprint_of_pem(&csr_pem)
                .map_err(|e| anyhow::anyhow!("计算 CSR 指纹失败：{e}"))?;
            let nonce = Uuid::new_v4().to_string();
            let requested_at = chrono::Utc::now().to_rfc3339();

            let proof_message = platform_proto::format_ensure_registration_proof(
                1,
                installation_id,
                &csr_fp,
                &nonce,
                &requested_at,
            );
            let proof_signature = self
                .credentials
                .sign_proof(&proof_message)
                .map_err(to_anyhow)?;

            let req = EnsureRegistrationRequestDto {
                protocol_version: 1,
                installation_id: installation_id.to_string(),
                node_name: node_name.clone(),
                os_type: os_type.clone(),
                os_version: os_version.clone(),
                agent_version: agent_version.clone(),
                requested_slots: config.execution.requested_slots,
                csr_pem,
                request_nonce: nonce,
                requested_at,
                proof_signature,
                wait_seconds: 15,
            };

            match self.master.ensure_registration(req).await {
                Ok(RegistrationOutcome::Approved {
                    node_id,
                    approved_slots: _,
                    client_certificate_pem,
                }) => {
                    tracing::info!(
                        phase = RuntimePhase::CredentialSaved.as_str(),
                        node_id = %node_id,
                        "注册申请已获批准，正在原子保存客户端证书..."
                    );

                    self.credentials
                        .save_approved_certificate(&client_certificate_pem, &node_id)
                        .map_err(to_anyhow)?;

                    let local_state = self.credentials.load_state().map_err(to_anyhow)?;
                    if let LocalCredentialState::Ready { credential } = local_state {
                        return Ok(credential);
                    } else {
                        bail!("证书保存后状态校验失败");
                    }
                }
                Ok(RegistrationOutcome::Pending {
                    node_id: _,
                    retry_after,
                }) => {
                    tracing::info!(
                        phase = RuntimePhase::RegistrationPending.as_str(),
                        retry_after_secs = retry_after.as_secs(),
                        "节点注册申请已提交，等待管理员审核中..."
                    );
                    // 正常等待，不计入故障退避
                    tokio::time::sleep(retry_after).await;
                }
                Ok(RegistrationOutcome::Rejected { reason }) => {
                    tracing::error!(
                        phase = RuntimePhase::StoppedIdentityError.as_str(),
                        reason = %reason,
                        "管理员拒绝了节点注册申请"
                    );
                    bail!("注册申请已被拒绝：{reason}");
                }
                Ok(RegistrationOutcome::Expired) => {
                    tracing::error!(
                        phase = RuntimePhase::StoppedIdentityError.as_str(),
                        "节点注册申请已过期"
                    );
                    bail!("注册申请已过期，请重新申请");
                }
                Err(ConnectError::RateLimited { retry_after }) => {
                    tracing::warn!(retry_after_secs = retry_after.as_secs(), "注册请求被限流");
                    tokio::time::sleep(retry_after).await;
                }
                Err(ConnectError::IdentityConflict) => {
                    tracing::error!(
                        phase = RuntimePhase::StoppedIdentityError.as_str(),
                        "检测到身份冲突或公钥不一致，禁止自动覆盖"
                    );
                    bail!("节点身份冲突，请联系管理员处理");
                }
                Err(ConnectError::Network { retry_after }) => {
                    let delay = retry_after.unwrap_or(backoff);
                    let jittered = add_jitter(delay);
                    tracing::warn!(
                        phase = RuntimePhase::BackingOff.as_str(),
                        retry_after_ms = jittered.as_millis() as u64,
                        "网络连接异常，退避重试注册中..."
                    );
                    tokio::time::sleep(jittered).await;
                    backoff = (backoff * 2).min(max_backoff);
                }
                Err(e) => return Err(to_anyhow(e)),
            }
        }
    }

    /// 正式 mTLS 连接长连接与业务循环。
    async fn run_online_loop(
        &self,
        config: WorkerConfig,
        credential: ClientCredential,
    ) -> Result<()> {
        let node_id = credential.node_id.clone();
        std::fs::create_dir_all(&config.storage.data_dir)?;
        let outbox = LocalStore::open(&config.storage.data_dir.join("worker.db"))?;

        let nas_probe = NasProbeManager::start(config.storage.clone(), node_id.clone()).await;
        let config_state = Arc::new(ConfigState::new());
        let (bus, mut bus_rx) = OutboundEventBus::new(outbox.clone());

        let slot_manager = Arc::new(SlotManager::new(
            config.execution.requested_slots,
            config.clone(),
            config_state.clone(),
            outbox.clone(),
            bus.clone(),
            node_id.clone(),
        ));

        let mut sys = System::new_with_specifics(
            RefreshKind::new()
                .with_cpu(CpuRefreshKind::everything())
                .with_memory(MemoryRefreshKind::everything()),
        );

        let mut backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(30);

        let legacy_identity = SavedIdentity {
            node_id: credential.node_id.clone(),
            node_token: String::new(),
            node_name: Some(sysinfo::System::host_name().unwrap_or_default()),
            certificate_fingerprint: None,
            installation_id: Some(credential.installation_id.clone()),
            registration_session: None,
            registration_challenge: None,
            status: Some("已批准".to_string()),
            client_certificate_pem: Some(credential.client_cert_pem.clone()),
            ca_certificate_pem: None,
        };

        loop {
            tracing::info!(
                phase = RuntimePhase::ConnectingMtls.as_str(),
                endpoint = %config.master.endpoint,
                node_id = %credential.node_id,
                "正在建立 mTLS 正式长连接..."
            );

            // 通过抽象端口打开 mTLS 传输会话
            let session_res = self.master.open_link(&credential).await;
            if let Err(ConnectError::Fatal(e)) = session_res {
                return Err(e);
            }

            // 调用底层 client 进行通信
            let session = crate::client::create_connection(
                &config,
                &credential.node_id,
                &legacy_identity,
                &outbox,
                &config_state,
                &slot_manager,
                &nas_probe,
            );

            let run_res = session.serve(&mut bus_rx, &mut sys).await;

            match run_res {
                Ok(()) => {
                    tracing::info!(
                        phase = RuntimePhase::Disconnected.as_str(),
                        "与 Master 的长连接已关闭，准备重连"
                    );
                    backoff = Duration::from_secs(1);
                }
                Err(err) => {
                    let jittered = add_jitter(backoff);
                    tracing::warn!(
                        phase = RuntimePhase::BackingOff.as_str(),
                        error = %err,
                        retry_after_ms = jittered.as_millis() as u64,
                        "与 Master 的长连接异常"
                    );
                    tokio::time::sleep(jittered).await;
                    backoff = (backoff * 2).min(max_backoff);
                }
            }
        }
    }
}

fn add_jitter(duration: Duration) -> Duration {
    let millis = duration.as_millis() as u64;
    let jitter = rand::thread_rng().gen_range(0..=(millis / 4).max(100));
    Duration::from_millis(millis + jitter)
}

fn to_anyhow(e: ConnectError) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}
