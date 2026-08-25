//! 馆藏扫描配置（方案第 6 节）。
//!
//! 约束：
//! 1. `id` 在单个 Worker 内唯一且为稳定别名（如 portable_202608）；
//! 2. 路径在启动时规范化并验证实际存在；
//! 3. 扫描器严格只读，不跟随外部符号链接。

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 馆藏扫描根配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryConfig {
    /// 是否启用馆藏扫描。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 哈希计算并发数（默认 2）。
    #[serde(default = "default_hash_concurrency")]
    pub hash_concurrency: usize,
    /// 单批上报条数（默认 200）。
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    /// 单文件大小上限（字节，默认 2GB）。
    #[serde(default = "default_max_file_size")]
    pub max_file_size_bytes: u64,
    /// 是否跟随符号链接（默认 false）。
    #[serde(default)]
    pub follow_symlinks: bool,
    /// 登记的扫描根目录。
    #[serde(default)]
    pub roots: Vec<InventoryRootConfig>,
}

fn default_true() -> bool {
    true
}

fn default_hash_concurrency() -> usize {
    2
}

fn default_batch_size() -> usize {
    200
}

fn default_max_file_size() -> u64 {
    2 * 1024 * 1024 * 1024
}

impl Default for InventoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            hash_concurrency: default_hash_concurrency(),
            batch_size: default_batch_size(),
            max_file_size_bytes: default_max_file_size(),
            follow_symlinks: false,
            roots: Vec::new(),
        }
    }
}

/// 单个存储根目录配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryRootConfig {
    /// 稳定别名编号（不泄露本地绝对路径）。
    pub id: String,
    /// 本地真实物理路径。
    pub path: PathBuf,
    /// 存储类型（Local, NAS, S3, OSS）。
    #[serde(default = "default_backend_local")]
    pub backend: String,
    /// 展示名称。
    pub display_name: String,
    /// 是否只读（强制为 true）。
    #[serde(default = "default_true")]
    pub read_only: bool,
}

fn default_backend_local() -> String {
    "Local".to_string()
}

impl InventoryConfig {
    /// 校验配置。
    pub fn validate(&self) -> Result<()> {
        let mut seen_ids = std::collections::HashSet::new();
        for root in &self.roots {
            let id = root.id.trim();
            if id.is_empty() {
                bail!("馆藏扫描根目录 id 不能为空");
            }
            if !seen_ids.insert(id.to_string()) {
                bail!("馆藏扫描根目录 id 重复: {}", id);
            }
            if root.display_name.trim().is_empty() {
                bail!("馆藏扫描根目录 display_name 不能为空: {}", id);
            }
        }
        Ok(())
    }

    /// 查找指定 root_id 的物理路径。
    pub fn find_root(&self, root_id: &str) -> Option<&InventoryRootConfig> {
        self.roots.iter().find(|r| r.id == root_id)
    }
}
