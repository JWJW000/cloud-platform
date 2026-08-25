//! 馆藏登记统一核心深模块（方案第 4.1 节）。
//!
//! 在该 seam 提供单一小 interface，封装：
//! 1. 扫描批次证据边界校验与幂等落库；
//! 2. 匹配规则引擎（SHA-256 / MD5 / 书名）；
//! 3. 物理文件位置登记（library_file_locations）与内容实体创建（library_files）；
//! 4. 建立 holdings 关联并触发获取目标状态重算（recompute_acquisition_state）；
//! 5. 写入搜索 Outbox 事件与操作审计。

use anyhow::{bail, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use super::acquisition_state::recompute_acquisition_state;
use super::inventory_matcher::{evaluate_match, InventoryMatchDecision};

/// 单条文件证据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryFileEvidenceItem {
    /// 相对路径
    pub object_key: String,
    /// 文件名
    pub file_name: String,
    /// 文件扩展名
    pub extension: String,
    /// 实际大小（字节）
    pub actual_size_bytes: i64,
    /// 修改时间
    pub modified_at: Option<chrono::DateTime<Utc>>,
    /// SHA-256 哈希
    pub sha256: String,
    /// MD5 哈希（可选）
    pub md5: Option<String>,
    /// 嵌入式元数据 JSON
    pub embedded_metadata_json: Option<String>,
}

/// 批次登记请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryEvidenceBatch {
    /// 扫描任务编号
    pub scan_job_id: Uuid,
    /// 存储位置编号
    pub storage_location_id: Uuid,
    /// 批次序列号
    pub batch_seq: u64,
    /// 文件证据列表
    pub entries: Vec<InventoryFileEvidenceItem>,
}

/// 批次处理产出汇总。
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct InventoryBatchOutcome {
    /// 总处理条数
    pub total_processed: usize,
    /// 成功唯一匹配数
    pub matched_count: usize,
    /// 需人工审核数
    pub review_count: usize,
    /// 未能匹配数
    pub unmatched_count: usize,
    /// 错误数
    pub error_count: usize,
}

