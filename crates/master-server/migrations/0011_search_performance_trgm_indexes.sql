-- 图书馆总库检索性能优化迁移脚本 (v0.3.2)
--
-- 1. 启用 pg_trgm 扩展加速任意子串模糊搜索（ILIKE '%xxx%'）；
-- 2. 为书名、规范名、出版社建立 GIN 倒排三元组索引；
-- 3. 为 identifiers、editions(updated_at, id) 复合游标分页建立覆盖索引；
-- 4. 为 acquisition_targets 状态与调度建立高效部分索引。

CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- 1. 书名与出版信息 GIN 三元组索引（加速 ILIKE 模糊匹配）
CREATE INDEX IF NOT EXISTS idx_editions_title_trgm ON editions USING gin (edition_title gin_trgm_ops);
CREATE INDEX IF NOT EXISTS idx_works_norm_title_trgm ON works USING gin (normalized_title gin_trgm_ops);
CREATE INDEX IF NOT EXISTS idx_works_preferred_title_trgm ON works USING gin (preferred_title gin_trgm_ops);
CREATE INDEX IF NOT EXISTS idx_editions_publisher_trgm ON editions USING gin (publisher gin_trgm_ops);
CREATE INDEX IF NOT EXISTS idx_identifiers_raw_trgm ON identifiers USING gin (raw_value gin_trgm_ops);
CREATE INDEX IF NOT EXISTS idx_identifiers_norm_val ON identifiers (normalized_value);

-- 2. 游标分页复合覆盖索引
CREATE INDEX IF NOT EXISTS idx_editions_updated_at_id ON editions (updated_at DESC, id DESC);

-- 3. 贡献者作者模糊索引
CREATE INDEX IF NOT EXISTS idx_contributors_name_trgm ON contributors USING gin (name gin_trgm_ops);

-- 4. 采集目标状态与调度索引
CREATE INDEX IF NOT EXISTS idx_acquisition_targets_status_ed ON acquisition_targets (status, edition_id);
