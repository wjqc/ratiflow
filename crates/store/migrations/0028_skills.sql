-- 0028_skills：技能（Agent 指令包，用户级）。元数据入库，正文存 objects（内容寻址，秘密扫描总闸）。
-- 启用的技能在 Run 装配时作为「技能段」注入提示词（profile 层之后，预算内）。
CREATE TABLE skills (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    body_object_sha256 TEXT NOT NULL,
    body_bytes INTEGER NOT NULL DEFAULT 0,
    enabled INTEGER NOT NULL DEFAULT 1,
    source TEXT NOT NULL DEFAULT 'manual' CHECK (source IN ('manual','import')),
    revision INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_skills_enabled ON skills(enabled, name);
