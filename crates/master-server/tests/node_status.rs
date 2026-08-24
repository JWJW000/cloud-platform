//! 节点状态迁移的真实数据库测试（第 14.2 节「审核在线节点的状态迁移」）。
//!
//! 这一组用例存在的唯一理由是：第 3.7 节的修复几乎全部落在 SQL 里
//! （`apply_heartbeat` 的 `CASE`、`record_node_online` 的 `COALESCE(NULLIF(...))`、
//! `refresh_available_slots` 的 `UPDATE ... FROM`），而本项目用的是 sqlx 运行时查询，
//! 这些语句在 `cargo build` 时一行都没被检查过。单元测试能证明
//! `adopt_reported_worker_status` 的判定正确，但证明不了「这条 UPDATE 真的会那样改库」。
//!
//! 覆盖的四个缺陷（对应第 3.7 节）：
//! 1. 审核通过后节点停在「离线」，链路已建立时没有任何路径把它改成在线；
//! 2. 心跳丢弃 `node_status`，被判离线的节点无法靠心跳自愈；
//! 3. `available_slots` 被离线巡检清零后不再恢复；
//! 4. `applied_config_version` 从未落库，配置是否生效无从核对。

mod support;

use master_server::store;
use master_server::store::node::HeartbeatMetrics;
use platform_domain::WorkerStatus;
use sqlx::PgPool;
use uuid::Uuid;

/// 造一个刚注册完、处于「待审核」的节点。
async fn 新注册节点(pool: &PgPool, name: &str) -> Uuid {
    let mut conn = pool.acquire().await.expect("取连接失败");
    let node = store::node::upsert_node(
        &mut conn,
        name,
        "测试主机",
        "Linux",
        "6.1.0",
        "0.1.0-test",
        2,
        "散列占位",
    )
    .await
    .expect("注册节点失败");
    assert_eq!(node.status, WorkerStatus::PendingApproval.as_str());
    node.id
}

/// 一条只带自评状态的心跳指标；其余字段取默认值即可，本组用例不看它们。
fn 心跳(reported: Option<WorkerStatus>) -> HeartbeatMetrics {
    HeartbeatMetrics {
        nas_healthy: true,
        nas_free_gb: 100,
        staging_free_gb: 50,
        cpu_percent: 12.5,
        memory_used_mb: 2048,
        memory_total_mb: 16384,
        agent_version: String::new(),
        applied_config_version: String::new(),
        reported_status: reported,
    }
}

/// 把最近心跳时间推到过去，用来触发离线巡检而不必真的等待。
async fn 让心跳过期(pool: &PgPool, node_id: Uuid) {
    sqlx::query(
        "UPDATE worker_nodes SET last_heartbeat_at = now() - interval '1 hour' WHERE id = $1",
    )
    .bind(node_id)
    .execute(pool)
    .await
    .expect("回拨心跳时间失败");
}

/// 审核前的心跳不能把节点从「待审核」推进到「在线」。
///
/// 批准是管理员动作。Worker 在审核前就允许连接（只是不派活），它的心跳会诚实地
/// 报「我在线」——如果这条心跳能改状态，等于节点自己批准了自己。
#[tokio::test]
async fn 待审核节点的心跳不会自我批准() {
    let db = require_db!();
    let node_id = 新注册节点(&db.pool, "待审核节点").await;

    let status = store::node::apply_heartbeat(&db.pool, node_id, &心跳(Some(WorkerStatus::Online)))
        .await
        .expect("写心跳失败");

    assert_eq!(
        status,
        WorkerStatus::PendingApproval.as_str(),
        "待审核属于管理员治理范围，心跳不得解除"
    );
    let node = store::node::get_node(&db.pool, node_id)
        .await
        .expect("读节点失败");
    assert_eq!(node.status, WorkerStatus::PendingApproval.as_str());
    // 状态没变，但指标和连接标记要照常写入——否则这台机器看起来像失联。
    assert!(node.connected, "心跳必须把节点标记为已连接");
    assert_eq!(node.nas_free_gb, 100);
    assert!(node.last_heartbeat_at.is_some());

    db.teardown().await;
}

