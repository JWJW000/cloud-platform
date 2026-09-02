//! 真实 PostgreSQL 集成测试的公共装置（第 14.2 节）。
//!
//! 这些测试跑的是**真实迁移和真实 SQL**。理由很直接：本项目全部使用 sqlx 的运行时
//! 查询，列名拼错、参数类型推断失败、`CASE` 写反都不会在 `cargo build` 时报错，
//! 只会在生产的第一条心跳上报时报错。
//!
//! 连接串取自 `TEST_DATABASE_URL`，没有它时退回 `DATABASE_URL`。两个都没有时
//! 整组测试**跳过**——测试机上没有数据库是环境事实，不是代码结论。
//!
//! 跳过是危险的：`cargo test` 会照样打印 `ok`，而 `ok` 什么也没证明。为此提供
//! `REQUIRE_TEST_DATABASE=1`：设了它以后「没有数据库」直接判失败而不是跳过。
//! **第 21 节的验收必须带这个开关跑**，否则一次全绿的 `cargo test` 只能说明
//! 这些用例被跳过了（第 19.14 节）。
//!
//! 隔离方式是「一个测试一个 schema」而不是「一个测试一个数据库」：
//! 建库要连到 `postgres` 维护库、要独占连接、还常常被云托管数据库禁止；
//! `pg_trgm` 是数据库级扩展，固定装在 `public`；测试 schema 的 `search_path` 同时包含
//! `public`，避免并发测试在不同 schema 争抢同一个扩展后找不到 `gin_trgm_ops`。

use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};
use uuid::Uuid;

/// 一套独占 schema 的测试数据库。
///
/// `schema`/`url`/`teardown` 只在部分测试二进制中使用；clippy 按编译单元
/// 检查时会误报 dead_code，这里统一放行。
#[allow(dead_code)]
pub struct TestDb {
    /// 已建表、`search_path` 指向本测试 schema 的连接池。
    pub pool: PgPool,
    schema: String,
    url: String,
}

#[allow(dead_code)]
impl TestDb {
    /// 建立测试库；未配置连接串时返回 `None`（调用方应打印跳过原因并直接返回）。
    pub async fn setup() -> Option<TestDb> {
        let url = std::env::var("TEST_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .ok()
            .filter(|url| !url.trim().is_empty())?;

        let schema = format!("测试_{}", Uuid::new_v4().simple());
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(10))
            .connect(&url)
            .await
            .expect("连接测试用 PostgreSQL 失败");
        admin
            .execute("CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public")
            .await
            .expect("创建 pg_trgm 测试扩展失败");
        admin
            .execute(format!("CREATE SCHEMA \"{schema}\"").as_str())
            .await
            .expect("创建测试 schema 失败");
        admin.close().await;

        let schema_for_connect = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(10))
            .after_connect(move |conn, _meta| {
                // 每条连接都要设：连接池会在测试中途新建连接，漏设会让那条连接看不到表。
                let schema = schema_for_connect.clone();
                Box::pin(async move {
                    conn.execute(format!("SET search_path TO \"{schema}\", public").as_str())
                        .await?;
                    Ok(())
                })
            })
            .connect(&url)
            .await
            .expect("连接测试 schema 失败");

        master_server::store::run_migrations(&pool)
            .await
            .expect("执行真实迁移失败");

        Some(TestDb { pool, schema, url })
    }

    /// 清理本测试的 schema。
    ///
    /// 必须显式调用而不是靠 `Drop`：删 schema 是一次 `await`，而 `Drop` 里没法等。
    /// 断言失败时这个调用不会执行，留下的 schema 正好可以用来查现场。
    pub async fn teardown(self) {
        self.pool.close().await;
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.url)
            .await
            .expect("清理时连接失败");
        admin
            .execute(format!("DROP SCHEMA \"{}\" CASCADE", self.schema).as_str())
            .await
            .expect("删除测试 schema 失败");
        admin.close().await;
    }

    /// 构造用于测试的 AppState 实例
    pub fn create_test_state(&self) -> master_server::state::AppState {
        master_server::state::AppState {
            pool: self.pool.clone(),
            config: std::sync::Arc::new(master_server::config::MasterConfig {
                server: Default::default(),
                database: master_server::config::DatabaseConfig {
                    url: self.url.clone(),
                    max_connections: 5,
                    auto_migrate: false,
                },
                security: master_server::config::SecurityConfig {
                    jwt_secret: "1234567890123456".to_string(),
                    jwt_hours: 12,
                    field_key_base64: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string(),
                    ca_cert_path: std::path::PathBuf::from("data/ca.crt"),
                    ca_key_path: std::path::PathBuf::from("data/ca.key"),
                    node_cert_days: 30,
                    require_client_cert: false,
                    cookie_secure: true,
                },
                scheduler: Default::default(),
                nas: Default::default(),
                webshare: master_server::config::WebshareConfig::default(),
                opensearch: master_server::config::OpenSearchConfig::default(),
            }),
            cipher: std::sync::Arc::new(
                master_server::security::FieldCipher::from_base64(
                    "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
                )
                .unwrap(),
            ),
            tokens: std::sync::Arc::new(master_server::security::TokenIssuer::new(
                "1234567890123456",
                12,
            )),
            ca: std::sync::Arc::new(master_server::security::NodeCa::generate(30).unwrap()),
            events: master_server::events::EventHub::default(),
            links: master_server::state::NodeLinks::new(),
            search: None,
            catalog_stats_cache: std::sync::Arc::new(std::sync::Mutex::new(None)),
            catalog_stats_refresh_lock: std::sync::Arc::new(tokio::sync::Mutex::new(())),
        }
    }
}

/// 未配置连接串时统一的跳过提示。
#[macro_export]
macro_rules! require_db {
    () => {
        match $crate::support::TestDb::setup().await {
            Some(db) => db,
            None => {
                if std::env::var("REQUIRE_TEST_DATABASE").as_deref() == Ok("1") {
                    panic!("要求执行 PostgreSQL 集成测试，但未设置 TEST_DATABASE_URL/DATABASE_URL");
                }
                eprintln!("跳过：未配置测试数据库");
                return;
            }
        }
    };
}
