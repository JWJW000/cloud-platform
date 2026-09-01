-- 总库只表示“已经拥有”；下载批次中的候选书在成功前不进入总库。

-- 旧下载状态机中的 books 仅作为 Worker 任务载体；成功后通过此字段关联总库版本。
ALTER TABLE books
    ADD COLUMN IF NOT EXISTS catalog_edition_id UUID REFERENCES editions (id) ON DELETE SET NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_books_catalog_edition
    ON books (catalog_edition_id)
    WHERE catalog_edition_id IS NOT NULL;

-- catalog_bridge 过去使用 edition UUID 作为 books UUID，可直接回填已有关系。
UPDATE books b
SET catalog_edition_id = e.id
FROM editions e
WHERE b.id = e.id AND b.catalog_edition_id IS NULL;

-- 旧逻辑为每条总库版本自动建立获取目标。总库现在代表“已拥有”，这些派生目标
-- 不应继续排队。执行中的任务允许安全收尾，其余尚未执行的镜像任务统一取消。
-- 批量修正不为每一行制造 OpenSearch outbox 事件；查询层兼容解释旧索引中的状态，
-- 后续正常变更仍由重新启用的触发器增量同步。
ALTER TABLE acquisition_targets DISABLE TRIGGER acquisition_targets_catalog_outbox;

UPDATE book_tasks bt
SET status = '已取消',
    cancel_requested = TRUE,
    last_error = '总库已拥有，不再自动下载',
    lease_node_id = NULL,
    lease_session_id = NULL,
    lease_execution_id = NULL,
    lease_expires_at = NULL,
    updated_at = now()
FROM acquisition_targets at
WHERE bt.id = at.id
  AND bt.status IN ('待处理', '失败', '已跳过', '已取消', '待确认');

UPDATE batch_books bb
SET display_status = '已取消'
FROM book_tasks bt, acquisition_targets at
WHERE bt.id = at.id
  AND bb.book_id = bt.book_id
  AND bt.status = '已取消';

UPDATE acquisition_targets
SET status = '暂不获取',
    lease_node_id = NULL,
    lease_session_id = NULL,
    lease_execution_id = NULL,
    lease_expires_at = NULL,
    last_error = '总库已拥有，不再自动下载',
    updated_at = now()
WHERE status IN ('待下载', '排队中', '暂时失败', '来源无效', '人工确认');

ALTER TABLE acquisition_targets ENABLE TRIGGER acquisition_targets_catalog_outbox;

-- 出版社统计改为“拥有书目 + 当前有效文件”，不再把历史获取目标当成拥有关系。
WITH stats AS (
    SELECT e.publisher_id,
           count(DISTINCT e.work_id) AS works_c,
           count(DISTINCT e.id) AS editions_c,
           count(DISTINCT CASE WHEN lf.verify_status = '有效' THEN h.id END) AS files_c,
           count(DISTINCT CASE
               WHEN lf.verify_status = '有效' AND h.meets_strategy THEN e.id
           END) AS editions_with_files_c
    FROM editions e
    LEFT JOIN holdings h ON h.edition_id = e.id
    LEFT JOIN library_files lf ON lf.id = h.library_file_id
    WHERE e.publisher_id IS NOT NULL
    GROUP BY e.publisher_id
)
UPDATE publishers p
SET works_count = stats.works_c,
    editions_count = stats.editions_c,
    holdings_count = stats.files_c,
    acquired_count = stats.editions_with_files_c,
    updated_at = now()
FROM stats
WHERE p.id = stats.publisher_id;
