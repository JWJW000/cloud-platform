//! 结果上报：归因、迟到判定与状态落地（第 10.3、14.4、14.5 节）。
//!
//! 这里回答三个层层递进的问题：
//! 1. **这次上报还算不算数**（[`applicability`]）：Worker 会重连、会重放、会在网络恢复后
//!    把十分钟前的结果送上来。判定依据是「执行编号 + 阶段版本」这一对，而不是时间戳——
//!    时钟不可信，而这两个值都是 Master 自己发出去的。
//! 2. **这次失败该由谁背**（[`platform_domain::failure`]）：代理故障不该消耗图书的重试
//!    次数，站点级限流不该把账号标成额度耗尽。
//! 3. **任务下一步落到哪个状态**（[`decide_task`]）：由归因结论加上「还剩几次重试」得出。
//!
//! 前两问的答案都是纯函数，因此本模块的判断逻辑可以在没有数据库的环境里完整测试；
//! 真正落库的部分只是把这些结论写下去。

use std::time::Duration;

use platform_domain::failure::Attribution;
use platform_domain::{
    classify_failure, AccountRegistrationTaskStatus, AccountStatus, ExecutionResult, FailureClass,
    ProxyStatus, SessionStatus, SlotStatus, TaskStatus,
};
use uuid::Uuid;

use crate::config::SchedulerConfig;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::store;

/// Worker 随成功结果一起送上来的文件证据（第 9.2 节）。
#[derive(Debug, Clone)]
pub struct FileEvidence {
    /// NAS 相对路径。
    pub nas_relative_path: String,
    /// 最终文件名。
    pub file_name: String,
    /// 字节数。
    pub size_bytes: i64,
    /// SHA-256。
    pub sha256: String,
    /// 格式（技术标识）。
    pub format: String,
}

/// 一次结果上报。
#[derive(Debug, Clone)]
pub struct ResultReport {
    /// 会话编号。
    pub session_id: Uuid,
    /// 执行编号。
    pub execution_id: Uuid,
    /// 任务编号。
    pub task_id: Uuid,
    /// 上报节点（证据来源，R6 第 12.1 节）。
    pub node_id: Option<Uuid>,
    /// Worker 给出的执行结果。
    pub result: ExecutionResult,
    /// 原因文本，失败归因的输入。
    pub reason: String,
    /// Worker 回带的阶段版本。
    pub stage_version: i32,
    /// 耗时毫秒。
    pub duration_ms: Option<i64>,
    /// 站点配额指示器读数 `(已用, 总额)`，第 10.3 节归因的关键输入。
    pub quota: Option<(u32, u32)>,
    /// 文件证据，仅成功入库时有。
    pub file: Option<FileEvidence>,
}

/// 上报处理结果，供 gRPC 层决定要不要回 `EventAck` 之外的动作。
#[derive(Debug, Clone)]
pub struct SubmitOutcome {
    /// 是否真的改变了状态。`false` 表示被判为迟到/重复，已留档。
    pub applied: bool,
    /// 中文说明，会写进事件记账的 `detail`。
    pub detail: String,
    /// 任务最终落到的状态。
    pub task_status: Option<TaskStatus>,
    /// 是否要求 Worker 结束当前会话（换账号或换代理）。
    pub end_session: bool,
}

/// 判定一次上报是否还算数所需的全部事实。
#[derive(Debug, Clone, Copy)]
pub struct ReportFacts {
    /// 任务当前持有的执行编号。
    pub lease_execution_id: Option<Uuid>,
    /// 本次上报声明的执行编号。
    pub reported_execution_id: Uuid,
    /// 任务当前状态。
    pub task_status: TaskStatus,
    /// 任务当前阶段版本。
    pub task_stage_version: i32,
    /// 本次上报回带的阶段版本。
    pub reported_stage_version: i32,
    /// 是否为「带文件证据的成功」。只有这种上报才允许补记，
    /// 因为没有文件证据的迟到成功无法与「Worker 记错了」区分开。
    pub provable_success: bool,
}

/// 一次上报的适用性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applicability {
    /// 正当其时：执行编号与阶段版本都对得上，按正常路径处理。
    Current,
    /// 迟到但可补记：任务已被回收成「待确认」，而这次上报带着 NAS 上真实存在的文件。
    Backfill,
    /// 只留档不改状态，附带中文原因。
    AuditOnly(&'static str),
}

/// 判定一次上报是否还算数（第 14.5 节）。
///
/// 判定顺序是刻意的：**先看任务是不是已经有定论**。已完成的任务再收到任何上报都只留档，
/// 否则一次重放就能把 `completed_count` 和当日统计加两遍；已取消的任务同理，
/// 管理员的取消意图不该被一个迟到的成功推翻。
pub fn applicability(facts: &ReportFacts) -> Applicability {
    let lease_matches = facts.lease_execution_id == Some(facts.reported_execution_id);
    let version_matches = facts.reported_stage_version == facts.task_stage_version;

    match facts.task_status {
        TaskStatus::Completed => Applicability::AuditOnly("任务已完成，重复上报仅留档"),
        TaskStatus::Cancelled => Applicability::AuditOnly("任务已取消，上报仅留档"),
        // 「待确认」正是在等一个说法：不论成功还是失败，这次上报都能解开不确定性
        TaskStatus::NeedsConfirm => Applicability::Backfill,
        _ if lease_matches && version_matches => Applicability::Current,
        // 租约或阶段版本已经变了，但文件确实在 NAS 上：补记完成比重下一遍划算，
        // 当前那次执行随后会因为「任务已完成」被判为重复上报。
        // V4 第 11.5 节收紧：**只有任务不再持有任何活动租约**（待确认/待处理）才允许
        // 补记；任务已被新执行领取时禁止提交，否则新执行的书还没下完就被旧结果终结。
        _ if facts.provable_success && facts.lease_execution_id.is_none() => {
            Applicability::Backfill
        }
        _ if !lease_matches => Applicability::AuditOnly("执行编号已被新的分配取代，仅留档"),
        _ => Applicability::AuditOnly("阶段版本已过期，仅留档"),
    }
}

/// 任务的下一步。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskDecision {
    /// 任务应落到的状态。
    pub status: TaskStatus,
    /// 对 `attempts` 的修正量。
    ///
    /// 领取时已经把 `attempts` 加过一次，因此这里只需要在「本次失败不该消耗重试次数」
    /// 时回退 `-1`。把修正量而不是绝对值传出来，是为了让落库语句保持
    /// `attempts = attempts + $n`，避免读-改-写之间的竞争。
    pub attempts_delta: i32,
    /// 距离下次可领取的等待时长；非重试状态为零。
    pub retry_after: Duration,
}

