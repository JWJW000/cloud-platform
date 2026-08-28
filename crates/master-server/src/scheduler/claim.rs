//! 领取图书任务（第 7.2 节）。
//!
//! 领取要同时满足四件事，缺一件就会出现「两个 Worker 下同一本书」或「任务卡死」：
//! 1. **只从「执行中」批次取**：暂停一个批次必须立刻停止发新任务；
//! 2. **按批次优先级、批次创建时间、导入行号排序**：让同一批书的完成顺序可预期，
//!    导出 CSV 时行号与结果一一对应，管理员能直接比对；
//! 3. **`FOR UPDATE SKIP LOCKED`**：并发领取时后来者跳过被锁的行，而不是排队等锁；
//! 4. **一次分配对应一个新的执行编号与新的阶段版本**：上一轮那个 Worker 的迟到上报
//!    因此自动作废（第 14.5 节），不必依赖时间戳判新旧。
//!
//! 排序键要聚合（一本书可能同时属于多个批次，取其中优先级最高的那个），
//! 而 PostgreSQL 不允许聚合查询带 `FOR UPDATE`。因此这里分两段：
//! 先用 CTE 算出候选与排序键，再在外层对 `book_tasks` 单表加锁。

use chrono::{DateTime, Utc};
use platform_domain::{BatchStatus, NasLayout, SessionStatus, TaskStatus, TaskType};
use uuid::Uuid;

use crate::error::AppResult;
use crate::scheduler::allocate::Unavailable;
use crate::state::AppState;
use crate::store;

/// 一次分配里下发给 Worker 的图书信息。
#[derive(Debug, Clone)]
pub struct BookTarget {
    /// 图书编号。
    pub book_id: Uuid,
    /// 全局序号，NAS 文件名的 6 位前缀。
    pub book_seq: i64,
    /// 原始书名，Worker 用它作为站内搜索词。
    pub title: String,
    /// 原始作者。
    pub author: Option<String>,
    /// 原始出版社。
    pub publisher: Option<String>,
    /// 原始 ISBN，仅用于结果卡片辅助匹配。
    pub isbn: Option<String>,
    /// 目标格式（技术标识 `pdf`/`epub`）。
    pub format: String,
}

/// 分配成功后交给 gRPC 层组装 `AssignTask` 的全部要素。
#[derive(Debug, Clone)]
pub struct TaskAssignment {
    /// 会话编号。
    pub session_id: Uuid,
    /// 本次分配的执行编号，全局唯一。
    pub execution_id: Uuid,
    /// 任务编号。
    pub task_id: Uuid,
    /// 节点编号。
    pub node_id: Uuid,
    /// 任务类型。
    pub task_type: TaskType,
    /// 目标图书。
    pub book: BookTarget,
    /// 最终文件的 NAS 相对路径。
    pub nas_relative_path: String,
    /// 最终文件名。
    pub final_file_name: String,
    /// 「上传中」临时文件名。
    pub uploading_file_name: String,
    /// 这是第几次尝试。
    pub attempt: i32,
    /// 本次分配的阶段版本，Worker 上报时必须回带。
    pub stage_version: i32,
    /// 任务租约到期时间。
    pub lease_expires_at: DateTime<Utc>,
    /// 下载停滞判定秒数。
    pub stall_timeout_secs: u32,
}

/// 领取结果。
#[derive(Debug, Clone)]
pub enum ClaimOutcome {
    /// 领到一个任务。
    Assigned(Box<TaskAssignment>),
    /// 暂时没有可领的任务，附带中文原因与建议重试间隔。
    Unavailable(Unavailable),
    /// 会话应当收尾（额度用尽、达到单会话上限、会话已在结束流程中）。
    SessionShouldEnd {
        /// 中文原因，直接进 `EndSession.reason`。
        reason: String,
    },
}

