//! 进程内共享状态。
//!
//! 这里只放「进程活着时才存在」的东西：连接池、密钥、CA、事件总线，
//! 以及每个在线 Worker 的下行命令通道。**任何业务真相都不放在这里**——
//! 状态的唯一事实来源是 PostgreSQL（第 5.3 节），因此进程重启只会丢掉
//! 「还没发出去的命令」，而这些命令都能从数据库重新推导出来。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use platform_proto::MasterMessage;
use sqlx::PgPool;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::config::{MasterConfig, SchedulerConfig};
use crate::events::EventHub;
use crate::opensearch::OpenSearchClient;
use crate::security::{FieldCipher, NodeCa, TokenIssuer};

/// 单个节点下行通道的容量。
///
/// 队列满意味着这个 Worker 的下行已经堵住（网络卡死或 Agent 卡在某个同步调用上）。
/// 这时**丢弃命令**比让调度器阻塞更安全：调度器一旦被一个坏节点卡住，
/// 其余节点也会跟着停摆；而丢掉的命令会在租约回收后由数据库状态重新推导出来。
const COMMAND_QUEUE_CAPACITY: usize = 64;

/// 一个在线节点的下行句柄。
#[derive(Debug, Clone)]
pub struct NodeLink {
    /// 链路代次。同一节点每次连接单调递增，用来区分「新旧两条流」。
    pub epoch: u64,
    /// 下行命令发送端。
    pub sender: mpsc::Sender<MasterMessage>,
    /// 本次连接建立时间。
    pub connected_at: DateTime<Utc>,
}

/// 登记链路时拿到的凭证，注销时必须回传。
///
/// 用代次而不是时间戳做身份：两次连接在同一微秒内发生时时间戳可能相同，
/// 而「谁是新的」这个判断不能依赖时钟精度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkToken {
    /// 节点编号。
    pub node_id: Uuid,
    /// 链路代次。
    pub epoch: u64,
}

/// 登记结果。
#[derive(Debug)]
pub struct Registration {
    /// 本次链路的凭证。
    pub token: LinkToken,
    /// 被顶掉的旧链路（如果有）。
    pub displaced: Option<NodeLink>,
}

/// 对账 ACK 处理结果（V4 第 10.5 节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileAckOutcome {
    /// 该执行已从待收集中移除，且集合已清空——可以下发 reconciliation_complete。
    Completed,
    /// 仍在等待其余 ACK（或本 ACK 被拒绝/动作不匹配，未移除）。
    Pending,
    /// 该节点当前没有待收对账集合（ACK 属于旧链路或伪造），不应触发 complete。
    Unknown,
}

/// 在线节点登记表。
///
/// 只回答「现在能不能把命令推给某个节点」，不回答「节点是否可用」——
/// 后者要看 `worker_nodes.status`，两者故意分开：进程刚重启时表是空的，
/// 但数据库里的节点状态、会话租约都还在，回收逻辑必须照旧生效。
#[derive(Debug, Clone, Default)]
pub struct NodeLinks {
    inner: Arc<Mutex<HashMap<Uuid, NodeLink>>>,
    epochs: Arc<AtomicU64>,
    /// 对账 ACK 追踪：节点编号 → （执行编号 → 期望的裁决动作）。
    pending_reconciles: Arc<Mutex<HashMap<Uuid, HashMap<String, i32>>>>,
}

impl NodeLinks {
    /// 新建空表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一条新链路。
    ///
    /// 同一个节点重连时旧链路可能还没被发现已死（TCP 半开），因此这里以**后来者为准**：
    /// 调用方拿到 `displaced` 后应当把它丢弃，让旧流的发送端在下一次发送时得到关闭错误。
    pub fn register(&self, node_id: Uuid, sender: mpsc::Sender<MasterMessage>) -> Registration {
        let epoch = self.epochs.fetch_add(1, Ordering::Relaxed) + 1;
        let link = NodeLink {
            epoch,
            sender,
            connected_at: Utc::now(),
        };
        let displaced = self.lock().insert(node_id, link);
        Registration {
            token: LinkToken { node_id, epoch },
            displaced,
        }
    }

    /// 注销链路。
    ///
    /// 只在代次匹配时才真的删除，避免「旧流的清理代码把新流的登记删掉」这种交叉：
    /// 旧流断开的处理往往晚于新流的建立。
    pub fn unregister(&self, token: &LinkToken) -> bool {
        let mut guard = self.lock();
        match guard.get(&token.node_id) {
            Some(link) if link.epoch == token.epoch => {
                guard.remove(&token.node_id);
                true
            }
            _ => false,
        }
    }

    /// 强制断开某节点链路。
    pub fn force_disconnect(&self, node_id: Uuid) -> bool {
        let mut guard = self.lock();
        guard.remove(&node_id).is_some()
    }

    /// 节点当前是否有可用链路。
    pub fn is_online(&self, node_id: Uuid) -> bool {
        self.lock()
            .get(&node_id)
            .is_some_and(|link| !link.sender.is_closed())
    }

