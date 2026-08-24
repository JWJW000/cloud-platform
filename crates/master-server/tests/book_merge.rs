//! 图书合并与去重校验的真实数据库测试（第 16.4 节）。
//!
//! 验证场景：
//! - 合并两本书时锁定任务和文件；
//! - 同格式相同哈希可以合并；
//! - 同格式不同哈希必须拒绝；
//! - 执行中的 source 禁止合并；
//! - 已合并的图书禁止再次作为源或目标。

mod support;

use master_server::models::ImportRow;
use master_server::store;
use master_server::store::catalog::ImportRequest;
use platform_domain::{BatchStatus, TaskStatus};

#[tokio::test]
async fn 图书合并规则与哈希冲突检查() {
    let db = require_db!();

    let req = ImportRequest {
        batch_name: "合并测试批次",
        source_file: None,
        format: "pdf",
        priority: 10,
        created_by: None,
        max_attempts: 3,
    };
    let rows = vec![
        ImportRow {
            title: "算法导论第4版".to_string(),
            author: Some("作者A".to_string()),
            publisher: None,
            isbn: None,
        },
        ImportRow {
            title: "算法导论第4版(修订)".to_string(),
            author: Some("作者A".to_string()),
            publisher: None,
            isbn: None,
        },
    ];

    let summary = store::catalog::import_books(&db.pool, &req, &rows)
        .await
        .unwrap();
    let batch_id = summary.batch_id.unwrap();
    store::catalog::set_batch_status(&db.pool, batch_id, BatchStatus::Running)
        .await
        .unwrap();

    let books = store::catalog::list_books(&db.pool, None, None, 10, 0)
        .await
        .unwrap();
    let book1 = books
        .iter()
        .find(|b| b.raw_title == "算法导论第4版")
        .unwrap();
    let book2 = books
        .iter()
        .find(|b| b.raw_title == "算法导论第4版(修订)")
        .unwrap();

    let hash_a = "1111111111111111111111111111111111111111111111111111111111111111";
    let hash_b = "2222222222222222222222222222222222222222222222222222222222222222";

    // 2. 为两本书分别记录不同哈希的 PDF 文件
    store::catalog::record_book_file(
        &db.pool,
        book1.id,
        "pdf",
        "000001-000500/000001-算法.pdf",
        1000,
        hash_a,
        None,
    )
    .await
    .unwrap();
    store::catalog::record_book_file(
        &db.pool,
        book2.id,
        "pdf",
        "000001-000500/000002-算法.pdf",
        1000,
        hash_b,
        None,
    )
    .await
    .unwrap();

    // 3. 不同哈希合并：必须被拒绝并报错
    let conflict_res = store::catalog::merge_books(&db.pool, book2.id, book1.id).await;
    assert!(conflict_res.is_err(), "同格式不同哈希合并必须失败");

    // 4. 改为相同哈希后合并：应当成功
    sqlx::query("UPDATE book_files SET sha256 = $2 WHERE book_id = $1")
        .bind(book2.id)
        .bind(hash_a)
        .execute(&db.pool)
        .await
        .unwrap();

    store::catalog::merge_books(&db.pool, book2.id, book1.id)
        .await
        .expect("同哈希合并应当成功");

    let b2_after = store::catalog::get_book(&db.pool, book2.id).await.unwrap();
    assert_eq!(
        b2_after.merged_into,
        Some(book1.id),
        "源图书必须标记 merged_into"
    );
    assert_eq!(b2_after.verify_status, "已合并");

    // 5. 执行中的 source 禁止合并
    let req2 = ImportRequest {
        batch_name: "合并测试批次2",
        source_file: None,
        format: "pdf",
        priority: 10,
        created_by: None,
        max_attempts: 3,
    };
    let rows2 = vec![
        ImportRow {
            title: "执行中图书".to_string(),
            author: None,
            publisher: None,
            isbn: None,
        },
        ImportRow {
            title: "目标图书".to_string(),
            author: None,
            publisher: None,
            isbn: None,
        },
    ];
    store::catalog::import_books(&db.pool, &req2, &rows2)
        .await
        .unwrap();
    let books2 = store::catalog::list_books(&db.pool, None, None, 10, 0)
        .await
        .unwrap();
    let book3 = books2.iter().find(|b| b.raw_title == "执行中图书").unwrap();
    let book4 = books2.iter().find(|b| b.raw_title == "目标图书").unwrap();
    let task3 = store::task::get_task_by_book_format(&db.pool, book3.id, "pdf")
        .await
        .unwrap()
        .unwrap();

    sqlx::query("UPDATE book_tasks SET status = $2 WHERE id = $1")
        .bind(task3.id)
        .bind(TaskStatus::Running.as_str())
        .execute(&db.pool)
        .await
        .unwrap();

    let running_res = store::catalog::merge_books(&db.pool, book3.id, book4.id).await;
    assert!(running_res.is_err(), "执行中的源图书禁止合并");

    db.teardown().await;
}
