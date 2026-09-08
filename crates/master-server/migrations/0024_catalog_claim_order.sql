-- 领取先按优先级与入队时间取一条，避免每次排序全部待下载目标。
CREATE INDEX IF NOT EXISTS idx_acq_targets_pending_order
ON acquisition_targets (priority DESC, next_attempt_at, created_at)
WHERE status IN ('待下载', '排队中', '暂时失败');
