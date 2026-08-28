//! 全局图书下载调度闸门的真实 PostgreSQL 验收测试。

mod support;

use master_server::scheduler::{
    control::global_download_is_paused, get_global_download_control, set_global_download_paused,
};

#[tokio::test]
async fn 全局下载开关_持久化并在领取事务内生效() {
    let db = require_db!();

    let initial = get_global_download_control(&db.pool).await.unwrap();
    assert!(!initial.paused);

    let paused = set_global_download_paused(&db.pool, true).await.unwrap();
    assert!(paused.paused);

    let mut tx = db.pool.begin().await.unwrap();
    assert!(global_download_is_paused(&mut tx).await.unwrap());
    tx.rollback().await.unwrap();

    let resumed = set_global_download_paused(&db.pool, false).await.unwrap();
    assert!(!resumed.paused);
    assert!(!get_global_download_control(&db.pool).await.unwrap().paused);

    db.teardown().await;
}

#[tokio::test]
async fn 全局下载开关_异常配置按暂停处理() {
    let db = require_db!();
    sqlx::query(
        "UPDATE settings SET value = '{\"unexpected\":true}' WHERE key = 'global_download_paused'",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    assert!(get_global_download_control(&db.pool).await.unwrap().paused);
    let mut tx = db.pool.begin().await.unwrap();
    assert!(global_download_is_paused(&mut tx).await.unwrap());
    tx.rollback().await.unwrap();

    db.teardown().await;
}

#[tokio::test]
async fn 全局暂停_等待并发领取检查完成后再返回() {
    let db = require_db!();
    let mut claim_tx = db.pool.begin().await.unwrap();
    assert!(!global_download_is_paused(&mut claim_tx).await.unwrap());

    let pool = db.pool.clone();
    let pause = tokio::spawn(async move { set_global_download_paused(&pool, true).await });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!pause.is_finished(), "暂停必须等待持有共享锁的领取事务");

    claim_tx.rollback().await.unwrap();
    let paused = tokio::time::timeout(std::time::Duration::from_secs(2), pause)
        .await
        .expect("释放领取锁后暂停应及时完成")
        .unwrap()
        .unwrap();
    assert!(paused.paused);

    db.teardown().await;
}
