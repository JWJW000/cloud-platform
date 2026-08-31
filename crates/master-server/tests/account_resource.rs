//! 账号列表统计与手动重置额度的真实数据库测试。

mod support;

use platform_domain::AccountStatus;
use uuid::Uuid;

async fn insert_account(
    pool: &sqlx::PgPool,
    email: &str,
    status: AccountStatus,
    daily_used: i32,
) -> Uuid {
    let account = master_server::store::resource::create_account(
        pool,
        email,
        "cipher-test",
        "测试账号",
        10,
        status,
    )
    .await
    .expect("插入测试账号失败");

    sqlx::query("UPDATE accounts SET daily_used = $1 WHERE email = $2")
        .bind(daily_used)
        .bind(email)
        .execute(pool)
        .await
        .expect("更新测试账号额度失败");
    account.id
}

#[tokio::test]
async fn account_list_total_status_summary_and_manual_reset() {
    let db = require_db!();

    let _registered_id = insert_account(
        &db.pool,
        &format!("registered-{}@test.local", Uuid::new_v4().simple()),
        AccountStatus::Registered,
        0,
    )
    .await;
    let exhausted_email = format!("exhausted-{}@test.local", Uuid::new_v4().simple());
    let exhausted_id = insert_account(
        &db.pool,
        &exhausted_email,
        AccountStatus::ExhaustedToday,
        10,
    )
    .await;
    let _disabled_id = insert_account(
        &db.pool,
        &format!("disabled-{}@test.local", Uuid::new_v4().simple()),
        AccountStatus::Disabled,
        0,
    )
    .await;

    let total = master_server::store::resource::count_accounts(&db.pool, None)
        .await
        .unwrap();
    assert_eq!(total, 3);

    let counts = master_server::store::resource::account_status_counts(&db.pool)
        .await
        .unwrap();
    let by_status = |status: &str| {
        counts
            .iter()
            .find(|(value, _)| value == status)
            .map(|(_, count)| *count)
            .unwrap_or(0)
    };
    assert_eq!(by_status("已注册"), 1);
    assert_eq!(by_status("今日额度耗尽"), 1);
    assert_eq!(by_status("已禁用"), 1);

    let reset_count = master_server::store::resource::reset_exhausted_quota(&db.pool)
        .await
        .unwrap();
    assert_eq!(reset_count, 1);

    let account = master_server::store::resource::get_account(&db.pool, exhausted_id)
        .await
        .unwrap();
    assert_eq!(account.status, "已注册");
    assert_eq!(account.daily_used, 0);

    let reset_disabled_count = master_server::store::resource::reset_disabled_accounts(&db.pool)
        .await
        .unwrap();
    assert_eq!(reset_disabled_count, 1);

    let disabled_account = master_server::store::resource::get_account(&db.pool, _disabled_id)
        .await
        .unwrap();
    assert_eq!(disabled_account.status, "已注册");
    assert_eq!(disabled_account.daily_used, 0);

    let available = master_server::store::resource::count_available_accounts(&db.pool)
        .await
        .unwrap();
    assert_eq!(available, 3);

    db.teardown().await;
}
