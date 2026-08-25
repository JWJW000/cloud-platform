//! 分批流式上传与对账协调器。

use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use super::checkpoint::ImportCheckpointState;

/// 上传单批次记录至 Master 服务端。
pub async fn process_catalog_file(
    endpoint: &str,
    token: Option<&str>,
    file_path: &Path,
    source: &str,
    batch_size: usize,
    checkpoint: &mut ImportCheckpointState,
    state_file: &Path,
) -> Result<()> {
    let file_str = file_path.to_string_lossy().to_string();
    if checkpoint.completed_files.contains(&file_str) {
        tracing::info!(file = %file_str, "文件已完全导入，跳过");
        return Ok(());
    }

    let file =
        File::open(file_path).with_context(|| format!("打开文件失败: {}", file_path.display()))?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();

    let mut current_row = 0u64;
    let start_row = *checkpoint.file_progress.get(&file_str).unwrap_or(&0);

    let mut batch_lines = Vec::with_capacity(batch_size);

    while reader.read_line(&mut line)? > 0 {
        current_row += 1;
        if current_row <= start_row {
            line.clear();
            continue;
        }

        let trimmed = line.trim();
        if !trimmed.is_empty() {
            batch_lines.push(trimmed.to_string());
        }
        line.clear();

        if batch_lines.len() >= batch_size {
            send_batch(endpoint, token, source, file_path, &batch_lines).await?;
            checkpoint.total_imported += batch_lines.len() as u64;
            checkpoint
                .file_progress
                .insert(file_str.clone(), current_row);
            let _ = checkpoint.save(state_file);
            batch_lines.clear();
            tracing::info!(file = %file_str, row = current_row, "已成功导入批次");
        }
    }

    if !batch_lines.is_empty() {
        send_batch(endpoint, token, source, file_path, &batch_lines).await?;
        checkpoint.total_imported += batch_lines.len() as u64;
        batch_lines.clear();
    }

    checkpoint.completed_files.push(file_str);
    checkpoint
        .file_progress
        .remove(&checkpoint.completed_files.last().unwrap().clone());
    let _ = checkpoint.save(state_file);

    Ok(())
}

async fn send_batch(
    _endpoint: &str,
    _token: Option<&str>,
    _source: &str,
    _file_path: &Path,
    _lines: &[String],
) -> Result<()> {
    // 模拟或调用 Master /api/catalog/imports/submit 批量接口
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    Ok(())
}
