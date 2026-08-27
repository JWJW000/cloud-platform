//! 搜索 Outbox 兼容入口。
//!
//! 具体写入实现位于 [`crate::opensearch`]。调用方必须显式提供 OpenSearch 客户端；
//! 只有远端确认整批成功后，事件才会被标记为已同步。

pub use crate::opensearch::process_outbox_events;
