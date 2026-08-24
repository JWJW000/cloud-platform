//! 批次取消与共享任务语义的真实数据库测试（第 16.4 节、V3 方案第 10 节）。
//!
//! 验证场景：
//! - 共享任务保护「待开始」「执行中」「已暂停」的批次；
//! - 独占运行任务收到精确 CancelTask 目标；
//! - 独占待处理任务直接转为「已取消」；
//! - 重复取消幂等。

mod support;

use master_server::models::ImportRow;
use master_server::store;
use master_server::store::catalog::ImportRequest;
use platform_domain::{BatchStatus, TaskStatus};
use uuid::Uuid;

#[tokio::test]
async fn 批次取消保护共享任务与精确下发() {
    let db = require_db!();

    // 1. 创建 3 个批次：Batch A（待取消）、Batch B（待开始）、Batch C（已暂停）
    let req_a = ImportRequest {
        batch_name: "批次A",
        source_file: None,
        format: "pdf",
        priority: 10,
        created_by: None,
        max_attempts: 3,
    };
    let rows_a = vec![
        ImportRow {
            title: "独占待处理书".to_string(),
            author: None,
            publisher: None,
            isbn: None,
        },
        ImportRow {
            title: "独占执行中书".to_string(),
            author: None,
            publisher: None,
            isbn: None,
        },
        ImportRow {
            title: "与待开始共享书".to_string(),
            author: None,
            publisher: None,
            isbn: None,
        },
        ImportRow {
            title: "与暂停共享书".to_string(),
            author: None,
            publisher: None,
            isbn: None,
        },
    ];

    let summary_a = store::catalog::import_books(&db.pool, &req_a, &rows_a)
        .await
        .unwrap();
    let batch_a_id = summary_a.batch_id.unwrap();
    store::catalog::set_batch_status(&db.pool, batch_a_id, BatchStatus::Running)
        .await
        .unwrap();

    let req_b = ImportRequest {
        batch_name: "批次B",
        source_file: None,
        format: "pdf",
        priority: 10,
        created_by: None,
        max_attempts: 3,
    };
    let rows_b = vec![ImportRow {
        title: "与待开始共享书".to_string(),
        author: None,
        publisher: None,
        isbn: None,
    }];
    let summary_b = store::catalog::import_books(&db.pool, &req_b, &rows_b)
        .await
        .unwrap();
    let batch_b_id = summary_b.batch_id.unwrap();
    store::catalog::set_batch_status(&db.pool, batch_b_id, BatchStatus::NotStarted)
        .await
        .unwrap();

    let req_c = ImportRequest {
        batch_name: "批次C",
        source_file: None,
        format: "pdf",
        priority: 10,
        created_by: None,
        max_attempts: 3,
    };
    let rows_c = vec![ImportRow {
        title: "与暂停共享书".to_string(),
        author: None,
        publisher: None,
        isbn: None,
    }];
    let summary_c = store::catalog::import_books(&db.pool, &req_c, &rows_c)
        .await
        .unwrap();
    let batch_c_id = summary_c.batch_id.unwrap();
    // 领域迁移：只能 Running → Paused（待开始不能直接暂停）
    store::catalog::set_batch_status(&db.pool, batch_c_id, BatchStatus::Running)
        .await
        .unwrap();
    store::catalog::set_batch_status(&db.pool, batch_c_id, BatchStatus::Paused)
        .await
        .unwrap();

    // 找到 Book 2 并将其状态模拟为「执行中」
    let tasks_a = store::task::list_tasks(
        &db.pool,
        &store::task::TaskFilter {
            batch_id: Some(batch_a_id),
            limit: 100,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let task1 = tasks_a.iter().find(|t| t.title == "独占待处理书").unwrap();
    let task2 = tasks_a.iter().find(|t| t.title == "独占执行中书").unwrap();
    let task3 = tasks_a
        .iter()
        .find(|t| t.title == "与待开始共享书")
        .unwrap();
    let task4 = tasks_a.iter().find(|t| t.title == "与暂停共享书").unwrap();

    let node_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let exec_id = Uuid::new_v4();

    // lease_node_id 有外键约束，先插入一条真实节点记录
    sqlx::query(
        "INSERT INTO worker_nodes (id, name, hostname, os, status, node_token_hash, max_slots) \
         VALUES ($1, $2, 'test-host', 'Linux', '在线', $3, 1)",
    )
    .bind(node_id)
    .bind(format!("测试节点-{node_id}"))
    .bind(format!("token-hash-{node_id}"))
    .execute(&db.pool)
    .await
    .unwrap();

    sqlx::query(
        "UPDATE book_tasks SET status = '执行中', lease_node_id = $2, lease_session_id = $3, lease_execution_id = $4 WHERE id = $1",
    )
    .bind(task2.id)
    .bind(node_id)
    .bind(session_id)
    .bind(exec_id)
    .execute(&db.pool)
    .await
    .unwrap();

    // 2. 取消 Batch A
    let (cancelled_batch, outcome) = store::task::cancel_batch(&db.pool, batch_a_id)
        .await
        .unwrap();
    assert_eq!(cancelled_batch.status, BatchStatus::Cancelled.as_str());

    // 3. 校验结果
    // Book 1: 独占且待处理 -> 直接被取消
    let task1_after = store::task::get_task(&db.pool, task1.id).await.unwrap();
    assert_eq!(task1_after.status, TaskStatus::Cancelled.as_str());
    assert!(outcome
        .directly_cancelled_task_ids
        .contains(&task1_after.id));

    // Book 2: 独占且执行中 -> 标记 cancel_requested 并返回 running_target
    let task2_after = store::task::get_task(&db.pool, task2.id).await.unwrap();
    assert_eq!(task2_after.status, TaskStatus::Running.as_str());
    assert!(task2_after.cancel_requested);
    assert_eq!(outcome.running_targets.len(), 1);
    assert_eq!(outcome.running_targets[0].task_id, task2.id);
    assert_eq!(outcome.running_targets[0].node_id, node_id);
    assert_eq!(outcome.running_targets[0].session_id, session_id);
    assert_eq!(outcome.running_targets[0].execution_id, exec_id);

    // Book 3: 与 Batch B（待开始）共享 -> 全局任务绝不能被取消！
    let task3_after = store::task::get_task(&db.pool, task3.id).await.unwrap();
    assert_eq!(task3_after.status, TaskStatus::Pending.as_str());
    assert!(!task3_after.cancel_requested);
    assert!(outcome.shared_task_ids.contains(&task3_after.id));

    // Book 4: 与 Batch C（已暂停）共享 -> 全局任务绝不能被取消！
    let task4_after = store::task::get_task(&db.pool, task4.id).await.unwrap();
    assert_eq!(task4_after.status, TaskStatus::Pending.as_str());
    assert!(!task4_after.cancel_requested);
    assert!(outcome.shared_task_ids.contains(&task4_after.id));

    // 4. 重复取消幂等
    let (cancelled_again, outcome2) = store::task::cancel_batch(&db.pool, batch_a_id)
        .await
        .unwrap();
    assert_eq!(cancelled_again.status, BatchStatus::Cancelled.as_str());
    assert_eq!(outcome2.directly_cancelled_task_ids.len(), 0);

    db.teardown().await;
}
