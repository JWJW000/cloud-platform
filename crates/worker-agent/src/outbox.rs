//! Worker 本地 SQLite 存储：Outbox 队列 + 执行现场（第 5.3、10.3 节）。
//!
//! 这里存两类东西，它们回答两个不同的问题：
//!
//! - **Outbox**：「这条事件 Master 收到了吗？」可靠事件先落盘再上网，
//!   收到 `EventAck.accepted=true` 才删除。断网五分钟后完成的下载，
//!   结果就是靠这张表活下来的。
//! - **执行现场（`execution_state`）**：「进程重启前我干到哪一步了？」
//!   没有它，Worker 重启后只能把已经下完、甚至已经落到 NAS 的书重下一遍。
//!
//! 两张表都以「业务主键」而不是自增 id 去重：Outbox 用 `event_id`，
//! 现场用 `execution_id`。于是同一条记录写两次是安全的，
//! 而这正是崩溃恢复路径上最常发生的事。

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use platform_proto::v1 as pb;
use platform_proto::v1::ExecutionStage;
use prost::Message;
use rusqlite::{params, Connection, OptionalExtension};

/// 建表语句。`open` 与 `memory` 共用，避免两处 schema 漂移。
const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS outbox (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        event_id      TEXT UNIQUE NOT NULL,
        payload_bytes BLOB NOT NULL,
        created_at    TEXT NOT NULL,
        -- 待发送 / 已发送。已发送仍留在表里，直到 EventAck 到达才删除：
        -- 「发出去了」不等于「对方处理了」。
        state         TEXT NOT NULL DEFAULT '待发送',
        attempts      INTEGER NOT NULL DEFAULT 0,
        last_sent_at  TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_outbox_event_id ON outbox(event_id);

    CREATE TABLE IF NOT EXISTS execution_state (
        execution_id      TEXT PRIMARY KEY,
        slot_index        INTEGER NOT NULL,
        session_id        TEXT NOT NULL,
        task_id           TEXT NOT NULL,
        stage_version     INTEGER NOT NULL,
        -- 技术阶段枚举值（V4 第 10.1 节）：唯一裁决依据，禁止自由字符串比较
        stage_enum        INTEGER NOT NULL DEFAULT 0,
        -- 中文展示阶段（派生展示，落库避免重复计算）
        task_status       TEXT NOT NULL,
        staging_dir       TEXT NOT NULL DEFAULT '',
        nas_relative_path TEXT NOT NULL DEFAULT '',
        source_sha256     TEXT NOT NULL DEFAULT '',
        -- V4 第 10.2 节补充字段：完整恢复所需现场
        format            TEXT NOT NULL DEFAULT '',
        local_file_path   TEXT NOT NULL DEFAULT '',
        source_size_bytes INTEGER NOT NULL DEFAULT 0,
        result_event_id   TEXT NOT NULL DEFAULT '',
        node_id           TEXT NOT NULL DEFAULT '',
        created_at        TEXT NOT NULL DEFAULT '',
        updated_at        TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_execution_state_session ON execution_state(session_id);
";

/// 已确认的事件结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcknowledgedEvent {
    /// 事件编号。
    pub event_id: String,
    /// 对应的执行编号（若有）。
    pub execution_id: Option<String>,
}

/// 本地事件存储项。
#[derive(Debug, Clone)]
pub struct OutboxItem {
    /// 自增序号，决定补报顺序。
    pub id: i64,
    /// 业务幂等键。
    pub event_id: String,
    /// prost 编码后的 `WorkerMessage`。
    pub payload_bytes: Vec<u8>,
    /// 入队时间。
    pub created_at: String,
    /// 已尝试发送次数。
    pub attempts: i64,
}

/// 一次任务执行的本地现场（第 10.3 节、V4 第 10.2 节）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionState {
    /// 所属槽位。
    pub slot_index: u32,
    /// 会话编号。
    pub session_id: String,
    /// 任务编号。
    pub task_id: String,
    /// 执行编号，本表主键。
    pub execution_id: String,
    /// Master 下发的租约世代，重连对账时必须原样送回。
    pub stage_version: u32,
    /// 技术阶段枚举：裁决的唯一依据（V4 第 10.1 节）。
    pub stage: ExecutionStage,
    /// 中文展示阶段（派生自 stage，供日志与 UI）。
    pub task_status: String,
    /// 本任务独占的暂存目录。
    pub staging_dir: String,
    /// NAS 相对路径。
    pub nas_relative_path: String,
    /// 本地源文件 SHA-256，用于重启后判断是否已经算过。
    pub source_sha256: String,
    /// 任务格式（pdf / epub）。
    pub format: String,
    /// 本地文件完成后的源文件完整路径（可证明恢复）。
    pub local_file_path: String,
    /// 本地源文件字节数。
    pub source_size_bytes: i64,
    /// 结果已入 Outbox 时的事件编号（定向重放，禁止笼统重放前 N 条）。
    pub result_event_id: String,
    /// 所属节点编号。
    pub node_id: String,
    /// 创建时间（RFC 3339）。
    pub created_at: String,
}

