//! 导入断点与本地状态持久化（方案第 12 节）。

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// 导入断点文件。
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ImportCheckpointState {
    pub run_id: String,
    pub source: String,
    pub completed_files: Vec<String>,
    pub file_progress: HashMap<String, u64>, // file_path -> last_processed_row
    pub total_imported: u64,
    pub total_quarantined: u64,
    pub total_duplicate: u64,
}

impl ImportCheckpointState {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        let state = serde_json::from_str(&text)?;
        Ok(state)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }
}
