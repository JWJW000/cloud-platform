//! Tonic gRPC Master 适配器（V7 实施方案第 4.3 节、第 5 节）。

use std::pin::Pin;
use std::time::Duration;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use platform_proto::v1 as pb;
use platform_proto::v1::worker_link_client::WorkerLinkClient;
use platform_proto::v1::RegistrationState;
use platform_proto::{METADATA_AGENT_VERSION, METADATA_NODE_ID, METADATA_PROTOCOL_VERSION};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;
use tonic::{Code, Request, Status, Streaming};

use crate::config::MasterLinkConfig;
use crate::master_port::{
    ClientCredential, ConnectError, EnsureRegistrationRequestDto, MasterLinkSession, MasterPort,
    RegistrationOutcome,
};
use crate::tls;

/// 基于 Tonic 的 Master 适配器。
#[derive(Debug, Clone)]
pub struct TonicMasterAdapter {
    config: MasterLinkConfig,
}

impl TonicMasterAdapter {
    /// 创建适配器。
    pub fn new(config: MasterLinkConfig) -> Self {
        Self { config }
    }

    async fn connect_registration_channel(&self) -> Result<Channel, ConnectError> {
        let endpoint_str = self
            .config
            .enroll_endpoint
            .as_deref()
            .unwrap_or(&self.config.endpoint)
            .trim();

        tls::connect_tls_endpoint(&self.config, endpoint_str, None)
            .await
            .map_err(map_transport_error)
    }

    async fn connect_mtls_channel(
        &self,
        credential: &ClientCredential,
    ) -> Result<Channel, ConnectError> {
        tls::connect_tls_endpoint(
            &self.config,
            &self.config.endpoint,
            Some((&credential.client_cert_pem, &credential.client_key_pem)),
        )
        .await
        .map_err(map_transport_error)
    }
}

#[async_trait]
impl MasterPort for TonicMasterAdapter {
    async fn ensure_registration(
        &self,
        req: EnsureRegistrationRequestDto,
    ) -> Result<RegistrationOutcome, ConnectError> {
        let channel = self.connect_registration_channel().await?;
        let mut client = WorkerLinkClient::new(channel);

        let pb_req = pb::EnsureRegistrationRequest {
            protocol_version: req.protocol_version,
            installation_id: req.installation_id.clone(),
            profile: Some(pb::NodeProfile {
                node_name: req.node_name.clone(),
                os_type: req.os_type.clone(),
                os_version: req.os_version.clone(),
                agent_version: req.agent_version.clone(),
                requested_slots: req.requested_slots,
            }),
            csr_pem: req.csr_pem.clone(),
            request_nonce: req.request_nonce.clone(),
            requested_at: req.requested_at.clone(),
            proof_signature: req.proof_signature.clone(),
            wait_seconds: req.wait_seconds,
        };

        match client.ensure_registration(Request::new(pb_req)).await {
            Ok(resp) => {
                let r = resp.into_inner();
                let state =
                    RegistrationState::try_from(r.state).unwrap_or(RegistrationState::Unspecified);
                match state {
                    RegistrationState::Pending => Ok(RegistrationOutcome::Pending {
                        node_id: r.node_id,
                        retry_after: Duration::from_secs(r.retry_after_seconds.max(5) as u64),
                    }),
                    RegistrationState::Approved => Ok(RegistrationOutcome::Approved {
                        node_id: r.node_id,
                        approved_slots: r.approved_slots.max(1),
                        client_certificate_pem: r.client_certificate_pem,
                    }),
                    RegistrationState::Rejected => Ok(RegistrationOutcome::Rejected {
                        reason: if r.rejection_reason.is_empty() {
                            "节点注册已被管理员拒绝".to_string()
                        } else {
                            r.rejection_reason
                        },
                    }),
                    RegistrationState::Expired => Ok(RegistrationOutcome::Expired),
                    RegistrationState::Unspecified => {
                        Err(ConnectError::Fatal(anyhow::anyhow!("未知注册状态响应")))
                    }
                }
            }
            Err(status) if status.code() == Code::Unimplemented => {
                // 阶段 3 兼容回退：尝试旧 V5 RegisterNode
                tracing::warn!("Master 未实现 EnsureRegistration，正在回退使用旧注册协议...");
                fallback_v5_registration(&mut client, &req).await
            }
            Err(status) => Err(map_grpc_status(status)),
        }
    }

