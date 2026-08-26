-- 图书馆总库已有馆藏扫描与物理副本多位置架构迁移脚本（实施方案 V1）
--
-- 原则：
-- 1. 存储位置（storage_locations）：抽象节点存储根，使用稳定别名（root_key）隔离本地绝对路径；
-- 2. 物理副本（library_file_locations）：支持同一 SHA-256 内容实体在多个 Worker / NAS 副本共存；
-- 3. 扫描任务与暂存（inventory_scan_jobs / entries）：支持断点续扫、批量上报与去重；
-- 4. 待确认候选（inventory_match_candidates）：支持多候选置信度裁决与人工确认；
-- 5. 零停机迁移：自动回填现有 library_files 记录至默认 legacy 存储位置。

-- ============================================================ 1. 存储位置表
CREATE TABLE IF NOT EXISTS storage_locations (
    id                  UUID PRIMARY KEY,
    node_id             UUID REFERENCES worker_nodes(id) ON DELETE SET NULL,
    root_key            TEXT NOT NULL,
    backend             TEXT NOT NULL CHECK (backend IN ('Local', 'NAS', 'S3', 'OSS')),
    display_name        TEXT NOT NULL,
    availability        TEXT NOT NULL DEFAULT '未知'
                        CHECK (availability IN ('在线', '离线', '未知', '已停用')),
    last_seen_at        TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (node_id, root_key)
);

CREATE INDEX IF NOT EXISTS idx_storage_locations_node ON storage_locations (node_id);
CREATE INDEX IF NOT EXISTS idx_storage_locations_avail ON storage_locations (availability);

-- ============================================================ 2. 文件物理副本表
CREATE TABLE IF NOT EXISTS library_file_locations (
    id                  UUID PRIMARY KEY,
    library_file_id     UUID NOT NULL REFERENCES library_files(id) ON DELETE CASCADE,
    storage_location_id UUID NOT NULL REFERENCES storage_locations(id) ON DELETE CASCADE,
    object_key          TEXT NOT NULL,
    actual_size_bytes   BIGINT NOT NULL,
    verify_status       TEXT NOT NULL DEFAULT '待校验'
                        CHECK (verify_status IN ('待校验', '有效', '损坏', '丢失', '离线')),
    verified_at         TIMESTAMPTZ,
    last_seen_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (storage_location_id, object_key)
);

CREATE INDEX IF NOT EXISTS idx_file_locations_file ON library_file_locations (library_file_id);
CREATE INDEX IF NOT EXISTS idx_file_locations_loc ON library_file_locations (storage_location_id);
CREATE INDEX IF NOT EXISTS idx_file_locations_status ON library_file_locations (verify_status);

-- ============================================================ 3. 扫描任务与检查点
CREATE TABLE IF NOT EXISTS inventory_scan_jobs (
    id                  UUID PRIMARY KEY,
    node_id             UUID NOT NULL REFERENCES worker_nodes(id),
    storage_location_id UUID NOT NULL REFERENCES storage_locations(id),
    status              TEXT NOT NULL CHECK (status IN ('待下发', '扫描中', '暂停', '已完成', '部分失败', '已取消', '失败')),
    scan_mode           TEXT NOT NULL CHECK (scan_mode IN ('增量', '全量复核')),
    checkpoint          JSONB NOT NULL DEFAULT '{}'::jsonb,
    discovered_count    BIGINT NOT NULL DEFAULT 0,
    hashed_count        BIGINT NOT NULL DEFAULT 0,
    matched_count       BIGINT NOT NULL DEFAULT 0,
    review_count        BIGINT NOT NULL DEFAULT 0,
    unmatched_count     BIGINT NOT NULL DEFAULT 0,
    skipped_count       BIGINT NOT NULL DEFAULT 0,
    error_count         BIGINT NOT NULL DEFAULT 0,
    started_at          TIMESTAMPTZ,
    finished_at         TIMESTAMPTZ,
    last_error          TEXT,
    created_by          UUID REFERENCES users(id),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_inventory_scan_jobs_node ON inventory_scan_jobs (node_id, status);
CREATE INDEX IF NOT EXISTS idx_inventory_scan_jobs_status ON inventory_scan_jobs (status, created_at DESC);

-- ============================================================ 4. 扫描条目暂存表
CREATE TABLE IF NOT EXISTS inventory_scan_entries (
    id                  UUID PRIMARY KEY,
    scan_job_id         UUID NOT NULL REFERENCES inventory_scan_jobs(id) ON DELETE CASCADE,
    storage_location_id UUID NOT NULL REFERENCES storage_locations(id),
    object_key          TEXT NOT NULL,
    file_name           TEXT NOT NULL,
    extension           TEXT NOT NULL,
    actual_size_bytes   BIGINT NOT NULL,
    modified_at         TIMESTAMPTZ,
    sha256              TEXT NOT NULL,
    md5                 TEXT,
    embedded_metadata   JSONB NOT NULL DEFAULT '{}'::jsonb,
    resolution_status   TEXT NOT NULL CHECK (resolution_status IN ('待处理', '已匹配', '待确认', '未匹配', '已忽略', '失败')),
    matched_edition_id  UUID REFERENCES editions(id),
    match_method        TEXT,
    match_score         INT,
    error_reason        TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (scan_job_id, object_key)
);

CREATE INDEX IF NOT EXISTS idx_scan_entries_job ON inventory_scan_entries (scan_job_id, resolution_status);
CREATE INDEX IF NOT EXISTS idx_scan_entries_sha ON inventory_scan_entries (sha256);
CREATE INDEX IF NOT EXISTS idx_scan_entries_edition ON inventory_scan_entries (matched_edition_id) WHERE matched_edition_id IS NOT NULL;

-- ============================================================ 5. 待确认候选表
CREATE TABLE IF NOT EXISTS inventory_match_candidates (
    id                  UUID PRIMARY KEY,
    scan_entry_id       UUID NOT NULL REFERENCES inventory_scan_entries(id) ON DELETE CASCADE,
    edition_id          UUID NOT NULL REFERENCES editions(id) ON DELETE CASCADE,
    match_score         INT NOT NULL,
    matched_fields      JSONB NOT NULL DEFAULT '[]'::jsonb,
    conflict_fields     JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (scan_entry_id, edition_id)
);

CREATE INDEX IF NOT EXISTS idx_match_candidates_entry ON inventory_match_candidates (scan_entry_id);
CREATE INDEX IF NOT EXISTS idx_match_candidates_edition ON inventory_match_candidates (edition_id);

-- ============================================================ 6. 现有 library_files 数据回填
-- 创建默认 legacy 存储位置并为已有 library_files 建立物理副本映射
DO $$
DECLARE
    legacy_loc_id UUID := '00000000-0000-0000-0000-000000000001'::uuid;
BEGIN
    INSERT INTO storage_locations (id, node_id, root_key, backend, display_name, availability)
    VALUES (legacy_loc_id, NULL, 'legacy_root', 'NAS', '默认存储 (Legacy)', '在线')
    ON CONFLICT (node_id, root_key) DO NOTHING;

    INSERT INTO library_file_locations (
        id,
        library_file_id,
        storage_location_id,
        object_key,
        actual_size_bytes,
        verify_status,
        verified_at,
        last_seen_at
    )
    SELECT
        gen_random_uuid(),
        lf.id,
        legacy_loc_id,
        lf.object_key,
        lf.actual_size_bytes,
        lf.verify_status,
        lf.verified_at,
        lf.created_at
    FROM library_files lf
    ON CONFLICT (storage_location_id, object_key) DO NOTHING;
END $$;
