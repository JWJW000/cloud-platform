//! `worker-agent` 库模块声明。

pub mod bus;
pub mod client;
pub mod config;
pub mod dynamic;
pub mod outbox;
pub mod proxy_forward;
pub mod registration;
pub mod slot;
pub mod storage;
pub mod tls;

pub use config::{SavedIdentity, WorkerConfig};
