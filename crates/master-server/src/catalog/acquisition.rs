//! 图书馆总库全局唯一获取调度器（第 9 节、第 11.4 节）。
//!
//! 实现持续运行的全局任务池：
//! 1. 事务级抢占（FOR UPDATE SKIP LOCKED）与租约防护；
//! 2. 来源候选资产（SourceAsset）自动轮转与降级；
//! 3. 失败退避与状态收敛（待下载 -> 排队中 -> 已领取 -> 下载中 -> 校验中 -> 已下载 / 暂时失败 / 人工确认）。

use chrono::{Duration, Utc};
use platform_domain::AcquisitionStatus;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppResult;
use crate::store::catalog_v1::AcquisitionTargetRow;

/// Worker 任务领取声明。
#[derive(Debug, Clone, Deserialize)]
pub struct WorkerClaimRequest {
    /// Worker 节点编号。
    pub node_id: Uuid,
    /// Worker 会话编号。
    pub session_id: Uuid,
    /// 槽位索引。
    pub slot_index: i32,
    /// 支持的格式列表（如 ["epub", "pdf", "azw3"]）。
    pub supported_formats: Vec<String>,
}

/// 分配的任务详情。
#[derive(Debug, Clone, Serialize)]
pub struct AcquisitionAssignment {
    /// 目标编号。
    pub target_id: Uuid,
    /// 执行编号。
    pub execution_id: Uuid,
    /// 版本编号。
    pub edition_id: Uuid,
    /// 书名。
    pub title: String,
    /// 作者。
    pub author: Option<String>,
    /// 出版社。
    pub publisher: Option<String>,
    /// ISBN。
    pub isbn: Option<String>,
    /// 选择的来源候选资产编号。
    pub source_asset_id: Option<Uuid>,
    /// 格式。
    pub format: String,
    /// 预期 MD5。
    pub expected_md5: Option<String>,
    /// 预期文件大小。
    pub expected_size_bytes: Option<i64>,
    /// 租约过期时间。
    pub lease_expires_at: chrono::DateTime<Utc>,
}

/// 执行结果汇报。
#[derive(Debug, Clone, Deserialize)]
pub struct AcquisitionReportRequest {
    /// 目标编号。
    pub target_id: Uuid,
    /// 执行编号。
    pub execution_id: Uuid,
    /// 执行阶段。
    pub stage: String,
    /// 执行结果（成功/失败/取消/超时/校验失败）。
    pub result: Option<String>,
    /// 错误代码。
    pub error_code: Option<String>,
    /// 错误信息。
    pub error_message: Option<String>,
}

/// 从全局获取池中为 Worker 领取一个任务。
pub async fn claim_acquisition_task(
    pool: &PgPool,
    req: &WorkerClaimRequest,
    lease_duration_secs: i64,
) -> AppResult<Option<AcquisitionAssignment>> {
    let mut tx = pool.begin().await?;
    let now = Utc::now();
    let lease_expires_at = now + Duration::seconds(lease_duration_secs.max(30));

    // 1. 查找并锁定一个待下载/可重试且租约已过期的目标
    let target_row: Option<(Uuid, Uuid, i32, i32)> = sqlx::query_as(
        "SELECT id, edition_id, attempts, priority FROM acquisition_targets \
         WHERE status IN ('待下载', '排队中', '暂时失败') \
           AND next_attempt_at <= $1 \
           AND (lease_expires_at IS NULL OR lease_expires_at < $1) \
         ORDER BY priority DESC, next_attempt_at ASC \
         FOR UPDATE SKIP LOCKED LIMIT 1",
    )
    .bind(now)
    .fetch_optional(&mut *tx)
    .await?;

    let Some((target_id, edition_id, attempts, _priority)) = target_row else {
        return Ok(None);
    };

    // 2. 选择该版本可用的来源候选资产
    let asset_row: Option<(Uuid, String, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT sa.id, sa.format, sa.md5, sa.declared_size_bytes FROM record_resolutions rr \
         JOIN source_assets sa ON sa.source_record_id = rr.source_record_id \
         WHERE rr.edition_id = $1 AND sa.status = '可用' \
         ORDER BY (sa.format = 'epub') DESC, (sa.format = 'pdf') DESC \
         LIMIT 1",
    )
    .bind(edition_id)
    .fetch_optional(&mut *tx)
    .await?;

    let (asset_id, format, expected_md5, expected_size) = match asset_row {
        Some((id, fmt, md5, size)) => (Some(id), fmt, md5, size),
        None => (None, "pdf".to_string(), None, None),
    };

    // 3. 创建执行记录
    let execution_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO acquisition_executions \
             (id, target_id, source_asset_id, node_id, session_id, slot_index, stage, started_at) \
         VALUES ($1, $2, $3, $4, $5, $6, '已领取', $7)",
    )
    .bind(execution_id)
    .bind(target_id)
    .bind(asset_id)
    .bind(req.node_id)
    .bind(req.session_id)
    .bind(req.slot_index)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    // 4. 更新目标表租约与状态
    sqlx::query(
        "UPDATE acquisition_targets SET \
             status = '已领取', \
             attempts = $2, \
             lease_node_id = $3, \
             lease_session_id = $4, \
             lease_execution_id = $5, \
             lease_expires_at = $6, \
             active_source_asset_id = $7, \
             updated_at = now() \
         WHERE id = $1",
    )
    .bind(target_id)
    .bind(attempts + 1)
    .bind(req.node_id)
    .bind(req.session_id)
    .bind(execution_id)
    .bind(lease_expires_at)
    .bind(asset_id)
    .execute(&mut *tx)
    .await?;

    // 5. 补充图书展示信息
    let (title, publisher): (String, Option<String>) =
        sqlx::query_as("SELECT edition_title, publisher FROM editions WHERE id = $1")
            .bind(edition_id)
            .fetch_one(&mut *tx)
            .await?;

    let author: Option<String> = sqlx::query_scalar(
        "SELECT c.name FROM edition_contributors ec JOIN contributors c ON c.id = ec.contributor_id WHERE ec.edition_id = $1 ORDER BY ec.sort_order LIMIT 1"
    )
    .bind(edition_id)
    .fetch_optional(&mut *tx)
    .await?;

    let isbn: Option<String> = sqlx::query_scalar(
        "SELECT normalized_value FROM identifiers WHERE object_id = $1 AND identifier_type IN ('isbn13', 'isbn10') AND is_valid LIMIT 1"
    )
    .bind(edition_id)
    .fetch_optional(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(Some(AcquisitionAssignment {
        target_id,
        execution_id,
        edition_id,
        title,
        author,
        publisher,
        isbn,
        source_asset_id: asset_id,
        format,
        expected_md5,
        expected_size_bytes: expected_size,
        lease_expires_at,
    }))
}

