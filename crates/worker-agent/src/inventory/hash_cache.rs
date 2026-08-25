//! 馆藏扫描 SQLite 哈希缓存（方案第 7.2 节）。
//!
//! 在 Worker 数据目录下建立本地哈希缓存库（如 `data/inventory_hash_cache.db`），
//! 依据 `(root_id, relative_path, file_size, modified_timestamp)` 缓存 SHA-256 和 MD5，
//! 避免大目录或移动硬盘重复扫描时产生海量无谓磁盘 I/O。

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;

/// 缓存条目。
#[derive(Debug, Clone)]
pub struct CachedFileHash {
    pub sha256: String,
    pub md5: Option<String>,
}

/// 本地哈希缓存。
pub struct InventoryHashCache {
    conn: Mutex<Connection>,
}

impl InventoryHashCache {
    /// 打开或创建哈希缓存。
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("创建数据目录失败: {}", data_dir.display()))?;
        let db_path = data_dir.join("inventory_hash_cache.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("打开哈希缓存 SQLite 失败: {}", db_path.display()))?;

        // 优化 SQLite 并发读写
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;
             CREATE TABLE IF NOT EXISTS file_hash_cache (
                 root_id             TEXT NOT NULL,
                 relative_path       TEXT NOT NULL,
                 file_size           INTEGER NOT NULL,
                 modified_timestamp  INTEGER NOT NULL,
                 sha256              TEXT NOT NULL,
                 md5                 TEXT,
                 cached_at           INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
                 PRIMARY KEY (root_id, relative_path)
             );
             CREATE INDEX IF NOT EXISTS idx_file_hash_lookup
                 ON file_hash_cache (root_id, relative_path, file_size, modified_timestamp);",
        )?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// 查询缓存中的哈希。若大小或修改时间不一致则返回 None。
    pub fn get(
        &self,
        root_id: &str,
        relative_path: &str,
        file_size: u64,
        modified_timestamp: i64,
    ) -> Option<CachedFileHash> {
        let conn = self.conn.lock().ok()?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT sha256, md5 FROM file_hash_cache
                 WHERE root_id = ?1 AND relative_path = ?2
                   AND file_size = ?3 AND modified_timestamp = ?4",
            )
            .ok()?;

        stmt.query_row(
            params![root_id, relative_path, file_size as i64, modified_timestamp],
            |row| {
                Ok(CachedFileHash {
                    sha256: row.get(0)?,
                    md5: row.get(1)?,
                })
            },
        )
        .ok()
    }

    /// 写入或更新哈希缓存。
    pub fn put(
        &self,
        root_id: &str,
        relative_path: &str,
        file_size: u64,
        modified_timestamp: i64,
        sha256: &str,
        md5: Option<&str>,
    ) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Mutex lock error: {e}"))?;
        conn.execute(
            "INSERT INTO file_hash_cache
                 (root_id, relative_path, file_size, modified_timestamp, sha256, md5, cached_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, strftime('%s', 'now'))
             ON CONFLICT (root_id, relative_path) DO UPDATE SET
                 file_size = excluded.file_size,
                 modified_timestamp = excluded.modified_timestamp,
                 sha256 = excluded.sha256,
                 md5 = excluded.md5,
                 cached_at = strftime('%s', 'now')",
            params![
                root_id,
                relative_path,
                file_size as i64,
                modified_timestamp,
                sha256,
                md5
            ],
        )?;
        Ok(())
    }
}
