-- 出版社专属书库按出版年份、更新时间分页时直接从索引读取，
-- 避免为获取一页数据扫描并排序该出版社的全部 editions。
CREATE INDEX IF NOT EXISTS idx_editions_publisher_browse
    ON editions (publisher_id, publish_year DESC NULLS LAST, updated_at DESC, id DESC)
    WHERE publisher_id IS NOT NULL;