/// 把技术阶段枚举转成中文展示值（唯一来源见 platform_proto::display）。
fn stage_display(stage: ExecutionStage) -> String {
    stage.display_name().to_string()
}

/// 本地 SQLite 存储管理器。
#[derive(Clone)]
pub struct LocalStore {
    conn: Arc<Mutex<Connection>>,
}

impl LocalStore {
    /// 打开或创建本地 SQLite 数据库。
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("打开本地 SQLite 失败：{}", path.display()))?;

        // WAL + busy_timeout：Worker 里多个槽位协程会并发写这两张表。
        // 没有 busy_timeout 时并发写会直接返回 SQLITE_BUSY，
        // 而那一刻丢掉的可能正好是一条 TaskResult。
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;",
        )?;
        conn.execute_batch(SCHEMA)?;
        Self::migrate(&conn)?;
        restrict_permissions(path);

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// 内存数据库（测试用）。
    pub fn memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// 为旧版本数据库补齐新增列。
    ///
    /// 早期版本的 `outbox` 只有四列。直接 `ALTER TABLE` 并忽略「列已存在」错误，
    /// 比维护一张版本号表更省事，因为这里的迁移永远是「加一列，带默认值」。
    fn migrate(conn: &Connection) -> Result<()> {
        for stmt in [
            "ALTER TABLE outbox ADD COLUMN state TEXT NOT NULL DEFAULT '待发送'",
            "ALTER TABLE outbox ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE outbox ADD COLUMN last_sent_at TEXT",
            // V4 第 10.2 节：执行现场补充字段
            "ALTER TABLE execution_state ADD COLUMN stage_enum INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE execution_state ADD COLUMN format TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE execution_state ADD COLUMN local_file_path TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE execution_state ADD COLUMN source_size_bytes INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE execution_state ADD COLUMN result_event_id TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE execution_state ADD COLUMN node_id TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE execution_state ADD COLUMN created_at TEXT NOT NULL DEFAULT ''",
        ] {
            match conn.execute(stmt, []) {
                Ok(_) => {}
                Err(err) if err.to_string().contains("duplicate column name") => {}
                Err(err) => return Err(err.into()),
            }
        }
        // 历史中文阶段回填为枚举值（可审计：只处理能识别的中文值，未知值保持 0）
        let known: &[(&str, i32)] = &[
            ("已接受", ExecutionStage::Accepted as i32),
            ("搜索中", ExecutionStage::Searching as i32),
            ("下载中", ExecutionStage::Downloading as i32),
            ("本地文件完成", ExecutionStage::LocalFileReady as i32),
            ("NAS 上传中", ExecutionStage::NasUploading as i32),
            ("NAS已原子落盘", ExecutionStage::NasCommitted as i32),
            ("NAS 已原子落盘", ExecutionStage::NasCommitted as i32),
            ("结果待上报", ExecutionStage::ResultPending as i32),
        ];
        for (chinese, value) in known {
            let _ = conn.execute(
                "UPDATE execution_state SET stage_enum = ?2 WHERE stage_enum = 0 AND task_status = ?1",
                params![chinese, value],
            );
        }
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        match self.conn.lock() {
            Ok(guard) => guard,
            // 持锁代码里没有 panic 点；即便被毒化，数据本身仍然一致。
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// 插入一条待发送事件到 outbox。
    ///
    /// `INSERT OR IGNORE`：同一个 `event_id` 重复入队是正常现象
    /// （例如结果已入队但还没确认，进程重启后又走了一遍上报路径）。
    pub fn enqueue(&self, event_id: &str, msg: &pb::WorkerMessage) -> Result<()> {
        let payload_bytes = msg.encode_to_vec();
        let conn = self.lock();
        conn.execute(
            "INSERT OR IGNORE INTO outbox (event_id, payload_bytes, created_at, state) \
             VALUES (?1, ?2, datetime('now'), '待发送')",
            params![event_id, payload_bytes],
        )?;
        Ok(())
    }

    /// 获取前 N 条待补报的事件（按入队顺序）。
    pub fn fetch_pending(&self, limit: usize) -> Result<Vec<OutboxItem>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, event_id, payload_bytes, created_at, attempts \
             FROM outbox ORDER BY id ASC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(OutboxItem {
                id: row.get(0)?,
                event_id: row.get(1)?,
                payload_bytes: row.get(2)?,
                created_at: row.get(3)?,
                attempts: row.get(4)?,
            })
        })?;

        let mut items = Vec::new();
        for r in rows {
            items.push(r?);
        }
        Ok(items)
    }

    /// 按事件编号定向取出一条待补报事件（V4 第 10.5 节：定向重放）。
    pub fn fetch_by_event_id(&self, event_id: &str) -> Result<Option<OutboxItem>> {
        let conn = self.lock();
        let item = conn
            .query_row(
                "SELECT id, event_id, payload_bytes, created_at, attempts \
                 FROM outbox WHERE event_id = ?1",
                params![event_id],
                |row| {
                    Ok(OutboxItem {
                        id: row.get(0)?,
                        event_id: row.get(1)?,
                        payload_bytes: row.get(2)?,
                        created_at: row.get(3)?,
                        attempts: row.get(4)?,
                    })
                },
            )
            .optional()?;
        Ok(item)
    }

    /// 记录一次补报尝试。
    pub fn mark_sent(&self, event_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE outbox SET state = '已发送', attempts = attempts + 1, \
                 last_sent_at = datetime('now') WHERE event_id = ?1",
            params![event_id],
        )?;
        Ok(())
    }

    /// 收到 Master 的 `EventAck.accepted=true` 后删除对应事件。
    pub fn acknowledge(&self, event_id: &str) -> Result<bool> {
        let ack = self.acknowledge_event(event_id)?;
        Ok(!ack.event_id.is_empty())
    }

    /// 在单个 SQLite 事务中完成 ACK 处理：提取 execution_id、清理 execution_state、删除 outbox 记录。
    pub fn acknowledge_event(&self, event_id: &str) -> Result<AcknowledgedEvent> {
        let mut conn = self.lock();
        let payload: Option<Vec<u8>> = conn
            .query_row(
                "SELECT payload_bytes FROM outbox WHERE event_id = ?1",
                params![event_id],
                |r| r.get(0),
            )
            .optional()?;

        let mut execution_id = None;
        if let Some(bytes) = payload {
            if let Ok(msg) = pb::WorkerMessage::decode(&bytes[..]) {
                if let Some(pb::worker_message::Payload::TaskResult(res)) = msg.payload {
                    if !res.execution_id.trim().is_empty() {
                        execution_id = Some(res.execution_id);
                    }
                }
            }
        }

        let tx = conn.transaction()?;
        if let Some(exec_id) = &execution_id {
            tx.execute(
                "UPDATE execution_state SET task_status = '已确认', updated_at = datetime('now') WHERE execution_id = ?1",
                params![exec_id],
            )?;
            tx.execute(
                "DELETE FROM execution_state WHERE execution_id = ?1",
                params![exec_id],
            )?;
        }
        tx.execute("DELETE FROM outbox WHERE event_id = ?1", params![event_id])?;
        tx.commit()?;

        Ok(AcknowledgedEvent {
            event_id: event_id.to_string(),
            execution_id,
        })
    }

    /// 获取当前积压事件数量。
    pub fn pending_count(&self) -> Result<usize> {
        let conn = self.lock();
        let count: i64 = conn.query_row("SELECT count(*) FROM outbox", [], |r| r.get(0))?;
        Ok(count as usize)
    }

    /// 写入或更新一条执行现场。
    pub fn upsert_execution(&self, state: &ExecutionState) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO execution_state (execution_id, slot_index, session_id, task_id, \
                 stage_version, stage_enum, task_status, staging_dir, nas_relative_path, \
                 source_sha256, format, local_file_path, source_size_bytes, result_event_id, \
                 node_id, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, datetime('now')) \
             ON CONFLICT(execution_id) DO UPDATE SET \
                 slot_index = excluded.slot_index, \
                 session_id = excluded.session_id, \
                 task_id = excluded.task_id, \
                 stage_version = excluded.stage_version, \
                 stage_enum = excluded.stage_enum, \
                 task_status = excluded.task_status, \
                 staging_dir = excluded.staging_dir, \
                 nas_relative_path = excluded.nas_relative_path, \
                 -- 已经算出来的哈希/大小/本地路径不该被一次不带证据的阶段写入抹掉
                 source_sha256 = CASE WHEN excluded.source_sha256 = '' \
                                      THEN execution_state.source_sha256 \
                                      ELSE excluded.source_sha256 END, \
                 format = CASE WHEN excluded.format = '' \
                               THEN execution_state.format \
                               ELSE excluded.format END, \
                 local_file_path = CASE WHEN excluded.local_file_path = '' \
                                        THEN execution_state.local_file_path \
                                        ELSE excluded.local_file_path END, \
                 source_size_bytes = CASE WHEN excluded.source_size_bytes = 0 \
                                          THEN execution_state.source_size_bytes \
                                          ELSE excluded.source_size_bytes END, \
                 result_event_id = CASE WHEN excluded.result_event_id = '' \
                                        THEN execution_state.result_event_id \
                                        ELSE excluded.result_event_id END, \
                 node_id = CASE WHEN excluded.node_id = '' \
                                THEN execution_state.node_id \
                                ELSE excluded.node_id END, \
                 updated_at = datetime('now')",
            params![
                state.execution_id,
                state.slot_index,
                state.session_id,
                state.task_id,
                state.stage_version,
                state.stage as i32,
                state.task_status,
                state.staging_dir,
                state.nas_relative_path,
                state.source_sha256,
                state.format,
                state.local_file_path,
                state.source_size_bytes,
                state.result_event_id,
                state.node_id,
                state.created_at,
            ],
        )?;
        Ok(())
    }

    /// 只推进阶段，不改动其他字段（技术枚举 + 中文展示一并写入）。
    pub fn set_execution_stage(&self, execution_id: &str, stage: ExecutionStage) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE execution_state SET stage_enum = ?2, task_status = ?3, updated_at = datetime('now') \
             WHERE execution_id = ?1",
            params![execution_id, stage as i32, stage_display(stage)],
        )?;
        Ok(())
    }

    /// 记录本地源文件证据（「本地文件完成」阶段：路径、大小、哈希）。
    pub fn set_execution_source_evidence(
        &self,
        execution_id: &str,
        local_file_path: &str,
        size_bytes: i64,
        sha256: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE execution_state SET local_file_path = ?2, source_size_bytes = ?3, \
                 source_sha256 = ?4, updated_at = datetime('now') \
             WHERE execution_id = ?1",
            params![execution_id, local_file_path, size_bytes, sha256],
        )?;
        Ok(())
    }

    /// 记录结果事件编号（「结果待上报」阶段，供定向重放）。
    pub fn set_execution_result_event(&self, execution_id: &str, event_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE execution_state SET result_event_id = ?2, updated_at = datetime('now') \
             WHERE execution_id = ?1",
            params![execution_id, event_id],
        )?;
        Ok(())
    }

    /// 读取一条执行现场。
    pub fn get_execution(&self, execution_id: &str) -> Result<Option<ExecutionState>> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT slot_index, session_id, task_id, execution_id, stage_version, \
                        stage_enum, task_status, staging_dir, nas_relative_path, source_sha256, \
                        format, local_file_path, source_size_bytes, result_event_id, \
                        node_id, created_at \
                 FROM execution_state WHERE execution_id = ?1",
                params![execution_id],
                map_execution,
            )
            .optional()?;
        Ok(row)
    }

    /// 列出全部未收尾的执行现场，供重连对账使用。
    ///
    /// ACK 到达时现场记录即被删除，因此表里剩下的都是需要上报的活动现场。
    pub fn list_active_executions(&self) -> Result<Vec<ExecutionState>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT slot_index, session_id, task_id, execution_id, stage_version, \
                    stage_enum, task_status, staging_dir, nas_relative_path, source_sha256, \
                    format, local_file_path, source_size_bytes, result_event_id, \
                    node_id, created_at \
             FROM execution_state ORDER BY slot_index ASC",
        )?;
        let rows = stmt.query_map([], map_execution)?;
        let mut items = Vec::new();
        for r in rows {
            items.push(r?);
        }
        Ok(items)
    }

    /// 删除一条执行现场（结果已被 Master 确认，或任务已被明确放弃）。
    pub fn clear_execution(&self, execution_id: &str) -> Result<bool> {
        let conn = self.lock();
        let affected = conn.execute(
            "DELETE FROM execution_state WHERE execution_id = ?1",
            params![execution_id],
        )?;
        Ok(affected > 0)
    }
}

