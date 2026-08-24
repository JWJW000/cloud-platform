-- 生产化修复 V4 一致性迁移（第 17 节）
--
-- 约束规则：
-- 1. 不修改已部署的 0001 / 0002，只新增；
-- 2. 新增约束先检查历史数据，遇到非法值必须失败并报告记录编号，不得静默改成默认值；
-- 3. 所有业务枚举值的中文 CHECK 与 platform-domain 保持一致。

-- ============================================================ 1. users 补齐时间列
-- 历史迁移漏掉了 updated_at，而 set_user_status / set_user_password 都写它。
ALTER TABLE users ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

-- ============================================================ 2. 待确认任务的证据字段约束（第 12.1 / 17.2 节）
-- 期望大小：要么为空，要么非负
ALTER TABLE book_tasks DROP CONSTRAINT IF EXISTS book_tasks_expected_size_nonnegative;
ALTER TABLE book_tasks
    ADD CONSTRAINT book_tasks_expected_size_nonnegative
    CHECK (expected_size_bytes IS NULL OR expected_size_bytes >= 0);

-- 期望 SHA-256：要么为空，要么是 64 位十六进制
ALTER TABLE book_tasks DROP CONSTRAINT IF EXISTS book_tasks_expected_sha256_format;
ALTER TABLE book_tasks
    ADD CONSTRAINT book_tasks_expected_sha256_format
    CHECK (expected_sha256 IS NULL OR expected_sha256 ~ '^[0-9a-fA-F]{64}$');

-- 期望格式：与全局任务格式一致
ALTER TABLE book_tasks DROP CONSTRAINT IF EXISTS book_tasks_expected_format_valid;
ALTER TABLE book_tasks
    ADD CONSTRAINT book_tasks_expected_format_valid
    CHECK (expected_format IS NULL OR expected_format IN ('pdf', 'epub'));

-- 期望相对路径：不允许以 / 开头（禁止绝对路径进入证据）
ALTER TABLE book_tasks DROP CONSTRAINT IF EXISTS book_tasks_expected_path_relative;
ALTER TABLE book_tasks
    ADD CONSTRAINT book_tasks_expected_path_relative
    CHECK (expected_nas_relative_path IS NULL OR expected_nas_relative_path NOT LIKE '/%');

-- ============================================================ 3. 会话 token 哈希查询索引（第 17.1 节）
-- 0002 已建 idx_admin_sessions_token；此处确保存在（幂等），供按 jti 反查时走索引。
CREATE INDEX IF NOT EXISTS idx_admin_sessions_token ON admin_sessions (token_hash);

-- ============================================================ 4. Webshare 代理身份唯一约束（第 15.3 / 17.2 节）
-- 同一个 provider 的 external_id 必须唯一：避免同一代理因地址变化生成无法关联的重复记录。
-- 先检查历史数据：若存在重复 external_id，迁移必须失败而不是静默保留。
DO $$
DECLARE
    dup RECORD;
BEGIN
    FOR dup IN
        SELECT provider, external_id, count(*) AS n
        FROM proxies
        WHERE external_id IS NOT NULL
        GROUP BY provider, external_id
        HAVING count(*) > 1
        LIMIT 5
    LOOP
        RAISE EXCEPTION '迁移中止：proxies 存在重复 external_id（provider=%, external_id=%, 重复数=%），请先人工合并后再迁移',
            dup.provider, dup.external_id, dup.n;
    END LOOP;
END $$;

CREATE UNIQUE INDEX IF NOT EXISTS idx_proxies_provider_external_id
    ON proxies (provider, external_id)
    WHERE external_id IS NOT NULL;

-- ============================================================ 5. 同步世代索引（第 15.4 节）
CREATE INDEX IF NOT EXISTS idx_proxies_sync_generation ON proxies (sync_generation);

-- ============================================================ 6. 业务枚举中文 CHECK（第 3.4 节）
-- 0001 已带 CHECK；此处只兜底 0002 新增列的取值域。
ALTER TABLE proxies DROP CONSTRAINT IF EXISTS proxies_provider_valid_check;
ALTER TABLE proxies
    ADD CONSTRAINT proxies_provider_valid_check
    CHECK (provider_valid IN (TRUE, FALSE));

ALTER TABLE proxies DROP CONSTRAINT IF EXISTS proxies_retire_after_release_check;
ALTER TABLE proxies
    ADD CONSTRAINT proxies_retire_after_release_check
    CHECK (retire_after_release IN (TRUE, FALSE));

-- ============================================================ 7. 历史空值回填（第 17.1 节：可审计回填）
-- 已完成的旧任务若证据字段为空，回填自其自身 nas_relative_path / format，
-- 让「已完成」任务保留可追溯的期望路径（幂等，不覆盖已有证据）。
UPDATE book_tasks
   SET expected_nas_relative_path = nas_relative_path,
       expected_file_name = (
           SELECT regexp_replace(nas_relative_path, '^.*/', '')
           WHERE nas_relative_path IS NOT NULL
       ),
       expected_format = format
 WHERE status = '已完成'
   AND expected_nas_relative_path IS NULL
   AND nas_relative_path IS NOT NULL;
