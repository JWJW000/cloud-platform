//! PostgreSQL 访问层。
//!
//! 全部使用 sqlx 的**运行时**查询（`query`/`query_as`）而不是 `query!` 宏：
//! 宏需要编译期连上数据库，会让「只想 `cargo build` 一下」变成必须先起一个
//! PostgreSQL。代价是列名拼错要到运行时才发现，因此每个模块都配了针对真实
//! 数据库的集成测试（见 `tests/`），并且模型结构体的字段名与迁移脚本严格一致。
//!
//! 分层约定：
//! - 本层只做「读写一行/一批行」，不做跨表的业务决策；
//! - 需要在一个事务里跨多张表推进状态的逻辑（领任务、提交结果、回收租约）
//!   放在 [`crate::scheduler`]，它调用本层的事务版本函数。

pub mod account_registration;
pub mod admin;
pub mod catalog;
pub mod catalog_v1;
pub mod import_job;
pub mod mail_provider;
pub mod manual_action;
pub mod node;
pub mod registration;
pub mod registration_request;
pub mod resource;
pub mod session;
pub mod task;

use anyhow::{Context, Result};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Postgres, Transaction};

use crate::config::DatabaseConfig;

/// 事务别名，供上层标注函数签名。
pub type Tx<'c> = Transaction<'c, Postgres>;

/// 建立连接池。
pub async fn connect(config: &DatabaseConfig) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(config.max_connections.max(1))
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&config.url)
        .await
        .context("连接 PostgreSQL 失败")?;
    Ok(pool)
}

/// 执行内嵌的迁移脚本。
///
/// `sqlx_macros::migrate!` 在编译期把 `migrations/` 目录嵌进二进制，因此部署时不需要
/// 额外携带 SQL 文件，也不需要容器里装 `psql`。
pub async fn run_migrations(pool: &PgPool) -> Result<()> {
    sqlx_macros::migrate!("./migrations")
        .run(pool)
        .await
        .context("执行数据库迁移失败")?;
    Ok(())
}

/// 便捷判断：唯一约束冲突。
///
/// 导入图书、注册节点等路径依赖「撞唯一键说明已经存在」，
/// 把 sqlx 的错误结构判断收在这里，避免调用处到处 match。
pub fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(db) if db.code().as_deref() == Some("23505"))
}
