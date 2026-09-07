-- 只创建小型快照表；百万级统计由运行时后台刷新，不阻塞迁移和启动。
CREATE TABLE query_snapshots (
    snapshot_key TEXT PRIMARY KEY,
    payload JSONB NOT NULL,
    computed_at TIMESTAMPTZ NOT NULL
);
