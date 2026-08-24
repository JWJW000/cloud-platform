-- 生产化修复 V5：Worker 直连注册（前端云端化与 Worker 直连注册修复实施方案 v5 第 6.6 节）
--
-- 原则：
-- 1. 向前兼容：不删除旧列/旧表（enroll_codes 保留至旧 Worker 全部升级）；
-- 2. 身份字段唯一约束为部分索引（旧节点这些列为 NULL，不影响）；
-- 3. 审批字段外键关联管理员；状态字段带 CHECK；审批更新使用行锁（在应用层事务内 FOR UPDATE）。

-- ============================================================ 1. worker_nodes 直连注册字段
ALTER TABLE worker_nodes ADD COLUMN IF NOT EXISTS installation_id UUID;
ALTER TABLE worker_nodes ADD COLUMN IF NOT EXISTS public_key_fingerprint TEXT;
ALTER TABLE worker_nodes ADD COLUMN IF NOT EXISTS registration_status TEXT NOT NULL DEFAULT '待审核'
    CHECK (registration_status IN ('待审核', '已批准', '已拒绝', '已过期'));
ALTER TABLE worker_nodes ADD COLUMN IF NOT EXISTS requested_slots INT;
ALTER TABLE worker_nodes ADD COLUMN IF NOT EXISTS configured_slots INT;
ALTER TABLE worker_nodes ADD COLUMN IF NOT EXISTS registration_expires_at TIMESTAMPTZ;
ALTER TABLE worker_nodes ADD COLUMN IF NOT EXISTS first_seen_ip TEXT;
ALTER TABLE worker_nodes ADD COLUMN IF NOT EXISTS last_registration_at TIMESTAMPTZ;
ALTER TABLE worker_nodes ADD COLUMN IF NOT EXISTS rejected_at TIMESTAMPTZ;
ALTER TABLE worker_nodes ADD COLUMN IF NOT EXISTS rejected_by UUID REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE worker_nodes ADD COLUMN IF NOT EXISTS reject_reason TEXT;

-- 槽位必须在系统允许范围内（第 6.6 节：1..50）
ALTER TABLE worker_nodes DROP CONSTRAINT IF EXISTS worker_nodes_requested_slots_range;
ALTER TABLE worker_nodes
    ADD CONSTRAINT worker_nodes_requested_slots_range
    CHECK (requested_slots IS NULL OR requested_slots BETWEEN 1 AND 50);
ALTER TABLE worker_nodes DROP CONSTRAINT IF EXISTS worker_nodes_configured_slots_range;
ALTER TABLE worker_nodes
    ADD CONSTRAINT worker_nodes_configured_slots_range
    CHECK (configured_slots IS NULL OR configured_slots BETWEEN 1 AND 50);

-- 唯一身份：安装标识 / 公钥指纹（旧节点为 NULL，不受约束）
CREATE UNIQUE INDEX IF NOT EXISTS idx_worker_nodes_installation
    ON worker_nodes (installation_id) WHERE installation_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_worker_nodes_fingerprint
    ON worker_nodes (public_key_fingerprint) WHERE public_key_fingerprint IS NOT NULL;

-- 历史回填：非「待审核」的既有节点视为已批准（直连注册引入前的节点都经过注册码+审核）
UPDATE worker_nodes SET registration_status = '已批准'
 WHERE registration_status = '待审核' AND status <> '待审核';

-- ============================================================ 2. 注册会话表
CREATE TABLE IF NOT EXISTS worker_registration_sessions (
    id              UUID PRIMARY KEY,
    node_id         UUID NOT NULL REFERENCES worker_nodes (id) ON DELETE CASCADE,
    token_hash      TEXT NOT NULL UNIQUE,
    csr_pem         TEXT NOT NULL,
    csr_fingerprint TEXT NOT NULL,
    challenge       TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT '待审核'
                    CHECK (status IN ('待审核', '已批准', '已拒绝', '已过期', '已领取')),
    -- 批准后待一次性下发的节点令牌（明文）。仅存在于会话行，领用即清空；
    -- 注册会话令牌本身只存哈希（第 6.4 节）。
    pending_node_token TEXT,
    expires_at      TIMESTAMPTZ NOT NULL,
    attempt_count   INT NOT NULL DEFAULT 0,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_reg_sessions_node ON worker_registration_sessions (node_id, status);
CREATE INDEX IF NOT EXISTS idx_reg_sessions_fingerprint ON worker_registration_sessions (csr_fingerprint);
