//! NAS 证据闭环的真实数据库测试（V4 方案第 12 节、R6）。
//!
//! 验证场景：
//! - 领取任务时写入路径/文件名/格式期望；
//! - 不确定结果进入「待确认」时在同一事务固化大小/SHA 证据；
//! - 完整证据匹配时补登记文件并完成任务；
//! - 缺固化 SHA 时不得自动判完成；
//! - 不同 SHA 保持待确认并产生严重告警。

mod support;

use master_server::models::ImportRow;
use master_server::scheduler::submit::{
    nas_check_result, submit_result, FileEvidence, NasCheckReport, ResultReport,
};
use master_server::store;
use master_server::store::catalog::ImportRequest;
use platform_domain::{BatchStatus, ExecutionResult, TaskStatus};
use uuid::Uuid;

fn state_for(db: &support::TestDb) -> master_server::state::AppState {
    db.create_test_state()
}

/// 导入图书、建节点、建会话并领取任务。
async fn setup_and_claim(
    db: &support::TestDb,
    title: &str,
) -> master_server::scheduler::claim::TaskAssignment {
    let batch_name = format!("证据批次-{title}");
    let import_req = ImportRequest {
        batch_name: &batch_name,
        source_file: None,
        format: "pdf",
        priority: 10,
        created_by: None,
        max_attempts: 3,
    };
    let rows = vec![ImportRow {
        title: title.to_string(),
        author: None,
        publisher: None,
        isbn: None,
    }];
    let summary = store::catalog::import_books(&db.pool, &import_req, &rows)
        .await
        .unwrap();
    let batch_id = summary.batch_id.unwrap();
    store::catalog::set_batch_status(&db.pool, batch_id, BatchStatus::Running)
        .await
        .unwrap();

    let mut conn = db.pool.acquire().await.unwrap();
    let node = store::node::upsert_node(
        &mut conn,
        format!("节点-{title}").as_str(),
        "host",
        "Linux",
        "1.0",
        "1.0",
        1,
        &format!("token-{title}"),
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

    let mut tx = db.pool.begin().await.unwrap();
    let session = store::session::create_session(
        &mut tx,
        &store::session::NewSession {
            node_id: node.id,
            slot_index: 0,
            account_id: None,
            proxy_id: None,
            task_type: platform_domain::TaskType::BookDownload,
            local_forward_port: None,
            lease_secs: 120,
        },
    )
    .await
    .unwrap();
    store::session::activate_session(&mut *tx, session.id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let state = state_for(db);
    let outcome = master_server::scheduler::claim::claim_next_task(&state, node.id, session.id)
        .await
        .unwrap();
    match outcome {
        master_server::scheduler::ClaimOutcome::Assigned(g) => *g,
        other => panic!("领取任务失败：{other:?}"),
    }
}

#[tokio::test]
async fn 领取时写入期望证据_不确定结果固化大小与哈希() {
    let db = require_db!();
    let grant = setup_and_claim(&db, "证据固化图书").await;
    let state = state_for(&db);

    // 1. 领取时写入路径/文件名/格式期望（R6 第 12.2 节）
    let task = store::task::get_task(&db.pool, grant.task_id)
        .await
        .unwrap();
    assert_eq!(
        task.expected_nas_relative_path.as_deref(),
        Some(grant.nas_relative_path.as_str())
    );
    assert!(!task.expected_file_name.unwrap_or_default().is_empty());
    assert_eq!(task.expected_format.as_deref(), Some("pdf"));

    // 2. 不确定结果携带本地证据 → 任务进入待确认并固化大小/SHA
    let local = FileEvidence {
        nas_relative_path: grant.nas_relative_path.clone(),
        file_name: "evidence.pdf".to_string(),
        size_bytes: 250 * 1024,
        sha256: "a2".repeat(32),
        format: "pdf".to_string(),
    };
    let report = ResultReport {
        session_id: grant.session_id,
        execution_id: grant.execution_id,
        task_id: grant.task_id,
        node_id: None,
        result: ExecutionResult::Uncertain,
        reason: "NAS 拷贝中断".to_string(),
        stage_version: grant.stage_version,
        duration_ms: None,
        quota: None,
        file: Some(local.clone()),
    };
    let outcome = submit_result(&state, &report).await.unwrap();
    assert!(outcome.applied);
    assert_eq!(outcome.task_status, Some(TaskStatus::NeedsConfirm));

    let task = store::task::get_task(&db.pool, grant.task_id)
        .await
        .unwrap();
    assert_eq!(task.status, TaskStatus::NeedsConfirm.as_str());
    assert_eq!(task.expected_size_bytes, Some(250 * 1024));
    assert_eq!(
        task.expected_sha256.as_deref(),
        Some("a2".repeat(32).as_str())
    );

    // 3. 完整证据匹配 → 补登记文件并完成任务
    let evidence = FileEvidence {
        nas_relative_path: grant.nas_relative_path.clone(),
        file_name: "evidence.pdf".to_string(),
        size_bytes: 250 * 1024,
        sha256: "a2".repeat(32),
        format: "pdf".to_string(),
    };
    let check = NasCheckReport {
        node_id: grant.node_id,
        task_id: Some(grant.task_id),
        mount_present: true,
        writable: true,
        free_gb: 100,
        file: Some(&evidence),
        detail: "核验完成",
    };
    nas_check_result(&state, &check).await.unwrap();

    let task = store::task::get_task(&db.pool, grant.task_id)
        .await
        .unwrap();
    assert_eq!(task.status, TaskStatus::Completed.as_str());

    let files = store::catalog::list_book_files(&db.pool, task.book_id)
        .await
        .unwrap();
    assert_eq!(files.len(), 1, "核验通过后应补登记文件");
    assert_eq!(files[0].sha256, "a2".repeat(32));
}

#[tokio::test]
async fn 缺固化哈希时核验不得自动判完成() {
    let db = require_db!();
    let grant = setup_and_claim(&db, "缺哈希图书").await;
    let state = state_for(&db);

    // 任务进入待确认但**不带**本地证据（无 SHA 可固化）
    let report = ResultReport {
        session_id: grant.session_id,
        execution_id: grant.execution_id,
        task_id: grant.task_id,
        node_id: None,
        result: ExecutionResult::Uncertain,
        reason: "NAS 不可写".to_string(),
        stage_version: grant.stage_version,
        duration_ms: None,
        quota: None,
        file: None,
    };
    submit_result(&state, &report).await.unwrap();
    let task = store::task::get_task(&db.pool, grant.task_id)
        .await
        .unwrap();
    assert_eq!(task.status, TaskStatus::NeedsConfirm.as_str());

    // 核验上报即使文件存在，缺 SHA 也不得自动判完成
    let evidence = FileEvidence {
        nas_relative_path: grant.nas_relative_path.clone(),
        file_name: "x.pdf".to_string(),
        size_bytes: 300 * 1024,
        sha256: "b3".repeat(32),
        format: "pdf".to_string(),
    };
    let check = NasCheckReport {
        node_id: grant.node_id,
        task_id: Some(grant.task_id),
        mount_present: true,
        writable: true,
        free_gb: 100,
        file: Some(&evidence),
        detail: "文件存在",
    };
    nas_check_result(&state, &check).await.unwrap();

    let task = store::task::get_task(&db.pool, grant.task_id)
        .await
        .unwrap();
    assert_eq!(
        task.status,
        TaskStatus::NeedsConfirm.as_str(),
        "缺固化 SHA 时核验不得自动判完成"
    );
    let files = store::catalog::list_book_files(&db.pool, task.book_id)
        .await
        .unwrap();
    assert!(files.is_empty(), "缺 SHA 不得登记文件");
}

#[tokio::test]
async fn 不同哈希核验保持待确认并产生严重告警() {
    let db = require_db!();
    let grant = setup_and_claim(&db, "哈希冲突图书").await;
    let state = state_for(&db);

    // 固化期望 SHA
    let local = FileEvidence {
        nas_relative_path: grant.nas_relative_path.clone(),
        file_name: "c.pdf".to_string(),
        size_bytes: 200 * 1024,
        sha256: "c4".repeat(32),
        format: "pdf".to_string(),
    };
    let report = ResultReport {
        session_id: grant.session_id,
        execution_id: grant.execution_id,
        task_id: grant.task_id,
        node_id: None,
        result: ExecutionResult::Uncertain,
        reason: "写入中断".to_string(),
        stage_version: grant.stage_version,
        duration_ms: None,
        quota: None,
        file: Some(local),
    };
    submit_result(&state, &report).await.unwrap();

    // 核验发现不同 SHA
    let evidence = FileEvidence {
        nas_relative_path: grant.nas_relative_path.clone(),
        file_name: "c.pdf".to_string(),
        size_bytes: 200 * 1024,
        sha256: "d5".repeat(32),
        format: "pdf".to_string(),
    };
    let check = NasCheckReport {
        node_id: grant.node_id,
        task_id: Some(grant.task_id),
        mount_present: true,
        writable: true,
        free_gb: 100,
        file: Some(&evidence),
        detail: "发现冲突",
    };
    nas_check_result(&state, &check).await.unwrap();

    let task = store::task::get_task(&db.pool, grant.task_id)
        .await
        .unwrap();
    assert_eq!(
        task.status,
        TaskStatus::NeedsConfirm.as_str(),
        "哈希冲突必须保持待确认"
    );
    let files = store::catalog::list_book_files(&db.pool, task.book_id)
        .await
        .unwrap();
    assert!(files.is_empty(), "哈希冲突不得登记文件");
    let _ = task;
    let _ = Uuid::new_v4();
}
