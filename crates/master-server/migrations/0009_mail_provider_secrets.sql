-- 邮件 Provider 密钥与普通 settings JSON 分离；内容使用 Master 的 AES-GCM 字段密钥加密。
CREATE TABLE IF NOT EXISTS mail_provider_secrets (
    secret_ref      TEXT PRIMARY KEY,
    cipher_text     TEXT NOT NULL,
    created_by      TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

COMMENT ON TABLE mail_provider_secrets IS
    'Outlook API Key 密钥存储；settings 仅保存 secret_ref，不保存明文或密文';

ALTER TABLE worker_nodes
    ADD COLUMN IF NOT EXISTS applied_mail_provider_version BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS mail_provider_name TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS mail_provider_health TEXT NOT NULL DEFAULT '';

-- 老版本曾允许保存人工输入；升级时主动清除历史验证码，之后代码路径始终写 NULL。
UPDATE manual_actions
SET input_code = NULL, updated_at = now()
WHERE action_type = '邮箱验证码' AND input_code IS NOT NULL;

-- 一个注册任务同一时刻只能有一个邮箱验证码待办，重放与自动降级不会制造重复事项。
-- 老版本若已经产生重复待办，迁移时保留最新一条，其余安全地标记为已取消。
WITH ranked_pending_mail_codes AS (
    SELECT id,
           row_number() OVER (
               PARTITION BY registration_task_id, action_type
               ORDER BY created_at DESC, id DESC
           ) AS position
    FROM manual_actions
    WHERE status = '待处理'
      AND registration_task_id IS NOT NULL
      AND action_type = '邮箱验证码'
)
UPDATE manual_actions
SET status = '已取消', updated_at = now()
WHERE id IN (
    SELECT id FROM ranked_pending_mail_codes WHERE position > 1
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_manual_actions_one_pending_mail_code
    ON manual_actions (registration_task_id, action_type)
    WHERE status = '待处理' AND registration_task_id IS NOT NULL AND action_type = '邮箱验证码';
