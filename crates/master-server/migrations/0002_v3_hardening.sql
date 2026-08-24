-- 生产化修复 V3 加固迁移
-- 1. 用户会话与令牌版本
ALTER TABLE users ADD COLUMN IF NOT EXISTS token_version BIGINT NOT NULL DEFAULT 1;

CREATE TABLE IF NOT EXISTS admin_sessions (
    id              UUID PRIMARY KEY,
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash      TEXT NOT NULL UNIQUE,
    issued_at       TIMESTAMPTZ NOT NULL,
    expires_at      TIMESTAMPTZ NOT NULL,
    revoked_at      TIMESTAMPTZ,
    revoke_reason   TEXT,
    last_seen_at    TIMESTAMPTZ,
    user_agent_hash TEXT,
    ip_prefix       TEXT
);

CREATE INDEX IF NOT EXISTS idx_admin_sessions_user ON admin_sessions (user_id);
CREATE INDEX IF NOT EXISTS idx_admin_sessions_token ON admin_sessions (token_hash);

-- 2. 代理有效快照同步字段
ALTER TABLE proxies ADD COLUMN IF NOT EXISTS last_seen_at TIMESTAMPTZ;
ALTER TABLE proxies ADD COLUMN IF NOT EXISTS provider_valid BOOLEAN NOT NULL DEFAULT TRUE;
ALTER TABLE proxies ADD COLUMN IF NOT EXISTS retire_after_release BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE proxies ADD COLUMN IF NOT EXISTS sync_generation BIGINT NOT NULL DEFAULT 0;

-- 3. 待确认任务的期望 NAS 证据字段
ALTER TABLE book_tasks ADD COLUMN IF NOT EXISTS expected_nas_relative_path TEXT;
ALTER TABLE book_tasks ADD COLUMN IF NOT EXISTS expected_file_name TEXT;
ALTER TABLE book_tasks ADD COLUMN IF NOT EXISTS expected_format TEXT;
ALTER TABLE book_tasks ADD COLUMN IF NOT EXISTS expected_size_bytes BIGINT;
ALTER TABLE book_tasks ADD COLUMN IF NOT EXISTS expected_sha256 TEXT;
ALTER TABLE book_tasks ADD COLUMN IF NOT EXISTS evidence_execution_id UUID;
ALTER TABLE book_tasks ADD COLUMN IF NOT EXISTS evidence_node_id UUID;
ALTER TABLE book_tasks ADD COLUMN IF NOT EXISTS evidence_recorded_at TIMESTAMPTZ;

-- 4. 业务操作来源中文化迁移（Worker -> 工作节点）
UPDATE operation_logs SET source = '工作节点' WHERE source = 'Worker';
ALTER TABLE operation_logs DROP CONSTRAINT IF EXISTS operation_logs_source_check;
ALTER TABLE operation_logs ADD CONSTRAINT operation_logs_source_check CHECK (source IN ('管理员', '调度器', '工作节点', '系统任务'));

UPDATE task_events SET source = '工作节点' WHERE source = 'Worker';
ALTER TABLE task_events DROP CONSTRAINT IF EXISTS task_events_source_check;
ALTER TABLE task_events ADD CONSTRAINT task_events_source_check CHECK (source IN ('管理员', '调度器', '工作节点', '系统任务'));