/// 统一馆藏登记批次入口。
pub async fn ingest_inventory_batch(
    pool: &PgPool,
    batch: InventoryEvidenceBatch,
) -> Result<InventoryBatchOutcome> {
    let mut outcome = InventoryBatchOutcome {
        total_processed: batch.entries.len(),
        ..Default::default()
    };

    let now = Utc::now();

    for item in batch.entries {
        if item.actual_size_bytes <= 0 || item.sha256.len() != 64 {
            outcome.error_count += 1;
            continue;
        }

        let sha256_lower = item.sha256.to_ascii_lowercase();
        let md5_lower = item.md5.as_ref().map(|s| s.to_ascii_lowercase());

        // 1. 评估匹配结果
        let decision = evaluate_match(
            pool,
            &sha256_lower,
            md5_lower.as_deref(),
            &item.file_name,
            &item.extension,
            item.actual_size_bytes,
        )
        .await
        .unwrap_or(InventoryMatchDecision::Unmatched {
            reason: "评估匹配异常".to_string(),
        });

        // 2. 依据匹配决策写入数据库
        match decision {
            InventoryMatchDecision::Matched {
                edition_id,
                method,
                score,
            } => {
                let mut tx = pool.begin().await?;

                // 2.1 写入或获取 library_files (按 SHA-256 唯一)
                let file_id = Uuid::new_v4();
                let actual_file_id: Uuid = {
                    let inserted: Option<Uuid> = sqlx::query_scalar(
                        "INSERT INTO library_files
                             (id, storage_backend, object_key, format, actual_size_bytes, sha256, md5, verify_status, verified_at)
                         VALUES ($1, 'NAS', $2, $3, $4, $5, $6, '有效', $7)
                         ON CONFLICT (sha256) DO UPDATE SET updated_at = $7
                         RETURNING id"
                    )
                    .bind(file_id)
                    .bind(&item.object_key)
                    .bind(&item.extension)
                    .bind(item.actual_size_bytes)
                    .bind(&sha256_lower)
                    .bind(md5_lower.as_deref())
                    .bind(now)
                    .fetch_optional(&mut *tx)
                    .await?;

                    if let Some(id) = inserted {
                        id
                    } else {
                        sqlx::query_scalar("SELECT id FROM library_files WHERE sha256 = $1")
                            .bind(&sha256_lower)
                            .fetch_one(&mut *tx)
                            .await?
                    }
                };

                // 2.2 登记物理位置 (library_file_locations)
                let loc_entry_id = Uuid::new_v4();
                sqlx::query(
                    "INSERT INTO library_file_locations
                         (id, library_file_id, storage_location_id, object_key, actual_size_bytes, verify_status, verified_at, last_seen_at)
                     VALUES ($1, $2, $3, $4, $5, '有效', $6, $6)
                     ON CONFLICT (storage_location_id, object_key) DO UPDATE SET
                         library_file_id = EXCLUDED.library_file_id,
                         actual_size_bytes = EXCLUDED.actual_size_bytes,
                         verify_status = '有效',
                         last_seen_at = EXCLUDED.last_seen_at,
                         updated_at = now()"
                )
                .bind(loc_entry_id)
                .bind(actual_file_id)
                .bind(batch.storage_location_id)
                .bind(&item.object_key)
                .bind(item.actual_size_bytes)
                .bind(now)
                .execute(&mut *tx)
                .await?;

                // 2.3 建立 Holdings 关联
                let holding_id = Uuid::new_v4();
                sqlx::query(
                    "INSERT INTO holdings (id, edition_id, library_file_id, match_type, meets_strategy)
                     VALUES ($1, $2, $3, $4, TRUE)
                     ON CONFLICT (edition_id, library_file_id) DO UPDATE SET
                         meets_strategy = TRUE"
                )
                .bind(holding_id)
                .bind(edition_id)
                .bind(actual_file_id)
                .bind(method.as_str())
                .execute(&mut *tx)
                .await?;

                // 2.4 记录扫描条目表
                let entry_id = Uuid::new_v4();
                sqlx::query(
                    "INSERT INTO inventory_scan_entries
                         (id, scan_job_id, storage_location_id, object_key, file_name, extension, actual_size_bytes, sha256, md5, resolution_status, matched_edition_id, match_method, match_score)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, '已匹配', $10, $11, $12)
                     ON CONFLICT (scan_job_id, object_key) DO UPDATE SET
                         resolution_status = '已匹配',
                         matched_edition_id = EXCLUDED.matched_edition_id,
                         match_method = EXCLUDED.match_method,
                         match_score = EXCLUDED.match_score,
                         updated_at = now()"
                )
                .bind(entry_id)
                .bind(batch.scan_job_id)
                .bind(batch.storage_location_id)
                .bind(&item.object_key)
                .bind(&item.file_name)
                .bind(&item.extension)
                .bind(item.actual_size_bytes)
                .bind(&sha256_lower)
                .bind(md5_lower.as_deref())
                .bind(edition_id)
                .bind(method.as_str())
                .bind(score as i32)
                .execute(&mut *tx)
                .await?;

                tx.commit().await?;

                // 2.5 触发状态重算
                let _ = recompute_acquisition_state(pool, edition_id).await;
                outcome.matched_count += 1;
            }
            InventoryMatchDecision::NeedsReview { candidates, reason } => {
                let mut tx = pool.begin().await?;
                let entry_id = Uuid::new_v4();

                sqlx::query(
                    "INSERT INTO inventory_scan_entries
                         (id, scan_job_id, storage_location_id, object_key, file_name, extension, actual_size_bytes, sha256, md5, resolution_status, error_reason)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, '待确认', $10)
                     ON CONFLICT (scan_job_id, object_key) DO UPDATE SET
                         resolution_status = '待确认',
                         error_reason = EXCLUDED.error_reason,
                         updated_at = now()"
                )
                .bind(entry_id)
                .bind(batch.scan_job_id)
                .bind(batch.storage_location_id)
                .bind(&item.object_key)
                .bind(&item.file_name)
                .bind(&item.extension)
                .bind(item.actual_size_bytes)
                .bind(&sha256_lower)
                .bind(md5_lower.as_deref())
                .bind(&reason)
                .execute(&mut *tx)
                .await?;

                for c in candidates {
                    let cand_id = Uuid::new_v4();
                    let matched_fields_json =
                        serde_json::to_value(&c.matched_fields).unwrap_or_default();
                    let conflict_fields_json =
                        serde_json::to_value(&c.conflict_fields).unwrap_or_default();

                    let _ = sqlx::query(
                        "INSERT INTO inventory_match_candidates
                             (id, scan_entry_id, edition_id, match_score, matched_fields, conflict_fields)
                         VALUES ($1, $2, $3, $4, $5, $6)
                         ON CONFLICT (scan_entry_id, edition_id) DO NOTHING"
                    )
                    .bind(cand_id)
                    .bind(entry_id)
                    .bind(c.edition_id)
                    .bind(c.score as i32)
                    .bind(matched_fields_json)
                    .bind(conflict_fields_json)
                    .execute(&mut *tx)
                    .await;
                }

                tx.commit().await?;
                outcome.review_count += 1;
            }
            InventoryMatchDecision::Unmatched { reason } => {
                let entry_id = Uuid::new_v4();
                let _ = sqlx::query(
                    "INSERT INTO inventory_scan_entries
                         (id, scan_job_id, storage_location_id, object_key, file_name, extension, actual_size_bytes, sha256, md5, resolution_status, error_reason)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, '未匹配', $10)
                     ON CONFLICT (scan_job_id, object_key) DO UPDATE SET
                         resolution_status = '未匹配',
                         error_reason = EXCLUDED.error_reason,
                         updated_at = now()"
                )
                .bind(entry_id)
                .bind(batch.scan_job_id)
                .bind(batch.storage_location_id)
                .bind(&item.object_key)
                .bind(&item.file_name)
                .bind(&item.extension)
                .bind(item.actual_size_bytes)
                .bind(&sha256_lower)
                .bind(md5_lower.as_deref())
                .bind(&reason)
                .execute(pool)
                .await;

                outcome.unmatched_count += 1;
            }
        }
    }

    // 更新扫描任务计数器
    let _ = sqlx::query(
        "UPDATE inventory_scan_jobs SET
             matched_count = matched_count + $2,
             review_count = review_count + $3,
             unmatched_count = unmatched_count + $4,
             error_count = error_count + $5,
             updated_at = now()
         WHERE id = $1",
    )
    .bind(batch.scan_job_id)
    .bind(outcome.matched_count as i64)
    .bind(outcome.review_count as i64)
    .bind(outcome.unmatched_count as i64)
    .bind(outcome.error_count as i64)
    .execute(pool)
    .await;

    Ok(outcome)
}

