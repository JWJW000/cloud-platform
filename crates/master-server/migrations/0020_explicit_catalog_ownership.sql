-- 显式区分“书目候选”和“已经拥有”。版本元数据可以服务下载调度，但只有
-- owned_at 非空的版本才进入我的书目总库、出版社统计和 OpenSearch 索引。

ALTER TABLE editions ADD COLUMN IF NOT EXISTS owned_at TIMESTAMPTZ;

-- 既有总库默认沿用原入库时间，避免部署当天的“今日新增”被历史数据冲高。
-- owned_at 本身不进入搜索文档，批量回填期间关闭触发器，最终只为候选版本生成
-- 一次删除事件，避免 6 万条数据产生两轮重复 Outbox。
ALTER TABLE editions DISABLE TRIGGER editions_catalog_outbox;
UPDATE editions SET owned_at = created_at WHERE owned_at IS NULL;
ALTER TABLE editions ALTER COLUMN owned_at SET DEFAULT now();

CREATE INDEX IF NOT EXISTS idx_editions_owned_updated
    ON editions (updated_at DESC, id DESC)
    WHERE owned_at IS NOT NULL;

-- Oxford-Academic-待下载书单.csv 是历史待下载候选，不是已拥有清单。
-- 已有有效文件的版本以及同时被其他数据源证明拥有的版本保持 owned。
WITH oxford_candidates AS (
    SELECT DISTINCT rr.edition_id
    FROM catalog_sources cs
    JOIN import_files f ON f.source_id = cs.id
    JOIN source_records sr ON sr.import_file_id = f.id
    JOIN record_resolutions rr ON rr.source_record_id = sr.id
    WHERE cs.name = '牛津'
      AND f.file_path = 'Oxford-Academic-待下载书单.csv'
), unowned AS (
    SELECT c.edition_id
    FROM oxford_candidates c
    WHERE NOT EXISTS (
              SELECT 1
              FROM holdings h
              JOIN library_files lf ON lf.id = h.library_file_id
              WHERE h.edition_id = c.edition_id AND lf.verify_status = '有效'
          )
      AND NOT EXISTS (
              SELECT 1
              FROM record_resolutions rr
              JOIN source_records sr ON sr.id = rr.source_record_id
              JOIN import_files f ON f.id = sr.import_file_id
              JOIN catalog_sources cs ON cs.id = f.source_id
              WHERE rr.edition_id = c.edition_id
                AND NOT (cs.name = '牛津' AND f.file_path = 'Oxford-Academic-待下载书单.csv')
          )
)
UPDATE editions e
SET owned_at = NULL, updated_at = now()
FROM unowned u
WHERE e.id = u.edition_id;

ALTER TABLE editions ENABLE TRIGGER editions_catalog_outbox;

INSERT INTO catalog_outbox (event_type, aggregate_type, aggregate_id, payload, status)
SELECT 'catalog.ownership_changed', 'edition', e.id, '{}'::jsonb, '待同步'
FROM editions e
WHERE e.owned_at IS NULL;

-- 为候选版本准备旧 Worker 协议所需的 books 载体。候选仍保留 editions 元数据，
-- 但 owned_at 为空，因此不会污染总库；下载成功后由应用层原子转为已拥有。
WITH unowned AS (
    SELECT e.id, e.work_id, e.edition_title, e.publisher
    FROM editions e
    WHERE e.owned_at IS NULL
)
INSERT INTO books (
    id, raw_title, raw_author, raw_publisher, raw_isbn,
    normalized_title, normalized_author, normalized_publisher, normalized_isbn,
    dedup_key, verify_status, catalog_edition_id
)
SELECT u.id,
       u.edition_title,
       author.name,
       u.publisher,
       isbn.raw_value,
       w.normalized_title,
       author.normalized_name,
       CASE WHEN u.publisher IS NULL THEN NULL
            ELSE regexp_replace(lower(trim(u.publisher)), '[^a-z0-9\u4e00-\u9fa5]', '', 'g') END,
       isbn.normalized_value,
       'catalog-edition:' || u.id::text,
       '已确认',
       u.id