/// 审核通过后，链路已建立的节点会被主动置为在线并按槽位表重算可用槽位。
///
/// 这是第 3.7 节现象的正面用例：`approve_node` 只把状态改成「离线」，
/// `api::workers::approve_worker` 在发现链路仍在时补做这两步。这里验证
/// 那两步组合起来的库内结果，而不是各自单独的返回值。
#[tokio::test]
async fn 审核已连接节点后状态与可用槽位一起就绪() {
    let db = require_db!();
    let node_id = 新注册节点(&db.pool, "审核后上线节点").await;

    // 审核前节点已经把链路挂上来了：发过心跳，槽位表也已按 max_slots 建好。
    store::node::apply_heartbeat(&db.pool, node_id, &心跳(Some(WorkerStatus::Online)))
        .await
        .expect("写心跳失败");
    let mut conn = db.pool.acquire().await.expect("取连接失败");
    store::node::ensure_slots(&mut conn, node_id, 2)
        .await
        .expect("对齐槽位失败");
    drop(conn);

    let approved = store::node::approve_node(&db.pool, node_id, None)
        .await
        .expect("审核失败");
    assert_eq!(
        approved.status,
        WorkerStatus::Offline.as_str(),
        "审核本身只把节点放到离线——这正是需要补一步的原因"
    );
    assert_eq!(approved.available_slots, 0, "审核不会顺手算可用槽位");

    // `approve_worker` 在链路仍在时补做的两步。
    store::node::set_node_status(&db.pool, node_id, WorkerStatus::Online)
        .await
        .expect("置为在线失败");
    let available = store::node::refresh_available_slots(&db.pool, node_id)
        .await
        .expect("刷新可用槽位失败");

    assert_eq!(available, 2, "两个空闲槽位应当立刻可派活");
    let node = store::node::get_node(&db.pool, node_id)
        .await
        .expect("读节点失败");
    assert_eq!(node.status, WorkerStatus::Online.as_str());
    assert_eq!(node.available_slots, 2);
    assert!(node.approved_at.is_some(), "审核时间必须落库");

    db.teardown().await;
}

/// 重复审核不会得到第二次成功：`approve_node` 带 `status = 待审核` 条件。
#[tokio::test]
async fn 重复审核只成功一次() {
    let db = require_db!();
    let node_id = 新注册节点(&db.pool, "重复审核节点").await;

    store::node::approve_node(&db.pool, node_id, None)
        .await
        .expect("首次审核失败");
    let second = store::node::approve_node(&db.pool, node_id, None).await;

    assert!(
        second.is_err(),
        "已审核的节点再审核必须失败，否则会重置审核记录"
    );

    db.teardown().await;
}

/// 被离线巡检判死的节点，靠一条自评心跳就能回到在线，可用槽位同时恢复。
///
/// 缺这条路径时的现象是：节点其实一直在发心跳，后台却永远显示离线 + 0 可用槽位，
/// 只能靠重启 Master 恢复。
#[tokio::test]
async fn 被判离线的节点靠心跳自愈并恢复可用槽位() {
    let db = require_db!();
    let node_id = 新注册节点(&db.pool, "自愈节点").await;
    store::node::approve_node(&db.pool, node_id, None)
        .await
        .expect("审核失败");
    let mut conn = db.pool.acquire().await.expect("取连接失败");
    store::node::ensure_slots(&mut conn, node_id, 3)
        .await
        .expect("对齐槽位失败");
    drop(conn);
    store::node::set_node_status(&db.pool, node_id, WorkerStatus::Online)
        .await
        .expect("置为在线失败");
    store::node::refresh_available_slots(&db.pool, node_id)
        .await
        .expect("刷新可用槽位失败");

    让心跳过期(&db.pool, node_id).await;
    let offline = store::node::mark_stale_nodes_offline(&db.pool, 60)
        .await
        .expect("离线巡检失败");
    assert!(offline.contains(&node_id), "心跳超时的在线节点必须被判离线");
    let node = store::node::get_node(&db.pool, node_id)
        .await
        .expect("读节点失败");
    assert_eq!(node.status, WorkerStatus::Offline.as_str());
    assert_eq!(node.available_slots, 0, "离线节点不应显示可用槽位");
    assert!(!node.connected);

    // 网络恢复，Agent 的下一跳自评「在线」。
    let status = store::node::apply_heartbeat(&db.pool, node_id, &心跳(Some(WorkerStatus::Online)))
        .await
        .expect("写心跳失败");
    let available = store::node::refresh_available_slots(&db.pool, node_id)
        .await
        .expect("刷新可用槽位失败");

    assert_eq!(
        status,
        WorkerStatus::Online.as_str(),
        "离线不属于管理员治理范围，心跳可以拉回"
    );
    assert_eq!(
        available, 3,
        "可用槽位由槽位表算出，不依赖 Worker 自报的数字"
    );

    db.teardown().await;
}

