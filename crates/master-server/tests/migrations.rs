//! 数据库迁移验收测试（V4 方案第 17.3 节）。
//!
//! 验证场景：
//! - 空数据库执行全部迁移成功；
//! - 重复启动不会重复迁移（幂等）；
//! - 0003 新增的证据字段 CHECK 约束生效（非法 SHA/大小被拒绝）；
//! - proxies (provider, external_id) 唯一索引生效；
//! - users 补齐 updated_at。

mod support;

use uuid::Uuid;

#[tokio::test]
async fn 全迁移_幂等_约束生效() {
    let db = require_db!();

    // 1. 全部迁移已由 TestDb::setup 执行，这里验证关键 schema 元素存在
    let users_updated_at: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns \
         WHERE table_schema = current_schema() AND table_name = 'users' \
           AND column_name = 'updated_at'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(users_updated_at, 1, "users.updated_at 必须存在");

    let evidence_checks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.table_constraints \
         WHERE table_schema = current_schema() AND table_name = 'book_tasks' \
           AND constraint_name IN \
           ('book_tasks_expected_size_nonnegative', 'book_tasks_expected_sha256_format', \
            'book_tasks_expected_format_valid', 'book_tasks_expected_path_relative')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(evidence_checks, 4, "证据字段 CHECK 约束必须全部存在");

    let global_download_paused: serde_json::Value =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'global_download_paused'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(global_download_paused, serde_json::json!(false));

    // 2. 重复执行迁移：幂等，不报错（sqlx 按版本记录跳过）
    master_server::store::run_migrations(&db.pool)
        .await
        .unwrap();

    // 3. 非法 SHA 被 CHECK 拒绝
    let book_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO books (id, raw_title, normalized_title, dedup_key, verify_status) \
         VALUES ($1, '迁移测试', '迁移测试', $2, '已确认')",
    )
    .bind(book_id)
    .bind("title:迁移测试".to_string())
    .execute(&db.pool)
    .await
    .unwrap();
    let bad_sha = sqlx::query(
        "INSERT INTO book_tasks (id, book_id, format, status, expected_sha256) \
         VALUES ($1, $2, 'pdf', '待处理', 'not-a-valid-sha')",
    )
    .bind(Uuid::new_v4())
    .bind(book_id)
    .execute(&db.pool)
    .await;
    assert!(
        bad_sha.is_err(),
        "非法 expected_sha256 必须被 CHECK 约束拒绝"
    );

    // 非法负大小被拒绝
    let bad_size = sqlx::query(
        "INSERT INTO book_tasks (id, book_id, format, status, expected_size_bytes) \
         VALUES ($1, $2, 'pdf', '待处理', -5)",
    )
    .bind(Uuid::new_v4())
    .bind(book_id)
    .execute(&db.pool)
    .await;
    assert!(
        bad_size.is_err(),
        "负的 expected_size_bytes 必须被 CHECK 约束拒绝"
    );

    // 4. proxies (provider, external_id) 唯一索引生效
    let _: Uuid = sqlx::query_scalar(
        "INSERT INTO proxies (id, provider, external_id, label, scheme, host, port, status) \
         VALUES ($1, 'Webshare', 'ext-1', 'p1', 'http', '1.2.3.4', 8080, '可用') RETURNING id",
    )
    .bind(Uuid::new_v4())
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let dup = sqlx::query(
        "INSERT INTO proxies (id, provider, external_id, label, scheme, host, port, status) \
         VALUES ($1, 'Webshare', 'ext-1', 'p2', 'http', '5.6.7.8', 9090, '可用')",
    )
    .bind(Uuid::new_v4())
    .execute(&db.pool)
    .await;
    assert!(
        dup.is_err(),
        "同一 provider 的重复 external_id 必须被唯一索引拒绝"
    );
}
