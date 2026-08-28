-- 全局下载调度开关。
--
-- 固定种子行使调度领取可以用 FOR SHARE 锁住它；管理员切换时使用 FOR UPDATE，
-- 从而保证“暂停接口返回成功”之后不会再有并发领取穿透。
INSERT INTO settings (key, value)
VALUES ('global_download_paused', 'false'::jsonb)
ON CONFLICT (key) DO NOTHING;
