//! 馆藏扫描批量聚合与上报（方案第 7.3 节、第 9 节）。

use anyhow::{bail, Context, Result};
use std::sync::Arc;
use tokio::sync::{mpsc, Semaphore};
use tokio_util::sync::CancellationToken;

use super::config::InventoryConfig;
use super::hash_cache::InventoryHashCache;
use super::scanner::{inspect_file, walk_directory_files};
use platform_proto::{AssignInventoryScan, InventoryEvidenceBatch, InventoryScanProgress};

/// 扫描结果汇总。
#[derive(Debug, Default, Clone)]
pub struct InventoryScanSummary {
    pub discovered_count: u64,
    pub hashed_count: u64,
    pub sent_count: u64,
    pub skipped_count: u64,
    pub error_count: u64,
    pub status: String,
    pub error_message: Option<String>,
}

/// 执行一次馆藏目录扫描并分批上报。
pub async fn run_inventory_scan(
    config: &InventoryConfig,
    data_dir: &std::path::Path,
    assignment: AssignInventoryScan,
    batch_tx: mpsc::Sender<InventoryEvidenceBatch>,
    progress_tx: mpsc::Sender<InventoryScanProgress>,
    cancel_token: CancellationToken,
) -> Result<InventoryScanSummary> {
    let mut summary = InventoryScanSummary::default();

    // 1. 查找根目录配置
    let root_cfg = match config.find_root(&assignment.root_id) {
        Some(r) => r.clone(),
        None => {
            bail!("Worker 本地未配置根目录 ID: {}", assignment.root_id);
        }
    };

    if !root_cfg.path.exists() {
        bail!("配置的存储根目录路径不存在: {}", root_cfg.path.display());
    }

    let canonical_root = root_cfg.path.canonicalize().with_context(|| {
        format!(
            "解析存储根目录真实物理路径失败: {}",
            root_cfg.path.display()
        )
    })?;

    // 2. 初始化哈希缓存
    let hash_cache = InventoryHashCache::open(data_dir).ok();

    // 3. 收集所有待处理文件路径
    let follow_symlinks = config.follow_symlinks;
    let root_path_clone = root_cfg.path.clone();
    let canonical_root_clone = canonical_root.clone();

    let files = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        walk_directory_files(
            &root_path_clone,
            &canonical_root_clone,
            follow_symlinks,
            &mut out,
        )?;
        Ok::<_, anyhow::Error>(out)
    })
    .await??;

    summary.discovered_count = files.len() as u64;

    // 4. 并发哈希与批量打包
    let concurrency = if config.hash_concurrency == 0 {
        2
    } else {
        config.hash_concurrency
    };
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let batch_size = if assignment.batch_size == 0 {
        config.batch_size
    } else {
        assignment.batch_size as usize
    };

    let mut current_batch = Vec::with_capacity(batch_size);
    let mut batch_seq = 1u64;

    for file_path in files {
        if cancel_token.is_cancelled() {
            summary.status = "已取消".to_string();
            return Ok(summary);
        }

        let evidence_opt = match inspect_file(
            &root_cfg,
            &canonical_root,
            &file_path,
            &assignment.allowed_formats,
            hash_cache.as_ref(),
            &semaphore,
        )
        .await
        {
            Ok(ev) => ev,
            Err(err) => {
                tracing::warn!(path = %file_path.display(), error = %err, "扫描文件失败");
                summary.error_count += 1;
                continue;
            }
        };

        if let Some(evidence) = evidence_opt {
            summary.hashed_count += 1;
            let current_path = evidence.object_key.clone();
            current_batch.push(evidence);

            if current_batch.len() >= batch_size {
                let batch = InventoryEvidenceBatch {
                    scan_job_id: assignment.scan_job_id.clone(),
                    root_id: assignment.root_id.clone(),
                    batch_seq,
                    entries: std::mem::replace(&mut current_batch, Vec::with_capacity(batch_size)),
                    checkpoint_json: format!(r#"{{"last_seq": {}}}"#, batch_seq),
                };
                summary.sent_count += batch.entries.len() as u64;
                batch_seq += 1;

                if let Err(e) = batch_tx.send(batch).await {
                    let err_msg = e.to_string();
                    tracing::error!(error = %err_msg, "发送批次证据失败");
                    summary.status = "失败".to_string();
                    summary.error_message = Some(err_msg);
                    return Ok(summary);
                }

                let _ = progress_tx
                    .send(InventoryScanProgress {
                        scan_job_id: assignment.scan_job_id.clone(),
                        discovered_count: summary.discovered_count,
                        hashed_count: summary.hashed_count,
                        sent_count: summary.sent_count,
                        current_relative_path: current_path,
                        stage: "上报中".to_string(),
                    })
                    .await;
            }
        } else {
            summary.skipped_count += 1;
        }
    }

    // 5. 刷新最后一批
    if !current_batch.is_empty() {
        let batch = InventoryEvidenceBatch {
            scan_job_id: assignment.scan_job_id.clone(),
            root_id: assignment.root_id.clone(),
            batch_seq,
            entries: current_batch,
            checkpoint_json: format!(r#"{{"last_seq": {}}}"#, batch_seq),
        };
        summary.sent_count += batch.entries.len() as u64;
        let _ = batch_tx.send(batch).await;
    }

    summary.status = if summary.error_count > 0 {
        "部分失败".to_string()
    } else {
        "已完成".to_string()
    };

    Ok(summary)
}
