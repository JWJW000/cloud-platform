-- ============================================================
-- 0016_publishers_management.sql: 出版社管理与图书总库关联升级
-- ============================================================

-- 1. 出版社主表
CREATE TABLE IF NOT EXISTS publishers (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name                TEXT NOT NULL,
    normalized_name     TEXT NOT NULL UNIQUE,
    country             TEXT,
    website             TEXT,
    description         TEXT,
    works_count         BIGINT NOT NULL DEFAULT 0,
    editions_count      BIGINT NOT NULL DEFAULT 0,
    holdings_count      BIGINT NOT NULL DEFAULT 0,
    acquired_count      BIGINT NOT NULL DEFAULT 0,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_publishers_norm ON publishers (normalized_name);
CREATE INDEX IF NOT EXISTS idx_publishers_counts ON publishers (editions_count DESC, acquired_count DESC);
CREATE INDEX IF NOT EXISTS idx_publishers_name_trgm ON publishers USING gin (name gin_trgm_ops);

-- 2. 出版社别名表
CREATE TABLE IF NOT EXISTS publisher_aliases (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    publisher_id        UUID NOT NULL REFERENCES publishers (id) ON DELETE CASCADE,
    alias_name          TEXT NOT NULL,
    normalized_alias    TEXT NOT NULL UNIQUE,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_publisher_aliases_publisher ON publisher_aliases (publisher_id);
CREATE INDEX IF NOT EXISTS idx_publisher_aliases_norm ON publisher_aliases (normalized_alias);

-- 3. 版本表关联扩展
ALTER TABLE editions ADD COLUMN IF NOT EXISTS publisher_id UUID REFERENCES publishers (id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS idx_editions_publisher_id ON editions (publisher_id) WHERE publisher_id IS NOT NULL;