    async fn open_link(
        &self,
        credential: &ClientCredential,
    ) -> Result<Box<dyn MasterLinkSession>, ConnectError> {
        let channel = self.connect_mtls_channel(credential).await?;
        let mut client = WorkerLinkClient::new(channel);

        let (tx, rx) = mpsc::channel::<pb::WorkerMessage>(128);
        let outbound_stream = ReceiverStream::new(rx);

        let mut request = Request::new(outbound_stream);

        // 设置协议元数据
        if let Ok(val) = MetadataValue::try_from("1") {
            request
                .metadata_mut()
                .insert(METADATA_PROTOCOL_VERSION, val);
        }
        if let Ok(val) = MetadataValue::try_from(env!("CARGO_PKG_VERSION")) {
            request.metadata_mut().insert(METADATA_AGENT_VERSION, val);
        }

        // 双栈兼容：若需要也可以塞入 node_id
        if let Ok(val) = MetadataValue::try_from(credential.node_id.as_str()) {
            request.metadata_mut().insert(METADATA_NODE_ID, val);
        }

        let response = client.open_link(request).await.map_err(map_grpc_status)?;

        let inbound = response.into_inner();
        Ok(Box::new(TonicMasterLinkSession {
            tx,
            inbound: Some(inbound),
        }))
    }
}

/// 阶段 3 双栈兼容回退处理：旧 RegisterNode
#[allow(deprecated)]
async fn fallback_v5_registration(
    client: &mut WorkerLinkClient<Channel>,
    req: &EnsureRegistrationRequestDto,
) -> Result<RegistrationOutcome, ConnectError> {
    let fp =
        tls::fingerprint_of_pem(&req.csr_pem).unwrap_or_else(|_| "legacy-fingerprint".to_string());
    let register_req = pb::RegisterNodeRequest {
        installation_id: req.installation_id.clone(),
        node_name: req.node_name.clone(),
        os_type: req.os_type.clone(),
        os_version: req.os_version.clone(),
        agent_version: req.agent_version.clone(),
        requested_slots: req.requested_slots,
        csr_pem: req.csr_pem.clone(),
        public_key_fingerprint: fp,
        nonce: req.request_nonce.clone(),
        nonce_signature: req.proof_signature.clone(),
    };

    let resp = client
        .register_node(Request::new(register_req))
        .await
        .map_err(map_grpc_status)?;

    let r = resp.into_inner();
    if r.registration_status == "已批准" {
        Ok(RegistrationOutcome::Approved {
            node_id: r.node_id,
            approved_slots: req.requested_slots,
            client_certificate_pem: String::new(),
        })
    } else if r.registration_status == "已拒绝" {
        Ok(RegistrationOutcome::Rejected {
            reason: "节点注册已被拒绝".to_string(),
        })
    } else {
        Ok(RegistrationOutcome::Pending {
            node_id: r.node_id,
            retry_after: Duration::from_secs(r.retry_after_seconds.max(5) as u64),
        })
    }
}

/// Tonic 长连接会话包装。
pub struct TonicMasterLinkSession {
    tx: mpsc::Sender<pb::WorkerMessage>,
    inbound: Option<Streaming<pb::MasterMessage>>,
}

impl MasterLinkSession for TonicMasterLinkSession {
    fn send(&mut self, msg: pb::WorkerMessage) -> Result<(), ConnectError> {
        self.tx.try_send(msg).map_err(|_| ConnectError::Network {
            retry_after: Some(Duration::from_secs(1)),
        })
    }

    fn inbound_stream(
        &mut self,
    ) -> Pin<Box<dyn Stream<Item = Result<pb::MasterMessage, ConnectError>> + Send + 'static>> {
        let inbound = self
            .inbound
            .take()
            .expect("inbound stream already consumed");
        let stream = inbound.map(|item| item.map_err(map_grpc_status));
        Box::pin(stream)
    }
}

fn map_transport_error(_e: anyhow::Error) -> ConnectError {
    ConnectError::Network {
        retry_after: Some(Duration::from_secs(3)),
    }
}

fn map_grpc_status(status: Status) -> ConnectError {
    match status.code() {
        Code::Unavailable => ConnectError::Network { retry_after: None },
        Code::ResourceExhausted => ConnectError::RateLimited {
            retry_after: Duration::from_secs(15),
        },
        Code::PermissionDenied => ConnectError::Rejected {
            reason: status.message().to_string(),
        },
        Code::Unauthenticated => ConnectError::Unauthorized,
        Code::AlreadyExists | Code::FailedPrecondition => ConnectError::IdentityConflict,
        Code::Unimplemented => ConnectError::ProtocolMismatch,
        _ => ConnectError::Fatal(anyhow::anyhow!("gRPC 错误: {}", status.message())),
    }
}
