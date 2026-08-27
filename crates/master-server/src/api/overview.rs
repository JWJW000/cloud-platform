//! 管理后台总览数据接口（第 16.1 节）。

use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::auth::AuthenticatedUser;
use crate::error::AppResult;
use crate::models::{DailyStat, TaskExecution};
use crate::state::AppState;
use crate::store;

/// 总览数据汇总。
#[derive(Debug, Serialize)]
pub struct OverviewSummary {
    /// Worker 节点统计。
    pub workers: WorkerStats,
    /// 槽位统计。
    pub slots: SlotStats,
    /// 今日执行统计。
    pub today: TodayStats,
    /// 账号池统计。
    pub accounts: AccountStats,
    /// 代理池统计。
    pub proxies: ProxyStats,
    /// 任务与批次统计。
    pub tasks: TaskStats,
    /// 未解决告警数。
    pub open_alerts: i64,
}

#[derive(Debug, Serialize)]
pub struct WorkerStats {
    pub total: usize,
    pub online: usize,
    pub storage_error: usize,
}

#[derive(Debug, Serialize)]
pub struct SlotStats {
    pub total: usize,
    pub idle: usize,
    pub running: usize,
    pub error: usize,
}

#[derive(Debug, Serialize)]
pub struct TodayStats {
    pub completed: i64,
    pub failed: i64,
    pub skipped: i64,
    pub bytes_total: i64,
    pub account_used: i64,
}

#[derive(Debug, Serialize)]
pub struct AccountStats {
    pub total: usize,
    pub available: i64,
    pub pending_reg: i64,
}

#[derive(Debug, Serialize)]
pub struct ProxyStats {
    pub total: usize,
    pub available: i64,
    pub occupied: usize,
    pub cooling: usize,
    pub error: usize,
}

#[derive(Debug, Serialize)]
pub struct TaskStats {
    pub pending: i64,
    pub running: i64,
    pub completed: i64,
    pub failed: i64,
    pub needs_confirm: i64,
    pub running_batches: usize,
}

#[derive(Debug, Deserialize)]
pub struct StatsQuery {
    #[serde(default = "default_stats_days")]
    pub days: i64,
}

fn default_stats_days() -> i64 {
    7
}

#[derive(Debug, Deserialize)]
pub struct LimitQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    20
}

/// GET /api/overview
pub async fn get_overview(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
) -> AppResult<Json<OverviewSummary>> {
    let nodes = store::node::list_nodes(&state.pool).await?;
    let total_workers = nodes.len();
    let online_workers = nodes.iter().filter(|n| n.connected).count();
    let storage_error_workers = nodes.iter().filter(|n| !n.nas_healthy).count();

    let all_slots = store::node::list_all_slots(&state.pool).await?;
    let total_slots = all_slots.len();
    let idle_slots = all_slots.iter().filter(|s| s.status == "空闲").count();
    let running_slots = all_slots
        .iter()
        .filter(|s| s.status == "执行中" || s.status == "启动中" || s.status == "预留")
        .count();
    let error_slots = all_slots.iter().filter(|s| s.status == "异常").count();

    let (total_acc, available_accounts, pending_reg): (i64, i64, i64) = sqlx::query_as(
        "SELECT \
         count(*)::bigint, \
         count(*) FILTER (WHERE status = '可用' AND lease_session_id IS NULL)::bigint, \
         count(*) FILTER (WHERE status = '待注册')::bigint \
         FROM accounts",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap_or((0, 0, 0));
    let total_accounts = total_acc as usize;

    let (total_p, available_proxies, occupied_p, cooling_p, error_p): (i64, i64, i64, i64, i64) =
        sqlx::query_as(
            "SELECT \
         count(*)::bigint, \
         count(*) FILTER (WHERE status = '可用' AND (cooldown_until IS NULL OR cooldown_until < now()) AND lease_session_id IS NULL)::bigint, \
         count(*) FILTER (WHERE status = '已占用')::bigint, \
         count(*) FILTER (WHERE status = '冷却中' OR (status = '可用' AND cooldown_until >= now()))::bigint, \
         count(*) FILTER (WHERE status = '异常')::bigint \
         FROM proxies",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap_or((0, 0, 0, 0, 0));
    let total_proxies = total_p as usize;
    let occupied_proxies = occupied_p as usize;
    let cooling_proxies = cooling_p as usize;
    let error_proxies = error_p as usize;

    let task_counts = store::task::count_by_status(&state.pool).await?;
    let mut pending_tasks = 0;
    let mut running_tasks = 0;
    let mut completed_tasks = 0;
    let mut failed_tasks = 0;
    let mut needs_confirm_tasks = 0;

    for (status, count) in task_counts {
        match status.as_str() {
            "待处理" => pending_tasks += count,
            "已分配" | "执行中" | "等待入库" => running_tasks += count,
            "已完成" => completed_tasks += count,
            "失败" => failed_tasks += count,
            "待确认" => needs_confirm_tasks += count,
            _ => {}
        }
    }

    let batches = store::catalog::list_batches(&state.pool).await?;
    let running_batches = batches.iter().filter(|b| b.status == "执行中").count();

    let open_alerts = store::admin::open_alert_count(&state.pool).await?;

    let today_stats_list = store::admin::recent_daily_stats(&state.pool, 1).await?;
    let today = if let Some(last) = today_stats_list.last() {
        TodayStats {
            completed: last.completed,
            failed: last.failed,
            skipped: last.skipped,
            bytes_total: last.bytes_total,
            account_used: last.account_used,
        }
    } else {
        TodayStats {
            completed: 0,
            failed: 0,
            skipped: 0,
            bytes_total: 0,
            account_used: 0,
        }
    };

    Ok(Json(OverviewSummary {
        workers: WorkerStats {
            total: total_workers,
            online: online_workers,
            storage_error: storage_error_workers,
        },
        slots: SlotStats {
            total: total_slots,
            idle: idle_slots,
            running: running_slots,
            error: error_slots,
        },
        today,
        accounts: AccountStats {
            total: total_accounts,
            available: available_accounts,
            pending_reg,
        },
        proxies: ProxyStats {
            total: total_proxies,
            available: available_proxies,
            occupied: occupied_proxies,
            cooling: cooling_proxies,
            error: error_proxies,
        },
        tasks: TaskStats {
            pending: pending_tasks,
            running: running_tasks,
            completed: completed_tasks,
            failed: failed_tasks,
            needs_confirm: needs_confirm_tasks,
            running_batches,
        },
        open_alerts,
    }))
}

/// GET /api/overview/stats
pub async fn get_stats(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Query(query): Query<StatsQuery>,
) -> AppResult<Json<Vec<DailyStat>>> {
    let stats = store::admin::recent_daily_stats(&state.pool, query.days).await?;
    Ok(Json(stats))
}

/// GET /api/overview/recent-executions
pub async fn get_recent_executions(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Query(query): Query<LimitQuery>,
) -> AppResult<Json<Vec<TaskExecution>>> {
    let executions = store::session::recent_executions(&state.pool, query.limit).await?;
    Ok(Json(executions))
}
