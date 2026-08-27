-- 将总库 acquisition_targets 复用到现有 Worker 下载状态机。
--
-- Worker 协议仍以 book_tasks 为任务载体。Master 会按需为总库目标创建同 ID 的
-- 镜像任务；本触发器把镜像任务的租约和状态原子同步回 acquisition_targets。

CREATE OR REPLACE FUNCTION sync_catalog_target_from_book_task()
RETURNS TRIGGER AS $$
BEGIN
    UPDATE acquisition_targets
    SET status = CASE NEW.status
            WHEN '待处理' THEN CASE
                WHEN NEW.last_error IS NULL OR NEW.last_error = '' THEN '待下载'
                ELSE '暂时失败'
            END
            WHEN '已分配' THEN '已领取'
            WHEN '执行中' THEN '下载中'
            WHEN '等待入库' THEN '校验中'
            WHEN '已完成' THEN '已下载'
            WHEN '失败' THEN '人工确认'
            WHEN '已跳过' THEN '来源无效'
            WHEN '已取消' THEN '暂不获取'
            WHEN '待确认' THEN '人工确认'
            ELSE acquisition_targets.status
        END,
        attempts = NEW.attempts,
        max_attempts = NEW.max_attempts,
        next_attempt_at = NEW.next_attempt_at,
        lease_node_id = NEW.lease_node_id,
        lease_session_id = NEW.lease_session_id,
        lease_execution_id = NEW.lease_execution_id,
        lease_expires_at = NEW.lease_expires_at,
        last_error = NEW.last_error,
        updated_at = now()
    WHERE id = NEW.id;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS book_tasks_sync_catalog_target ON book_tasks;
CREATE TRIGGER book_tasks_sync_catalog_target
AFTER INSERT OR UPDATE OF status, attempts, max_attempts, next_attempt_at,
    lease_node_id, lease_session_id, lease_execution_id, lease_expires_at, last_error
ON book_tasks
FOR EACH ROW EXECUTE FUNCTION sync_catalog_target_from_book_task();

