-- 下载站点搜索查询参数。空 extensions 表示继续按任务目标格式自动填充，
-- 从而与升级前的行为完全一致。
INSERT INTO settings (key, value)
VALUES (
    'download_search_options',
    '{"order":"bestmatch","extensions":[]}'::jsonb
)
ON CONFLICT (key) DO NOTHING;
