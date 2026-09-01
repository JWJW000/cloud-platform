//! 云端 Master：唯一事实来源（第 5.3 节）。
//!
//! 模块划分对应设计方案：
//! - [`config`]：第 16.1 节配置；
//! - [`security`]：第 15 节密码、令牌、字段加密与节点证书；
//! - [`events`]：管理后台实时推送；
//! - [`store`]：PostgreSQL 访问层，包含第 7.2 节的原子领取语句；
//! - [`scheduler`]：第 3.4 / 6.4 / 14 节的租约、回收与结果归因；
//! - [`grpc`]：第 13 节 Worker 接入；
//! - [`api`]：第 16 节管理后台接口。

#![warn(missing_docs)]

pub mod api;
pub mod catalog;
pub mod catalog_ownership;
pub mod config;
pub mod download_search;
pub mod error;
pub mod events;
pub mod grpc;
pub mod models;
pub mod opensearch;
pub mod scheduler;
pub mod security;
pub mod state;
pub mod store;
pub mod webshare;