/// 由归因结论与剩余重试次数决定任务的下一步（第 10.3 节 + 第 7.2 节第 5 条）。
pub fn decide_task(
    attribution: &Attribution,
    attempts: i32,
    max_attempts: i32,
    scheduler: &SchedulerConfig,
) -> TaskDecision {
    let attempts_delta = if attribution.consumes_retry { 0 } else { -1 };
    let effective = (attempts + attempts_delta).max(0);
    let target = attribution.task_status.unwrap_or(TaskStatus::Pending);

    if target != TaskStatus::Pending {
        return TaskDecision {
            status: target,
            attempts_delta,
            retry_after: Duration::ZERO,
        };
    }

    // 只有「消耗了重试次数」的失败才可能把任务耗尽：代理故障或站点限流反复出现时
    // 任务应当一直排队等资源恢复，而不是被判定为这本书下不下来。
    if attribution.consumes_retry && effective >= max_attempts.max(1) {
        return TaskDecision {
            status: TaskStatus::Failed,
            attempts_delta,
            retry_after: Duration::ZERO,
        };
    }

    TaskDecision {
        status: TaskStatus::Pending,
        attempts_delta,
        retry_after: scheduler.backoff_for(effective + 1),
    }
}

/// 成功上报的文件证据是否可信（第 9.2 节）。
///
/// 只做 Master 能独立验证的两件事：路径与任务分配时下发的一致、大小不小于阈值。
/// SHA-256 由 Worker 在本机算完再改名，Master 没有文件，无法复算，只能存档备查。
fn reject_file_evidence(
    file: &FileEvidence,
    expected_path: Option<&str>,
    expected_format: &str,
    minimum_bytes: u64,
) -> Option<String> {
    if file.size_bytes <= 0 || (file.size_bytes as u64) < minimum_bytes {
        return Some(format!(
            "文件过小（{} 字节），疑似站点错误页：checksum 不予采信",
            file.size_bytes
        ));
    }
    if file.sha256.trim().is_empty() {
        return Some("缺少 SHA-256，无法确认入库文件".to_string());
    }
    if !file.format.eq_ignore_ascii_case(expected_format) {
        return Some(format!(
            "格式不匹配：期望 {expected_format}，实际 {}",
            file.format
        ));
    }
    match expected_path {
        Some(expected) if expected != file.nas_relative_path => Some(format!(
            "文件名不匹配：期望 {expected}，实际 {}",
            file.nas_relative_path
        )),
        _ => None,
    }
}

/// 处理一次结果上报（V4 方案第 11.2 节：统一事务入口）。
///
/// 修复 V4-06：禁止「事务外判断、事务内无条件更新」。全部裁决在同一个
/// 事务内完成：
/// 1. `SELECT ... FROM book_tasks WHERE id=$1 FOR UPDATE` 锁任务行；
/// 2. 同一事务读取执行上下文；
/// 3. 同一事务重新判断 execution_id、stage_version、任务状态、cancel_requested；
/// 4. 带前置条件的 UPDATE（CAS），并检查 `rows_affected == 1`；
/// 5. 文件、任务、执行记录、额度、统计、批次展示在同一事务更新；
/// 6. 提交后再发布 SSE 事件。
pub async fn submit_result(state: &AppState, report: &ResultReport) -> AppResult<SubmitOutcome> {
    let mut tx = state.pool.begin().await?;

    // 1. 同一事务读取任务（FOR UPDATE）
    let task = lock_task_for_update(&mut tx, report.task_id).await?;
    let task_status = task.status.parse::<TaskStatus>()?;

    // 2. 同一事务读取执行上下文
    let Some(context) = store::session::execution_context(&mut *tx, report.execution_id).await?
    else {
        tx.rollback().await?;
        return Ok(audit_only("未知的执行编号，仅留档"));
    };
    if context.task_id != Some(report.task_id) {
        tx.rollback().await?;
        return Ok(audit_only("执行编号与任务编号不匹配，仅留档"));
    }

    // 3. 同一事务重新判断
    // 成功但文件证据不可信时，按「不可重试失败」处理而不是拒收：文件已经在 NAS 上，
    // 留下一条带原因的失败记录，管理员才能去看那个可疑文件到底是什么。
    let mut evidence_error: Option<String> = None;
    if report.result == ExecutionResult::Success {
        match &report.file {
            None => evidence_error = Some("成功上报缺少文件证据".to_string()),
            Some(file) => {
                evidence_error = reject_file_evidence(
                    file,
                    task.nas_relative_path.as_deref(),
                    &task.format,
                    state.config.nas.minimum_file_bytes,
                );
            }
        }
    }
    let provable_success = report.result == ExecutionResult::Success && evidence_error.is_none();

    let facts = ReportFacts {
        lease_execution_id: task.lease_execution_id,
        reported_execution_id: report.execution_id,
        task_status,
        task_stage_version: task.stage_version,
        reported_stage_version: report.stage_version,
        provable_success,
    };

    let outcome = match applicability(&facts) {
        Applicability::AuditOnly(reason) => {
            // 执行记录仍然要收尾：否则这条记录会永远挂在「未完成」上，
            // 让「某个节点还有几次执行在跑」这类统计永久失真。
            store::session::finish_execution(
                &mut *tx,
                report.execution_id,
                report.result,
                Some(reason),
                report.duration_ms,
            )
            .await?;
            audit_only(reason)
        }
        current @ (Applicability::Current | Applicability::Backfill) => {
            if provable_success {
                apply_success_in_tx(&mut tx, report, &context, current).await?
            } else {
                let reason = evidence_error.unwrap_or_else(|| report.reason.clone());
                apply_failure_in_tx(&mut tx, state, report, &context, &task, &reason, current)
                    .await?
            }
        }
    };

    tx.commit().await?;

    // 6. 提交后再发布 SSE 事件
    match &outcome.task_status {
        Some(TaskStatus::Completed) => {
            if let Some(file) = &report.file {
                state.events.publish(
                    "任务变更",
                    serde_json::json!({
                        "任务": report.task_id,
                        "状态": TaskStatus::Completed.as_str(),
                        "路径": file.nas_relative_path,
                    }),
                );
            }
        }
        Some(status) => {
            state.events.publish(
                "任务变更",
                serde_json::json!({
                    "任务": report.task_id,
                    "状态": status.as_str(),
                    "原因": report.reason,
                }),
            );
        }
        None => {}
    }
    if outcome.end_session {
        // 会话结束由 gRPC 层执行，这里只负责发布事件
        state
            .events
            .publish("会话变更", serde_json::json!({ "会话": report.session_id }));
    }

    Ok(outcome)
}

