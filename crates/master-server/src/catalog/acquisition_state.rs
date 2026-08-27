//! 馆藏获取状态统一重算模块（方案第 5.5 节）。
//!
//! 将“已下载”从一次性写值转为可重算事实：
//! - edition 至少有一个 meets_strategy=true 的 holding，且该 holding 指向的 library_file 至少存在一个有效/在线的 location => acquisition_target.status = '已下载'
//! - 曾经有馆藏但所有 location 均离线 => acquisition_target.status = '暂时失败' / 页面显示“文件离线”
//! - 确认损坏或丢失 => 重算回 '待下载'

use anyhow::Result;
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

/// 重算指定 edition_id 的获取目标状态。
pub async fn recompute_acquisition_state(
    pool: &PgPool,
    edition_id: Uuid,
) -> Result<Option<String>> {
    let mut tx = pool.begin().await?;

    // 1. 查询该 edition 是否拥有满足策略且有物理副本的馆藏
    let row: Option<(Uuid, i64, i64)> = sqlx::query_as(
        "SELECT h.id as holding_id,
                COUNT(lfl.id) as total_locations,
                COUNT(lfl.id) FILTER (WHERE lfl.verify_status = '有效') as valid_locations
         FROM holdings h
         JOIN library_files lf ON h.library_file_id = lf.id
         LEFT JOIN library_file_locations lfl ON lf.id = lfl.library_file_id
         WHERE h.edition_id = $1 AND h.meets_strategy = TRUE
         GROUP BY h.id, h.created_at
         ORDER BY valid_locations DESC, h.created_at ASC
         LIMIT 1",
    )
    .bind(edition_id)
    .fetch_optional(&mut *tx)
    .await?;

    let (new_status, satisfied_holding_id) = match row {
        Some((holding_id, _total_loc, valid_loc)) if valid_loc > 0 => ("已下载", Some(holding_id)),
        Some((holding_id, total_loc, _valid_loc)) if total_loc > 0 => {
            ("暂时失败", Some(holding_id)) // 文件离线/待校验
        }
        _ => ("待下载", None),
    };

    // 2. 更新 acquisition_targets
    sqlx::query(
        "UPDATE acquisition_targets SET
             status = $2,
             satisfied_holding_id = $3,
             updated_at = $4
         WHERE edition_id = $1",
    )
    .bind(edition_id)
    .bind(new_status)
    .bind(satisfied_holding_id)
    .bind(Utc::now())
    .execute(&mut *tx)
    .await?;

    if new_status == "待下载" {
        sqlx::query(
            "UPDATE book_tasks SET status = '待处理', attempts = 0, next_attempt_at = now(), \
                 stage = '', stage_version = stage_version + 1, cancel_requested = FALSE, \
                 lease_node_id = NULL, lease_session_id = NULL, lease_execution_id = NULL, \
                 lease_expires_at = NULL, last_error = NULL, updated_at = now() \
             WHERE id IN (SELECT id FROM acquisition_targets WHERE edition_id = $1)",
        )
        .bind(edition_id)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(Some(new_status.to_string()))
}
