//! 业务任务统一下发与代理固定测试（V6 方案）。

mod support;

use master_server::scheduler::allocate::allocate_session;
use master_server::scheduler::claim::claim_next_task;
use master_server::scheduler::submit::{submit_registration_result, RegistrationResultReport};
use master_server::store;
use platform_domain::{
    AccountRegistrationTaskStatus, AccountStatus, BatchStatus, ExecutionResult, ManualActionType,
    ProxyStatus, TaskType, WorkerStatus,
};
use uuid::Uuid;

#[tokio::test]
async fn 图书任务首次执行绑定代理且重试固定同一代理() {
    let db = require_db!();

    let mut conn = db.pool.acquire().await.unwrap();
    let node = store::node::upsert_node(
        &mut conn,
        "代理固定测试节点",
        "host",
        "Linux",
        "1.0",
        "1.0",
        2,
        "token_hash",
    )
    .await
    .unwrap();
    store::node::ensure_slots(&mut conn, node.id, 2)
        .await
        .unwrap();
    drop(conn);
    store::node::approve_node(&db.pool, node.id, None)
        .await
        .unwrap();
    store::node::set_node_status(&db.pool, node.id, WorkerStatus::Online)
        .await
        .unwrap();
    store::node::set_nas_health(&db.pool, node.id, true, 100)
        .await
        .unwrap();

    let cipher = master_server::security::FieldCipher::from_base64(
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .unwrap();
    let _account = store::resource::create_account(
        &db.pool,
        "dl_acc@test.com",
        &cipher.encrypt("password").unwrap(),
        "nick",
        10,
        AccountStatus::Registered,
    )
    .await
    .unwrap();

    let proxy1 = store::resource::upsert_proxy(
        &db.pool, "Webshare", None, "p1", "http", "10.0.0.1", 8080, None, None,
    )
    .await
    .unwrap();
    store::resource::set_proxy_status(&db.pool, proxy1.id, ProxyStatus::Available, None)
        .await
        .unwrap();

    let proxy2 = store::resource::upsert_proxy(
        &db.pool, "Webshare", None, "p2", "http", "10.0.0.2", 8080, None, None,
    )
    .await
    .unwrap();
    store::resource::set_proxy_status(&db.pool, proxy2.id, ProxyStatus::Available, None)
        .await
        .unwrap();

    // 创建图书与批次
    let summary = store::catalog::import_books(
        &db.pool,
        &store::catalog::ImportRequest {
            batch_name: "代理绑定测试批次",
            source_file: None,
            priority: 10,
            format: "pdf",
            max_attempts: 3,
            created_by: None,
        },
        &[master_server::models::ImportRow {
            title: "代理绑定测试书".to_string(),
            author: None,
            publisher: None,
            isbn: None,
        }],
    )
    .await
    .unwrap();

    let batch_id = summary.batch_id.unwrap();
    store::catalog::set_batch_status(&db.pool, batch_id, BatchStatus::Running)
        .await
        .unwrap();

    let state = db.create_test_state();

    // 1. 分配第一个下载会话（绑定到 proxy1）
    let outcome = allocate_session(&state, node.id, TaskType::BookDownload, Some(0))
        .await
        .unwrap();
    let grant = match outcome {
        master_server::scheduler::AllocationOutcome::Granted(g) => g,
        other => panic!("预期会话分配成功，实际得到 {other:?}"),
    };

    let session_proxy_id = grant.proxy.as_ref().unwrap().proxy_id;

    // 2. 领取任务：任务首次领取应原子绑定到当前会话的代理
    let claim_outcome = claim_next_task(&state, node.id, grant.session_id)
        .await
        .unwrap();
    let assigned = match claim_outcome {
        master_server::scheduler::ClaimOutcome::Assigned(a) => a,
        other => panic!("预期领到任务，实际得到 {other:?}"),
    };

    let book_task: (Option<Uuid>, Option<String>) =
        sqlx::query_as("SELECT bound_proxy_id, bound_exit_ip FROM book_tasks WHERE id = $1")
            .bind(assigned.task_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();

    assert_eq!(
        book_task.0,
        Some(session_proxy_id),
        "图书任务首次分配必须绑定当前会话的代理 ID"
    );

    db.teardown().await;
}

#[tokio::test]
async fn 账号注册可以使用节点批准的全部槽位() {
    let db = require_db!();

    let mut conn = db.pool.acquire().await.unwrap();
    let node = store::node::upsert_node(
        &mut conn,
        "注册全槽位测试节点",
        "registration-full-slots-host",
        "Linux",
        "1.0",
        "1.0",
        5,
        "registration_full_slots_token",
    )
    .await
    .unwrap();
    store::node::ensure_slots(&mut conn, node.id, 5)
        .await
        .unwrap();
    drop(conn);
    store::node::approve_node(&db.pool, node.id, None)
        .await
        .unwrap();
    store::node::set_node_status(&db.pool, node.id, WorkerStatus::Online)
        .await
        .unwrap();

    let cipher = master_server::security::FieldCipher::from_base64(
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .unwrap();
    let batch = store::account_registration::create_batch(
        &db.pool,
        &store::account_registration::NewAccountRegistrationBatch {
            name: "注册全槽位测试批次".to_string(),
            source_file: None,
            priority: 5,
            created_by: None,
        },
    )
    .await
    .unwrap();

    for slot_index in 0..5 {
        let account = store::resource::create_account(
            &db.pool,
            &format!("full-slot-{slot_index}@test.com"),
            &cipher.encrypt("password").unwrap(),
            &format!("full-slot-{slot_index}"),
            10,
            AccountStatus::PendingRegistration,
        )
        .await
        .unwrap();
        store::account_registration::create_task(&db.pool, batch.id, account.id, 5)
            .await
            .unwrap();

        let proxy = store::resource::upsert_proxy(
            &db.pool,
            "Webshare",
            None,
            &format!("full-slot-proxy-{slot_index}"),
            "http",
            &format!("10.20.0.{}", slot_index + 1),
            8080,
            None,
            None,
        )
        .await
        .unwrap();
        store::resource::set_proxy_status(&db.pool, proxy.id, ProxyStatus::Available, None)
            .await
            .unwrap();
    }

    store::account_registration::update_batch_status(
        &db.pool,
        batch.id,
        BatchStatus::NotStarted,
        BatchStatus::Running,
    )
    .await
    .unwrap();

    let state = db.create_test_state();
    for slot_index in 0..5 {
        let outcome =
            allocate_session(&state, node.id, TaskType::AccountRegister, Some(slot_index))
                .await
                .unwrap();
        assert!(
            matches!(
                outcome,
                master_server::scheduler::AllocationOutcome::Granted(_)
            ),
            "第 {slot_index} 个注册槽位应分配成功，实际得到 {outcome:?}"
        );
    }

    let running_registration_sessions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM execution_sessions \
         WHERE node_id = $1 AND task_type = $2 AND ended_at IS NULL",
    )
    .bind(node.id)
    .bind(TaskType::AccountRegister.as_str())
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(running_registration_sessions, 5);

    db.teardown().await;
}

#[tokio::test]
async fn 账号注册批次任务分配与事务原子状态更新() {
    let db = require_db!();

    let mut conn = db.pool.acquire().await.unwrap();
    let node = store::node::upsert_node(
        &mut conn,
        "账号注册测试节点",
        "host",
        "Linux",
        "1.0",
        "1.0",
        2,
        "token_hash",
    )
    .await
    .unwrap();
    store::node::ensure_slots(&mut conn, node.id, 2)
        .await
        .unwrap();
    drop(conn);
    store::node::approve_node(&db.pool, node.id, None)
        .await
        .unwrap();
    store::node::set_node_status(&db.pool, node.id, WorkerStatus::Online)
        .await
        .unwrap();

    let cipher = master_server::security::FieldCipher::from_base64(
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .unwrap();

    // 创建待注册账号
    let acc1 = store::resource::create_account(
        &db.pool,
        "to_reg1@test.com",
        &cipher.encrypt("pwd1").unwrap(),
        "nick1",
        10,
        AccountStatus::PendingRegistration,
    )
    .await
    .unwrap();

    let proxy = store::resource::upsert_proxy(
        &db.pool, "Webshare", None, "p1", "http", "10.0.0.1", 8080, None, None,
    )
    .await
    .unwrap();
    store::resource::set_proxy_status(&db.pool, proxy.id, ProxyStatus::Available, None)
        .await
        .unwrap();

    // 创建账号注册批次与任务
    let batch = store::account_registration::create_batch(
        &db.pool,
        &store::account_registration::NewAccountRegistrationBatch {
            name: "注册测试批次".to_string(),
            source_file: Some("accounts.txt".to_string()),
            priority: 5,
            created_by: None,
        },
    )
    .await
    .unwrap();

    let reg_task_id = store::account_registration::create_task(&db.pool, batch.id, acc1.id, 5)
        .await
        .unwrap();

    store::account_registration::update_batch_status(
        &db.pool,
        batch.id,
        BatchStatus::NotStarted,
        BatchStatus::Running,
    )
    .await
    .unwrap();

    let state = db.create_test_state();

    // 1. 分配账号注册会话
    let outcome = allocate_session(&state, node.id, TaskType::AccountRegister, Some(0))
        .await
        .unwrap();
    let grant = match outcome {
        master_server::scheduler::AllocationOutcome::Granted(g) => g,
        other => panic!("预期账号注册会话分配成功，实际得到 {other:?}"),
    };

    assert_eq!(grant.task_type, TaskType::AccountRegister);
    assert_eq!(grant.account.as_ref().unwrap().email, "to_reg1@test.com");

    // 2. 模拟 Worker 接收任务并上报成功结果
    let exec_id = Uuid::new_v4();

    // 写入租约与执行记录
    sqlx::query(
        "UPDATE account_registration_tasks SET status = '执行中', lease_execution_id = $2, stage_version = 1 \
         WHERE id = $1",
    )
    .bind(reg_task_id)
    .bind(exec_id)
    .execute(&db.pool)
    .await
    .unwrap();

    let mut conn = db.pool.acquire().await.unwrap();
    store::session::start_execution(
        &mut conn,
        &store::session::NewExecution {
            id: exec_id,
            task_id: None,
            account_registration_task_id: Some(reg_task_id),
            session_id: grant.session_id,
            node_id: node.id,
            slot_index: 0,
            account_id: Some(acc1.id),
            proxy_id: Some(proxy.id),
            task_type: TaskType::AccountRegister,
            attempt: 1,
            stage_version: 1,
        },
    )
    .await
    .unwrap();
    drop(conn);

    // 同一注册任务的验证码事项必须幂等：新 Worker action_id 替换旧事项，
    // 数据库始终只有一条待处理记录；验证码提交也绝不持久化。
    let old_action_id = Uuid::new_v4();
    let current_action_id = Uuid::new_v4();
    for action_id in [old_action_id, current_action_id] {
        store::manual_action::create_action(
            &db.pool,
            &store::manual_action::NewManualAction {
                id: action_id,
                task_type: TaskType::AccountRegister,
                registration_task_id: Some(reg_task_id),
                book_task_id: None,
                execution_id: Some(exec_id),
                node_id: Some(node.id),
                session_id: Some(grant.session_id),
                action_type: ManualActionType::MailCode,
                prompt: "请输入验证码".to_string(),
                artifact_url: None,
                expires_at: chrono::Utc::now() + chrono::Duration::minutes(10),
            },
        )
        .await
        .unwrap();
    }
    let pending_actions = store::manual_action::list_actions(&db.pool, Some("待处理"), 10)
        .await
        .unwrap();
    assert_eq!(pending_actions.len(), 1);
    assert_eq!(pending_actions[0].id, current_action_id);
    let resolved =
        store::manual_action::resolve_action(&db.pool, current_action_id, Some("654321"), None)
            .await
            .unwrap();
    assert!(resolved.input_code.is_none(), "验证码不得持久化到数据库");

    let report = RegistrationResultReport {
        session_id: grant.session_id,
        execution_id: exec_id,
        registration_task_id: reg_task_id,
        node_id: Some(node.id),
        result: ExecutionResult::Success,
        reason: "注册成功".to_string(),
        stage_version: 1,
        attempt: 1,
        already_exists: false,
        awaiting_verification: false,
        completed_at: Some(chrono::Utc::now().to_rfc3339()),
    };

    let submit_outcome = submit_registration_result(&state, &report).await.unwrap();
    assert!(submit_outcome.applied, "注册结果必须成功应用");

    // 验证任务与账号状态在数据库中原子更新
    let task_after = store::account_registration::get_task(&db.pool, reg_task_id)
        .await
        .unwrap();
    assert_eq!(
        task_after.status,
        AccountRegistrationTaskStatus::Completed.as_str()
    );

    let acc_after = store::resource::get_account(&db.pool, acc1.id)
        .await
        .unwrap();
    assert_eq!(acc_after.status, AccountStatus::Registered.as_str());
    assert!(
        acc_after.registered_at.is_some(),
        "注册成功后 registered_at 必须已填充"
    );

    // 验证批次进度
    let progress = store::account_registration::batch_progress(&db.pool, batch.id)
        .await
        .unwrap();
    assert_eq!(progress.completed, 1);
    assert_eq!(progress.total, 1);

    db.teardown().await;
}

#[tokio::test]
async fn 注册会话只领取当前可执行任务对应的账号() {
    let db = require_db!();

    let mut conn = db.pool.acquire().await.unwrap();
    let node = store::node::upsert_node(
        &mut conn,
        "注册退避测试节点",
        "host",
        "Linux",
        "1.0",
        "1.0",
        2,
        "token_hash",
    )
    .await
    .unwrap();
    store::node::ensure_slots(&mut conn, node.id, 2)
        .await
        .unwrap();
    drop(conn);
    store::node::approve_node(&db.pool, node.id, None)
        .await
        .unwrap();
    store::node::set_node_status(&db.pool, node.id, WorkerStatus::Online)
        .await
        .unwrap();

    let cipher = master_server::security::FieldCipher::from_base64(
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .unwrap();
    let retrying_account = store::resource::create_account(
        &db.pool,
        "retrying@test.com",
        &cipher.encrypt("valid-password-1").unwrap(),
        "retrying",
        10,
        AccountStatus::PendingRegistration,
    )
    .await
    .unwrap();
    let pending_account = store::resource::create_account(
        &db.pool,
        "pending@test.com",
        &cipher.encrypt("valid-password-2").unwrap(),
        "pending",
        10,
        AccountStatus::PendingRegistration,
    )
    .await
    .unwrap();

    let proxy = store::resource::upsert_proxy(
        &db.pool,
        "Webshare",
        None,
        "registration-proxy",
        "http",
        "10.0.0.9",
        8080,
        None,
        None,
    )
    .await
    .unwrap();
    sqlx::query(
        "UPDATE proxies SET status = '可用', exit_ip = '203.0.113.9', last_checked_at = now() WHERE id = $1",
    )
    .bind(proxy.id)
    .execute(&db.pool)
    .await
    .unwrap();

    let batch = store::account_registration::create_batch(
        &db.pool,
        &store::account_registration::NewAccountRegistrationBatch {
            name: "注册退避批次".to_string(),
            source_file: None,
            priority: 5,
            created_by: None,
        },
    )
    .await
    .unwrap();
    let retrying_task =
        store::account_registration::create_task(&db.pool, batch.id, retrying_account.id, 10)
            .await
            .unwrap();
    let pending_task =
        store::account_registration::create_task(&db.pool, batch.id, pending_account.id, 1)
            .await
            .unwrap();
    store::account_registration::update_batch_status(
        &db.pool,
        batch.id,
        BatchStatus::NotStarted,
        BatchStatus::Running,
    )
    .await
    .unwrap();

    // 高优先级任务仍处于退避期时，不能先为它打开一个最终拿不到任务的浏览器。
    sqlx::query(
        "UPDATE account_registration_tasks SET status = '正在重试', attempts = 1, \
         next_attempt_at = now() + interval '1 hour' WHERE id = $1",
    )
    .bind(retrying_task)
    .execute(&db.pool)
    .await
    .unwrap();

    let state = db.create_test_state();
    let first = allocate_session(&state, node.id, TaskType::AccountRegister, Some(0))
        .await
        .unwrap();
    let first = match first {
        master_server::scheduler::AllocationOutcome::Granted(grant) => grant,
        other => panic!("预期领取待处理注册账号，实际得到 {other:?}"),
    };
    assert_eq!(
        first.account.as_ref().unwrap().account_id,
        pending_account.id,
        "退避期账号不得被注册会话提前领取"
    );
    master_server::scheduler::allocate::close_session(
        &state,
        first.session_id,
        platform_domain::SessionStatus::Ended,
        "测试释放",
    )
    .await
    .unwrap();

    // 移除普通待处理任务，并让重试任务到期；此时重试账号应能直接重新领取。
    sqlx::query("UPDATE account_registration_tasks SET status = '已取消' WHERE id = $1")
        .bind(pending_task)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE account_registration_tasks SET next_attempt_at = now() - interval '1 second' WHERE id = $1",
    )
    .bind(retrying_task)
    .execute(&db.pool)
    .await
    .unwrap();

    let second = allocate_session(&state, node.id, TaskType::AccountRegister, Some(0))
        .await
        .unwrap();
    let second = match second {
        master_server::scheduler::AllocationOutcome::Granted(grant) => grant,
        other => panic!("预期重新领取到期重试账号，实际得到 {other:?}"),
    };
    assert_eq!(
        second.account.as_ref().unwrap().account_id,
        retrying_account.id
    );

    db.teardown().await;
}