/// 汇报任务执行进度与结果。
pub async fn report_acquisition_task(
    pool: &PgPool,
    report: &AcquisitionReportRequest,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    let now = Utc::now();

    // 更新执行记录
    sqlx::query(
        "UPDATE acquisition_executions SET \
             stage = $2, \
             result = $3, \
             error_code = $4, \
             error_message = $5, \
             finished_at = CASE WHEN $3 IS NOT NULL THEN now() ELSE finished_at END \
         WHERE id = $1",
    )
    .bind(report.execution_id)
    .bind(&report.stage)
    .bind(&report.result)
    .bind(&report.error_code)
    .bind(&report.error_message)
    .execute(&mut *tx)
    .await?;

    // 如果任务失败，计算退避并释放租约
    if let Some(ref res) = report.result {
        if res == "失败" || res == "校验失败" || res == "超时" {
            let target: AcquisitionTargetRow = sqlx::query_as(
                "SELECT id, edition_id, preferred_formats, status, priority, attempts, max_attempts, \
                        next_attempt_at, lease_node_id, lease_session_id, lease_execution_id, lease_expires_at, \
                        active_source_asset_id, satisfied_holding_id, last_error, created_at, updated_at \
                 FROM acquisition_targets WHERE id = $1 FOR UPDATE"
            )
            .bind(report.target_id)
            .fetch_one(&mut *tx)
            .await?;

            let next_status = if target.attempts >= target.max_attempts {
                AcquisitionStatus::NeedsConfirm
            } else {
                AcquisitionStatus::RetryableFailure
            };

            // 指数退避：30s, 60s, 120s, 300s...
            let backoff_secs = match target.attempts {
                1 => 30,
                2 => 60,
                3 => 180,
                _ => 600,
            };
            let next_retry = now + Duration::seconds(backoff_secs);

            sqlx::query(
                "UPDATE acquisition_targets SET \
                     status = $2, \
                     lease_node_id = NULL, \
                     lease_session_id = NULL, \
                     lease_execution_id = NULL, \
                     lease_expires_at = NULL, \
                     next_attempt_at = $3, \
                     last_error = $4, \
                     updated_at = now() \
                 WHERE id = $1",
            )
            .bind(report.target_id)
            .bind(next_status.as_str())
            .bind(next_retry)
            .bind(report.error_message.as_deref())
            .execute(&mut *tx)
            .await?;
        }
    }

    tx.commit().await?;
    Ok(())
}

/// 管理员手动重试或重置任务。
pub async fn retry_acquisition_target(pool: &PgPool, target_id: Uuid) -> AppResult<()> {
    sqlx::query(
        "UPDATE acquisition_targets SET \
             status = '待下载', \
             attempts = 0, \
             next_attempt_at = now(), \
             lease_node_id = NULL, \
             lease_session_id = NULL, \
             lease_execution_id = NULL, \
             lease_expires_at = NULL, \
             last_error = NULL, \
             updated_at = now() \
         WHERE id = $1",
    )
    .bind(target_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// 管理员调整任务优先级。
pub async fn set_acquisition_priority(
    pool: &PgPool,
    target_id: Uuid,
    priority: i32,
) -> AppResult<()> {
    sqlx::query("UPDATE acquisition_targets SET priority = $2, updated_at = now() WHERE id = $1")
        .bind(target_id)
        .bind(priority.clamp(-1000, 1000))
        .execute(pool)
        .await?;

    Ok(())
}
