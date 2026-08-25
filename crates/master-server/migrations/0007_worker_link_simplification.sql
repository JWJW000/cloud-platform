-- 生产化演进 V7：Worker–Master 连接简化实施方案（第 8 节）
--
-- 1. 增加 credential_mode 区分旧 Worker (token_and_certificate) 与新 Worker (certificate_only)；
-- 2. node_token_hash 改为允许 NULL，支持纯证书节点；
-- 3. 创建 worker_registration_requests 幂等注册请求表。

-- ============================================================ 1. 节点表增强
ALTER TABLE worker_nodes
    ADD COLUMN IF NOT EXISTS credential_mode TEXT NOT NULL DEFAULT 'token_and_certificate';

ALTER TABLE worker_nodes
    ALTER COLUMN node_token_hash DROP NOT NULL;

-- ============================================================ 2. 幂等注册请求表
CREATE TABLE IF NOT EXISTS worker_registration_requests (
    node_id                 UUID PRIMARY KEY REFERENCES worker_nodes(id) ON DELETE CASCADE,
    installation_id         UUID NOT NULL,
    csr_pem                 TEXT NOT NULL,
    public_key_fingerprint  TEXT NOT NULL,
    source_ip               TEXT,
    requested_slots         INTEGER NOT NULL,
    first_seen_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at              TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_worker_registration_requests_inst
    ON worker_registration_requests(installation_id);
CREATE INDEX IF NOT EXISTS idx_worker_registration_requests_fp
    ON worker_registration_requests(public_key_fingerprint);
