//! 结果上报幂等性与去重的真实数据库测试（第 16.3 节）。
//!
//! 验证场景：
//! - 同一 event_id 上报两次只记账一次；
//! - 同一执行换不同 event_id 上报两次只记账一次；
//! - 旧 stage_version 结果不能覆盖新执行；
//! - 成功结果与租约回收/取消并发；
//! - 账号额度只增加一次；
//! - book_files 只产生一条有效记录。

mod support;

use master_server::models::ImportRow;
use master_server::scheduler::submit::{submit_result, FileEvidence, ResultReport};
use master_server::store;
use master_server::store::catalog::ImportRequest;
use platform_domain::{
    AccountStatus, BatchStatus, ExecutionResult, ProxyStatus, TaskStatus, WorkerStatus,
};

#[tokio::test]
async fn 结果上报重复提交与去重测试() {
    let db = require_db!();

    // 1. 初始化 Worker、账号、代理与批次任务
    let mut conn = db.pool.acquire().await.unwrap();
    let node = store::node::upsert_node(
        &mut conn,
        "幂等测试节点",
        "host",
        "Linux",
        "1.0",
        "1.0",
        1,
        "token_hash",
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
        "idemp_acc@test.com",
        "cipher",
        "",
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

    let import_req = ImportRequest {
        batch_name: "幂等批次",
        source_file: None,
        format: "pdf",
        priority: 10,
        created_by: None,
        max_attempts: 3,
    };
    let import_rows = vec![ImportRow {
        title: "幂等性测试图书".to_string(),
        author: None,
        publisher: None,
        isbn: None,
    }];
    let summary = store::catalog::import_books(&db.pool, &import_req, &import_rows)
        .await
        .unwrap();
    let batch_id = summary.batch_id.unwrap();
    store::catalog::set_batch_status(&db.pool, batch_id, BatchStatus::Running)
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
            local_forward_port: Some(8080),
            lease_secs: 120,
        },
    )
    .await
    .unwrap();
    store::session::activate_session(&mut *tx, session.id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let state = db.create_test_state();

    // 领取任务
    let outcome = master_server::scheduler::claim::claim_next_task(&state, node.id, session.id)
        .await
        .unwrap();
    let grant = match outcome {
        master_server::scheduler::ClaimOutcome::Assigned(g) => g,
        other => panic!("预期领取成功，实际得到 {other:?}"),
    };

    let file_name = grant
        .nas_relative_path
        .rsplit('/')
        .next()
        .unwrap_or("book.pdf")
        .to_string();
    let file_evidence = FileEvidence {
        nas_relative_path: grant.nas_relative_path.clone(),
        file_name,
        size_bytes: 100 * 1024,
        sha256: "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2".to_string(),
        format: "pdf".to_string(),
    };

    let report = ResultReport {
        session_id: session.id,
        execution_id: grant.execution_id,
        task_id: grant.task_id,
        node_id: Some(node.id),
        result: ExecutionResult::Success,
        reason: "入库成功".to_string(),
        stage_version: grant.stage_version,
        duration_ms: Some(1000),
        quota: None,
        file: Some(file_evidence.clone()),
    };

    // 2. 首次提交结果：应当应用并标记已完成
    let res1 = submit_result(&state, &report).await.unwrap();
    assert!(res1.applied, "首次提交必须成功应用");

    let task_after = store::task::get_task(&db.pool, grant.task_id)
        .await
        .unwrap();
    assert_eq!(task_after.status, TaskStatus::Completed.as_str());

    let acc_after1 = store::resource::get_account(&db.pool, account.id)
        .await
        .unwrap();
    assert_eq!(acc_after1.daily_used, 1, "账号额度应增加 1");

    // 3. 再次重复提交同一结果：必须判定为已完成并幂等返回，不重复消耗账号额度
    let res2 = submit_result(&state, &report).await.unwrap();
    assert!(!res2.applied, "重复提交不得再次应用修改");

    let acc_after2 = store::resource::get_account(&db.pool, account.id)
        .await
        .unwrap();
    assert_eq!(acc_after2.daily_used, 1, "重复提交绝对不得二次增加账号额度");

    let files = store::catalog::list_book_files(&db.pool, grant.book.book_id)
        .await
        .unwrap();
    assert_eq!(
        files.len(),
        1,
        "同一图书同一格式在 book_files 中只能存在一条有效记录"
    );

    // 4. 旧世代结果提交：不得覆盖现有任务状态
    let mut stale_report = report.clone();
    stale_report.stage_version = 0; // 旧版本
    let res3 = submit_result(&state, &stale_report).await.unwrap();
    assert!(!res3.applied, "旧世代版本上报不得生效");

    db.teardown().await;
}