    /// 当前在线的节点编号。
    pub fn online_nodes(&self) -> Vec<Uuid> {
        self.lock().keys().copied().collect()
    }

    /// 在线节点数。
    pub fn online_count(&self) -> usize {
        self.lock().len()
    }

    /// 取出某节点的发送端。
    ///
    /// 单独提供是为了让调用方在**锁之外**再 `await` 发送，避免把互斥锁跨越 await。
    pub fn sender(&self, node_id: Uuid) -> Option<mpsc::Sender<MasterMessage>> {
        self.lock().get(&node_id).map(|link| link.sender.clone())
    }

    /// 尝试把一条命令推给节点。
    ///
    /// 用 `try_send` 而不是 `send().await`：见 [`COMMAND_QUEUE_CAPACITY`] 的说明。
    /// 返回 `false` 表示节点不在线或下行已堵塞，调用方应据此记日志/告警，
    /// 但**不应**因此改写业务状态——那由租约回收统一负责。
    pub fn try_dispatch(&self, node_id: Uuid, message: MasterMessage) -> bool {
        let Some(sender) = self.sender(node_id) else {
            return false;
        };
        sender.try_send(message).is_ok()
    }

    /// 登记本次对账待收的执行编号与期望动作（V4 第 10.5 节）。
    pub fn set_pending_reconciles(&self, node_id: Uuid, entries: Vec<(String, i32)>) {
        let mut map = self
            .pending_reconciles
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if entries.is_empty() {
            map.remove(&node_id);
        } else {
            map.insert(node_id, entries.into_iter().collect());
        }
    }

