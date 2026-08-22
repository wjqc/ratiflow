-- 0001_init: 基础元数据表（与 Epic 0 骨架兼容，历史骨架库可被迁移器接管）
CREATE TABLE IF NOT EXISTS app_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
INSERT INTO app_meta(key, value) VALUES ('schema_version', '1') ON CONFLICT(key) DO NOTHING;