fn map_execution(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExecutionState> {
    let stage_enum: i32 = row.get(5)?;
    Ok(ExecutionState {
        slot_index: row.get(0)?,
        session_id: row.get(1)?,
        task_id: row.get(2)?,
        execution_id: row.get(3)?,
        stage_version: row.get(4)?,
        stage: ExecutionStage::from_i32_safe(stage_enum),
        task_status: row.get(6)?,
        staging_dir: row.get(7)?,
        nas_relative_path: row.get(8)?,
        source_sha256: row.get(9)?,
        format: row.get(10)?,
        local_file_path: row.get(11)?,
        source_size_bytes: row.get(12)?,
        result_event_id: row.get(13)?,
        node_id: row.get(14)?,
        created_at: row.get(15)?,
    })
}

/// 把本地库收紧为「仅当前用户可读写」。
///
/// 这个库里躺着待补报的 `TaskResult`，其中包含 NAS 路径与哈希；
/// 更要紧的是它与身份文件同目录，权限习惯应当一致。失败只记日志：
/// 权限设不上（例如挂在不支持 POSIX 位的文件系统上）不该让 Worker 起不来。
fn restrict_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(err) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            tracing::warn!(path = %path.display(), error = %err, "收紧本地数据库文件权限失败");
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> ExecutionState {
        ExecutionState {
            slot_index: 1,
            session_id: "会话-1".to_string(),
            task_id: "任务-1".to_string(),
            execution_id: "执行-1".to_string(),
            stage_version: 7,
            stage: ExecutionStage::Accepted,
            task_status: "已接受".to_string(),
            staging_dir: "data/staging/task_任务-1".to_string(),
            nas_relative_path: "文件/000001-算法导论.pdf".to_string(),
            source_sha256: String::new(),
            format: "pdf".to_string(),
            local_file_path: String::new(),
            source_size_bytes: 0,
            result_event_id: String::new(),
            node_id: "节点-1".to_string(),
            created_at: "2026-08-21T12:00:00Z".to_string(),
        }
    }

    #[test]
    fn outbox_enqueue_fetch_and_ack() {
        let store = LocalStore::memory().unwrap();
        let msg = pb::WorkerMessage {
            event_id: "evt-123".to_string(),
            sent_at: "2026-08-21T12:00:00Z".to_string(),
            replayed: false,
            payload: None,
        };

        store.enqueue("evt-123", &msg).unwrap();
        assert_eq!(store.pending_count().unwrap(), 1);

        let items = store.fetch_pending(10).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].event_id, "evt-123");

        let decoded = pb::WorkerMessage::decode(&items[0].payload_bytes[..]).unwrap();
        assert_eq!(decoded.event_id, "evt-123");

        store.acknowledge("evt-123").unwrap();
        assert_eq!(store.pending_count().unwrap(), 0);
    }

    #[test]
    fn sent_event_survives_until_acknowledged() {
        // 「发出去了」不等于「Master 处理了」：只有 EventAck 才能删记录
        let store = LocalStore::memory().unwrap();
        let msg = pb::WorkerMessage::default();
        store.enqueue("evt-a", &msg).unwrap();
        store.mark_sent("evt-a").unwrap();
        assert_eq!(store.pending_count().unwrap(), 1);
        assert_eq!(store.fetch_pending(10).unwrap()[0].attempts, 1);

        store.mark_sent("evt-a").unwrap();
        assert_eq!(store.fetch_pending(10).unwrap()[0].attempts, 2);

        store.acknowledge("evt-a").unwrap();
        assert_eq!(store.pending_count().unwrap(), 0);
    }

    #[test]
    fn duplicate_enqueue_is_idempotent() {
        let store = LocalStore::memory().unwrap();
        let msg = pb::WorkerMessage::default();
        store.enqueue("evt-dup", &msg).unwrap();
        store.enqueue("evt-dup", &msg).unwrap();
        assert_eq!(store.pending_count().unwrap(), 1);
    }

    #[test]
    fn execution_state_round_trip() {
        let store = LocalStore::memory().unwrap();
        store.upsert_execution(&state()).unwrap();
        let loaded = store.get_execution("执行-1").unwrap().unwrap();
        assert_eq!(loaded, state());
    }

    #[test]
    fn stage_advances_without_losing_hash() {
        let store = LocalStore::memory().unwrap();
        store.upsert_execution(&state()).unwrap();
        store
            .set_execution_source_evidence(
                "执行-1",
                "data/staging/task-任务-1/book.pdf",
                4096,
                &"b".repeat(64),
            )
            .unwrap();
        store
            .set_execution_stage("执行-1", ExecutionStage::NasUploading)
            .unwrap();

        let loaded = store.get_execution("执行-1").unwrap().unwrap();
        assert_eq!(loaded.stage, ExecutionStage::NasUploading);
        assert_eq!(loaded.task_status, "NAS 上传中");
        assert_eq!(loaded.source_sha256, "b".repeat(64));
        assert_eq!(loaded.local_file_path, "data/staging/task-任务-1/book.pdf");
        assert_eq!(loaded.source_size_bytes, 4096);
    }

    #[test]
    fn upsert_does_not_erase_existing_hash() {
        // 重启后重放「已接受」阶段时不该把已算好的哈希清空
        let store = LocalStore::memory().unwrap();
        store.upsert_execution(&state()).unwrap();
        store
            .set_execution_source_evidence("执行-1", "p", 1, &"c".repeat(64))
            .unwrap();
        store.upsert_execution(&state()).unwrap();
        let loaded = store.get_execution("执行-1").unwrap().unwrap();
        assert_eq!(loaded.source_sha256, "c".repeat(64));
        assert_eq!(loaded.local_file_path, "p");
        assert_eq!(loaded.source_size_bytes, 1);
    }

    #[test]
    fn result_event_id_is_recorded_for_targeted_replay() {
        let store = LocalStore::memory().unwrap();
        store.upsert_execution(&state()).unwrap();
        store
            .set_execution_result_event("执行-1", "evt-res-执行-1")
            .unwrap();
        let loaded = store.get_execution("执行-1").unwrap().unwrap();
        assert_eq!(loaded.result_event_id, "evt-res-执行-1");
    }

    #[test]
    fn clearing_execution_removes_it() {
        let store = LocalStore::memory().unwrap();
        store.upsert_execution(&state()).unwrap();
        assert!(store.clear_execution("执行-1").unwrap());
        assert!(store.get_execution("执行-1").unwrap().is_none());
    }
}