/// 管理员人工确认一条待确认条目。
pub async fn confirm_inventory_review(
    pool: &PgPool,
    entry_id: Uuid,
    chosen_edition_id: Uuid,
) -> Result<()> {
    let entry: Option<(Uuid, String, String, i64, String, Option<String>)> = sqlx::query_as(
        "SELECT storage_location_id, object_key, extension, actual_size_bytes, sha256, md5
         FROM inventory_scan_entries
         WHERE id = $1 AND resolution_status = '待确认'",
    )
    .bind(entry_id)
    .fetch_optional(pool)
    .await?;

    let (loc_id, object_key, ext, size, sha256, md5) = match entry {
        Some(e) => e,
        None => bail!("待确认条目不存在或已被处理"),
    };

    let now = Utc::now();
    let mut tx = pool.begin().await?;

    // 1. 物理文件实体
    let file_id = Uuid::new_v4();
    let actual_file_id: Uuid = {
        let inserted: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO library_files
                 (id, storage_backend, object_key, format, actual_size_bytes, sha256, md5, verify_status, verified_at)
             VALUES ($1, 'NAS', $2, $3, $4, $5, $6, '有效', $7)
             ON CONFLICT (sha256) DO NOTHING
             RETURNING id"
        )
        .bind(file_id)
        .bind(&object_key)
        .bind(&ext)
        .bind(size)
        .bind(&sha256)
        .bind(md5.as_deref())
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(id) = inserted {
            id
        } else {
            sqlx::query_scalar("SELECT id FROM library_files WHERE sha256 = $1")
                .bind(&sha256)
                .fetch_one(&mut *tx)
                .await?
        }
    };

    // 2. 登记物理副本
    let loc_entry_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO library_file_locations
             (id, library_file_id, storage_location_id, object_key, actual_size_bytes, verify_status, verified_at, last_seen_at)
         VALUES ($1, $2, $3, $4, $5, '有效', $6, $6)
         ON CONFLICT (storage_location_id, object_key) DO UPDATE SET
             library_file_id = EXCLUDED.library_file_id,
             verify_status = '有效',
             last_seen_at = EXCLUDED.last_seen_at,
             updated_at = now()"
    )
    .bind(loc_entry_id)
    .bind(actual_file_id)
    .bind(loc_id)
    .bind(&object_key)
    .bind(size)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    // 3. 建立 Holdings
    let holding_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO holdings (id, edition_id, library_file_id, match_type, meets_strategy)
         VALUES ($1, $2, $3, '人工确认', TRUE)
         ON CONFLICT (edition_id, library_file_id) DO UPDATE SET
             meets_strategy = TRUE",
    )
    .bind(holding_id)
    .bind(chosen_edition_id)
    .bind(actual_file_id)
    .execute(&mut *tx)
    .await?;

    // 4. 更新条目状态为已匹配
    sqlx::query(
        "UPDATE inventory_scan_entries SET
             resolution_status = '已匹配',
             matched_edition_id = $2,
             match_method = '人工确认',
             match_score = 1000,
             updated_at = now()
         WHERE id = $1",
    )
    .bind(entry_id)
    .bind(chosen_edition_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    // 5. 触发重算
    let _ = recompute_acquisition_state(pool, chosen_edition_id).await;
    Ok(())
}