/// 事务内按主键锁定任务行（`FOR UPDATE`）。
async fn lock_task_for_update(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    task_id: Uuid,
) -> AppResult<crate::models::BookTask> {
    let task = sqlx::query_as::<_, crate::models::BookTask>(&format!(
        "SELECT {} FROM book_tasks t JOIN books b ON b.id = t.book_id \
         WHERE t.id = $1 FOR UPDATE",
        crate::store::task::TASK_COLUMNS
    ))
    .bind(task_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| AppError::missing("任务不存在"))?;
    Ok(task)
}

fn audit_only(detail: &str) -> SubmitOutcome {
    SubmitOutcome {
        applied: false,
        detail: detail.to_string(),
        task_status: None,
        end_session: false,
    }
}

/// 成功入库：事务内 CAS 判完成，登记文件、任务、账号额度、当日统计（第 11.3 节）。
async fn apply_success_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    report: &ResultReport,
    context: &store::session::ExecutionContext,
    applied_as: Applicability,
) -> AppResult<SubmitOutcome> {
    let file = report
        .file
        .as_ref()
        .expect("provable_success 已保证文件证据存在");
    let book_id: Uuid = sqlx::query_scalar("SELECT book_id FROM book_tasks WHERE id = $1")
        .bind(report.task_id)
        .fetch_one(&mut **tx)
        .await?;

    let node_id: Option<Uuid> =
        sqlx::query_scalar("SELECT node_id FROM task_executions WHERE id = $1")
            .bind(report.execution_id)
            .fetch_one(&mut **tx)
            .await?;

    // CAS 条件更新（第 11.3 节）：执行编号、阶段版本、未取消、活动状态全部匹配
    // 才允许判完成；影响行数必须恰好为 1（V4-06）。
    //
    // - Current：绑定租约执行编号与世代，防止旧执行覆盖新执行；
    // - Backfill（待确认 / 迟到的可证明成功）：任务已释放租约，不能按租约绑定，
    //   改为绑定「未取消 + 活动状态」，且绝不复活已取消任务。
    let affected = if applied_as == Applicability::Current {
        sqlx::query(
            "UPDATE book_tasks SET status = $2, stage = $3, stage_version = stage_version + 1, \
                 nas_relative_path = $4, downloaded_bytes = $5, total_bytes = GREATEST(total_bytes, $5), \
                 lease_node_id = NULL, lease_session_id = NULL, lease_execution_id = NULL, \
                 lease_expires_at = NULL, last_error = NULL, updated_at = now() \
             WHERE id = $1 AND lease_execution_id = $6 AND stage_version = $7 \
               AND cancel_requested = FALSE \
               AND status IN ('已分配', '执行中', '等待入库')",
        )
        .bind(report.task_id)
        .bind(TaskStatus::Completed.as_str())
        .bind("已完成")
        .bind(&file.nas_relative_path)
        .bind(file.size_bytes)
        .bind(report.execution_id)
        .bind(report.stage_version)
        .execute(&mut **tx)
        .await?
        .rows_affected()
    } else {
        sqlx::query(
            "UPDATE book_tasks SET status = $2, stage = $3, stage_version = stage_version + 1, \
                 nas_relative_path = $4, downloaded_bytes = $5, total_bytes = GREATEST(total_bytes, $5), \
                 lease_node_id = NULL, lease_session_id = NULL, lease_execution_id = NULL, \
                 lease_expires_at = NULL, last_error = NULL, updated_at = now() \
             WHERE id = $1 AND cancel_requested = FALSE \
               AND status IN ('已分配', '执行中', '等待入库', '待确认')",
        )
        .bind(report.task_id)
        .bind(TaskStatus::Completed.as_str())
        .bind("已完成")
        .bind(&file.nas_relative_path)
        .bind(file.size_bytes)
        .execute(&mut **tx)
        .await?
        .rows_affected()
    };

    if affected != 1 {
        // CAS 未命中：批次取消/重试/新执行可能已抢先提交。
        // 本次结果只留档，绝不继续扣额度或增加统计；
        // 但执行记录必须收尾（P1）：否则该 task_execution 永远挂在「未完成」，
        // 让「某节点还有几次执行在跑」的统计永久失真。
        let reason = "任务状态已被其他事务改变（取消/重试/新执行已抢先），成功结果仅留档";
        store::session::finish_execution(
            &mut **tx,
            report.execution_id,
            ExecutionResult::Success,
            Some(reason),
            report.duration_ms,
        )
        .await?;
        return Ok(audit_only(reason));
    }

    store::catalog::record_book_file(
        &mut **tx,
        book_id,
        &file.format.to_ascii_lowercase(),
        &file.nas_relative_path,
        file.size_bytes,
        &file.sha256,
        node_id,
    )
    .await?;

    store::session::finish_execution(
        &mut **tx,
        report.execution_id,
        ExecutionResult::Success,
        None,
        report.duration_ms,
    )
    .await?;

    // 会话计数与账号额度分开加：NAS 核验之类的会话也会走到这里，但它们不该吃账号额度。
    if let Some(session_id) = context.session_id {
        store::session::bump_completed(&mut **tx, session_id).await?;
    }
    let account_used = match context.account_id {
        Some(account_id) => {
            store::session::consume_account_quota(&mut **tx, account_id).await?;
            1
        }
        None => 0,
    };
    store::admin::bump_daily_stat(&mut **tx, 1, 0, 0, file.size_bytes, account_used).await?;
    store::task::sync_display_status(&mut **tx, book_id).await?;

    Ok(SubmitOutcome {
        applied: true,
        detail: format!("已入库：{}", file.nas_relative_path),
        task_status: Some(TaskStatus::Completed),
        end_session: false,
    })
}

