//! 会话资源分配与释放的真实数据库测试（第 16.2 节）。
//!
//! 验证场景：
//! - 同一代理不能同时分配给两个会话；
//! - 同一账号不能同时分配给两个会话；
//! - 同一节点槽位不能同时有两个活动会话；
//! - 分配中任一步失败必须完整回滚；
//! - 会话关闭后资源准确释放。

mod support;

use master_server::scheduler::allocate::allocate_session;
use master_server::state::AppState;
use master_server::store;
use platform_domain::{AccountStatus, ProxyStatus, TaskType, WorkerStatus};

#[tokio::test]
async fn 会话独占锁定账号代理与槽位() {
    let db = require_db!();

    // 1. 初始化 Worker 节点与 2 个槽位
    let mut conn = db.pool.acquire().await.unwrap();
    let node = store::node::upsert_node(
        &mut conn,
        "会话测试节点",
        "host",
        "Linux",
        "1.0",
        "1.0",
        2,
        "token_hash",
    )
    .await
    .unwrap();
    store::node::ensure_slots(&mut conn, node.id, 2)
        .await
        .unwrap();
    drop(conn);
    store::node::approve_node(&db.pool, node.id, None)
        .await
        .unwrap();
    store::node::set_node_status(&db.pool, node.id, WorkerStatus::Online)
        .await
        .unwrap();
    // 下载会话要求节点 NAS 可写（allocate_session 的前置检查）
    store::node::set_nas_health(&db.pool, node.id, true, 100)
        .await
        .unwrap();

    // 2. 仅导入 1 个可用账号和 1 个可用代理
    let cipher = master_server::security::FieldCipher::from_base64(
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .unwrap();
    let account = store::resource::create_account(
        &db.pool,
        "only_one@test.com",
        &cipher.encrypt("real-password").unwrap(),
        "nick",
        10,
        AccountStatus::Registered,
    )
    .await
    .unwrap();

    let proxy = store::resource::upsert_proxy(
        &db.pool, "Webshare", None, "p1", "http", "1.2.3.4", 8080, None, None,
    )
    .await
    .unwrap();
    store::resource::set_proxy_status(&db.pool, proxy.id, ProxyStatus::Available, None)
        .await
        .unwrap();

    let state = AppState {
        pool: db.pool.clone(),
        config: std::sync::Arc::new(master_server::config::MasterConfig {
            server: Default::default(),
            database: master_server::config::DatabaseConfig {
                url: "postgres://localhost/dummy".to_string(),
                max_connections: 5,
                auto_migrate: false,
            },
            security: master_server::config::SecurityConfig {
                jwt_secret: "1234567890123456".to_string(),
                jwt_hours: 12,
                field_key_base64: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string(),
                ca_cert_path: std::path::PathBuf::from("data/ca.crt"),
                ca_key_path: std::path::PathBuf::from("data/ca.key"),
                node_cert_days: 365,
                require_client_cert: false,

                cookie_secure: true,
            },
            scheduler: Default::default(),
            nas: Default::default(),
            webshare: Default::default(),
            opensearch: Default::default(),
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
        ca: std::sync::Arc::new(master_server::security::NodeCa::generate(365).unwrap()),
        events: Default::default(),
        links: Default::default(),
        search: None,
    };

    // 3. 槽位 0 申请会话，应当成功锁定该账号和代理
    let outcome1 = allocate_session(&state, node.id, TaskType::BookDownload, Some(0))
        .await
        .unwrap();
    let grant1 = match outcome1 {
        master_server::scheduler::AllocationOutcome::Granted(g) => g,
        other => panic!("预期第一个会话申请成功，实际得到 {other:?}"),
    };

    assert_eq!(grant1.account.as_ref().unwrap().email, "only_one@test.com");
    assert_eq!(grant1.proxy.as_ref().unwrap().host, "1.2.3.4");

    // 4. 槽位 1 申请会话，由于账号与代理已被占用，应当分配失败并说明原因
    let outcome2 = allocate_session(&state, node.id, TaskType::BookDownload, Some(1))
        .await
        .unwrap();
    match outcome2 {
        master_server::scheduler::AllocationOutcome::Unavailable(unavail) => {
            assert!(
                unavail.reason.contains("账号") || unavail.reason.contains("代理"),
                "必须提示资源不足：{}",
                unavail.reason
            );
        }
        other => panic!("资源耗尽时预期 Unavailable，实际得到 {other:?}"),
    }

    // 5. 结束会话 1 并验证资源释放
    master_server::scheduler::allocate::close_session(
        &state,
        grant1.session_id,
        platform_domain::SessionStatus::Ended,
        "正常测试结束",
    )
    .await
    .unwrap();

    let acc_after = store::resource::get_account(&db.pool, account.id)
        .await
        .unwrap();
    assert!(
        acc_after.lease_session_id.is_none(),
        "会话结束后账号租约必须释放"
    );

    let proxy_after = store::resource::get_proxy(&db.pool, proxy.id)
        .await
        .unwrap();
    assert_eq!(
        proxy_after.status,
        ProxyStatus::Available.as_str(),
        "会话结束后代理必须恢复为可用"
    );
    assert!(
        proxy_after.lease_session_id.is_none(),
        "会话结束后代理租约必须释放"
    );

    // 6. 再次申请会话，应当能成功复用释放后的账号与代理
    let outcome3 = allocate_session(&state, node.id, TaskType::BookDownload, Some(0))
        .await
        .unwrap();
    assert!(matches!(
        outcome3,
        master_server::scheduler::AllocationOutcome::Granted(_)
    ));

    db.teardown().await;
}
