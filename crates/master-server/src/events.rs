//! 管理后台实时事件推送（第 16.2 节）。
//!
//! 用 `tokio::sync::broadcast` 而不是每个连接一个 `mpsc`：
//! 后台页面数量少、事件量小，广播通道让「一次业务变更 → 所有打开的页面同时刷新」
//! 这件事只需要一次 `send`。订阅者跟不上时 broadcast 会丢最旧的事件并返回
//! `Lagged`，这对「刷新界面」的语义是可接受的：下一次全量拉取会自动纠正。
//!
//! 事件本身**不承载业务真相**，只是提示前端「某类数据变了」。因此即使丢事件，
//! 界面也只是晚一点更新，不会出现数据不一致。

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::broadcast;

/// 推给管理后台的一条事件。
#[derive(Debug, Clone, Serialize)]
pub struct AdminEvent {
    /// 中文事件类别，前端据此决定刷新哪个面板。
    pub kind: String,
    /// 附加数据，允许为空对象。
    pub payload: serde_json::Value,
    /// 产生时间。
    pub at: DateTime<Utc>,
}

/// 事件中心。克隆代价很低，可以随 `AppState` 到处传。
#[derive(Debug, Clone)]
pub struct EventHub {
    sender: broadcast::Sender<AdminEvent>,
}

impl EventHub {
    /// 创建事件中心，`capacity` 是积压上限。
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self { sender }
    }

    /// 订阅。返回的接收端从订阅那一刻之后的事件开始收。
    pub fn subscribe(&self) -> broadcast::Receiver<AdminEvent> {
        self.sender.subscribe()
    }

    /// 广播一条事件。没有订阅者时静默丢弃，这不是错误。
    pub fn publish(&self, kind: impl Into<String>, payload: serde_json::Value) {
        let event = AdminEvent {
            kind: kind.into(),
            payload,
            at: Utc::now(),
        };
        let _ = self.sender.send(event);
    }

    /// 广播一条只有类别、没有附加数据的事件。
    pub fn notify(&self, kind: impl Into<String>) {
        self.publish(kind, serde_json::json!({}));
    }

    /// 当前订阅者数量，用于诊断接口。
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl Default for EventHub {
    fn default() -> Self {
        Self::new(256)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscriber_receives_published_event() {
        let hub = EventHub::new(8);
        let mut rx = hub.subscribe();
        hub.publish("图书任务变更", serde_json::json!({"数量": 3}));
        let event = rx.recv().await.unwrap();
        assert_eq!(event.kind, "图书任务变更");
        assert_eq!(event.payload["数量"], 3);
    }

    #[tokio::test]
    async fn publishing_without_subscribers_is_not_an_error() {
        let hub = EventHub::new(4);
        hub.notify("无人监听");
        assert_eq!(hub.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn every_subscriber_sees_the_same_event() {
        let hub = EventHub::new(8);
        let mut first = hub.subscribe();
        let mut second = hub.subscribe();
        hub.notify("Worker状态变更");
        assert_eq!(first.recv().await.unwrap().kind, "Worker状态变更");
        assert_eq!(second.recv().await.unwrap().kind, "Worker状态变更");
    }
}
