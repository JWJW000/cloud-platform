-- 保证一个节点槽位在同一时刻最多只有一个活跃（未结束）会话。
--
-- 1. 迁移前先清理历史异常留下的重复活跃会话：
--    对于同一 (node_id, slot_index) 存在多个 ended_at IS NULL 的会话，
--    仅保留最新的一条，其余更早的未结束会话标记为结束并记录原因。
UPDATE execution_sessions es
SET ended_at = now(),
    status = '已结束',
    end_reason = '迁移清理历史未正常收敛会话'
WHERE ended_at IS NULL
  AND id NOT IN (
      SELECT DISTINCT ON (node_id, slot_index) id
      FROM execution_sessions
      WHERE ended_at IS NULL
      ORDER BY node_id, slot_index, started_at DESC, id DESC
  );

-- 2. 创建槽位活跃会话部分唯一索引
CREATE UNIQUE INDEX IF NOT EXISTS uq_execution_sessions_active_node_slot
ON execution_sessions (node_id, slot_index)
WHERE ended_at IS NULL;
