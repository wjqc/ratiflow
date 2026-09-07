-- 0045：技能市场源（可配置）。源 = 本机遵循 ZCode 插件市场目录布局的根目录
-- （known_marketplaces.json + installed_plugins.json + cache/）；不内置任何网络市场。
-- 首次浏览惰性播种两个默认源（zcode-plugins-official / claude-plugins-official），均可编辑删除。

-- skills.source 放宽加入 'market'（0028 的 CHECK 不含市场导入）。标准重建流程：
-- 迁移事务内 FK 关闭（migration.rs 保证），skill_versions → skills(id) 引用在
-- 重命名后继续指向同名表。
CREATE TABLE skills_rebuild_0045 (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    body_object_sha256 TEXT NOT NULL,
    body_bytes INTEGER NOT NULL DEFAULT 0,
    enabled INTEGER NOT NULL DEFAULT 1,
    source TEXT NOT NULL DEFAULT 'manual' CHECK (source IN ('manual','import','market')),
    revision INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    agent_profile_id TEXT REFERENCES agent_profiles(id)
);
INSERT INTO skills_rebuild_0045
    SELECT id, name, description, body_object_sha256, body_bytes, enabled, source,
           revision, created_at, updated_at, agent_profile_id
    FROM skills;
DROP TABLE skills;
ALTER TABLE skills_rebuild_0045 RENAME TO skills;
CREATE INDEX idx_skills_enabled ON skills(enabled, name);

CREATE TABLE skill_market_sources (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'zcode_local',
    root_path TEXT NOT NULL,
    marketplace_id TEXT NOT NULL DEFAULT '',
    enabled INTEGER NOT NULL DEFAULT 1,
    revision INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