/// 失败、跳过、取消或结果不确定：事务内先归因，再以 CAS 更新（第 11.4 节）。
async fn apply_failure_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    state: &AppState,
    report: &ResultReport,
    context: &store::session::ExecutionContext,
    task: &crate::models::BookTask,
    reason: &str,
    applied_as: Applicability,
) -> AppResult<SubmitOutcome> {
    let scheduler = state.scheduler();
    let class = match report.result {
        ExecutionResult::Skipped => FailureClass::BookNotFound,
        ExecutionResult::Uncertain => FailureClass::Uncertain,
        ExecutionResult::FatalFailure => FailureClass::Fatal,
        // 取消由管理员路径落状态，这里只把执行记录收尾
        ExecutionResult::Cancelled => {
            return apply_cancelled_in_tx(tx, report, task, applied_as).await
        }
        // 成功但证据不可信也走到这里：`reason` 已经被换成了证据问题的描述
        ExecutionResult::Success => FailureClass::Fatal,
        ExecutionResult::RetryableFailure => classify_failure(reason, report.quota),
    };
    let attribution = class.attribution();
    let decision = decide_task(&attribution, task.attempts, task.max_attempts, scheduler);

    // CAS 条件更新（第 11.4 节）：失败/跳过/待确认同样绑定执行编号、世代、
    // 活动状态与未取消；影响行数必须为 1，否则按当前事实重新处理。
    let affected = if applied_as == Applicability::Current {
        sqlx::query(
            "UPDATE book_tasks SET status = $2, stage = '', \
                 attempts = GREATEST(attempts + $3, 0), \
                 stage_version = stage_version + 1, \
                 next_attempt_at = now() + ($4 || ' seconds')::interval, \
                 lease_node_id = NULL, lease_session_id = NULL, lease_execution_id = NULL, \
                 lease_expires_at = NULL, last_error = $5, updated_at = now() \
             WHERE id = $1 AND lease_execution_id = $6 AND stage_version = $7 \
               AND cancel_requested = FALSE \
               AND status IN ('已分配', '执行中', '等待入库')",
        )
        .bind(report.task_id)
        .bind(decision.status.as_str())
        .bind(decision.attempts_delta)
        .bind(decision.retry_after.as_secs().to_string())
        .bind(reason)
        .bind(report.execution_id)
        .bind(report.stage_version)
        .execute(&mut **tx)
        .await?
        .rows_affected()
    } else {
        // Backfill（待确认任务收到失败/不确定上报）：只允许在「待确认 + 未取消」上落地，
        // 不允许把其他活动状态的任务改写掉。
        sqlx::query(
            "UPDATE book_tasks SET status = $2, stage = '', \
                 attempts = GREATEST(attempts + $3, 0), \
                 stage_version = stage_version + 1, \
                 next_attempt_at = now() + ($4 || ' seconds')::interval, \
                 lease_node_id = NULL, lease_session_id = NULL, lease_execution_id = NULL, \
                 lease_expires_at = NULL, last_error = $5, updated_at = now() \
             WHERE id = $1 AND cancel_requested = FALSE AND status = '待确认'",
        )
        .bind(report.task_id)
        .bind(decision.status.as_str())
        .bind(decision.attempts_delta)
        .bind(decision.retry_after.as_secs().to_string())
        .bind(reason)
        .execute(&mut **tx)
        .await?
        .rows_affected()
    };

    if affected != 1 {
        // CAS 未命中：只留档，但执行记录必须收尾（P1），避免永久「未完成」。
        let reason = "任务状态已被其他事务改变（取消/重试/新执行已抢先），失败结果仅留档";
        store::session::finish_execution(
            &mut **tx,
            report.execution_id,
            attribution.result,
            Some(reason),
            report.duration_ms,
        )
        .await?;
        return Ok(audit_only(reason));
    }

    // R6（V4 第 12.2 节）：任务进入「待确认」时，在同一事务固化全部已有证据。
    // 只固化一次（期望尚未写入时），后续核验不得用核验请求覆盖原期望。
    if decision.status == TaskStatus::NeedsConfirm {
        if let Some(file) = &report.file {
            sqlx::query(
                "UPDATE book_tasks SET expected_size_bytes = $2, expected_sha256 = $3, \
                     evidence_execution_id = $4, evidence_node_id = $5, \
                     evidence_recorded_at = now(), updated_at = now() \
                 WHERE id = $1 AND expected_sha256 IS NULL AND expected_size_bytes IS NULL",
            )
            .bind(report.task_id)
            .bind(file.size_bytes)
            .bind(&file.sha256)
            .bind(report.execution_id)
            .bind(report.node_id)
            .execute(&mut **tx)
            .await?;
        }
    }

    store::session::finish_execution(
        &mut **tx,
        report.execution_id,
        attribution.result,
        Some(reason),
        report.duration_ms,
    )
    .await?;

    if let (Some(account_id), Some(status)) = (context.account_id, attribution.account_status) {
        store::resource::set_account_status(&mut **tx, account_id, status, Some(reason)).await?;
    }
    if let (Some(proxy_id), Some(status)) = (context.proxy_id, attribution.proxy_status) {
        apply_proxy_status(tx, proxy_id, status, state).await?;
    }

    let (failed, skipped) = match decision.status {
        TaskStatus::Failed => (1, 0),
        TaskStatus::Skipped => (0, 1),
        _ => (0, 0),
    };
    if failed + skipped > 0 {
        store::admin::bump_daily_stat(&mut **tx, 0, failed, skipped, 0, 0).await?;
    }
    store::task::sync_display_status(&mut **tx, task.book_id).await?;

    if decision.status == TaskStatus::Failed {
        store::admin::raise_alert(
            &mut **tx,
            platform_domain::AlertLevel::Warn,
            "任务",
            &format!("《{}》达到重试上限", task.title),
            reason,
            None,
            Some(&format!("任务失败:{}", task.id)),
        )
        .await?;
    }

    Ok(SubmitOutcome {
        applied: true,
        detail: format!("{}：{reason}", decision.status),
        task_status: Some(decision.status),
        end_session: attribution.ends_session,
    })
}

// ---------------------------------------------------------------- 账号注册结果裁决

/// 账号注册结果上报。
#[derive(Debug, Clone)]
pub struct RegistrationResultReport {
    /// 会话编号。
    pub session_id: Uuid,
    /// 执行编号。
    pub execution_id: Uuid,
    /// 账号注册任务编号。
    pub registration_task_id: Uuid,
    /// 执行节点。
    pub node_id: Option<Uuid>,
    /// 执行结果。
    pub result: ExecutionResult,
    /// 原因说明。
    pub reason: String,
    /// 阶段版本。
    pub stage_version: i32,
    /// 尝试次数。
    pub attempt: i32,
    /// 站点是否已存在同邮箱。
    pub already_exists: bool,
    /// 是否等待验证码等人工确认。
    pub awaiting_verification: bool,
    /// 完成时间。
    pub completed_at: Option<String>,
}

