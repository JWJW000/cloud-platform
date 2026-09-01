//! 统一调度工作选择器（V6 方案第 6 节）。
//!
//! 核心原则：
//! - Worker 只报告空闲槽位与支持的能力（WorkRequest），Master 主导选择工作类型；
//! - Master 跨业务队列（图书下载、账号注册、NAS 核验、代理检测）按优先级与等待时间加成进行统一评分；
//! - 启动批次或节点上线后自动触发调度唤醒。

use platform_domain::{SlotStatus, TaskType, WorkerStatus};
use platform_proto::v1 as pb;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::error::AppResult;
use crate::grpc::convert;
use crate::scheduler::allocate::{self, AllocationOutcome};
use crate::state::AppState;
use crate::store;

/// 任务队列候选评分。
#[derive(Debug, Clone)]
struct QueueCandidate {
    task_type: TaskType,
    effective_priority: i64,
}

/// 收到 Worker 的 WorkRequest 后的处理。
pub async fn handle_work_request(
    state: &AppState,
    node_id: Uuid,
    slot_index: i32,
    supported_task_types: &[String],
    outbound: &mpsc::Sender<pb::MasterMessage>,
) -> AppResult<()> {
    let node = store::node::get_node(&state.pool, node_id).await?;
    let worker_status = node
        .status
        .parse::<WorkerStatus>()
        .unwrap_or(WorkerStatus::Offline);
    if !worker_status.can_accept_work() {
        tracing::debug!(node_id = %node_id, status = %node.status, "节点不可接收工作");
        return Ok(());
    }

    // 解析 Worker 支持的能力
    let supported: Vec<TaskType> = supported_task_types
        .iter()
        .filter_map(|s| s.parse::<TaskType>().ok())
        .collect();

    let chosen_type = select_best_task_type(state, node_id, &node, &supported).await?;
    let Some(task_type) = chosen_type else {
        tracing::debug!(node_id = %node_id, slot = slot_index, "当前无待执行任务或能力不匹配");
        return Ok(());
    };

    let outcome = allocate::allocate_session(state, node_id, task_type, Some(slot_index)).await?;
    match outcome {
        AllocationOutcome::Granted(grant) => {
            let msg = convert::create_session_message(&grant);
            if outbound.send(msg).await.is_err() {
                tracing::warn!(node_id = %node_id, "下发 CreateSession 失败：通道已关闭");
            } else {
                tracing::info!(
                    node_id = %node_id,
                    slot = slot_index,
                    task_type = %task_type,
                    session_id = %grant.session_id,
                    "统一调度成功分配会话"
                );
            }
        }
        AllocationOutcome::Unavailable(unavail) => {
            tracing::debug!(
                node_id = %node_id,
                slot = slot_index,
                task_type = %task_type,
                reason = %unavail.reason,
                "统一调度资源暂时不足"
            );
        }
    }

    Ok(())
}

/// 扫描所有在线节点的空闲槽位并触发工作分配（批次启动/恢复/节点上线时调用）。
pub async fn trigger_scheduler_sweep(state: &AppState) -> AppResult<()> {
    let online_nodes = state.links.online_nodes();
    for node_id in online_nodes {
        let Some(sender) = state.links.sender(node_id) else {
            continue;
        };
        // 查找该节点真正空闲（无未结束活跃会话）的槽位
        let idle_slots: Vec<i32> = sqlx::query_scalar(
            "SELECT ws.slot_index FROM worker_slots ws \
             WHERE ws.node_id = $1 AND ws.status = $2 AND ws.session_id IS NULL \
               AND NOT EXISTS ( \
                   SELECT 1 FROM execution_sessions es \
                   WHERE es.node_id = ws.node_id AND es.slot_index = ws.slot_index \
                     AND es.ended_at IS NULL AND es.status IN ('创建中', '运行中') \
               ) \
             ORDER BY ws.slot_index",
        )
        .bind(node_id)
        .bind(SlotStatus::Idle.as_str())
        .fetch_all(&state.pool)
        .await?;

        if idle_slots.is_empty() {
            continue;
        }

        // 与 Worker 申报能力一致（已完整支持「图书下载 / 账号注册 / 代理检测」）。
        let supported = vec![
            "图书下载".to_string(),
            "账号注册".to_string(),
            "代理检测".to_string(),
        ];

        for slot_index in idle_slots {
            let _ = handle_work_request(state, node_id, slot_index, &supported, &sender).await;
        }
    }
    Ok(())
}

