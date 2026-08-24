//! Webshare 快照同步与代理生命周期的真实数据库测试（第 16.4 节、V3 方案第 13 节）。
//!
//! 验证场景：
//! - 快照同步 upsert `valid=true` 代理；
//! - `valid=false` 标记异常/停用；
//! - 消失代理标记已停用；
//! - 已占用失效代理标记 `retire_after_release`，释放后自动停用；
//! - 管理员手工停用不因 `valid=true` 自动恢复。

mod support;

use master_server::store;
use master_server::store::resource::WebshareProxyData;
use platform_domain::ProxyStatus;
use uuid::Uuid;

#[tokio::test]
async fn webshare快照同步与失效代理治理() {
    let db = require_db!();

    // 1. 首次快照同步：2 个有效代理 p1, p2
    let snapshot1 = vec![
        WebshareProxyData {
            external_id: Some("ext_1".to_string()),
            host: "10.0.0.1".to_string(),
            port: 8001,
            username: Some("u1".to_string()),
            password_cipher: None,
            valid: true,
        },
        WebshareProxyData {
            external_id: Some("ext_2".to_string()),
            host: "10.0.0.2".to_string(),
            port: 8002,
            username: Some("u2".to_string()),
            password_cipher: None,
            valid: true,
        },
    ];

    let report1 = store::resource::sync_webshare_snapshot(&db.pool, &snapshot1)
        .await
        .unwrap();
    assert_eq!(report1.enabled_count, 2);

    let proxies1 = store::resource::list_proxies(&db.pool, None, 10, 0)
        .await
        .unwrap();
    assert_eq!(proxies1.len(), 2);
    assert_eq!(proxies1[0].status, ProxyStatus::Available.as_str());

    // 2. 模拟 p1 被某会话占用，p2 被管理员手工停用
    let p1_id = proxies1.iter().find(|p| p.host == "10.0.0.1").unwrap().id;
    let p2_id = proxies1.iter().find(|p| p.host == "10.0.0.2").unwrap().id;

    let session_id = Uuid::new_v4();
    sqlx::query("UPDATE proxies SET status = $2, lease_session_id = $3 WHERE id = $1")
        .bind(p1_id)
        .bind(ProxyStatus::Occupied.as_str())
        .bind(session_id)
        .execute(&db.pool)
        .await
        .unwrap();

    store::resource::set_proxy_status(&db.pool, p2_id, ProxyStatus::Disabled, None)
        .await
        .unwrap();

    // 3. 第二次快照同步：
    // - p1 在 Webshare 侧变为 valid=false；
    // - p2 仍然 valid=true；
    // - 新增 p3 (valid=true)；
    let snapshot2 = vec![
        WebshareProxyData {
            external_id: Some("ext_1".to_string()),
            host: "10.0.0.1".to_string(),
            port: 8001,
            username: Some("u1".to_string()),
            password_cipher: None,
            valid: false, // 变为失效
        },
        WebshareProxyData {
            external_id: Some("ext_2".to_string()),
            host: "10.0.0.2".to_string(),
            port: 8002,
            username: Some("u2".to_string()),
            password_cipher: None,
            valid: true,
        },
        WebshareProxyData {
            external_id: Some("ext_3".to_string()),
            host: "10.0.0.3".to_string(),
            port: 8003,
            username: Some("u3".to_string()),
            password_cipher: None,
            valid: true,
        },
    ];

    let report2 = store::resource::sync_webshare_snapshot(&db.pool, &snapshot2)
        .await
        .unwrap();
    assert_eq!(report2.disabled_count, 1);
    assert_eq!(report2.enabled_count, 2);

    // 校验 p1：由于正在占用中，状态仍保持已占用，但标记了 retire_after_release
    let p1_after = store::resource::get_proxy(&db.pool, p1_id).await.unwrap();
    assert_eq!(p1_after.status, ProxyStatus::Occupied.as_str());

    let (p1_valid, p1_retire): (bool, bool) =
        sqlx::query_as("SELECT provider_valid, retire_after_release FROM proxies WHERE id = $1")
            .bind(p1_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(!p1_valid, "p1 provider_valid 必须为 false");
    assert!(p1_retire, "已占用的失效代理必须标记 retire_after_release");

    // 校验 p2：管理员手工停用的代理，即便 Webshare valid=true 也不得自动恢复为可用
    let p2_after = store::resource::get_proxy(&db.pool, p2_id).await.unwrap();
    assert_eq!(p2_after.status, ProxyStatus::Disabled.as_str());

    // 4. 会话结束释放 p1：必须直接转为已停用，绝不回到可用池
    let mut tx = db.pool.begin().await.unwrap();
    store::session::release_session_resources(&mut tx, session_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let p1_released = store::resource::get_proxy(&db.pool, p1_id).await.unwrap();
    assert_eq!(
        p1_released.status,
        ProxyStatus::Disabled.as_str(),
        "释放后必须停用"
    );
    assert!(p1_released.lease_session_id.is_none());

    // 5. 第三次快照同步：p3 消失了
    let snapshot3 = vec![WebshareProxyData {
        external_id: Some("ext_2".to_string()),
        host: "10.0.0.2".to_string(),
        port: 8002,
        username: Some("u2".to_string()),
        password_cipher: None,
        valid: true,
    }];

    let report3 = store::resource::sync_webshare_snapshot(&db.pool, &snapshot3)
        .await
        .unwrap();
    assert!(report3.missing_count >= 1);

    let p3 = store::resource::list_proxies(&db.pool, None, 10, 0)
        .await
        .unwrap();
    let p3_obj = p3.iter().find(|p| p.host == "10.0.0.3").unwrap();
    assert_eq!(
        p3_obj.status,
        ProxyStatus::Disabled.as_str(),
        "消失的代理必须标记为已停用"
    );

    db.teardown().await;
}
