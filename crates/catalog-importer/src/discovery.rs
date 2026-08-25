//! 书目文件发现与格式扫描。

use anyhow::Result;
use std::path::{Path, PathBuf};

/// 发现指定只读目录下的书目文件（CSV, TSV, TXT）。
pub fn discover_catalog_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !root.exists() {
        return Ok(files);
    }

    if root.is_file() {
        if is_supported_catalog_file(root) {
            files.push(root.to_path_buf());
        }
        return Ok(files);
    }

    walk_files(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                if !name.starts_with('.') && !name.starts_with('$') {
                    walk_files(&p, out)?;
                }
            } else if p.is_file() && is_supported_catalog_file(&p) {
                out.push(p);
            }
        }
    }
    Ok(())
}

fn is_supported_catalog_file(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    matches!(ext.as_str(), "csv" | "tsv" | "txt" | "xlsx")
}
