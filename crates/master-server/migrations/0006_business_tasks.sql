-- 生产化迁移 V6：CSV 建批与业务任务统一下发（V6 方案）
--
-- 原则：
-- 1. 业务状态使用中文并带 CHECK 约束；
-- 2. 任务与资源模型清晰分离（账号是资源，账号注册任务是执行记录）；
-- 3. 图书任务增加任务级代理绑定（一本书固定同一 IP）；
-- 4. 执行记录支持图书与账号注册两种业务类型；
-- 5. 支持文件两阶段导入（预检 -> 提交）与人工确认机制。

-- ============================================================ 1. 导入任务表
CREATE TABLE IF NOT EXISTS import_jobs (
    id                      UUID PRIMARY KEY,
    import_type             TEXT NOT NULL CHECK (import_type IN ('图书', '账号')),
    status                  TEXT NOT NULL CHECK (status IN ('预检中', '待确认', '已提交', '已过期', '失败')),
    original_file_name      TEXT NOT NULL,
    file_sha256             TEXT NOT NULL,
    temp_path               TEXT,
    token_hash              TEXT NOT NULL UNIQUE,
    created_by              UUID REFERENCES users(id) ON DELETE SET NULL,
    expires_at              TIMESTAMPTZ NOT NULL,
    committed_at            TIMESTAMPTZ,
    committed_resource_id   UUID,
    summary                 JSONB NOT NULL DEFAULT '{}'::jsonb,
    payload_encrypted       TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_import_jobs_token ON import_jobs (token_hash);
CREATE INDEX IF NOT EXISTS idx_import_jobs_status ON import_jobs (status, expires_at);

-- ============================================================ 2. 账号注册批次表
CREATE TABLE IF NOT EXISTS account_registration_batches (
    id              UUID PRIMARY KEY,
    name            TEXT NOT NULL,
    source_file     TEXT,
    status          TEXT NOT NULL CHECK (status IN ('待开始', '执行中', '已暂停', '已完成', '已取消')),
    priority        INT NOT NULL DEFAULT 0,
    created_by      UUID REFERENCES users (id) ON DELETE SET NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_account_reg_batches_status ON account_registration_batches (status, priority DESC, created_at);

-- ============================================================ 3. 账号注册任务表
CREATE TABLE IF NOT EXISTS account_registration_tasks (
    id                  UUID PRIMARY KEY,
    batch_id            UUID NOT NULL REFERENCES account_registration_batches (id) ON DELETE CASCADE,
    account_id          UUID NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    status              TEXT NOT NULL CHECK (status IN (
                            '待处理', '已分配', '执行中', '等待人工确认',
                            '正在重试', '已完成', '失败', '已取消')),
    priority            INT NOT NULL DEFAULT 0,
    attempts            INT NOT NULL DEFAULT 0,
    max_attempts        INT NOT NULL DEFAULT 3,
    next_attempt_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_node_id       UUID REFERENCES worker_nodes (id) ON DELETE SET NULL,
    lease_session_id    UUID,
    lease_execution_id  UUID,
    lease_expires_at    TIMESTAMPTZ,
    stage               TEXT NOT NULL DEFAULT '',
    stage_version       INT NOT NULL DEFAULT 0,
    last_error          TEXT,
    cancel_requested    BOOLEAN NOT NULL DEFAULT FALSE,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(batch_id, account_id)
);

CREATE INDEX IF NOT EXISTS idx_account_reg_tasks_claimable ON account_registration_tasks (status, next_attempt_at);
CREATE INDEX IF NOT EXISTS idx_account_reg_tasks_lease ON account_registration_tasks (lease_expires_at) WHERE lease_expires_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_account_reg_tasks_batch ON account_registration_tasks (batch_id);
CREATE INDEX IF NOT EXISTS idx_account_reg_tasks_account ON account_registration_tasks (account_id);

-- ============================================================ 4. 执行记录表兼容扩展
ALTER TABLE task_executions ALTER COLUMN task_id DROP NOT NULL;
ALTER TABLE task_executions ADD COLUMN IF NOT EXISTS account_registration_task_id UUID REFERENCES account_registration_tasks (id) ON DELETE CASCADE;

CREATE INDEX IF NOT EXISTS idx_executions_account_reg_task ON task_executions (account_registration_task_id);

-- ============================================================ 5. 图书任务固定代理字段
ALTER TABLE book_tasks ADD COLUMN IF NOT EXISTS bound_proxy_id UUID REFERENCES proxies (id) ON DELETE SET NULL;
ALTER TABLE book_tasks ADD COLUMN IF NOT EXISTS bound_exit_ip TEXT;
ALTER TABLE book_tasks ADD COLUMN IF NOT EXISTS proxy_bound_at TIMESTAMPTZ;
ALTER TABLE book_tasks ADD COLUMN IF NOT EXISTS proxy_change_count INT NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_book_tasks_bound_proxy ON book_tasks (bound_proxy_id);

-- ============================================================ 6. 待确认事项表
CREATE TABLE IF NOT EXISTS manual_actions (
    id                      UUID PRIMARY KEY,
    task_type               TEXT NOT NULL CHECK (task_type IN ('图书下载', '账号注册', 'NAS核验', '代理检测')),
    registration_task_id    UUID REFERENCES account_registration_tasks(id) ON DELETE CASCADE,
    book_task_id            UUID REFERENCES book_tasks(id) ON DELETE CASCADE,
    execution_id            UUID,
    node_id                 UUID REFERENCES worker_nodes(id) ON DELETE SET NULL,
    session_id              UUID,
    action_type             TEXT NOT NULL CHECK (action_type IN ('邮箱验证码', '图片验证码', '人工确认', '风控')),
    prompt                  TEXT NOT NULL,
    status                  TEXT NOT NULL DEFAULT '待处理' CHECK (status IN ('待处理', '已解决', '已过期', '已取消')),
    artifact_url            TEXT,
    input_code              TEXT,
    expires_at              TIMESTAMPTZ NOT NULL,
    resolved_at             TIMESTAMPTZ,
    resolved_by             UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_manual_actions_pending ON manual_actions (status, expires_at);
CREATE INDEX IF NOT EXISTS idx_manual_actions_reg_task ON manual_actions (registration_task_id);

-- ============================================================ 7. 远程命令表
CREATE TABLE IF NOT EXISTS worker_commands (
    id              UUID PRIMARY KEY,
    command_type    TEXT NOT NULL,
    target_node_id  UUID REFERENCES worker_nodes(id) ON DELETE CASCADE,
    payload         JSONB NOT NULL DEFAULT '{}'::jsonb,
    status          TEXT NOT NULL DEFAULT '待下发' CHECK (status IN ('待下发', '已下发', '已接收', '执行中', '已完成', '失败', '已过期', '已取消')),
    idempotency_key TEXT UNIQUE,
    created_by      UUID REFERENCES users (id) ON DELETE SET NULL,
    sent_at         TIMESTAMPTZ,
    accepted_at     TIMESTAMPTZ,
    completed_at    TIMESTAMPTZ,
    expires_at      TIMESTAMPTZ,
    result          JSONB,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_worker_commands_node ON worker_commands (target_node_id, status);
