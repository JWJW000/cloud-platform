-- 已获授权：把误作已拥有导入的指定书单转换为待下载用途。
-- 单事务、可重复执行；不重置已有目标、任务、租约或馆藏文件。
BEGIN;
SET LOCAL statement_timeout = '120s';
CREATE TEMP TABLE repair_editions ON COMMIT DROP AS
SELECT DISTINCT e.id, r.import_file_id, r.started_at, r.completed_at
FROM import_runs r
JOIN import_files f ON f.id = r.import_file_id
JOIN source_records sr ON sr.import_file_id = f.id
JOIN record_resolutions rr ON rr.source_record_id = sr.id
JOIN editions e ON e.id = rr.edition_id
WHERE r.id = 'eadfde3c-9e41-49f8-ac02-6f8b0a3a172c'
  AND f.file_path = 'SN_Medicine_Books_for_download.csv'
  AND r.status = '已完成' AND r.total_rows = 27523;
CREATE UNIQUE INDEX ON repair_editions(id);
ANALYZE repair_editions;
DO $$ BEGIN
 IF (SELECT count(*) FROM repair_editions) <> 27523 THEN
   RAISE EXCEPTION 'Repair scope mismatch: expected exactly 27523 editions';
 END IF;
END $$;
-- 仅撤销该次导入中新建且没有其他来源或有效馆藏支持的已拥有标记。
CREATE TEMP TABLE repair_demoted ON COMMIT DROP AS
WITH changed AS (
UPDATE editions e SET owned_at = NULL, updated_at = now()
FROM repair_editions r
WHERE e.id = r.id AND e.created_at BETWEEN r.started_at AND r.completed_at
  AND e.owned_at BETWEEN r.started_at AND r.completed_at
  AND NOT EXISTS (SELECT 1 FROM holdings h JOIN library_files lf ON lf.id = h.library_file_id
                  WHERE h.edition_id = e.id AND lf.verify_status = '有效')
  AND NOT EXISTS (SELECT 1 FROM record_resolutions rr JOIN source_records sr ON sr.id = rr.source_record_id
                  WHERE rr.edition_id = e.id AND sr.import_file_id <> r.import_file_id)
RETURNING e.id, e.work_id
) SELECT * FROM changed;
INSERT INTO acquisition_targets(id, edition_id, status, priority)
SELECT gen_random_uuid(), r.id, '待下载', 0 FROM repair_editions r
WHERE NOT EXISTS (SELECT 1 FROM holdings h JOIN library_files lf ON lf.id = h.library_file_id
                  WHERE h.edition_id = r.id AND h.meets_strategy AND lf.verify_status = '有效')
ON CONFLICT (edition_id) DO NOTHING;
UPDATE import_runs SET import_mode = 'download', updated_at = now()
WHERE id = 'eadfde3c-9e41-49f8-ac02-6f8b0a3a172c';
INSERT INTO catalog_outbox(event_type, aggregate_type, aggregate_id, payload, status)
SELECT 'catalog.edition_indexed', 'edition', e.id,
       jsonb_build_object('edition_id', e.id, 'work_id', e.work_id), '待同步'
FROM repair_demoted e;
SELECT at.status, count(*) FROM repair_editions r
LEFT JOIN acquisition_targets at ON at.edition_id = r.id GROUP BY at.status;
COMMIT;
