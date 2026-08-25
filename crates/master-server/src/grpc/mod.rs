//! gRPC 服务实现与生命周期管理（第 13 节）。
//!
//! 包含两个主要 rpc：
//! - `Enroll`: 节点注册（先于 mTLS，用注册码换取 token 和证书）；
//! - `OpenLink`: 双向长连接流（mTLS + token 鉴权，负责心跳、会话调度与结果上报）。

pub mod auth;
pub mod convert;
pub mod enroll;
pub mod ensure_registration;
pub mod inbound;
pub mod link_identity;
pub mod registration;

use std::pin::Pin;

use futures::Stream;
use platform_proto::v1 as pb;
use platform_proto::v1::worker_link_server::{WorkerLink, WorkerLinkServer};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status, Streaming};

use crate::state::AppState;
use crate::store;

/// WorkerLink gRPC 服务实现。
#[derive(Clone)]
pub struct WorkerLinkService {
    state: AppState,
}

impl WorkerLinkService {
    /// 新建 gRPC 服务实例。
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// 包装为 tonic 服务。
    pub fn into_server(self) -> WorkerLinkServer<Self> {
        WorkerLinkServer::new(self)
    }
}

type MasterMessageStream =
    Pin<Box<dyn Stream<Item = Result<pb::MasterMessage, Status>> + Send + 'static>>;
type RegistrationEventStream =
    Pin<Box<dyn Stream<Item = Result<pb::RegistrationEvent, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl WorkerLink for WorkerLinkService {
    type OpenLinkStream = MasterMessageStream;
    type WatchRegistrationStream = RegistrationEventStream;

    /// V7 幂等注册与状态查询（先于 mTLS）。
    async fn ensure_registration(
        &self,
        request: Request<pb::EnsureRegistrationRequest>,
    ) -> Result<Response<pb::EnsureRegistrationResponse>, Status> {
        ensure_registration::ensure_registration(&self.state, request)
            .await
            .map(Response::new)
            .map_err(convert::to_status)
    }

    /// 节点初次注册（第 15.1 节，标记弃用，保留兼容）。
    async fn enroll(
        &self,
        request: Request<pb::EnrollRequest>,
    ) -> Result<Response<pb::EnrollResponse>, Status> {
        let req = request.into_inner();
        enroll::enroll(&self.state, req)
            .await
            .map(Response::new)
            .map_err(convert::to_status)
    }

    /// V5 直连注册申请（先于 mTLS）。
    async fn register_node(
        &self,
        request: Request<pb::RegisterNodeRequest>,
    ) -> Result<Response<pb::RegisterNodeResponse>, Status> {
        registration::register_node(&self.state, request)
            .await
            .map(Response::new)
            .map_err(convert::to_status)
    }

    /// V5 审批结果查询（先于 mTLS；批准后一次性领取证书与节点令牌）。
    async fn watch_registration(
        &self,
        request: Request<pb::WatchRegistrationRequest>,
    ) -> Result<Response<Self::WatchRegistrationStream>, Status> {
        let req = request.into_inner();
        let state = self.state.clone();
        let stream = futures::stream::once(async move {
            registration::watch_registration(&state, req)
                .await
                .map_err(convert::to_status)
        });
        Ok(Response::new(Box::pin(stream)))
    }

    /// 节点双向长连接（第 13.1 节）。
    ///
    /// V7：正式任务链路通过可信客户端证书指纹鉴权。
    async fn open_link(
        &self,
        request: Request<Streaming<pb::WorkerMessage>>,
    ) -> Result<Response<Self::OpenLinkStream>, Status> {
        let metadata = request.metadata();
        let identity = link_identity::authenticate_link(&self.state, metadata, true)
            .await
            .map_err(convert::to_status)?;

        let node_id = identity.node_id();
        let mut in_stream = request.into_inner();

        // 建立下行通道并登记
        let (out_tx, out_rx) = crate::state::command_channel();
        let reg = self.state.links.register(node_id, out_tx.clone());

        // 标记数据库连接状态为在线
        if let Err(e) = store::node::set_connected(&self.state.pool, node_id, true).await {
            tracing::warn!(node_id = %node_id, error = %e, "更新节点连接状态失败");
        }

        // 下发最新节点运行参数
        inbound::send_node_config(&self.state, &identity.node, &out_tx).await;

        let state_clone = self.state.clone();
        let identity_clone = identity.clone();
        let out_tx_clone = out_tx.clone();

        // 启动后台协程读取上行流
        tokio::spawn(async move {
            tracing::info!(node_id = %node_id, "Worker 节点 gRPC 长连接已建立");

            while let Some(msg_res) = in_stream.next().await {
                match msg_res {
                    Ok(msg) => {
                        if let Err(err) =
                            inbound::dispatch(&state_clone, &identity_clone, msg, &out_tx_clone)
                                .await
                        {
                            tracing::error!(
                                node_id = %node_id,
                                error = %err,
                                "处理 Worker 上行消息出错"
                            );
                        }
                    }
                    Err(status) => {
                        tracing::warn!(
                            node_id = %node_id,
                            status = %status,
                            "Worker 上行流接收异常"
                        );
                        break;
                    }
                }
            }

            // 链路断开清理
            tracing::info!(node_id = %node_id, "Worker 节点 gRPC 长连接已断开");
            state_clone.links.unregister(&reg.token);

            // 如果此时没有更新的连接接替，将数据库状态置为断开
            if !state_clone.links.is_online(node_id) {
                if let Err(e) = store::node::set_connected(&state_clone.pool, node_id, false).await
                {
                    tracing::warn!(node_id = %node_id, error = %e, "更新节点断开状态失败");
                }
                state_clone
                    .events
                    .publish("节点变更", serde_json::json!({ "节点": node_id }));
            }
        });

        let output_stream = ReceiverStream::new(out_rx).map(Ok);
        Ok(Response::new(Box::pin(output_stream)))
    }
}
