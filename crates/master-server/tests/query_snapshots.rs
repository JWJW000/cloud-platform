mod support;

use master_server::catalog::stats::{read_stats, refresh_stats};
use master_server::store::catalog_v1::get_catalog_stats;
use std::time::Duration;

#[tokio::test]
async fn missing_snapshot_never_waits_for_business_tables() {
    let db = require_db!();
    let state = db.create_test_state();
    let mut blocker = db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE editions IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(1), read_stats(&state))
        .await
        .unwrap()
        .unwrap();
    assert!(!response.ready);
    assert!(response.computed_at.is_none());
    blocker.rollback().await.unwrap();
    db.teardown().await;
}

#[tokio::test]
async fn persisted_snapshot_survives_restart_and_skips_fresh_recalculation() {
    let db = require_db!();
    sqlx::query("INSERT INTO works (id, preferred_title, normalized_title) VALUES ('00000000-0000-0000-0000-000000000001', 'book', 'book')").execute(&db.pool).await.unwrap();
    sqlx::query("INSERT INTO editions (id, work_id, edition_title) VALUES ('00000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000001', 'book')").execute(&db.pool).await.unwrap();
    let state = db.create_test_state();
    assert!(refresh_stats(&state).await.unwrap());
    let first = read_stats(&state).await.unwrap();
    assert!(first.ready);
    assert_eq!(first.stats.total_works, 1);
    assert_eq!(first.stats.total_editions, 1);
    assert_eq!(first.stats.missing_isbn_count, 1);
    assert_eq!(first.stats.missing_author_count, 1);
    let restarted = db.create_test_state();
    let mut blocker = db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE editions IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let stored = tokio::time::timeout(Duration::from_secs(1), read_stats(&restarted))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.computed_at, first.computed_at);
    assert_eq!(stored.stats.total_editions, 1);
    assert!(
        !tokio::time::timeout(Duration::from_secs(1), refresh_stats(&restarted))
            .await
            .unwrap()
            .unwrap()
    );
    blocker.rollback().await.unwrap();
    db.teardown().await;
}

#[tokio::test]
async fn failed_refresh_preserves_previous_snapshot() {
    let db = require_db!();
    let state = db.create_test_state();
    assert!(refresh_stats(&state).await.unwrap());
    sqlx::query("UPDATE query_snapshots SET computed_at = now() - interval '10 minutes'")
        .execute(&db.pool)
        .await
        .unwrap();
    let restarted = db.create_test_state();
    let before = read_stats(&restarted).await.unwrap();
    assert!(before.stale);
    sqlx::query("ALTER TABLE editions RENAME TO editions_unavailable")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        get_catalog_stats(&db.pool).await.is_err(),
        "计算失败必须传播，不能返回全零结果"
    );
    assert!(refresh_stats(&restarted).await.is_err());
    let after = read_stats(&restarted).await.unwrap();
    assert!(after.ready && after.stale);
    assert_eq!(after.computed_at, before.computed_at);
    sqlx::query("ALTER TABLE editions_unavailable RENAME TO editions")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        refresh_stats(&restarted).await.unwrap(),
        "失败事务必须释放锁，以便重试"
    );
    assert!(!read_stats(&restarted).await.unwrap().stale);
    db.teardown().await;
}

#[tokio::test]
async fn refresh_is_single_flight_across_instances() {
    let db = require_db!();
    let state = db.create_test_state();
    let other = db.create_test_state();
    let mut lock = db.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended(current_schema() || ':catalog_stats_v1', 0))").execute(&mut *lock).await.unwrap();
    assert!(
        !tokio::time::timeout(Duration::from_secs(1), refresh_stats(&state))
            .await
            .unwrap()
            .unwrap()
    );
    lock.rollback().await.unwrap();
    let (a, b) = tokio::join!(refresh_stats(&state), refresh_stats(&other));
    assert_ne!(
        a.unwrap(),
        b.unwrap(),
        "只有一个实例计算，另一个跳过锁或读取新快照"
    );
    assert!(read_stats(&other).await.unwrap().ready);
    db.teardown().await;
}
