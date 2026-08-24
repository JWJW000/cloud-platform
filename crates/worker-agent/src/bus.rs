//! 统一上行事件总线（第 5.1、5.3 节）。
//!
//! V2 第 3.1 节诊断的那个缺陷——「创建了接收端但从不消费」——根子在于
//! 槽位和连接各自持有一条通道。这里只留一条：**所有上行消息都写进同一个总线，
//! 连接管理器是唯一的消费者**。槽位不知道当前有没有连接，也不需要知道。
//!
//! 总线区分两类消息，这个区分是整套可靠性的基础：
//!
//! - [`OutboundEventBus::send_reliable`]：先写 SQLite Outbox，再尽力投递。
//!   `TaskResult` 这类事件即使此刻断网、进程随后崩溃，也会在重连后补报。
//! - [`OutboundEventBus::send_volatile`]：只尽力投递，丢了就丢了。
//!   心跳、进度、周期性的会话申请都属于这类——下一个周期会再来一遍，
//!   而把它们也持久化只会让 Outbox 里堆满没人关心的历史。
//!
//! 两个方法都**不会阻塞**。断线期间没有消费者，总线很快填满，此后 `try_send`
//! 直接失败：可靠事件已经落盘，易失事件本就允许丢弃。这比「等到能发为止」
//! 好得多——后者会让持有槽位的协程卡在发送上，连取消命令都处理不了。

use platform_proto::v1 as pb;
use tokio::sync::mpsc;

use crate::outbox::LocalStore;

/// 总线容量。
///
/// 128 条足够覆盖一次重连窗口内的实时消息；再多也没有意义，
/// 因为超出部分要么已经在 Outbox 里，要么是可以丢的进度。
pub const BUS_CAPACITY: usize = 128;

/// 上行事件总线的发送端，可自由克隆给各个槽位。
#[derive(Clone)]
pub struct OutboundEventBus {
    tx: mpsc::Sender<pb::WorkerMessage>,
    outbox: LocalStore,
}

impl OutboundEventBus {
    /// 建立总线，返回发送端与唯一的接收端。
    ///
    /// 接收端刻意不放进结构体：谁拿到它就是唯一消费者，
    /// 类型系统因此能保证「接收端被创建却没人读」这个缺陷不会重演。
    pub fn new(outbox: LocalStore) -> (Self, mpsc::Receiver<pb::WorkerMessage>) {
        let (tx, rx) = mpsc::channel(BUS_CAPACITY);
        (Self { tx, outbox }, rx)
    }

    /// 可靠事件：先落盘，再尽力实时投递。
    ///
    /// `event_id` 必须是**由业务内容决定的稳定值**（例如 `evt-res-{execution_id}`），
    /// 不能用随机 UUID：崩溃后重走一遍上报路径时，稳定 id 会命中
    /// `INSERT OR IGNORE` 而不是在 Outbox 里堆出第二条，
    /// Master 侧也才能靠 `event_id` 认出这是同一件事。
    pub async fn send_reliable(&self, event_id: &str, payload: pb::worker_message::Payload) {
        let msg = message(event_id.to_string(), payload);
        if let Err(err) = self.outbox.enqueue(event_id, &msg) {
            // 落盘失败是严重问题（磁盘满、权限丢失），但仍然要试着实时发出去：
            // 这条事件此刻还有机会被 Master 收到，放弃它只会让情况更糟。
            tracing::error!(event_id, error = %err, "可靠事件写入本地 Outbox 失败");
        }
        if self.tx.try_send(msg).is_err() {
            tracing::debug!(event_id, "上行总线暂不可用，可靠事件将由 Outbox 补报");
        }
    }

    /// 易失事件：只尽力投递，允许丢弃。
    pub fn send_volatile(&self, event_id: String, payload: pb::worker_message::Payload) {
        let _ = self.tx.try_send(message(event_id, payload));
    }

    /// 供连接管理器在重连成功后使用：把断线期间积压的易失消息丢掉。
    ///
    /// 断线时排队的心跳、进度和会话申请在恢复时已经全部过时了，
    /// 补发它们只会让 Master 收到一串「十分钟前的 CPU 占用」。
    /// 可靠事件不受影响——它们在 Outbox 里，紧接着会被完整补报。
    pub fn drain_stale(rx: &mut mpsc::Receiver<pb::WorkerMessage>) -> usize {
        let mut dropped = 0;
        while rx.try_recv().is_ok() {
            dropped += 1;
        }
        dropped
    }
}

fn message(event_id: String, payload: pb::worker_message::Payload) -> pb::WorkerMessage {
    pb::WorkerMessage {
        event_id,
        sent_at: chrono::Utc::now().to_rfc3339(),
        replayed: false,
        payload: Some(payload),
    }
}

/// 生成一个随机的易失事件编号。
pub fn volatile_id(prefix: &str) -> String {
    format!("evt-{prefix}-{}", uuid::Uuid::new_v4())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heartbeat() -> pb::worker_message::Payload {
        pb::worker_message::Payload::Heartbeat(pb::Heartbeat::default())
    }

    fn result() -> pb::worker_message::Payload {
        pb::worker_message::Payload::TaskResult(pb::TaskResult::default())
    }

    #[tokio::test]
    async fn reliable_event_is_persisted_and_delivered() {
        let store = LocalStore::memory().unwrap();
        let (bus, mut rx) = OutboundEventBus::new(store.clone());

        bus.send_reliable("evt-res-1", result()).await;

        assert_eq!(store.pending_count().unwrap(), 1);
        assert_eq!(rx.recv().await.unwrap().event_id, "evt-res-1");
    }

    #[tokio::test]
    async fn reliable_event_survives_a_full_bus() {
        // 断线场景：没有消费者，总线填满后可靠事件仍然必须留在 Outbox 里
        let store = LocalStore::memory().unwrap();
        let (bus, _rx) = OutboundEventBus::new(store.clone());

        for i in 0..(BUS_CAPACITY + 5) {
            bus.send_reliable(&format!("evt-res-{i}"), result()).await;
        }

        assert_eq!(store.pending_count().unwrap(), BUS_CAPACITY + 5);
    }

    #[tokio::test]
    async fn volatile_event_is_not_persisted() {
        let store = LocalStore::memory().unwrap();
        let (bus, mut rx) = OutboundEventBus::new(store.clone());

        bus.send_volatile(volatile_id("hb"), heartbeat());

        assert_eq!(store.pending_count().unwrap(), 0);
        assert!(rx.recv().await.is_some());
    }

    #[tokio::test]
    async fn volatile_send_never_blocks_when_bus_is_full() {
        let store = LocalStore::memory().unwrap();
        let (bus, _rx) = OutboundEventBus::new(store);
        for _ in 0..(BUS_CAPACITY * 2) {
            bus.send_volatile(volatile_id("hb"), heartbeat());
        }
        // 能走到这里就说明没有阻塞在满通道上
    }

    #[tokio::test]
    async fn stale_messages_are_dropped_on_reconnect() {
        let store = LocalStore::memory().unwrap();
        let (bus, mut rx) = OutboundEventBus::new(store);
        for _ in 0..5 {
            bus.send_volatile(volatile_id("hb"), heartbeat());
        }
        assert_eq!(OutboundEventBus::drain_stale(&mut rx), 5);
        assert_eq!(OutboundEventBus::drain_stale(&mut rx), 0);
    }
}
