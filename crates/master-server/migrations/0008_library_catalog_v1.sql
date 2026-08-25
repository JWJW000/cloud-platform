-- 图书馆总库与索引设计方案 V1 核心架构迁移脚本
--
-- 原则：
-- 1. PostgreSQL 作为唯一业务事实源；
-- 2. 原始记录（source_records）完整保留出处与原始载荷；
-- 3. 规范对象层（works, editions, identifiers, contributors, subjects）；
-- 4. 来源文件候选（source_assets）与真实馆藏（library_files, holdings）清晰分离；
-- 5. 唯一持续全局获取池（acquisition_targets, acquisition_executions）；
-- 6. 搜索同步事件事务外发（catalog_outbox）。

-- ============================================================ 1. 来源与导入体系

CREATE TABLE IF NOT EXISTS catalog_sources (
    id              UUID PRIMARY KEY,
    name            TEXT NOT NULL UNIQUE,
    source_type     TEXT NOT NULL DEFAULT 'excel',
    description     TEXT,
    priority        INT NOT NULL DEFAULT 0,
    enabled         BOOLEAN NOT NULL DEFAULT TRUE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_catalog_sources_enabled ON catalog_sources (enabled, priority DESC);

CREATE TABLE IF NOT EXISTS import_files (
    id                  UUID PRIMARY KEY,
    source_id           UUID NOT NULL REFERENCES catalog_sources (id) ON DELETE CASCADE,
    file_path           TEXT NOT NULL,
    file_sha256         TEXT NOT NULL,
    file_size_bytes     BIGINT NOT NULL DEFAULT 0,
    sheet_name          TEXT NOT NULL DEFAULT '',
    structure_version   TEXT NOT NULL DEFAULT 'v1',
    total_rows          BIGINT NOT NULL DEFAULT 0,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (source_id, file_path, sheet_name)
);

CREATE INDEX IF NOT EXISTS idx_import_files_source ON import_files (source_id);
CREATE INDEX IF NOT EXISTS idx_import_files_sha ON import_files (file_sha256);

CREATE TABLE IF NOT EXISTS import_runs (
    id                  UUID PRIMARY KEY,
    import_file_id      UUID NOT NULL REFERENCES import_files (id) ON DELETE CASCADE,
    status              TEXT NOT NULL DEFAULT '准备中' CHECK (status IN ('准备中', '运行中', '已暂停', '已完成', '部分失败', '失败')),
    checkpoint_row      BIGINT NOT NULL DEFAULT 0,
    total_rows          BIGINT NOT NULL DEFAULT 0,
    imported_count      BIGINT NOT NULL DEFAULT 0,
    quarantined_count   BIGINT NOT NULL DEFAULT 0,
    duplicate_count     BIGINT NOT NULL DEFAULT 0,
    error_summary       TEXT,
    started_at          TIMESTAMPTZ,
    completed_at        TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_import_runs_file ON import_runs (import_file_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_import_runs_status ON import_runs (status);

CREATE TABLE IF NOT EXISTS source_records (
    id                  UUID PRIMARY KEY,
    source_id           UUID NOT NULL REFERENCES catalog_sources (id) ON DELETE CASCADE,
    import_file_id      UUID NOT NULL REFERENCES import_files (id) ON DELETE CASCADE,
    external_id         TEXT,
    sheet_name          TEXT NOT NULL DEFAULT '',
    row_number          BIGINT NOT NULL,
    raw_payload         JSONB NOT NULL DEFAULT '{}'::jsonb,
    normalized_title    TEXT NOT NULL DEFAULT '',
    normalized_author   TEXT,
    normalized_publisher TEXT,
    raw_isbn            TEXT,
    raw_doi             TEXT,
    raw_year            TEXT,
    raw_language        TEXT,
    raw_category        TEXT,
    import_version      TEXT NOT NULL DEFAULT 'v1',
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (import_file_id, sheet_name, row_number)
);

CREATE INDEX IF NOT EXISTS idx_source_records_source ON source_records (source_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_source_records_ext ON source_records (source_id, external_id) WHERE external_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_source_records_file_row ON source_records (import_file_id, row_number);

CREATE TABLE IF NOT EXISTS quarantined_records (
    id                  UUID PRIMARY KEY,
    import_run_id       UUID REFERENCES import_runs (id) ON DELETE SET NULL,
    import_file_id      UUID REFERENCES import_files (id) ON DELETE CASCADE,
    sheet_name          TEXT NOT NULL DEFAULT '',
    row_number          BIGINT NOT NULL,
    raw_content         JSONB NOT NULL DEFAULT '{}'::jsonb,
    error_reason        TEXT NOT NULL,
    resolved            BOOLEAN NOT NULL DEFAULT FALSE,
    resolved_at         TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_quarantined_runs ON quarantined_records (import_run_id, resolved);
CREATE INDEX IF NOT EXISTS idx_quarantined_file_row ON quarantined_records (import_file_id, row_number);

-- ============================================================ 2. 规范书目体系

CREATE TABLE IF NOT EXISTS works (
    id                  UUID PRIMARY KEY,
    work_type           TEXT NOT NULL DEFAULT '整书' CHECK (work_type IN ('整书', '章节', '论文', '合集', '其他')),
    preferred_title     TEXT NOT NULL,
    normalized_title    TEXT NOT NULL,
    primary_language    TEXT NOT NULL DEFAULT 'zh',
    parent_work_id      UUID REFERENCES works (id) ON DELETE SET NULL,
    resolution_status   TEXT NOT NULL DEFAULT '已确认' CHECK (resolution_status IN ('已确认', '待消歧', '已合并', '已拆分', '已忽略')),
    merged_into_work_id UUID REFERENCES works (id) ON DELETE SET NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_works_search ON works (normalized_title, work_type, resolution_status);
CREATE INDEX IF NOT EXISTS idx_works_parent ON works (parent_work_id) WHERE parent_work_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_works_resolution ON works (resolution_status);

CREATE TABLE IF NOT EXISTS editions (
    id                  UUID PRIMARY KEY,
    work_id             UUID NOT NULL REFERENCES works (id) ON DELETE CASCADE,
    edition_title       TEXT NOT NULL,
    language            TEXT NOT NULL DEFAULT 'zh',
    publisher           TEXT,
    publish_year        INT,
    publish_date_text   TEXT,
    edition_number      TEXT,
    intro               TEXT,
    format_summary      TEXT,
    status              TEXT NOT NULL DEFAULT '已确认' CHECK (status IN ('已确认', '待消歧', '已合并', '已拆分')),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_editions_work ON editions (work_id, language, publish_year);
CREATE INDEX IF NOT EXISTS idx_editions_publisher_year ON editions (publisher, publish_year);

CREATE TABLE IF NOT EXISTS identifiers (
    id                  UUID PRIMARY KEY,
    object_type         TEXT NOT NULL CHECK (object_type IN ('work', 'edition', 'source_record')),
    object_id           UUID NOT NULL,
    identifier_type     TEXT NOT NULL CHECK (identifier_type IN ('isbn13', 'isbn10', 'doi', 'external_id', 'dams_code', 'custom')),
    raw_value           TEXT NOT NULL,
    normalized_value    TEXT NOT NULL,
    is_valid            BOOLEAN NOT NULL DEFAULT TRUE,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_identifiers_lookup ON identifiers (identifier_type, normalized_value);
CREATE INDEX IF NOT EXISTS idx_identifiers_obj ON identifiers (object_type, object_id);

CREATE TABLE IF NOT EXISTS contributors (
    id                  UUID PRIMARY KEY,
    name                TEXT NOT NULL,
    normalized_name     TEXT NOT NULL UNIQUE,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_contributors_norm ON contributors (normalized_name);

CREATE TABLE IF NOT EXISTS edition_contributors (
    id                  UUID PRIMARY KEY,
    edition_id          UUID NOT NULL REFERENCES editions (id) ON DELETE CASCADE,
    contributor_id      UUID NOT NULL REFERENCES contributors (id) ON DELETE CASCADE,
    role                TEXT NOT NULL DEFAULT '作者' CHECK (role IN ('作者', '译者', '编者', '其他')),
    sort_order          INT NOT NULL DEFAULT 0,
    UNIQUE (edition_id, contributor_id, role)
);

CREATE INDEX IF NOT EXISTS idx_edition_contributors_edition ON edition_contributors (edition_id, sort_order);
CREATE INDEX IF NOT EXISTS idx_edition_contributors_contributor ON edition_contributors (contributor_id);

CREATE TABLE IF NOT EXISTS subjects (
    id                  UUID PRIMARY KEY,
    subject_type        TEXT NOT NULL DEFAULT '分类' CHECK (subject_type IN ('中图分类号', '主题词', '关键词', '分类')),
    code                TEXT,
    name                TEXT NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (subject_type, name)
);

CREATE INDEX IF NOT EXISTS idx_subjects_type_name ON subjects (subject_type, name);

CREATE TABLE IF NOT EXISTS edition_subjects (
    id                  UUID PRIMARY KEY,
    edition_id          UUID NOT NULL REFERENCES editions (id) ON DELETE CASCADE,
    subject_id          UUID NOT NULL REFERENCES subjects (id) ON DELETE CASCADE,
    UNIQUE (edition_id, subject_id)
);

CREATE INDEX IF NOT EXISTS idx_edition_subjects_edition ON edition_subjects (edition_id);
CREATE INDEX IF NOT EXISTS idx_edition_subjects_subject ON edition_subjects (subject_id);

CREATE TABLE IF NOT EXISTS record_resolutions (
    id                  UUID PRIMARY KEY,
    source_record_id    UUID NOT NULL REFERENCES source_records (id) ON DELETE CASCADE,
    work_id             UUID REFERENCES works (id) ON DELETE SET NULL,
    edition_id          UUID REFERENCES editions (id) ON DELETE SET NULL,
    match_method        TEXT NOT NULL,
    confidence          DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    rule_version        TEXT NOT NULL DEFAULT 'v1',
    is_manual           BOOLEAN NOT NULL DEFAULT FALSE,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (source_record_id)
);

CREATE INDEX IF NOT EXISTS idx_resolutions_work ON record_resolutions (work_id);
CREATE INDEX IF NOT EXISTS idx_resolutions_edition ON record_resolutions (edition_id);

-- ============================================================ 3. 文件候选与馆藏体系

CREATE TABLE IF NOT EXISTS source_assets (
    id                  UUID PRIMARY KEY,
    source_record_id    UUID NOT NULL REFERENCES source_records (id) ON DELETE CASCADE,
    format              TEXT NOT NULL,
    declared_size_bytes BIGINT,
    md5                 TEXT,
    download_url        TEXT,
    status              TEXT NOT NULL DEFAULT '可用' CHECK (status IN ('可用', '不可用', '已损坏', '未知')),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_source_assets_record ON source_assets (source_record_id, format);
CREATE INDEX IF NOT EXISTS idx_source_assets_md5 ON source_assets (md5) WHERE md5 IS NOT NULL;

CREATE TABLE IF NOT EXISTS library_files (
    id                  UUID PRIMARY KEY,
    storage_backend     TEXT NOT NULL DEFAULT 'NAS' CHECK (storage_backend IN ('NAS', 'S3', 'OSS', 'Local')),
    object_key          TEXT NOT NULL,
    format              TEXT NOT NULL,
    actual_size_bytes   BIGINT NOT NULL,
    sha256              TEXT NOT NULL,
    md5                 TEXT,
    verify_status       TEXT NOT NULL DEFAULT '有效' CHECK (verify_status IN ('待校验', '有效', '损坏', '丢失')),
    verified_at         TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (storage_backend, object_key),
    UNIQUE (sha256)
);

CREATE INDEX IF NOT EXISTS idx_library_files_sha256 ON library_files (sha256);
CREATE INDEX IF NOT EXISTS idx_library_files_verify ON library_files (verify_status);

CREATE TABLE IF NOT EXISTS holdings (
    id                  UUID PRIMARY KEY,
    edition_id          UUID NOT NULL REFERENCES editions (id) ON DELETE CASCADE,
    library_file_id     UUID NOT NULL REFERENCES library_files (id) ON DELETE CASCADE,
    source_asset_id     UUID REFERENCES source_assets (id) ON DELETE SET NULL,
    match_type          TEXT NOT NULL DEFAULT '精确匹配',
    meets_strategy      BOOLEAN NOT NULL DEFAULT TRUE,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (edition_id, library_file_id)
);

CREATE INDEX IF NOT EXISTS idx_holdings_edition ON holdings (edition_id);
CREATE INDEX IF NOT EXISTS idx_holdings_file ON holdings (library_file_id);

-- ============================================================ 4. 全局获取目标与执行

CREATE TABLE IF NOT EXISTS acquisition_targets (
    id                      UUID PRIMARY KEY,
    edition_id              UUID NOT NULL REFERENCES editions (id) ON DELETE CASCADE,
    preferred_formats       JSONB NOT NULL DEFAULT '["epub", "azw3", "mobi", "pdf", "djvu"]'::jsonb,
    status                  TEXT NOT NULL DEFAULT '待下载' CHECK (status IN ('待下载', '排队中', '已领取', '下载中', '校验中', '已下载', '暂时失败', '来源无效', '人工确认', '暂不获取')),
    priority                INT NOT NULL DEFAULT 0,
    attempts                INT NOT NULL DEFAULT 0,
    max_attempts            INT NOT NULL DEFAULT 5,
    next_attempt_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_node_id           UUID REFERENCES worker_nodes (id) ON DELETE SET NULL,
    lease_session_id        UUID,
    lease_execution_id      UUID,
    lease_expires_at        TIMESTAMPTZ,
    active_source_asset_id  UUID REFERENCES source_assets (id) ON DELETE SET NULL,
    satisfied_holding_id    UUID REFERENCES holdings (id) ON DELETE SET NULL,
    last_error              TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (edition_id)
);

CREATE INDEX IF NOT EXISTS idx_acq_targets_claim ON acquisition_targets (status, priority DESC, next_attempt_at) WHERE status IN ('待下载', '排队中', '暂时失败');
CREATE INDEX IF NOT EXISTS idx_acq_targets_lease ON acquisition_targets (lease_expires_at) WHERE lease_expires_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_acq_targets_status ON acquisition_targets (status);

CREATE TABLE IF NOT EXISTS acquisition_executions (
    id                  UUID PRIMARY KEY,
    target_id           UUID NOT NULL REFERENCES acquisition_targets (id) ON DELETE CASCADE,
    source_asset_id     UUID REFERENCES source_assets (id) ON DELETE SET NULL,
    node_id             UUID REFERENCES worker_nodes (id) ON DELETE SET NULL,
    session_id          UUID,
    slot_index          INT,
    stage               TEXT NOT NULL DEFAULT '',
    result              TEXT CHECK (result IN ('成功', '失败', '取消', '超时', '校验失败')),
    error_code          TEXT,
    error_message       TEXT,
    started_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at         TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_acq_executions_target ON acquisition_executions (target_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_acq_executions_node ON acquisition_executions (node_id);

-- ============================================================ 5. 搜索 Outbox 队列

CREATE TABLE IF NOT EXISTS catalog_outbox (
    id                  BIGSERIAL PRIMARY KEY,
    event_type          TEXT NOT NULL,
    aggregate_type      TEXT NOT NULL,
    aggregate_id        UUID NOT NULL,
    payload             JSONB NOT NULL,
    status              TEXT NOT NULL DEFAULT '待同步' CHECK (status IN ('待同步', '已同步', '失败')),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    synced_at           TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_catalog_outbox_queue ON catalog_outbox (status, id) WHERE status = '待同步';