    /// 收到一条对账 ACK。
    ///
    /// 只有满足以下全部条件才移除待收记录：
    /// - 该节点确实存在待收集合（否则视为旧链路/伪造 ACK → [`ReconcileAckOutcome::Unknown`]）；
    /// - `accepted == true`（Worker 无法执行裁决时保留待确认，绝不当成功）；
    /// - ACK 的 action 与下发裁决一致（防止动作错位）。
    pub fn ack_reconcile(
        &self,
        node_id: Uuid,
        execution_id: &str,
        action: i32,
        accepted: bool,
    ) -> ReconcileAckOutcome {
        let mut guard = self
            .pending_reconciles
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(entries) = guard.get_mut(&node_id) else {
            return ReconcileAckOutcome::Unknown;
        };
        match entries.get(execution_id) {
            Some(expected) if *expected == action && accepted => {
                entries.remove(execution_id);
                if entries.is_empty() {
                    guard.remove(&node_id);
                    ReconcileAckOutcome::Completed
                } else {
                    ReconcileAckOutcome::Pending
                }
            }
            _ => {
                // accepted=false（裁决执行失败）或 action 与下发不一致：保留待收，
                // 由调用方告警；绝不触发 complete。
                ReconcileAckOutcome::Pending
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, NodeLink>> {
        // 锁内只有 HashMap 的增删查，不可能 panic，因此毒化后直接取回内容即可。
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 新建一对下行通道。返回的接收端交给 gRPC 流任务。
pub fn command_channel() -> (mpsc::Sender<MasterMessage>, mpsc::Receiver<MasterMessage>) {
    mpsc::channel(COMMAND_QUEUE_CAPACITY)
}

/// 全局共享状态。克隆代价只有几个 `Arc`。
#[derive(Clone)]
pub struct AppState {
    /// 数据库连接池。
    pub pool: PgPool,
    /// 运行配置。
    pub config: Arc<MasterConfig>,
    /// 字段级加解密（账号密码、代理密码）。
    pub cipher: Arc<FieldCipher>,
    /// 管理后台会话令牌签发与校验。
    pub tokens: Arc<TokenIssuer>,
    /// 节点证书 CA。
    pub ca: Arc<NodeCa>,
    /// 管理后台实时事件总线。
    pub events: EventHub,
    /// 在线 Worker 的下行通道。
    pub links: NodeLinks,
    /// 可选 OpenSearch 搜索投影客户端。
    pub search: Option<OpenSearchClient>,
    /// 书目统计内存快照缓存（(获取时间戳秒, CatalogStats)）
    pub catalog_stats_cache: Arc<Mutex<Option<(u64, crate::store::catalog_v1::CatalogStats)>>>,
}

impl std::fmt::Debug for AppState {
    /// 手写实现：绝不把密钥、CA 私钥之类的东西打进日志。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("online_nodes", &self.links.online_count())
            .field("subscribers", &self.events.subscriber_count())
            .finish_non_exhaustive()
    }
}

impl AppState {
    /// 按配置建立连接池、载入密钥与 CA。
    ///
    /// CA 不存在时会**生成**一份自签 CA（私钥 0600），因此首次启动不需要额外准备证书；
    /// 已存在则原样载入，绝不覆盖——覆盖会让所有已签发的节点证书一起失效。
    pub async fn bootstrap(config: MasterConfig) -> Result<Self> {
        let pool = crate::store::connect(&config.database).await?;
        if config.database.auto_migrate {
            crate::store::run_migrations(&pool).await?;
        }

        let cipher = FieldCipher::from_base64(&config.security.field_key_base64)
            .context("字段加密密钥无效：应为 base64 编码的 32 字节")?;
        let tokens = TokenIssuer::new(&config.security.jwt_secret, config.security.jwt_hours);
        let ca = NodeCa::load_or_create(
            &config.security.ca_cert_path,
            &config.security.ca_key_path,
            config.security.node_cert_days,
        )?;
        let search = if config.opensearch.enabled {
            Some(OpenSearchClient::new(config.opensearch.clone())?)
        } else {
            None
        };

        Ok(Self {
            pool,
            config: Arc::new(config),
            cipher: Arc::new(cipher),
            tokens: Arc::new(tokens),
            ca: Arc::new(ca),
            events: EventHub::default(),
            links: NodeLinks::new(),
            search,
            catalog_stats_cache: Arc::new(Mutex::new(None)),
        })
    }

    /// 调度参数的快捷访问。
    pub fn scheduler(&self) -> &SchedulerConfig {
        &self.config.scheduler
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message() -> MasterMessage {
        MasterMessage {
            sent_at: Utc::now().to_rfc3339(),
            payload: None,
        }
    }

    #[tokio::test]
    async fn dispatch_reaches_registered_node() {
        let links = NodeLinks::new();
        let node = Uuid::new_v4();
        let (sender, mut receiver) = command_channel();
        assert!(links.register(node, sender).displaced.is_none());

        assert!(links.is_online(node));
        assert!(links.try_dispatch(node, message()));
        assert!(receiver.recv().await.is_some());
    }

    #[tokio::test]
    async fn dispatch_to_unknown_node_is_reported_not_panicked() {
        let links = NodeLinks::new();
        assert!(!links.try_dispatch(Uuid::new_v4(), message()));
        assert_eq!(links.online_count(), 0);
    }

    #[tokio::test]
    async fn reconnect_displaces_the_previous_link() {
        let links = NodeLinks::new();
        let node = Uuid::new_v4();
        let (first, mut first_rx) = command_channel();
        let first = links.register(node, first);

        let (second, mut second_rx) = command_channel();
        let second = links.register(node, second);
        let displaced = second.displaced.expect("旧链路应被返回");
        assert_eq!(displaced.epoch, first.token.epoch);
        assert!(second.token.epoch > first.token.epoch);

        assert!(links.try_dispatch(node, message()));
        assert!(second_rx.recv().await.is_some());
        // 旧链路不再收到任何命令
        assert!(first_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn stale_unregister_does_not_remove_the_new_link() {
        let links = NodeLinks::new();
        let node = Uuid::new_v4();
        let (first, _first_rx) = command_channel();
        let stale = links.register(node, first).token;

        let (second, _second_rx) = command_channel();
        let fresh = links.register(node, second).token;

        // 旧流的清理代码晚到，不应把新链路删掉
        assert!(!links.unregister(&stale));
        assert!(links.is_online(node));
        // 新链路自己注销时才真的摘掉
        assert!(links.unregister(&fresh));
        assert!(!links.is_online(node));
    }

    #[tokio::test]
    async fn full_queue_drops_instead_of_blocking() {
        let links = NodeLinks::new();
        let node = Uuid::new_v4();
        let (sender, _receiver) = command_channel();
        links.register(node, sender);

        // 填满队列后继续投递应立即返回 false，而不是挂住调用者
        for _ in 0..COMMAND_QUEUE_CAPACITY {
            assert!(links.try_dispatch(node, message()));
        }
        assert!(!links.try_dispatch(node, message()));
    }

    #[test]
    fn reconcile_acks_only_complete_when_all_accepted_and_matching() {
        use ReconcileAckOutcome as O;
        let links = NodeLinks::new();
        let node = Uuid::new_v4();

        links.set_pending_reconciles(node, vec![("e1".into(), 2), ("e2".into(), 3)]);

        // 1. 动作不匹配：保留待收，绝不触发 complete
        assert_eq!(links.ack_reconcile(node, "e1", 3, true), O::Pending);
        // 2. accepted=false：保留待收（P0：执行失败不得被计为成功）
        assert_eq!(links.ack_reconcile(node, "e1", 2, false), O::Pending);
        // 3. 正确 ACK 一个：仍待收
        assert_eq!(links.ack_reconcile(node, "e1", 2, true), O::Pending);
        // 4. 全部收齐：Completed
        assert_eq!(links.ack_reconcile(node, "e2", 3, true), O::Completed);

        // 5. 没有待收集合时任意 ACK → Unknown，不得误触发 complete
        assert_eq!(links.ack_reconcile(node, "e2", 3, true), O::Unknown);
    }
}
