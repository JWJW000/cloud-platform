//! 结果提交与取消竞争的真实数据库测试（V4 方案第 11.8 节）。
//!
//! 验证场景：
//! - 批次取消先赢：任务进入取消，晚到的成功结果不得复活任务；
//! - 成功先赢：任务判完成，晚到的批次取消不复写全局完成任务；
//! - 结果 CAS 未命中时不登记文件、不扣额度、不加统计。

mod support;

use master_server::models::ImportRow;
use master_server::scheduler::submit::{submit_result, FileEvidence, ResultReport};
use master_server::store;
use master_server::store::catalog::ImportRequest;
use platform_domain::{BatchStatus, ExecutionResult, TaskStatus};
use uuid::Uuid;

/// 导入一本测试图书并启动批次，返回批次编号。
async fn import_book(db: &support::TestDb, title: &str) -> Uuid {
    let name = format!("竞争批次-{title}");
    let import_req = ImportRequest {
        batch_name: &name,
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
    batch_id
}

fn state_for(db: &support::TestDb) -> master_server::state::AppState {
    db.create_test_state()
}

/// 建立节点 + 会话并领取一个任务，返回任务分配。
async fn claim_task(
    db: &support::TestDb,
    node_name: &str,
) -> master_server::scheduler::claim::TaskAssignment {
    let mut conn = db.pool.acquire().await.unwrap();
    let node = store::node::upsert_node(
        &mut conn,
        node_name,
        "host",
        "Linux",
        "1.0",
        "1.0",
        1,
        &format!("token-{node_name}"),
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

fn success_report(
    grant: &master_server::scheduler::claim::TaskAssignment,
    sha: &str,
) -> ResultReport {
    let file = FileEvidence {
        nas_relative_path: grant.nas_relative_path.clone(),
        file_name: grant
            .nas_relative_path
            .rsplit('/')
            .next()
            .unwrap()
            .to_string(),
        size_bytes: 200 * 1024,
        sha256: sha.to_string(),
        format: "pdf".to_string(),
    };
    ResultReport {
        session_id: grant.session_id,
        execution_id: grant.execution_id,
        task_id: grant.task_id,
        node_id: None,
        result: ExecutionResult::Success,
        reason: "测试入库".to_string(),
        stage_version: grant.stage_version,
        duration_ms: Some(1),
        quota: None,
        file: Some(file),
    }
}

#[tokio::test]
async fn 取消先赢时晚到成功结果不得复活任务() {
    let db = require_db!();
    let _batch_id = import_book(&db, "取消先赢图书").await;
    let grant = claim_task(&db, "取消先赢节点").await;
    let state = state_for(&db);

    // 1. 管理员取消任务（先赢）：运行中任务标记 cancel_requested
    store::task::cancel_task(&db.pool, grant.task_id)
        .await
        .unwrap();
    let task = store::task::get_task(&db.pool, grant.task_id)
        .await
        .unwrap();
    assert!(task.cancel_requested, "取消后任务必须标记 cancel_requested");

    // 2. 晚到的成功结果：必须只留档，不得把任务改回已完成
    let outcome = submit_result(&state, &success_report(&grant, &"d1".repeat(32)))
        .await
        .unwrap();
    assert!(!outcome.applied, "取消先赢后，晚到成功必须只留档");

    let task = store::task::get_task(&db.pool, grant.task_id)
        .await
        .unwrap();
    assert_ne!(
        task.status,
        TaskStatus::Completed.as_str(),
        "晚到的成功结果不得复活任务"
    );
    assert!(task.cancel_requested, "任务必须保持取消标记");

    // 3. 文件记录不得产生
    let files = store::catalog::list_book_files(&db.pool, task.book_id)
        .await
        .unwrap();
    assert!(files.is_empty(), "取消先赢后不应登记任何文件");

    // 4. 执行记录必须被收尾（P1）：CAS 未命中不得让 task_execution 永远「未完成」
    let finished: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT finished_at FROM task_executions WHERE id = $1")
            .bind(grant.execution_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(
        finished.is_some(),
        "取消先赢后执行记录必须带完成时间，不能永远挂在运行中"
    );
}

#[tokio::test]
async fn 成功与取消真并发提交时执行记录必被收尾() {
    let db = require_db!();
    let _batch_id = import_book(&db, "真并发图书").await;
    let grant = claim_task(&db, "真并发节点").await;
    let state = state_for(&db);

    // 两个独立连接（连接池最多 4 条）真并发：一个事务取消、一个提交成功结果。
    let task_id = grant.task_id;
    let cancel_pool = db.pool.clone();
    let cancel_handle = tokio::spawn(async move {
        // 取消可能输掉（任务已先判完成 → 终态拒绝），这是合法结局
        store::task::cancel_task(&cancel_pool, task_id).await.ok()
    });
    let submit_outcome = submit_result(&state, &success_report(&grant, &"9b".repeat(32)))
        .await
        .unwrap();
    let _cancel_target = cancel_handle.await.unwrap();

    // 无论谁先赢，任务都必须落在一个一致状态：
    // - 取消先赢：cancel_requested=true，成功只留档；
    // - 成功先赢：任务已完成，取消被拒绝或幂等。
    let task = store::task::get_task(&db.pool, task_id).await.unwrap();
    if task.status == TaskStatus::Completed.as_str() {
        assert!(submit_outcome.applied, "成功先赢时成功必须应用");
    } else {
        assert!(task.cancel_requested, "未完成则必须保持取消标记");
        assert!(!submit_outcome.applied, "取消先赢时成功必须只留档");
    }

    // 关键不变量：执行记录必须已收尾（P1）
    let finished: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT finished_at FROM task_executions WHERE id = $1")
            .bind(grant.execution_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(
        finished.is_some(),
        "并发提交后执行记录必须带完成时间（result={}）",
        submit_outcome.detail
    );
}

#[tokio::test]
async fn 成功先赢时晚到批次取消不复写全局完成() {
    let db = require_db!();
    let batch_id = import_book(&db, "成功先赢图书").await;
    let grant = claim_task(&db, "成功先赢节点").await;
    let state = state_for(&db);

    // 1. 成功结果先提交：任务判完成
    let outcome = submit_result(&state, &success_report(&grant, &"e1".repeat(32)))
        .await
        .unwrap();
    assert!(outcome.applied, "正常成功必须应用");
    let task = store::task::get_task(&db.pool, grant.task_id)
        .await
        .unwrap();
    assert_eq!(task.status, TaskStatus::Completed.as_str());

    // 2. 批次先标记为已完成（领域迁移允许 Running → Completed），
    //    然后取消已完成批次必须被拒绝（V4 第 11.6 节）
    store::catalog::set_batch_status(&db.pool, batch_id, BatchStatus::Completed)
        .await
        .unwrap();
    let err = store::task::cancel_batch(&db.pool, batch_id)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("已完成"),
        "已完成批次取消应被拒绝：{err}"
    );

    let task_after = store::task::get_task(&db.pool, grant.task_id)
        .await
        .unwrap();
    assert_eq!(
        task_after.status,
        TaskStatus::Completed.as_str(),
        "晚到的取消不得复写全局完成任务"
    );
}

#[tokio::test]
async fn 旧执行晚到新执行已领取时成功只留档() {
    let db = require_db!();
    let _batch_id = import_book(&db, "世代竞争图书").await;
    let first = claim_task(&db, "世代竞争节点A").await;
    let state = state_for(&db);

    // 1. 旧执行的结果先到但被当作 Current 处理前，先模拟任务被新执行领走：
    //    直接更新租约（模拟 reaper 回收后新执行领取）
    let mut tx = db.pool.begin().await.unwrap();
    let new_exec = Uuid::new_v4();
    let new_session = Uuid::new_v4();
    sqlx::query(
        "UPDATE book_tasks SET stage_version = stage_version + 1, \
             lease_node_id = NULL, lease_session_id = $2, lease_execution_id = $3, \
             status = '执行中', cancel_requested = FALSE, updated_at = now() \
         WHERE id = $1",
    )
    .bind(first.task_id)
    .bind(new_session)
    .bind(new_exec)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // 2. 旧执行的成功结果晚到：租约/世代都不匹配，只能留档
    let outcome = submit_result(&state, &success_report(&first, &"f1".repeat(32)))
        .await
        .unwrap();
    assert!(!outcome.applied, "旧执行晚到成功必须只留档");

    let task = store::task::get_task(&db.pool, first.task_id)
        .await
        .unwrap();
    assert_eq!(
        task.status,
        TaskStatus::Running.as_str(),
        "新执行的任务不能被旧结果改动"
    );
    assert_eq!(
        task.lease_execution_id,
        Some(new_exec),
        "租约必须仍属于新执行"
    );
}