/// Master 评估各队列并选择最优先的工作类型。
async fn select_best_task_type(
    state: &AppState,
    node_id: Uuid,
    node: &crate::models::WorkerNode,
    supported: &[TaskType],
) -> AppResult<Option<TaskType>> {
    let mut candidates: Vec<QueueCandidate> = Vec::new();

    // 全局暂停只关闭图书下载队列；账号注册、NAS 核验和代理检测仍可正常运行。
    let download_paused = super::control::get_global_download_control(&state.pool)
        .await?
        .paused;

    // 1. 图书下载队列评估
    if !download_paused && supported.contains(&TaskType::BookDownload) && node.nas_healthy {
        let book_stat: Option<(i64, i32)> = sqlx::query_as(
            "SELECT count(*), COALESCE(max(b.priority), 0) \
             FROM book_tasks t \
             JOIN batch_books bb ON bb.book_id = t.book_id \
             JOIN download_batches b ON b.id = bb.batch_id AND b.download_format = t.format \
             WHERE t.status = '待处理' AND t.next_attempt_at <= now() \
               AND t.cancel_requested = FALSE AND t.attempts < t.max_attempts \
               AND b.status = '执行中'",
        )
        .fetch_optional(&state.pool)
        .await?;

        let (count, existing_priority) = book_stat.unwrap_or((0, 0));
        let catalog_priority = if count == 0 {
            super::catalog_bridge::next_target_priority(&state.pool).await?
        } else {
            None
        };
        if count > 0 || catalog_priority.is_some() {
            // 尚未物化的总库目标也必须让统一调度选择图书下载；真正领取时再原子物化。
            let max_priority = existing_priority.max(catalog_priority.unwrap_or(0));
            candidates.push(QueueCandidate {
                task_type: TaskType::BookDownload,
                effective_priority: (max_priority as i64) * 10 + 5,
            });
        }
    }

    // 2. 账号注册队列评估
    if supported.contains(&TaskType::AccountRegister) {
        // 检查该节点当前账号注册会话数是否超标
        let running_reg_slots: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM execution_sessions WHERE node_id = $1 AND task_type = $2 AND ended_at IS NULL",
        )
        .bind(node_id)
        .bind(TaskType::AccountRegister.as_str())
        .fetch_one(&state.pool)
        .await?;
        let max_reg_slots = (node.max_slots / 2).max(1) as i64;

        if running_reg_slots < max_reg_slots {
            let reg_stat: Option<(i64, i32)> = sqlx::query_as(
                "SELECT count(*), COALESCE(max(b.priority), 0) \
                 FROM account_registration_tasks t \
                 JOIN account_registration_batches b ON b.id = t.batch_id \
                 WHERE t.status IN ('待处理', '正在重试') AND t.next_attempt_at <= now() \
                   AND t.cancel_requested = FALSE AND t.attempts < t.max_attempts \
                   AND b.status = '执行中'",
            )
            .fetch_optional(&state.pool)
            .await?;

            if let Some((count, max_priority)) = reg_stat {
                if count > 0 {
                    candidates.push(QueueCandidate {
                        task_type: TaskType::AccountRegister,
                        effective_priority: (max_priority as i64) * 10 + 5,
                    });
                }
            }
        }
    }

    // 3. 代理检测队列评估
    if supported.contains(&TaskType::ProxyCheck) {
        let proxy_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxies \
             WHERE status IN ('可用', '异常') AND lease_session_id IS NULL \
               AND (last_checked_at IS NULL OR last_checked_at < now() - interval '10 minutes')",
        )
        .fetch_one(&state.pool)
        .await?;

        if proxy_count > 0 {
            // 当可用且新鲜（10分钟内）的已验证代理不足时，提升代理检测优先级，确保下载任务随时有代理可用
            let fresh_available_count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM proxies \
                 WHERE status = '可用' AND lease_session_id IS NULL \
                   AND exit_ip IS NOT NULL \
                   AND last_checked_at >= now() - interval '10 minutes' \
                   AND (cooldown_until IS NULL OR cooldown_until <= now())",
            )
            .fetch_one(&state.pool)
            .await?;

            // 没有形成最小健康代理池时必须先检测代理。这里不能只给一个普通数字
            // 优先级：高优先级注册批次可能超过它，随后又因没有健康代理而分配失败，
            // 形成“注册抢占代理检测、注册又拿不到代理”的活锁。
            if fresh_available_count < 5 {
                return Ok(Some(TaskType::ProxyCheck));
            }

            candidates.push(QueueCandidate {
                task_type: TaskType::ProxyCheck,
                effective_priority: 1,
            });
        }
    }

    if candidates.is_empty() {
        return Ok(None);
    }

    // 按 effective_priority 降序排序
    candidates.sort_by_key(|a| std::cmp::Reverse(a.effective_priority));
    Ok(Some(candidates[0].task_type))
}