/// 槽位被会话占用时，恢复后的可用槽位只数空闲的那些。
#[tokio::test]
async fn 可用槽位只统计空闲槽位() {
    let db = require_db!();
    let node_id = 新注册节点(&db.pool, "占用槽位节点").await;
    let mut conn = db.pool.acquire().await.expect("取连接失败");
    store::node::ensure_slots(&mut conn, node_id, 3)
        .await
        .expect("对齐槽位失败");
    drop(conn);

    store::node::set_slot(
        &db.pool,
        node_id,
        0,
        platform_domain::SlotStatus::Running,
        None,
        "占用中",
    )
    .await
    .expect("改槽位失败");

    let available = store::node::refresh_available_slots(&db.pool, node_id)
        .await
        .expect("刷新可用槽位失败");
    assert_eq!(available, 2);

    db.teardown().await;
}

/// 管理员设置的「维护中」「已禁用」和云端下发的「已暂停」，任何心跳都不能解除。
///
/// 最危险的一种是「已暂停」：Worker 进程重启后本地暂停标记已经丢了，
/// 它会诚实地报「我在线」，而那恰恰是最需要拦住的一次上报。
#[tokio::test]
async fn 管理员与云端设定的状态挡住每一条心跳() {
    let db = require_db!();

    for governed in [
        WorkerStatus::Maintenance,
        WorkerStatus::Disabled,
        WorkerStatus::Paused,
    ] {
        let node_id = 新注册节点(&db.pool, &format!("治理态节点_{governed}")).await;
        store::node::approve_node(&db.pool, node_id, None)
            .await
            .expect("审核失败");
        store::node::set_node_status(&db.pool, node_id, governed)
            .await
            .expect("设置治理态失败");

        for reported in [
            WorkerStatus::Online,
            WorkerStatus::Busy,
            WorkerStatus::StorageError,
        ] {
            let status = store::node::apply_heartbeat(&db.pool, node_id, &心跳(Some(reported)))
                .await
                .expect("写心跳失败");
            assert_eq!(
                status,
                governed.as_str(),
                "节点自评「{reported}」不得解除管理员/云端设定的「{governed}」"
            );
        }
    }

    db.teardown().await;
}

/// 在线节点自评「忙碌」「存储异常」要被采纳——这是它有权表达的运行状况。
#[tokio::test]
async fn 运行状况类自评被采纳() {
    let db = require_db!();
    let node_id = 新注册节点(&db.pool, "自评运行状况节点").await;
    store::node::approve_node(&db.pool, node_id, None)
        .await
        .expect("审核失败");

    for reported in [
        WorkerStatus::Online,
        WorkerStatus::Busy,
        WorkerStatus::StorageError,
    ] {
        let status = store::node::apply_heartbeat(&db.pool, node_id, &心跳(Some(reported)))
            .await
            .expect("写心跳失败");
        assert_eq!(status, reported.as_str());
    }

    db.teardown().await;
}

/// 不带自评的心跳（老版本 Agent）保留原状态，只更新指标。
#[tokio::test]
async fn 无自评的心跳不改状态() {
    let db = require_db!();
    let node_id = 新注册节点(&db.pool, "老版本Agent节点").await;
    store::node::approve_node(&db.pool, node_id, None)
        .await
        .expect("审核失败");
    store::node::set_node_status(&db.pool, node_id, WorkerStatus::Busy)
        .await
        .expect("置为忙碌失败");

    let status = store::node::apply_heartbeat(&db.pool, node_id, &心跳(None))
        .await
        .expect("写心跳失败");

    assert_eq!(status, WorkerStatus::Busy.as_str(), "没有信息就不要动状态");
    let node = store::node::get_node(&db.pool, node_id)
        .await
        .expect("读节点失败");
    assert!(node.connected, "指标照常更新");

    db.teardown().await;
}

/// 心跳落库时，空的版本字符串不覆盖库里已有的值。
///
/// `COALESCE(NULLIF($n, ''), 列)` 是这里唯一的保护。写错成 `= $n` 的后果是：
/// 任何一条没带版本号的心跳都会把已知的 Agent 版本和已生效配置版本擦成空字符串，
/// 后台从此无法判断配置到底生效了没有（第 3.3 节）。
#[tokio::test]
async fn 心跳写入版本号且空值不擦除已有版本() {
    let db = require_db!();
    let node_id = 新注册节点(&db.pool, "版本号节点").await;
    store::node::approve_node(&db.pool, node_id, None)
        .await
        .expect("审核失败");

    let mut metrics = 心跳(Some(WorkerStatus::Online));
    metrics.agent_version = "1.2.3".to_string();
    metrics.applied_config_version = "配置-第7版".to_string();
    store::node::apply_heartbeat(&db.pool, node_id, &metrics)
        .await
        .expect("写心跳失败");

    let node = store::node::get_node(&db.pool, node_id)
        .await
        .expect("读节点失败");
    assert_eq!(node.agent_version, "1.2.3");
    assert_eq!(node.applied_config_version, "配置-第7版");

    // 下一跳没带版本号
    store::node::apply_heartbeat(&db.pool, node_id, &心跳(Some(WorkerStatus::Online)))
        .await
        .expect("写心跳失败");
    let node = store::node::get_node(&db.pool, node_id)
        .await
        .expect("读节点失败");
    assert_eq!(node.agent_version, "1.2.3", "空版本号不得擦除已知版本");
    assert_eq!(node.applied_config_version, "配置-第7版");

    db.teardown().await;
}

