-- 云端 Master 核心数据模型（设计方案第 12 节）
--
-- 约定：
--   * 字段名、UUID、时间戳、操作系统名、文件扩展名等技术标识保持英文/标准值；
--   * 一切业务状态、类型、结果值使用中文，并由 CHECK 约束固定取值范围，
--     数据库层面直接拒绝写入 'registered' 之类的英文状态。

-- ============================================================ 管理与节点

CREATE TABLE users (
    id              UUID PRIMARY KEY,
    username        TEXT NOT NULL UNIQUE,
    password_hash   TEXT NOT NULL,
    role            TEXT NOT NULL CHECK (role IN ('超级管理员', '任务管理员', '只读用户')),
    status          TEXT NOT NULL DEFAULT '启用' CHECK (status IN ('启用', '已禁用')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_login_at   TIMESTAMPTZ
);

-- 一次性节点注册码（第 15.1 节）
CREATE TABLE enroll_codes (
    code            TEXT PRIMARY KEY,
    note            TEXT,
    max_slots       INT NOT NULL DEFAULT 5 CHECK (max_slots BETWEEN 1 AND 64),
    created_by      UUID REFERENCES users (id) ON DELETE SET NULL,
    expires_at      TIMESTAMPTZ NOT NULL,
    used_at         TIMESTAMPTZ,
    used_by_node    UUID,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE worker_nodes (
    id                      UUID PRIMARY KEY,
    name                    TEXT NOT NULL UNIQUE,
    hostname                TEXT NOT NULL,
    os                      TEXT NOT NULL CHECK (os IN ('Windows', 'macOS', 'Linux')),
    os_version              TEXT NOT NULL DEFAULT '',
    agent_version           TEXT NOT NULL DEFAULT '',
    status                  TEXT NOT NULL CHECK (status IN (
                                '待审核', '在线', '忙碌', '已暂停',
                                '存储异常', '维护中', '离线', '已禁用')),
    -- 管理员设定的上限；Worker 可临时下调可用槽位，但不得超过该值（第 6.1 节）
    max_slots               INT NOT NULL DEFAULT 5 CHECK (max_slots BETWEEN 0 AND 64),
    available_slots         INT NOT NULL DEFAULT 0 CHECK (available_slots >= 0),
    upload_concurrency      INT NOT NULL DEFAULT 2 CHECK (upload_concurrency BETWEEN 1 AND 16),
    node_token_hash         TEXT NOT NULL,
    config_version          TEXT NOT NULL DEFAULT '1',
    applied_config_version  TEXT NOT NULL DEFAULT '',
    diagnostics_enabled     BOOLEAN NOT NULL DEFAULT FALSE,
    nas_healthy             BOOLEAN NOT NULL DEFAULT FALSE,
    nas_free_gb             BIGINT NOT NULL DEFAULT 0,
    staging_free_gb         BIGINT NOT NULL DEFAULT 0,
    cpu_percent             DOUBLE PRECISION NOT NULL DEFAULT 0,
    memory_used_mb          BIGINT NOT NULL DEFAULT 0,
    memory_total_mb         BIGINT NOT NULL DEFAULT 0,
    connected               BOOLEAN NOT NULL DEFAULT FALSE,
    last_heartbeat_at       TIMESTAMPTZ,
    approved_at             TIMESTAMPTZ,
    approved_by             UUID REFERENCES users (id) ON DELETE SET NULL,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_worker_nodes_status ON worker_nodes (status);

CREATE TABLE worker_slots (
    id              UUID PRIMARY KEY,
    node_id         UUID NOT NULL REFERENCES worker_nodes (id) ON DELETE CASCADE,
    slot_index      INT NOT NULL CHECK (slot_index >= 0),
    status          TEXT NOT NULL CHECK (status IN (
                        '空闲', '已预留', '启动中', '执行中', '收尾中', '异常', '已停用')),
    session_id      UUID,
    detail          TEXT NOT NULL DEFAULT '',
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (node_id, slot_index)
);

CREATE INDEX idx_worker_slots_status ON worker_slots (node_id, status);

CREATE TABLE node_certificates (
    id                  UUID PRIMARY KEY,
    node_id             UUID NOT NULL REFERENCES worker_nodes (id) ON DELETE CASCADE,
    fingerprint         TEXT NOT NULL UNIQUE,
    certificate_pem     TEXT NOT NULL,
    issued_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    not_after           TIMESTAMPTZ NOT NULL,
    revoked_at          TIMESTAMPTZ,
    revoke_reason       TEXT
);

CREATE TABLE node_config_versions (
    id              UUID PRIMARY KEY,
    node_id         UUID NOT NULL REFERENCES worker_nodes (id) ON DELETE CASCADE,
    version         TEXT NOT NULL,
    payload         JSONB NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (node_id, version)
);

-- ============================================================ 图书与批次

CREATE TABLE books (
    id                      UUID PRIMARY KEY,
    -- 全局序号，NAS 文件名的 6 位前缀来源（第 8.3 节）
    seq                     BIGSERIAL NOT NULL UNIQUE,
    raw_title               TEXT NOT NULL,
    raw_author              TEXT,
    raw_publisher           TEXT,
    raw_isbn                TEXT,
    normalized_title        TEXT NOT NULL,
    normalized_author       TEXT,
    normalized_publisher    TEXT,
    normalized_isbn         TEXT,
    -- 去重键：'isbn:...' / 'tap:...' / 'title:...'
    dedup_key               TEXT NOT NULL UNIQUE,
    verify_status           TEXT NOT NULL CHECK (verify_status IN ('已确认', '待确认', '已合并')),
    merged_into             UUID REFERENCES books (id) ON DELETE SET NULL,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_books_normalized_title ON books (normalized_title);
CREATE INDEX idx_books_isbn ON books (normalized_isbn);

-- 全局唯一文件：同一图书同一格式只保留一份（第 8.3 节）
CREATE TABLE book_files (
    id                  UUID PRIMARY KEY,
    book_id             UUID NOT NULL REFERENCES books (id) ON DELETE CASCADE,
    format              TEXT NOT NULL CHECK (format IN ('pdf', 'epub')),
    nas_relative_path   TEXT NOT NULL UNIQUE,
    size_bytes          BIGINT NOT NULL CHECK (size_bytes > 0),
    sha256              TEXT NOT NULL,
    status              TEXT NOT NULL DEFAULT '有效' CHECK (status IN ('有效', '待核验', '已失效')),
    ingested_by_node    UUID REFERENCES worker_nodes (id) ON DELETE SET NULL,
    ingested_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (book_id, format)
);

CREATE TABLE download_batches (
    id              UUID PRIMARY KEY,
    name            TEXT NOT NULL,
    source_file     TEXT,
    status          TEXT NOT NULL CHECK (status IN ('待开始', '执行中', '已暂停', '已完成', '已取消')),
    -- 数值越大优先级越高，默认 0（第 7.2 节）
    priority        INT NOT NULL DEFAULT 0,
    download_format TEXT NOT NULL DEFAULT 'pdf' CHECK (download_format IN ('pdf', 'epub')),
    created_by      UUID REFERENCES users (id) ON DELETE SET NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_batches_status_priority ON download_batches (status, priority DESC, created_at);

-- 批次与图书的多对多关联：同一本书出现在多个批次只增加关联，不复制任务
CREATE TABLE batch_books (
    id              UUID PRIMARY KEY,
    batch_id        UUID NOT NULL REFERENCES download_batches (id) ON DELETE CASCADE,
    book_id         UUID NOT NULL REFERENCES books (id) ON DELETE CASCADE,
    import_line     INT NOT NULL,
    display_status  TEXT NOT NULL CHECK (display_status IN (
                        '待处理', '已分配', '执行中', '等待入库', '待确认',
                        '已完成', '失败', '已跳过', '已取消')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (batch_id, book_id)
);

CREATE INDEX idx_batch_books_book ON batch_books (book_id);

-- 图书任务：全局唯一（图书 + 格式），跨批次共享
CREATE TABLE book_tasks (
    id                  UUID PRIMARY KEY,
    book_id             UUID NOT NULL REFERENCES books (id) ON DELETE CASCADE,
    format              TEXT NOT NULL CHECK (format IN ('pdf', 'epub')),
    status              TEXT NOT NULL CHECK (status IN (
                            '待处理', '已分配', '执行中', '等待入库', '待确认',
                            '已完成', '失败', '已跳过', '已取消')),
    attempts            INT NOT NULL DEFAULT 0,
    max_attempts        INT NOT NULL DEFAULT 3,
    -- 失败重试延迟：未到时间不得领取（第 7.2 节第 5 条）
    next_attempt_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    stage               TEXT NOT NULL DEFAULT '',
    -- 递增阶段版本：Master 丢弃旧版本事件（第 14.5 节）
    stage_version       INT NOT NULL DEFAULT 0,
    downloaded_bytes    BIGINT NOT NULL DEFAULT 0,
    total_bytes         BIGINT NOT NULL DEFAULT 0,
    lease_node_id       UUID REFERENCES worker_nodes (id) ON DELETE SET NULL,
    lease_session_id    UUID,
    lease_execution_id  UUID,
    lease_expires_at    TIMESTAMPTZ,
    nas_relative_path   TEXT,
    last_error          TEXT,
    cancel_requested    BOOLEAN NOT NULL DEFAULT FALSE,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (book_id, format)
);

CREATE INDEX idx_book_tasks_claimable ON book_tasks (status, next_attempt_at);
CREATE INDEX idx_book_tasks_lease ON book_tasks (lease_expires_at) WHERE lease_expires_at IS NOT NULL;

-- ============================================================ 账号与代理

CREATE TABLE accounts (
    id                  UUID PRIMARY KEY,
    email               TEXT NOT NULL UNIQUE,
    -- 应用层加密（AES-256-GCM），密钥不与数据库备份同处存放（第 15.3 节）
    password_cipher     TEXT NOT NULL,
    nickname            TEXT NOT NULL DEFAULT '',
    status              TEXT NOT NULL CHECK (status IN (
                            '待注册', '已注册', '待验证', '登录失败', '今日额度耗尽', '已禁用')),
    daily_used          INT NOT NULL DEFAULT 0,
    daily_limit         INT NOT NULL DEFAULT 10,
    reset_date          DATE NOT NULL DEFAULT current_date,
    lease_session_id    UUID,
    lease_expires_at    TIMESTAMPTZ,
    last_error          TEXT,
    registered_at       TIMESTAMPTZ,
    last_login_at       TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_accounts_available ON accounts (status, lease_session_id);

CREATE TABLE proxies (
    id                  UUID PRIMARY KEY,
    provider            TEXT NOT NULL DEFAULT 'Webshare',
    external_id         TEXT,
    label               TEXT NOT NULL,
    scheme              TEXT NOT NULL DEFAULT 'http' CHECK (scheme IN ('http', 'https', 'socks5')),
    host                TEXT NOT NULL,
    port                INT NOT NULL CHECK (port BETWEEN 1 AND 65535),
    username            TEXT,
    password_cipher     TEXT,
    status              TEXT NOT NULL CHECK (status IN ('可用', '已占用', '冷却中', '异常', '已停用')),
    exit_ip             TEXT,
    latency_ms          INT,
    success_count       BIGINT NOT NULL DEFAULT 0,
    failure_count       BIGINT NOT NULL DEFAULT 0,
    throttle_count      BIGINT NOT NULL DEFAULT 0,
    cooldown_until      TIMESTAMPTZ,
    lease_session_id    UUID,
    lease_expires_at    TIMESTAMPTZ,
    last_checked_at     TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (provider, host, port, username)
);

CREATE INDEX idx_proxies_available ON proxies (status, cooldown_until);

CREATE TABLE execution_sessions (
    id                  UUID PRIMARY KEY,
    node_id             UUID NOT NULL REFERENCES worker_nodes (id) ON DELETE CASCADE,
    slot_index          INT NOT NULL,
    account_id          UUID REFERENCES accounts (id) ON DELETE SET NULL,
    proxy_id            UUID REFERENCES proxies (id) ON DELETE SET NULL,
    task_type           TEXT NOT NULL CHECK (task_type IN ('账号注册', '图书下载', 'NAS核验', '代理检测')),
    status              TEXT NOT NULL CHECK (status IN (
                            '创建中', '运行中', '断线保护', '正在结束', '已结束', '失败')),
    local_forward_port  INT,
    completed_count     INT NOT NULL DEFAULT 0,
    lease_expires_at    TIMESTAMPTZ NOT NULL,
    protected_until     TIMESTAMPTZ,
    last_renewed_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at            TIMESTAMPTZ,
    end_reason          TEXT
);

CREATE INDEX idx_sessions_live ON execution_sessions (status, lease_expires_at);

CREATE TABLE task_executions (
    -- 执行编号：每次分配唯一（第 3.3 节）
    id                  UUID PRIMARY KEY,
    task_id             UUID NOT NULL REFERENCES book_tasks (id) ON DELETE CASCADE,
    session_id          UUID REFERENCES execution_sessions (id) ON DELETE SET NULL,
    node_id             UUID REFERENCES worker_nodes (id) ON DELETE SET NULL,
    slot_index          INT,
    account_id          UUID REFERENCES accounts (id) ON DELETE SET NULL,
    proxy_id            UUID REFERENCES proxies (id) ON DELETE SET NULL,
    task_type           TEXT NOT NULL CHECK (task_type IN ('账号注册', '图书下载', 'NAS核验', '代理检测')),
    attempt             INT NOT NULL DEFAULT 1,
    result              TEXT CHECK (result IN (
                            '成功', '可重试失败', '不可重试失败', '跳过', '取消', '结果不确定')),
    stage_version       INT NOT NULL DEFAULT 0,
    error               TEXT,
    duration_ms         BIGINT,
    started_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at         TIMESTAMPTZ
);

CREATE INDEX idx_executions_task ON task_executions (task_id, started_at DESC);
CREATE INDEX idx_executions_finished ON task_executions (finished_at);

-- ============================================================ 事件与审计

-- 幂等去重表：同一事件编号只处理一次（第 3.3 / 14.5 节）
CREATE TABLE task_events (
    event_id        TEXT PRIMARY KEY,
    node_id         UUID,
    session_id      UUID,
    task_id         UUID,
    event_type      TEXT NOT NULL,
    source          TEXT NOT NULL CHECK (source IN ('管理员', '调度器', 'Worker', '系统任务')),
    payload         JSONB NOT NULL DEFAULT '{}'::jsonb,
    replayed        BOOLEAN NOT NULL DEFAULT FALSE,
    applied         BOOLEAN NOT NULL DEFAULT FALSE,
    detail          TEXT,
    received_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_task_events_received ON task_events (received_at DESC);

CREATE TABLE operation_logs (
    id          UUID PRIMARY KEY,
    source      TEXT NOT NULL CHECK (source IN ('管理员', '调度器', 'Worker', '系统任务')),
    level       TEXT NOT NULL CHECK (level IN ('调试', '信息', '警告', '错误')),
    actor       TEXT NOT NULL DEFAULT '',
    action      TEXT NOT NULL,
    target      TEXT NOT NULL DEFAULT '',
    detail      TEXT NOT NULL DEFAULT '',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_operation_logs_created ON operation_logs (created_at DESC);

CREATE TABLE alerts (
    id          UUID PRIMARY KEY,
    level       TEXT NOT NULL CHECK (level IN ('提示', '警告', '严重')),
    category    TEXT NOT NULL,
    title       TEXT NOT NULL,
    detail      TEXT NOT NULL DEFAULT '',
    node_id     UUID REFERENCES worker_nodes (id) ON DELETE SET NULL,
    -- 去重键：同一未解决问题不重复刷屏
    dedup_key   TEXT,
    resolved_at TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX idx_alerts_open_dedup ON alerts (dedup_key) WHERE resolved_at IS NULL;
CREATE INDEX idx_alerts_created ON alerts (created_at DESC);

CREATE TABLE daily_stats (
    stat_date       DATE PRIMARY KEY,
    completed       BIGINT NOT NULL DEFAULT 0,
    failed          BIGINT NOT NULL DEFAULT 0,
    skipped         BIGINT NOT NULL DEFAULT 0,
    bytes_total     BIGINT NOT NULL DEFAULT 0,
    account_used    BIGINT NOT NULL DEFAULT 0,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE settings (
    key         TEXT PRIMARY KEY,
    value       JSONB NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