/// 确认接受账号注册任务。
pub async fn accept_registration_task(
    state: &AppState,
    execution_id: Uuid,
    registration_task_id: Uuid,
) -> AppResult<bool> {
    let affected = sqlx::query(
        "UPDATE account_registration_tasks SET status = $3, stage = $4, updated_at = now() \
         WHERE id = $1 AND lease_execution_id = $2 AND status = $5",
    )
    .bind(registration_task_id)
    .bind(execution_id)
    .bind(AccountRegistrationTaskStatus::Running.as_str())
    .bind("已接受")
    .bind(AccountRegistrationTaskStatus::Claimed.as_str())
    .execute(&state.pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// 记录账号注册进度。
pub async fn record_registration_progress(
    state: &AppState,
    execution_id: Uuid,
    registration_task_id: Uuid,
    stage: &str,
    stage_version: i32,
) -> AppResult<bool> {
    let affected = sqlx::query(
        "UPDATE account_registration_tasks SET stage = $3, updated_at = now() \
         WHERE id = $1 AND lease_execution_id = $2 AND stage_version = $4 \
           AND status IN ($5, $6)",
    )
    .bind(registration_task_id)
    .bind(execution_id)
    .bind(stage)
    .bind(stage_version)
    .bind(AccountRegistrationTaskStatus::Claimed.as_str())
    .bind(AccountRegistrationTaskStatus::Running.as_str())
    .execute(&state.pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// 提交账号注册任务结果（事务内原子裁决并更新账号与任务状态）。
pub async fn submit_registration_result(
    state: &AppState,
    report: &RegistrationResultReport,
) -> AppResult<SubmitOutcome> {
    let mut tx = state.pool.begin().await?;

    let task = sqlx::query_as::<_, (Uuid, Uuid, Uuid, String, i32, i32, i32, Option<Uuid>)>(
        "SELECT id, batch_id, account_id, status, attempts, max_attempts, stage_version, lease_execution_id \
         FROM account_registration_tasks WHERE id = $1 FOR UPDATE",
    )
    .bind(report.registration_task_id)
    .fetch_optional(&mut *tx)
    .await?;

    let Some((
        task_id,
        batch_id,
        account_id,
        _current_status,
        attempts,
        max_attempts,
        current_stage_ver,
        lease_exec,
    )) = task
    else {
        tx.rollback().await?;
        return Ok(SubmitOutcome {
            applied: false,
            detail: "账号注册任务不存在".to_string(),
            task_status: None,
            end_session: true,
        });
    };

    // 检查版本与执行编号
    let is_current =
        lease_exec == Some(report.execution_id) && current_stage_ver == report.stage_version;
    if !is_current {
        // 迟到结果：记录 task_executions 但不改任务与账号
        store::session::finish_execution(
            &mut *tx,
            report.execution_id,
            report.result,
            Some(&report.reason),
            None,
        )
        .await?;
        tx.commit().await?;
        return Ok(SubmitOutcome {
            applied: false,
            detail: format!(
                "迟到或过期执行结果（执行={:?}，当前世代={}），已留档",
                lease_exec, current_stage_ver
            ),
            task_status: None,
            end_session: true,
        });
    }

    let reason = &report.reason;
    let (new_task_status, new_account_status, is_success, end_session) = if report.result
        == ExecutionResult::Success
        && !report.already_exists
        && !report.awaiting_verification
    {
        (
            AccountRegistrationTaskStatus::Completed,
            AccountStatus::Registered,
            true,
            true,
        )
    } else if report.already_exists {
        (
            AccountRegistrationTaskStatus::Completed,
            AccountStatus::Disabled,
            false,
            true,
        )
    } else if report.awaiting_verification {
        (
            AccountRegistrationTaskStatus::AwaitingManualConfirm,
            AccountStatus::VerificationPending,
            false,
            false,
        )
    } else if report.result == ExecutionResult::Cancelled {
        (
            AccountRegistrationTaskStatus::Cancelled,
            AccountStatus::PendingRegistration,
            false,
            true,
        )
    } else if report.result == ExecutionResult::FatalFailure || attempts >= max_attempts {
        (
            AccountRegistrationTaskStatus::Failed,
            AccountStatus::LoginFailed,
            false,
            true,
        )
    } else {
        // 可重试失败
        (
            AccountRegistrationTaskStatus::Retrying,
            AccountStatus::PendingRegistration,
            false,
            true,
        )
    };

    // 1. 更新执行记录
    store::session::finish_execution(
        &mut *tx,
        report.execution_id,
        report.result,
        Some(reason),
        None,
    )
    .await?;

    // 2. 更新任务表
    let retry_secs = if new_task_status == AccountRegistrationTaskStatus::Retrying {
        "60"
    } else {
        "0"
    };

    sqlx::query(
        "UPDATE account_registration_tasks SET \
             status = $2, stage = '', stage_version = stage_version + 1, \
             lease_node_id = NULL, lease_session_id = NULL, lease_execution_id = NULL, \
             lease_expires_at = NULL, last_error = $3, \
             attempts = attempts + 1, \
             next_attempt_at = now() + ($4 || ' seconds')::interval, \
             updated_at = now() \
         WHERE id = $1",
    )
    .bind(task_id)
    .bind(new_task_status.as_str())
    .bind(reason)
    .bind(retry_secs)
    .execute(&mut *tx)
    .await?;

    // 3. 更新账号表
    if is_success {
        sqlx::query(
            "UPDATE accounts SET status = $2, registered_at = now(), last_error = NULL, \
                 lease_session_id = NULL, lease_expires_at = NULL, updated_at = now() \
             WHERE id = $1",
        )
        .bind(account_id)
        .bind(new_account_status.as_str())
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query(
            "UPDATE accounts SET status = $2, last_error = $3, \
                 lease_session_id = NULL, lease_expires_at = NULL, updated_at = now() \
             WHERE id = $1",
        )
        .bind(account_id)
        .bind(new_account_status.as_str())
        .bind(reason)
        .execute(&mut *tx)
        .await?;
    }

    // 4. 检查批次完成状态
    store::account_registration::check_batch_completion(&mut tx, batch_id).await?;

    tx.commit().await?;

    state.events.publish(
        "账号注册变更",
        serde_json::json!({
            "任务": task_id,
            "批次": batch_id,
            "账号": account_id,
            "状态": new_task_status.as_str(),
            "结果": report.result.as_str(),
        }),
    );

    Ok(SubmitOutcome {
        applied: true,
        detail: format!("账号注册{}：{}", new_task_status.as_str(), reason),
        task_status: None,
        end_session,
    })
}

/// 取消上报：任务状态由管理员那条路径决定，这里只收尾执行记录。
async fn apply_cancelled_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    report: &ResultReport,
    task: &crate::models::BookTask,
    applied_as: Applicability,
) -> AppResult<SubmitOutcome> {
    // 专用取消收尾路径（第 11.4 节例外）：取消允许不要求 cancel_requested，
    // 但必须绑定执行编号与世代，防止旧取消消息改写新执行。
    let affected = if applied_as == Applicability::Current {
        sqlx::query(
            "UPDATE book_tasks SET status = $2, stage = '', stage_version = stage_version + 1, \
                 lease_node_id = NULL, lease_session_id = NULL, lease_execution_id = NULL, \
                 lease_expires_at = NULL, last_error = $3, updated_at = now() \
             WHERE id = $1 AND lease_execution_id = $4 AND stage_version = $5 \
               AND status IN ('已分配', '执行中', '等待入库')",
        )
        .bind(report.task_id)
        .bind(TaskStatus::Cancelled.as_str())
        .bind(&report.reason)
        .bind(report.execution_id)
        .bind(report.stage_version)
        .execute(&mut **tx)
        .await?
        .rows_affected()
    } else {
        sqlx::query(
            "UPDATE book_tasks SET status = $2, stage = '', stage_version = stage_version + 1, \
                 lease_node_id = NULL, lease_session_id = NULL, lease_execution_id = NULL, \
                 lease_expires_at = NULL, last_error = $3, updated_at = now() \
             WHERE id = $1 AND status = '待确认'",
        )
        .bind(report.task_id)
        .bind(TaskStatus::Cancelled.as_str())
        .bind(&report.reason)
        .execute(&mut **tx)
        .await?
        .rows_affected()
    };

    if affected != 1 {
        // CAS 未命中：只留档，但执行记录必须收尾（P1），避免永久「未完成」。
        let reason = "任务状态已被其他事务改变（取消/重试/新执行已抢先），取消结果仅留档";
        store::session::finish_execution(
            &mut **tx,
            report.execution_id,
            ExecutionResult::Cancelled,
            Some(reason),
            report.duration_ms,
        )
        .await?;
        return Ok(audit_only(reason));
    }

    store::session::finish_execution(
        &mut **tx,
        report.execution_id,
        ExecutionResult::Cancelled,
        Some(&report.reason),
        report.duration_ms,
    )
    .await?;
    store::task::sync_display_status(&mut **tx, task.book_id).await?;

    Ok(SubmitOutcome {
        applied: true,
        detail: "已取消".to_string(),
        task_status: Some(TaskStatus::Cancelled),
        end_session: false,
    })
}

/// 代理的两种降级走不同的语句：冷却要带时长与限流计数，异常只改状态。
async fn apply_proxy_status(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    proxy_id: Uuid,
    status: ProxyStatus,
    state: &AppState,
) -> AppResult<()> {
    match status {
        ProxyStatus::CoolingDown => {
            store::resource::cool_down_proxy(
                &mut **tx,
                proxy_id,
                state.config.webshare.cooldown_minutes,
            )
            .await?;
        }
        other => {
            store::resource::set_proxy_status(&mut **tx, proxy_id, other, None).await?;
        }
    }
    Ok(())
}

/// 取消上报：任务状态由管理员那条路径决定，这里只收尾执行记录。
/// Worker 确认收到分配（`TaskAccepted`）：任务从「已分配」进入「执行中」。
pub async fn accept_task(state: &AppState, execution_id: Uuid, task_id: Uuid) -> AppResult<bool> {
    let affected = sqlx::query(
        "UPDATE book_tasks SET status = $3, stage = $4, updated_at = now() \
         WHERE id = $1 AND lease_execution_id = $2 AND status = $5",
    )
    .bind(task_id)
    .bind(execution_id)
    .bind(TaskStatus::Running.as_str())
    .bind("已接受")
    .bind(TaskStatus::Claimed.as_str())
    .execute(&state.pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// 进度上报（第 13.3 节：Worker 侧已按 2 秒 / 5MB 节流）。
///
/// 只在执行编号与阶段版本都对得上时才写：迟到的进度会把已经完成的任务的
/// 字节数改回去，界面上就会出现「进度倒退」。返回 `false` 表示这条进度被丢弃。
pub async fn record_progress(
    state: &AppState,
    execution_id: Uuid,
    task_id: Uuid,
    downloaded_bytes: i64,
    total_bytes: i64,
    stage: &str,
    stage_version: i32,
) -> AppResult<bool> {
    let affected = sqlx::query(
        "UPDATE book_tasks SET downloaded_bytes = $3, \
             total_bytes = GREATEST(total_bytes, $4), \
             stage = $5, \
             status = CASE WHEN status = $7 THEN $8 ELSE status END, \
             updated_at = now() \
         WHERE id = $1 AND lease_execution_id = $2 AND stage_version = $6 \
           AND status IN ($7, $8, $9)",
    )
    .bind(task_id)
    .bind(execution_id)
    .bind(downloaded_bytes.max(0))
    .bind(total_bytes.max(0))
    .bind(stage)
    .bind(stage_version)
    .bind(TaskStatus::Claimed.as_str())
    .bind(TaskStatus::Running.as_str())
    .bind(TaskStatus::AwaitingIngest.as_str())
    .execute(&state.pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// 会话自行结束（`SessionClosed`）：走与回收相同的释放路径。
pub async fn session_closed(
    state: &AppState,
    session_id: Uuid,
    status: SessionStatus,
    reason: &str,
) -> AppResult<()> {
    let status = match status {
        SessionStatus::Failed => SessionStatus::Failed,
        _ => SessionStatus::Ended,
    };
    crate::scheduler::allocate::close_session(state, session_id, status, reason).await
}

/// 代理检测结果落库（`ProxyCheckResult`）。
pub async fn proxy_check_result(
    state: &AppState,
    proxy_id: Uuid,
    reachable: bool,
    exit_ip: Option<&str>,
    latency_ms: Option<i32>,
    detail: &str,
) -> AppResult<()> {
    store::resource::record_proxy_check(&state.pool, proxy_id, reachable, exit_ip, latency_ms)
        .await?;
    store::admin::log(
        &state.pool,
        platform_domain::OperationSource::Worker,
        if reachable {
            platform_domain::LogLevel::Info
        } else {
            platform_domain::LogLevel::Warn
        },
        "Worker",
        "代理检测",
        &proxy_id.to_string(),
        detail,
    )
    .await?;
    state
        .events
        .publish("代理变更", serde_json::json!({ "代理": proxy_id }));
    Ok(())
}

/// 一次 NAS 核验上报（`NasCheckResult`）。
///
/// 打包成结构体而不是一长串参数：这几个字段总是一起出现，
/// 而 `bool` 挨着 `bool` 的参数列表最容易在调用处被写反。
#[derive(Debug, Clone)]
pub struct NasCheckReport<'a> {
    /// 上报的节点。
    pub node_id: Uuid,
    /// 被核验的任务，为 `None` 时只是一次例行挂载体检。
    pub task_id: Option<Uuid>,
    /// 挂载点是否存在。
    pub mount_present: bool,
    /// 是否可写。
    pub writable: bool,
    /// NAS 剩余空间（GB）。
    pub free_gb: i64,
    /// 找到的文件证据，`None` 表示目标文件不存在。
    pub file: Option<&'a FileEvidence>,
    /// 中文说明。
    pub detail: &'a str,
}

/// NAS 核验结果落库（第 14.4 节：「待确认」任务的最终裁决）。
///
/// 核验的意义就是把「不确定」变成确定：文件在，就补记完成；文件不在，就放回待处理重下。
/// 这一步之后任务不再停留在「待确认」，因此不需要额外的人工介入。
pub async fn nas_check_result(state: &AppState, check: &NasCheckReport<'_>) -> AppResult<()> {
    let node_id = check.node_id;
    let detail = check.detail;
    let healthy = check.mount_present && check.writable;
    store::node::set_nas_health(&state.pool, node_id, healthy, check.free_gb).await?;

    if !healthy {
        store::admin::raise_alert(
            &state.pool,
            platform_domain::AlertLevel::Critical,
            "NAS",
            "NAS 挂载不可写",
            detail,
            Some(node_id),
            Some(&format!("NAS不可写:{node_id}")),
        )
        .await?;
    } else {
        store::admin::resolve_alert_by_key(&state.pool, &format!("NAS不可写:{node_id}")).await?;
        // 空间告警与可写性分开判断：能写但快满了，也要在写不下去之前提醒。
        if check.free_gb < state.config.nas.free_space_alert_gb {
            store::admin::raise_alert(
                &state.pool,
                platform_domain::AlertLevel::Warn,
                "NAS",
                "NAS 剩余空间不足",
                &format!("剩余 {} GB", check.free_gb),
                Some(node_id),
                Some(&format!("NAS空间:{node_id}")),
            )
            .await?;
        } else {
            store::admin::resolve_alert_by_key(&state.pool, &format!("NAS空间:{node_id}")).await?;
        }
    }

    let Some(task_id) = check.task_id else {
        return Ok(());
    };
    let task = store::task::get_task(&state.pool, task_id).await?;
    if task.status.parse::<TaskStatus>()? != TaskStatus::NeedsConfirm {
        return Ok(());
    }

    // R6（V4 第 12.3 节）：核验只使用**已固化**的期望字段。
    // 缺关键字段时不得自动判完成：
    // - 缺路径：协议/数据错误，保持待确认并告警；
    // - 缺 SHA：允许检查文件是否存在，但不得自动判完成；
    // - 缺大小：允许计算实际大小，但不得把未知当匹配。
    #[derive(Default)]
    struct FrozenExpectations {
        path: Option<String>,
        format: Option<String>,
        size: Option<i64>,
        sha: Option<String>,
    }
    type FrozenRow = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<String>,
    );
    let row: Option<FrozenRow> = sqlx::query_as(
        "SELECT expected_nas_relative_path, expected_file_name, expected_format, \
                    expected_size_bytes, expected_sha256 \
             FROM book_tasks WHERE id = $1",
    )
    .bind(task_id)
    .fetch_optional(&state.pool)
    .await?;
    let frozen = match row {
        Some((path, _file_name, format, size, sha)) => FrozenExpectations {
            path,
            format,
            size,
            sha: sha
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty()),
        },
        None => return Ok(()),
    };

    // 1. 缺路径：数据错误，保持待确认并严重告警，禁止继续
    let Some(expected_path) = frozen.path.as_deref() else {
        store::admin::raise_alert(
            &state.pool,
            platform_domain::AlertLevel::Critical,
            "NAS",
            "核验缺少期望路径",
            &format!("任务 {task_id} 处于待确认但缺少 expected_nas_relative_path，数据错误"),
            Some(node_id),
            Some(&format!("NAS缺期望路径:{task_id}")),
        )
        .await?;
        return Ok(());
    };

    let evidence_error = match check.file {
        Some(evidence) => {
            let basic_err = reject_file_evidence(
                evidence,
                Some(expected_path),
                frozen.format.as_deref().unwrap_or(&task.format),
                state.config.nas.minimum_file_bytes,
            );
            if basic_err.is_some() {
                basic_err
            } else if let Some(exp_sha) = &frozen.sha {
                // 有固化 SHA：严格比对，不一致 = 严重冲突
                if !exp_sha.eq_ignore_ascii_case(&evidence.sha256) {
                    Some(format!(
                        "SHA-256 哈希冲突（期望 {exp_sha} vs 实际 {}）",
                        evidence.sha256
                    ))
                } else if let Some(exp_size) = frozen.size {
                    // 有固化大小：一并比对
                    if evidence.size_bytes != exp_size {
                        Some(format!(
                            "大小与期望不一致（期望 {exp_size} 字节，实际 {} 字节）",
                            evidence.size_bytes
                        ))
                    } else {
                        None
                    }
                } else {
                    // 缺大小：允许计算实际大小，但大小不是匹配依据——只有 SHA 匹配才判成功
                    None
                }
            } else {
                // 缺 SHA：绝不自动判完成（V4 第 12.3 / 23 节禁止项）
                Some("缺少固化的期望 SHA-256，不允许自动判完成".to_string())
            }
        }
        None => Some("NAS 未找到文件".to_string()),
    };

    if let Some(err_reason) = evidence_error {
        if err_reason.contains("NAS 未找到文件") {
            // 文件缺失：清理租约后放回待处理重下（第 12.5 节）
            sqlx::query(
                "UPDATE book_tasks SET status = $2, stage = '', \
                     attempts = GREATEST(attempts - 1, 0), \
                     stage_version = stage_version + 1, next_attempt_at = now(), \
                     last_error = $3, updated_at = now() WHERE id = $1",
            )
            .bind(task_id)
            .bind(TaskStatus::Pending.as_str())
            .bind(format!("NAS 核验未通过：{err_reason} (附注: {detail})"))
            .execute(&state.pool)
            .await?;
            store::task::sync_display_status(&state.pool, task.book_id).await?;
        } else {
            // 哈希冲突 / 大小不符 / 路径不符 / 缺 SHA：保持待确认并严重告警，
            // 禁止自动重试或覆盖，等待人工核验（第 12.5 节）。
            store::admin::raise_alert(
                &state.pool,
                platform_domain::AlertLevel::Critical,
                "NAS",
                "NAS 核验证据不一致",
                &format!("任务 {task_id} 核验发现：{err_reason}"),
                Some(node_id),
                Some(&format!("NAS核验不一致:{task_id}")),
            )
            .await?;
        }
    } else if let Some(evidence) = check.file {
        let mut tx = state.pool.begin().await?;
        store::catalog::record_book_file(
            &mut *tx,
            task.book_id,
            &evidence.format.to_ascii_lowercase(),
            &evidence.nas_relative_path,
            evidence.size_bytes,
            &evidence.sha256,
            Some(node_id),
        )
        .await?;
        sqlx::query(
            "UPDATE book_tasks SET status = $2, stage = $3, \
                 stage_version = stage_version + 1, nas_relative_path = $4, \
                 last_error = NULL, updated_at = now() WHERE id = $1",
        )
        .bind(task_id)
        .bind(TaskStatus::Completed.as_str())
        .bind("已完成")
        .bind(&evidence.nas_relative_path)
        .execute(&mut *tx)
        .await?;
        store::admin::bump_daily_stat(&mut *tx, 1, 0, 0, evidence.size_bytes, 0).await?;
        store::task::sync_display_status(&mut *tx, task.book_id).await?;
        tx.commit().await?;
    }

    state
        .events
        .publish("任务变更", serde_json::json!({ "任务": task_id }));
    Ok(())
}

/// 槽位状态上报（`SlotStatusReport`）：以 Worker 的自述为准刷新展示。
pub async fn slot_status_report(
    state: &AppState,
    node_id: Uuid,
    slot_index: i32,
    status: SlotStatus,
    session_id: Option<Uuid>,
    detail: &str,
) -> AppResult<()> {
    store::node::set_slot(&state.pool, node_id, slot_index, status, session_id, detail).await?;
    store::node::refresh_available_slots(&state.pool, node_id).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> ReportFacts {
        let execution = Uuid::new_v4();
        ReportFacts {
            lease_execution_id: Some(execution),
            reported_execution_id: execution,
            task_status: TaskStatus::Running,
            task_stage_version: 3,
            reported_stage_version: 3,
            provable_success: false,
        }
    }

    #[test]
    fn matching_lease_and_version_is_current() {
        assert_eq!(applicability(&facts()), Applicability::Current);
    }

    #[test]
    fn stale_stage_version_is_audit_only() {
        let mut f = facts();
        f.reported_stage_version = 2;
        assert!(matches!(
            applicability(&f),
            Applicability::AuditOnly("阶段版本已过期，仅留档")
        ));
    }

    #[test]
    fn superseded_execution_is_audit_only() {
        let mut f = facts();
        f.lease_execution_id = Some(Uuid::new_v4());
        assert!(matches!(
            applicability(&f),
            Applicability::AuditOnly("执行编号已被新的分配取代，仅留档")
        ));
    }

    #[test]
    fn late_provable_success_backfills_only_when_lease_is_released() {
        // V4 第 11.5 节收紧：任务已被新执行领取时禁止提交
        let mut f = facts();
        f.lease_execution_id = Some(Uuid::new_v4());
        f.provable_success = true;
        assert!(matches!(applicability(&f), Applicability::AuditOnly(_)));

        // 任务不再持有任何活动租约（待处理/待确认）时允许补记
        let mut f = facts();
        f.lease_execution_id = None;
        f.provable_success = true;
        assert_eq!(applicability(&f), Applicability::Backfill);
    }

    #[test]
    fn needs_confirm_accepts_either_verdict() {
        let mut f = facts();
        f.task_status = TaskStatus::NeedsConfirm;
        f.reported_stage_version = 1;
        assert_eq!(applicability(&f), Applicability::Backfill);
    }

    #[test]
    fn completed_task_never_applies_again() {
        // 一次 outbox 重放不该把完成数与当日统计加两遍
        let mut f = facts();
        f.task_status = TaskStatus::Completed;
        f.provable_success = true;
        assert!(matches!(applicability(&f), Applicability::AuditOnly(_)));
    }

    #[test]
    fn cancelled_task_is_not_revived_by_late_success() {
        let mut f = facts();
        f.task_status = TaskStatus::Cancelled;
        f.provable_success = true;
        assert!(matches!(applicability(&f), Applicability::AuditOnly(_)));
    }

    fn scheduler() -> SchedulerConfig {
        SchedulerConfig::default()
    }

    #[test]
    fn proxy_failure_refunds_the_attempt() {
        let attribution = FailureClass::ProxyFailure.attribution();
        let decision = decide_task(&attribution, 1, 3, &scheduler());
        assert_eq!(decision.status, TaskStatus::Pending);
        assert_eq!(decision.attempts_delta, -1);
        // 退还后仍是第 1 次尝试，因此退避取第一档
        assert_eq!(decision.retry_after, Duration::from_secs(60));
    }

    #[test]
    fn quota_exhaustion_never_exhausts_the_book() {
        // 账号额度耗尽反复出现时任务应当一直排队，而不是被判为这本书下不下来
        let attribution = FailureClass::AccountQuotaExhausted.attribution();
        let decision = decide_task(&attribution, 3, 3, &scheduler());
        assert_eq!(decision.status, TaskStatus::Pending);
        assert!(attribution.ends_session);
    }

    #[test]
    fn retryable_failure_exhausts_after_max_attempts() {
        let attribution = FailureClass::Retryable.attribution();
        let first = decide_task(&attribution, 1, 3, &scheduler());
        assert_eq!(first.status, TaskStatus::Pending);
        assert_eq!(first.attempts_delta, 0);
        assert_eq!(first.retry_after, Duration::from_secs(300));

        let last = decide_task(&attribution, 3, 3, &scheduler());
        assert_eq!(last.status, TaskStatus::Failed);
        assert_eq!(last.retry_after, Duration::ZERO);
    }

    #[test]
    fn not_found_skips_without_retry_delay() {
        let attribution = FailureClass::BookNotFound.attribution();
        let decision = decide_task(&attribution, 1, 3, &scheduler());
        assert_eq!(decision.status, TaskStatus::Skipped);
        assert_eq!(decision.retry_after, Duration::ZERO);
    }

    #[test]
    fn fatal_failure_fails_immediately() {
        let decision = decide_task(&FailureClass::Fatal.attribution(), 1, 3, &scheduler());
        assert_eq!(decision.status, TaskStatus::Failed);
    }

    #[test]
    fn uncertain_result_waits_for_nas_verification() {
        let decision = decide_task(&FailureClass::Uncertain.attribution(), 1, 3, &scheduler());
        assert_eq!(decision.status, TaskStatus::NeedsConfirm);
    }

    fn evidence(size: i64) -> FileEvidence {
        FileEvidence {
            nas_relative_path: "文件/000001-算法导论.pdf".to_string(),
            file_name: "000001-算法导论.pdf".to_string(),
            size_bytes: size,
            sha256: "a".repeat(64),
            format: "pdf".to_string(),
        }
    }

    #[test]
    fn tiny_file_is_rejected_as_error_page() {
        let reason = reject_file_evidence(&evidence(1024), None, "pdf", 32 * 1024);
        assert!(reason.unwrap().contains("文件过小"));
    }

    #[test]
    fn path_mismatch_is_rejected() {
        let reason = reject_file_evidence(
            &evidence(1_000_000),
            Some("文件/000002-别的书.pdf"),
            "pdf",
            32 * 1024,
        );
        assert!(reason.unwrap().contains("文件名不匹配"));
    }

    #[test]
    fn matching_evidence_is_accepted() {
        assert!(reject_file_evidence(
            &evidence(1_000_000),
            Some("文件/000001-算法导论.pdf"),
            "pdf",
            32 * 1024,
        )
        .is_none());
    }

    #[test]
    fn wrong_format_is_rejected() {
        let reason = reject_file_evidence(&evidence(1_000_000), None, "epub", 32 * 1024);
        assert!(reason.unwrap().contains("格式不匹配"));
    }

    #[test]
    fn empty_checksum_is_rejected() {
        let mut file = evidence(1_000_000);
        file.sha256 = String::new();
        assert!(reject_file_evidence(&file, None, "pdf", 32 * 1024).is_some());
    }
}