/// 事务内取到的候选任务与图书信息。
///
/// 刻意不带 `attempts`/`stage_version`：这两个值必须取自随后那条 `UPDATE ... RETURNING`
/// 的结果，而不是 CTE 里读到的旧值——否则并发下发出去的尝试次数会比实际少一次。
#[derive(Debug, Clone, sqlx::FromRow)]
struct ClaimCandidate {
    id: Uuid,
    book_id: Uuid,
    format: String,
    seq: i64,
    raw_title: String,
    raw_author: Option<String>,
    raw_publisher: Option<String>,
    raw_isbn: Option<String>,
}

/// 为一个运行中的会话领取下一本书。
pub async fn claim_next_task(
    state: &AppState,
    node_id: Uuid,
    session_id: Uuid,
) -> AppResult<ClaimOutcome> {
    let scheduler = state.scheduler();
    let session = store::session::get_session(&state.pool, session_id).await?;
    let status = session.status.parse::<SessionStatus>()?;

    // 会话已经在收尾或已结束：不再发新任务，也不把它当成错误——
    // Worker 的「要下一本」和 Master 的「你该结束了」本来就可能在网上交错。
    if !matches!(status, SessionStatus::Creating | SessionStatus::Running) {
        return Ok(ClaimOutcome::SessionShouldEnd {
            reason: format!("会话状态为{status}，不再分配新任务"),
        });
    }
    if session.node_id != node_id {
        return Ok(ClaimOutcome::SessionShouldEnd {
            reason: "会话不属于该节点".to_string(),
        });
    }
    if session.completed_count >= scheduler.session_max_downloads.max(1) {
        return Ok(ClaimOutcome::SessionShouldEnd {
            reason: format!("会话已完成 {} 本，按上限收尾", session.completed_count),
        });
    }

    let task_type = session.task_type.parse::<TaskType>()?;
    if task_type != TaskType::BookDownload {
        // 账号注册、代理检测、NAS 核验的工作内容都由会话本身或专门的命令携带，
        // 它们不从 book_tasks 领书。返回原因而不是报错，Worker 收到后直接收尾。
        return Ok(ClaimOutcome::SessionShouldEnd {
            reason: format!("{task_type}会话不领取图书任务"),
        });
    }

    // 账号额度是会话能否继续的硬前提：额度耗尽还继续领书，只会白跑一次浏览器。
    if let Some(account_id) = session.account_id {
        let account = store::resource::get_account(&state.pool, account_id).await?;
        if account.daily_used >= account.daily_limit {
            return Ok(ClaimOutcome::SessionShouldEnd {
                reason: "账号今日额度已用尽".to_string(),
            });
        }
    }

    let layout = NasLayout::default();
    let lease_secs = scheduler.task_lease_secs as i64;
    let execution_id = Uuid::new_v4();

    let proxy_id = session.proxy_id;
    let proxy_info = if let Some(pid) = proxy_id {
        store::resource::get_proxy(&state.pool, pid).await.ok()
    } else {
        None
    };
    let proxy_exit_ip = proxy_info.as_ref().and_then(|p| p.exit_ip.clone());

    let mut tx = state.pool.begin().await?;
    if super::control::global_download_is_paused(&mut tx).await? {
        tx.rollback().await?;
        return Ok(ClaimOutcome::Unavailable(Unavailable {
            reason: "全局图书下载已暂停".to_string(),
            retry_after_secs: 20,
        }));
    }
    // 总库是唯一持续任务池。按需物化一个目标到现有 Worker 状态机，避免一次性
    // 为数万条目标复制任务，同时保持旧 Worker 协议与断线恢复逻辑不变。
    super::catalog_bridge::materialize_next_target(&mut tx).await?;
    let Some(candidate) = lock_candidate(&mut tx, proxy_id).await? else {
        tx.rollback().await?;
        return Ok(ClaimOutcome::Unavailable(Unavailable {
            reason: "没有待处理的图书任务".to_string(),
            retry_after_secs: 20,
        }));
    };

    let nas_relative_path =
        layout.final_relative_path(candidate.seq, &candidate.raw_title, &candidate.format);
    let final_file_name =
        layout.final_file_name(candidate.seq, &candidate.raw_title, &candidate.format);

    // 加锁之后仍要在 UPDATE 里复查状态：READ COMMITTED 下 `FOR UPDATE` 只保证
    // 拿到该行的最新版本，不保证它还满足 CTE 里的条件（可能刚被管理员取消）。
    // R6（V4 第 12.2 节）：领取时写入路径/文件名/格式期望，作为唯一证据来源。
    // V6 第 10 节：一本书固定同一代理 IP，首次领取时绑定代理。
    let leased: Option<(i32, i32)> = sqlx::query_as(
        "UPDATE book_tasks SET status = $2, attempts = attempts + 1, stage = $3, \
             stage_version = stage_version + 1, \
             lease_node_id = $4, lease_session_id = $5, lease_execution_id = $6, \
             lease_expires_at = now() + ($7 || ' seconds')::interval, \
             nas_relative_path = $8, last_error = NULL, \
             expected_nas_relative_path = $8, expected_file_name = $10, \
             expected_format = $11, \
             bound_proxy_id = COALESCE(bound_proxy_id, $12), \
             bound_exit_ip = COALESCE(bound_exit_ip, $13), \
             proxy_bound_at = CASE WHEN bound_proxy_id IS NULL AND $12 IS NOT NULL THEN now() ELSE proxy_bound_at END, \
             updated_at = now() \
         WHERE id = $1 AND status = $9 AND cancel_requested = FALSE \
         RETURNING attempts, stage_version",
    )
    .bind(candidate.id)
    .bind(TaskStatus::Claimed.as_str())
    .bind("已分配")
    .bind(node_id)
    .bind(session_id)
    .bind(execution_id)
    .bind(lease_secs.clamp(1, 24 * 3600).to_string())
    .bind(&nas_relative_path)
    .bind(TaskStatus::Pending.as_str())
    .bind(&final_file_name)
    .bind(&candidate.format)
    .bind(proxy_id)
    .bind(proxy_exit_ip)
    .fetch_optional(&mut *tx)
    .await?;

    let Some((attempts, stage_version)) = leased else {
        // 被别人抢先改掉了。立刻重试一次是对的：候选池里通常还有别的书。
        tx.rollback().await?;
        return Ok(ClaimOutcome::Unavailable(Unavailable {
            reason: "任务已被其他节点领取".to_string(),
            retry_after_secs: 2,
        }));
    };

    store::session::start_execution(
        &mut tx,
        &store::session::NewExecution {
            id: execution_id,
            task_id: Some(candidate.id),
            account_registration_task_id: None,
            session_id,
            node_id,
            slot_index: session.slot_index,
            account_id: session.account_id,
            proxy_id: session.proxy_id,
            task_type,
            attempt: attempts,
            stage_version,
        },
    )
    .await?;
    super::catalog_bridge::execution_started(
        &mut tx,
        candidate.id,
        execution_id,
        node_id,
        session_id,
        session.slot_index,
    )
    .await?;

    let lease_expires_at: DateTime<Utc> =
        sqlx::query_scalar("SELECT lease_expires_at FROM book_tasks WHERE id = $1")
            .bind(candidate.id)
            .fetch_one(&mut *tx)
            .await?;

    store::task::sync_display_status(&mut *tx, candidate.book_id).await?;
    store::node::set_slot(
        &mut *tx,
        node_id,
        session.slot_index,
        platform_domain::SlotStatus::Running,
        Some(session_id),
        &format!("正在下载《{}》", candidate.raw_title),
    )
    .await?;
    tx.commit().await?;

    let assignment = TaskAssignment {
        session_id,
        execution_id,
        task_id: candidate.id,
        node_id,
        task_type,
        book: BookTarget {
            book_id: candidate.book_id,
            book_seq: candidate.seq,
            title: candidate.raw_title,
            author: candidate.raw_author,
            publisher: candidate.raw_publisher,
            isbn: candidate.raw_isbn,
            format: candidate.format,
        },
        nas_relative_path,
        uploading_file_name: layout.uploading_file_name(
            &final_file_name,
            &candidate.id.to_string(),
            &node_id.to_string(),
        ),
        final_file_name,
        attempt: attempts,
        stage_version,
        lease_expires_at,
        stall_timeout_secs: scheduler.stall_timeout_secs as u32,
    };

    state.events.publish(
        "任务变更",
        serde_json::json!({
            "任务": assignment.task_id,
            "状态": TaskStatus::Claimed.as_str(),
            "节点": node_id,
        }),
    );
    Ok(ClaimOutcome::Assigned(Box::new(assignment)))
}

