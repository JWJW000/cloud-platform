//! 任务并发领取的真实数据库测试（第 16.1 节）。
//!
//! 验证场景：
//! - 5 个 Worker 同时领取同一个候选任务；
//! - 只能有 1 个 Worker 成功；
//! - stage_version 单调递增；
//! - 只有一个 lease_execution_id。

mod support;

use futures::future::join_all;
use master_server::models::ImportRow;
use master_server::scheduler::claim::{claim_next_task, ClaimOutcome};
use master_server::store;
use master_server::store::catalog::ImportRequest;
use platform_domain::{AccountStatus, BatchStatus, ProxyStatus, TaskStatus, WorkerStatus};

#[tokio::test]
async fn 五个worker并发领取同一任务只能有一个成功() {
    let db = require_db!();

    // 1. 创建批次与图书任务
    let import_req = ImportRequest {
        batch_name: "并发测试批次",
        source_file: None,
        format: "pdf",
        priority: 10,
        created_by: None,
        max_attempts: 3,
    };
    let import_rows = vec![ImportRow {
        title: "并发测试导论".to_string(),
        author: Some("作者A".to_string()),
        publisher: Some("出版社B".to_string()),
        isbn: Some("9787111111111".to_string()),
    }];

    let summary = store::catalog::import_books(&db.pool, &import_req, &import_rows)
        .await
        .expect("导入图书失败");
    let batch_id = summary.batch_id.unwrap();
    store::catalog::set_batch_status(&db.pool, batch_id, BatchStatus::Running)
        .await
        .expect("设置批次状态失败");

    // 2. 创建 5 个 Worker 节点、会话与槽位
    let mut session_nodes = Vec::new();
    for i in 0..5 {
        let mut conn = db.pool.acquire().await.unwrap();
        let node = store::node::upsert_node(
            &mut conn,
            &format!("Worker并发节点_{i}"),
            "host",
            "Linux",
            "1.0",
            "1.0",
            1,
            "hash",
        )
        .await
        .unwrap();
        store::node::ensure_slots(&mut conn, node.id, 1)
            .await
            .unwrap();
        drop(conn);
        store::node::approve_node(&db.pool, node.id, None)
            .await
            .unwrap();
        store::node::set_node_status(&db.pool, node.id, WorkerStatus::Online)
            .await
            .unwrap();

        let account = store::resource::create_account(
            &db.pool,
            &format!("acc_{i}@test.com"),
            "cipher",
            "",
            10,
            AccountStatus::Registered,
        )
        .await
        .unwrap();
        let proxy = store::resource::upsert_proxy(
            &db.pool,
            "test",
            None,
            &format!("proxy_{i}"),
            "http",
            "127.0.0.1",
            8000 + i,
            None,
            None,
        )
        .await
        .unwrap();
        store::resource::set_proxy_status(&db.pool, proxy.id, ProxyStatus::Available, None)
            .await
            .unwrap();

        let mut tx = db.pool.begin().await.unwrap();
        let session = store::session::create_session(
            &mut tx,
            &store::session::NewSession {
                node_id: node.id,
                slot_index: 0,
                account_id: Some(account.id),
                proxy_id: Some(proxy.id),
                task_type: platform_domain::TaskType::BookDownload,
                local_forward_port: Some(8000 + i),
                lease_secs: 120,
            },
        )
        .await
        .unwrap();
        store::session::activate_session(&mut *tx, session.id)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        session_nodes.push((node.id, session.id));
    }

    let state = std::sync::Arc::new(db.create_test_state());

    // 3. 并发调用 claim_next_task
    let handles: Vec<_> = session_nodes
        .iter()
        .map(|&(node_id, session_id)| {
            let st = state.clone();
            tokio::spawn(async move {
                let outcome = claim_next_task(&st, node_id, session_id).await;
                (session_id, outcome)
            })
        })
        .collect();

    let results = join_all(handles).await;
    let mut granted_count = 0;
    let mut empty_count = 0;

    for res in results {
        let (_sid, outcome) = res.unwrap();
        match outcome.unwrap() {
            ClaimOutcome::Assigned(_) => granted_count += 1,
            ClaimOutcome::Unavailable(_) => empty_count += 1,
            ClaimOutcome::SessionShouldEnd { .. } => empty_count += 1,
        }
    }

    assert_eq!(
        granted_count, 1,
        "同一全局任务在 5 个并发请求中只能被成功领取一次"
    );
    assert_eq!(empty_count, 4, "其余 4 个请求应收到暂无任务");

    let tasks = store::task::list_tasks(
        &db.pool,
        &store::task::TaskFilter {
            batch_id: Some(batch_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(tasks.len(), 1);
    let task = &tasks[0];
    assert_eq!(task.status, TaskStatus::Claimed.as_str());
    assert_eq!(task.stage_version, 1, "初次领取后 stage_version 应增加为 1");
    assert!(
        task.lease_execution_id.is_some(),
        "必须记录 lease_execution_id"
    );
    assert!(task.lease_session_id.is_some(), "必须记录 lease_session_id");

    db.teardown().await;
}
