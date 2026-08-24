-- 生产化修复 V5：节点显示名不再是唯一身份（第 6.3 节）
--
-- V5 直连注册的节点身份是「安装标识 + 公钥指纹」，「名称」只是可读的显示名。
-- 0001 中 worker_nodes.name 的 UNIQUE 约束会拒绝两台同名机器分别注册
-- （同一主机名/显示名是常态），而 upsert_node 的 ON CONFLICT (name) 更会把
-- 两台不同安装标识的机器悄悄合并成一条记录 —— 身份串号。
--
-- 因此：
-- 1. 删除 name 上的 UNIQUE 约束（历史重装语义改由应用层 find-then-update 承担）；
-- 2. 身份唯一性由 0004 的部分唯一索引（installation_id / public_key_fingerprint）保证。

ALTER TABLE worker_nodes DROP CONSTRAINT IF EXISTS worker_nodes_name_key;