FROM unowned u
JOIN works w ON w.id = u.work_id
LEFT JOIN LATERAL (
    SELECT c.name, c.normalized_name
    FROM edition_contributors ec
    JOIN contributors c ON c.id = ec.contributor_id
    WHERE ec.edition_id = u.id
    ORDER BY ec.sort_order
    LIMIT 1
) author ON TRUE
LEFT JOIN LATERAL (
    SELECT i.raw_value, i.normalized_value
    FROM identifiers i
    WHERE i.object_type = 'edition' AND i.object_id = u.id AND i.is_valid
      AND i.identifier_type IN ('isbn13', 'isbn10')
    ORDER BY (i.identifier_type = 'isbn13') DESC
    LIMIT 1
) isbn ON TRUE
ON CONFLICT (id) DO UPDATE SET
    raw_title = EXCLUDED.raw_title,
    raw_author = EXCLUDED.raw_author,
    raw_publisher = EXCLUDED.raw_publisher,
    raw_isbn = EXCLUDED.raw_isbn,
    normalized_title = EXCLUDED.normalized_title,
    normalized_author = EXCLUDED.normalized_author,
    normalized_publisher = EXCLUDED.normalized_publisher,
    normalized_isbn = EXCLUDED.normalized_isbn,
    catalog_edition_id = EXCLUDED.catalog_edition_id,
    updated_at = now();

-- 重新开放候选获取目标；活跃执行若存在则让其安全收尾，不抢租约。
UPDATE acquisition_targets at
SET status = '待下载', attempts = 0, next_attempt_at = now(),
    lease_node_id = NULL, lease_session_id = NULL, lease_execution_id = NULL,
    lease_expires_at = NULL, active_source_asset_id = NULL,
    satisfied_holding_id = NULL, last_error = NULL, updated_at = now()
FROM editions e
WHERE at.edition_id = e.id AND e.owned_at IS NULL
  AND at.status NOT IN ('已领取', '下载中', '校验中');

-- 一次性物化全部候选，让后台立即显示完整任务总数，而不是随着 Worker 领取逐条增长。
INSERT INTO book_tasks (id, book_id, format, status, attempts, max_attempts, next_attempt_at)
SELECT at.id, at.edition_id, 'pdf', '待处理', 0, at.max_attempts, now()
FROM acquisition_targets at
JOIN editions e ON e.id = at.edition_id
WHERE e.owned_at IS NULL
ON CONFLICT (id) DO UPDATE SET
    status = '待处理', attempts = 0, next_attempt_at = now(), stage = '',
    stage_version = book_tasks.stage_version + 1, cancel_requested = FALSE,
    lease_node_id = NULL, lease_session_id = NULL, lease_execution_id = NULL,
    lease_expires_at = NULL, last_error = NULL, updated_at = now()
WHERE book_tasks.status NOT IN ('已分配', '执行中', '等待入库');

INSERT INTO download_batches (id, name, source_file, status, priority, download_format)
VALUES ('00000000-0000-0000-0000-0000000000ac', '牛津待下载任务（PDF）',
        'Oxford-Academic-待下载书单.csv', '执行中', 0, 'pdf')
ON CONFLICT (id) DO UPDATE SET
    name = EXCLUDED.name,
    source_file = EXCLUDED.source_file,
    status = CASE WHEN download_batches.status IN ('已暂停', '已取消')
                  THEN download_batches.status ELSE '执行中' END,
    updated_at = now();

INSERT INTO batch_books (id, batch_id, book_id, import_line, display_status)
SELECT gen_random_uuid(), '00000000-0000-0000-0000-0000000000ac', bt.book_id,
       LEAST(b.seq, 2147483647)::int, bt.status
FROM book_tasks bt
JOIN books b ON b.id = bt.book_id
JOIN editions e ON e.id = b.catalog_edition_id
WHERE e.owned_at IS NULL AND bt.format = 'pdf'
ON CONFLICT (batch_id, book_id) DO UPDATE SET display_status = EXCLUDED.display_status;

-- 出版社统计只计算已拥有版本；候选版本仍可用于任务搜索，但不进入我的总库数字。
WITH stats AS (
    SELECT p.id AS publisher_id,
           count(DISTINCT e.work_id) FILTER (WHERE e.owned_at IS NOT NULL) AS works_c,
           count(DISTINCT e.id) FILTER (WHERE e.owned_at IS NOT NULL) AS editions_c,
           count(DISTINCT CASE WHEN e.owned_at IS NOT NULL AND lf.verify_status = '有效' THEN h.id END) AS files_c,
           count(DISTINCT CASE WHEN e.owned_at IS NOT NULL AND lf.verify_status = '有效' AND h.meets_strategy THEN e.id END) AS editions_with_files_c
    FROM publishers p
    LEFT JOIN editions e ON e.publisher_id = p.id
    LEFT JOIN holdings h ON h.edition_id = e.id
    LEFT JOIN library_files lf ON lf.id = h.library_file_id
    GROUP BY p.id
)
UPDATE publishers p
SET works_count = stats.works_c,
    editions_count = stats.editions_c,
    holdings_count = stats.files_c,
    acquired_count = stats.editions_with_files_c,
    updated_at = now()
FROM stats
WHERE p.id = stats.publisher_id;