/// 候选池大小。
///
/// 只锁候选池里的一行：池子太小时并发高峰会出现「全被锁住 → 空手而归」，
/// 太大则每次领取都要多算几十行的排序。50 是个折中值，
/// 空手时调用方会以很短的间隔重试，不会真的把任务饿死。
const CANDIDATE_POOL: i64 = 50;

/// 按第 7.2 节与第 10 节的优先级与代理绑定规则锁定一个候选任务。
async fn lock_candidate(
    tx: &mut sqlx::PgConnection,
    proxy_id: Option<Uuid>,
) -> AppResult<Option<ClaimCandidate>> {
    let candidate = sqlx::query_as::<_, ClaimCandidate>(
        "WITH candidate AS ( \
             SELECT t.id, \
                    max(b.priority) AS priority, \
                    min(b.created_at) AS batch_created_at, \
                    min(bb.import_line) AS import_line \
             FROM book_tasks t \
             JOIN batch_books bb ON bb.book_id = t.book_id \
             JOIN download_batches b ON b.id = bb.batch_id AND b.download_format = t.format \
             JOIN books bk ON bk.id = t.book_id \
             WHERE t.status = $1 \
               AND t.next_attempt_at <= now() \
               AND t.cancel_requested = FALSE \
               AND t.attempts < t.max_attempts \
               AND ($5::uuid IS NULL OR t.bound_proxy_id IS NULL OR t.bound_proxy_id = $5) \
               AND b.status = $2 \
               AND bk.merged_into IS NULL \
               AND NOT EXISTS ( \
                     SELECT 1 FROM book_files f \
                     WHERE f.book_id = t.book_id AND f.format = t.format AND f.status = $3) \
             GROUP BY t.id \
             ORDER BY priority DESC, batch_created_at, import_line \
             LIMIT $4 \
         ) \
         SELECT t.id, t.book_id, t.format, \
                bk.seq, bk.raw_title, bk.raw_author, bk.raw_publisher, bk.raw_isbn \
         FROM candidate c \
         JOIN book_tasks t ON t.id = c.id \
         JOIN books bk ON bk.id = t.book_id \
         ORDER BY c.priority DESC, c.batch_created_at, c.import_line \
         FOR UPDATE OF t SKIP LOCKED LIMIT 1",
    )
    .bind(TaskStatus::Pending.as_str())
    .bind(BatchStatus::Running.as_str())
    .bind("有效")
    .bind(CANDIDATE_POOL)
    .bind(proxy_id)
    .fetch_optional(&mut *tx)
    .await?;
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_pool_is_bounded_but_not_tiny() {
        // 池子过小会在并发高峰把领取变成「大概率空手」，过大则白算排序
        const { assert!(CANDIDATE_POOL >= 10 && CANDIDATE_POOL <= 200) };
    }

    #[test]
    fn assignment_file_names_follow_nas_layout() {
        let layout = NasLayout::default();
        let final_name = layout.final_file_name(7, "算法导论", "pdf");
        assert_eq!(final_name, "000007-算法导论.pdf");
        assert_eq!(
            layout.final_relative_path(7, "算法导论", "pdf"),
            "文件/000007-算法导论.pdf"
        );
        // 临时名必须带任务与节点编号，两个 Worker 同时重试同一本也不会互相覆盖
        let uploading = layout.uploading_file_name(&final_name, "任务", "节点");
        assert!(uploading.starts_with(&final_name));
        assert!(uploading.contains("上传中"));
    }
}
