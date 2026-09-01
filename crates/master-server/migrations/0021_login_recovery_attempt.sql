-- 登录失败账号只允许自动恢复一次。恢复仍失败后保留时间戳，
-- 调度器会切换下一账号，不再反复打开同一个错误账号。
ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS login_recovery_attempted_at TIMESTAMPTZ;