/// `NodeOnline` 的自报会落库，且同样不被空值擦除。
#[tokio::test]
async fn 上线自报记录已生效配置版本() {
    let db = require_db!();
    let node_id = 新注册节点(&db.pool, "上线自报节点").await;

    store::node::record_node_online(&db.pool, node_id, "2.0.0", "macOS", "15.1", "配置-第9版")
        .await
        .expect("记录上线失败");
    let node = store::node::get_node(&db.pool, node_id)
        .await
        .expect("读节点失败");
    assert_eq!(node.agent_version, "2.0.0");
    assert_eq!(node.os, "macOS");
    assert_eq!(node.os_version, "15.1");
    assert_eq!(node.applied_config_version, "配置-第9版");
    assert!(node.connected);
    assert_eq!(
        node.status,
        WorkerStatus::PendingApproval.as_str(),
        "上线自报只记录事实，状态归属由调用方判定"
    );

    // 空白字段（含只有空格的）一律按「没上报」处理
    store::node::record_node_online(&db.pool, node_id, "  ", "", " ", "")
        .await
        .expect("记录上线失败");
    let node = store::node::get_node(&db.pool, node_id)
        .await
        .expect("读节点失败");
    assert_eq!(node.agent_version, "2.0.0");
    assert_eq!(node.os, "macOS");
    assert_eq!(node.applied_config_version, "配置-第9版");

    db.teardown().await;
}

/// 心跳写到一个不存在的节点要报错，而不是静默成功。
///
/// 静默成功会让一台被删除的机器持续「心跳正常」，运维看不出任何异常。
#[tokio::test]
async fn 对不存在的节点写心跳会报错() {
    let db = require_db!();

    let result =
        store::node::apply_heartbeat(&db.pool, Uuid::new_v4(), &心跳(Some(WorkerStatus::Online)))
            .await;

    assert!(result.is_err(), "节点不存在时必须报错");

    db.teardown().await;
}

/// 「管理员刚点了维护中」与「节点刚发来心跳」两个并发写不会互相覆盖。
///
/// 这条用例验证的是 `apply_heartbeat` 文档里的那句断言，而它只能由真实数据库证明：
/// `CASE WHEN status IN (...)` 读的是本语句取得行锁之后的**最新**行版本。
/// 做法是让管理员的事务先拿住行锁不提交，心跳因此阻塞；提交之后心跳才继续，
/// 此时它必须看到「维护中」并保留它。
///
/// 如果这段逻辑写成「先 `get_node` 判断、再 `UPDATE`」，心跳读到的是加锁前的
/// 「在线」快照，提交后就会把管理员的决定冲掉——那正是本用例要挡住的实现。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 心跳与管理员改状态并发时管理员的决定胜出() {
    let db = require_db!();
    let node_id = 新注册节点(&db.pool, "并发写节点").await;
    store::node::approve_node(&db.pool, node_id, None)
        .await
        .expect("审核失败");
    store::node::set_node_status(&db.pool, node_id, WorkerStatus::Online)
        .await
        .expect("置为在线失败");

    let mut tx = db.pool.begin().await.expect("开事务失败");
    store::node::set_node_status(&mut *tx, node_id, WorkerStatus::Maintenance)
        .await
        .expect("设置维护中失败");

    // 心跳在另一条连接上发起，会阻塞在管理员事务持有的行锁上。
    let pool = db.pool.clone();
    let heartbeat = tokio::spawn(async move {
        store::node::apply_heartbeat(&pool, node_id, &心跳(Some(WorkerStatus::Online))).await
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!heartbeat.is_finished(), "心跳应当仍在等待行锁");

    tx.commit().await.expect("提交管理员事务失败");
    let status = heartbeat.await.expect("心跳任务panic").expect("写心跳失败");

    assert_eq!(
        status,
        WorkerStatus::Maintenance.as_str(),
        "心跳解锁后必须看到管理员写入的维护中并保留它"
    );
    let node = store::node::get_node(&db.pool, node_id)
        .await
        .expect("读节点失败");
    assert_eq!(node.status, WorkerStatus::Maintenance.as_str());
    assert!(node.connected, "状态没被改写，但心跳带来的连接与指标要生效");

    db.teardown().await;
}
