-- 迁移 0026：为批量有效标识读取增加 object_id 索引。
-- idx_identifiers_obj 以 object_type 为首列，不能覆盖 object_id=ANY(...) AND is_valid；
-- 生产可先用同名同定义的 CREATE INDEX CONCURRENTLY 建立，再由本迁移保留该索引。
CREATE INDEX IF NOT EXISTS idx_identifiers_valid_object_id
    ON identifiers (object_id)
    WHERE is_valid;
