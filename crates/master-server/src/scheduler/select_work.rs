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
        // 查找该节点的空闲槽位
        let idle_slots: Vec<i32> = sqlx::query_scalar(
            "SELECT slot_index FROM worker_slots \
             WHERE node_id = $1 AND status = $2 AND session_id IS NULL \
             ORDER BY slot_index",
        )
        .bind(node_id)
        .bind(SlotStatus::Idle.as_str())
        .fetch_all(&state.pool)
        .await?;

        if idle_slots.is_empty() {
            continue;
        }

        // 默认该节点支持所有任务类型
        let default_supported = vec![
            "图书下载".to_string(),
            "账号注册".to_string(),
            "NAS核验".to_string(),
            "代理检测".to_string(),
        ];

        for slot_index in idle_slots {
            let _ =
                handle_work_request(state, node_id, slot_index, &default_supported, &sender).await;
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

        if let Some((count, max_priority)) = book_stat {
            if count > 0 {
                // 图书下载基础权重 + 批次优先级
                candidates.push(QueueCandidate {
                    task_type: TaskType::BookDownload,
                    effective_priority: (max_priority as i64) * 10 + 5,
                });
            }
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
                 WHERE t.status = '待处理' AND t.next_attempt_at <= now() \
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
               AND (last_checked_at IS NULL OR last_checked_at < now() - interval '1 hour')",
        )
        .fetch_one(&state.pool)
        .await?;

        if proxy_count > 0 {
            candidates.push(QueueCandidate {
                task_type: TaskType::ProxyCheck,
                effective_priority: 1, // 较低优先级背景检测
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
