-- 迁移 0025：统一 Worker 租约防护屏障与 Workflow 存储收敛至 PostgreSQL（P4）
--
-- 1. 为 acquisition_targets 增加 fencing_token，支持多 Worker 抢占式租约自增防护；
-- 2. 建立统一多内容模型（书目、学术论文、小说及其统一标识）；
-- 3. 将 Workflow 工作空间、定义、不可变发布版本、执行运行与 attempt 收敛至 PG。

-- ============================================================ 1. 统一租约防护屏障
DO $$
BEGIN
    IF EXISTS (SELECT FROM pg_tables WHERE schemaname = 'public' AND tablename = 'acquisition_targets') THEN
        ALTER TABLE acquisition_targets ADD COLUMN IF NOT EXISTS fencing_token BIGINT NOT NULL DEFAULT 0;
    END IF;
    IF EXISTS (SELECT FROM pg_tables WHERE schemaname = 'public' AND tablename = 'book_tasks') THEN
        ALTER TABLE book_tasks ADD COLUMN IF NOT EXISTS fencing_token BIGINT NOT NULL DEFAULT 0;
    END IF;
END $$;

-- 统一外部委托身份映射 (A3)
CREATE TABLE IF NOT EXISTS user_delegated_identities (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    issuer          TEXT NOT NULL,
    subject         TEXT NOT NULL,
    user_id         UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (issuer, subject)
);

CREATE INDEX IF NOT EXISTS idx_user_delegated_lookup ON user_delegated_identities (issuer, subject);
CREATE INDEX IF NOT EXISTS idx_user_delegated_user ON user_delegated_identities (user_id);

-- ============================================================ 2. 统一多内容实体模型
CREATE TABLE IF NOT EXISTS content_entities (
    id              UUID PRIMARY KEY,
    kind            TEXT NOT NULL CHECK (kind IN ('book', 'article', 'novel')),
    title           TEXT NOT NULL,
    source_name     TEXT NOT NULL DEFAULT '',
    source_id       TEXT NOT NULL DEFAULT '',
    metadata        JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_content_entities_kind ON content_entities (kind);
CREATE INDEX IF NOT EXISTS idx_content_entities_source ON content_entities (source_name, source_id);

-- 内容标识体系 (ISBN, DOI, Novel ID, URL 等多重标识)
CREATE TABLE IF NOT EXISTS content_identifiers (
    id              BIGSERIAL PRIMARY KEY,
    content_id      UUID NOT NULL REFERENCES content_entities (id) ON DELETE CASCADE,
    scheme          TEXT NOT NULL, -- 如 'isbn', 'doi', 'biquge_id', 'url'
    value           TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (scheme, value)
);

CREATE INDEX IF NOT EXISTS idx_content_identifiers_lookup ON content_identifiers (scheme, value);
CREATE INDEX IF NOT EXISTS idx_content_identifiers_content ON content_identifiers (content_id);

-- ============================================================ 3. Workflow 存储收敛至 PG
CREATE TABLE IF NOT EXISTS unified_workspaces (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    slug            TEXT NOT NULL UNIQUE,
    status          TEXT NOT NULL DEFAULT 'active',
    settings        JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS unified_workflows (
    id              TEXT PRIMARY KEY,
    workspace_id    TEXT NOT NULL REFERENCES unified_workspaces (id) ON DELETE RESTRICT,
    name            TEXT NOT NULL,
    title           TEXT NOT NULL,
    draft_source    TEXT NOT NULL,
    draft_revision  BIGINT NOT NULL DEFAULT 1,
    status          TEXT NOT NULL DEFAULT 'active',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, name)
);

CREATE TABLE IF NOT EXISTS unified_workflow_versions (
    id              TEXT PRIMARY KEY,
    workflow_id     TEXT NOT NULL REFERENCES unified_workflows (id) ON DELETE CASCADE,
    version         BIGINT NOT NULL,
    source          TEXT NOT NULL,
    content_hash    TEXT NOT NULL,
    permissions     JSONB NOT NULL DEFAULT '[]'::jsonb,
    change_note     TEXT NOT NULL DEFAULT '',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workflow_id, version)
);

CREATE TABLE IF NOT EXISTS unified_runs (
    id              TEXT PRIMARY KEY,
    workflow_id     TEXT,
    workflow_name   TEXT NOT NULL,
    workflow_hash   TEXT NOT NULL,
    version         BIGINT,
    status          TEXT NOT NULL,
    inputs          JSONB NOT NULL DEFAULT '{}'::jsonb,
    outputs         JSONB,
    error_code      TEXT,
    error_message   TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_unified_runs_status ON unified_runs (status, created_at DESC);

-- 统一 Worker 执行 Attempt 与租约表
CREATE TABLE IF NOT EXISTS worker_execution_attempts (
    id                  UUID PRIMARY KEY,
    task_type           TEXT NOT NULL CHECK (task_type IN ('catalog_download', 'workflow_run', 'academic_doi')),
    task_id             TEXT NOT NULL,
    worker_id           UUID REFERENCES worker_nodes (id) ON DELETE SET NULL,
    fencing_token       BIGINT NOT NULL,
    lease_expires_at    TIMESTAMPTZ NOT NULL,
    status              TEXT NOT NULL CHECK (status IN ('运行中', '成功', '失败', '超时', '已作废')),
    result_summary      JSONB NOT NULL DEFAULT '{}'::jsonb,
    started_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at         TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_worker_attempts_task ON worker_execution_attempts (task_type, task_id, fencing_token DESC);
CREATE INDEX IF NOT EXISTS idx_worker_attempts_lease ON worker_execution_attempts (lease_expires_at) WHERE status = '运行中';
