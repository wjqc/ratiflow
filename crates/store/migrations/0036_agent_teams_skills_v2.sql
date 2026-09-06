-- 0036：Agent Team 版本化与 Skill 不可变版本生命周期
-- （EvoFlow 方案 M4-01/04 / ADR-038；原 §7.2 0035 顺延至此，见状态区裁定 8）。
-- 版本冻结边界：Team/Skill 绑定一律指向具体 version；active 单份；事实只追加。

-- 1) Agent Team：逻辑身份 + 不可变版本（§6.8）。
CREATE TABLE agent_teams (
    id TEXT PRIMARY KEY,
    key TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE agent_team_versions (
    id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES agent_teams(id),
    version_no INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft','active','deprecated')),
    lead_role_key TEXT NOT NULL DEFAULT 'lead',
    max_concurrency INTEGER NOT NULL DEFAULT 3,
    required_caps_json TEXT NOT NULL DEFAULT '[]',
    review_policy TEXT NOT NULL DEFAULT 'none'
        CHECK (review_policy IN ('none','peer_review','lead_review')),
    fallback_mode TEXT NOT NULL DEFAULT 'generic'
        CHECK (fallback_mode IN ('generic','fail_closed')),
    content_digest TEXT NOT NULL,
    created_by TEXT NOT NULL DEFAULT 'local',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (team_id, version_no)
);

-- 单 active。
CREATE UNIQUE INDEX idx_atv_single_active
    ON agent_team_versions(team_id) WHERE status = 'active';

-- 成员：role_key → 具体 profile version（不可变绑定）。
CREATE TABLE agent_team_members (
    team_version_id TEXT NOT NULL REFERENCES agent_team_versions(id),
    role_key TEXT NOT NULL,
    profile_version_id TEXT NOT NULL REFERENCES agent_profile_versions(id),
    fallback_mode TEXT NOT NULL DEFAULT 'generic'
        CHECK (fallback_mode IN ('generic','fail_closed')),
    created_at TEXT NOT NULL,
    PRIMARY KEY (team_version_id, role_key)
);

-- 2) Skill 不可变版本（§6.9）：draft → active → deprecated → revoked；
--    更新正文创建新 version，不覆盖；revoked 立即阻止未来注入。
CREATE TABLE skill_versions (
    id TEXT PRIMARY KEY,
    skill_id TEXT NOT NULL REFERENCES skills(id),
    version_no INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft','active','deprecated','revoked')),
    body_object_sha256 TEXT NOT NULL,
    body_bytes INTEGER NOT NULL DEFAULT 0,
    description TEXT NOT NULL DEFAULT '',
    content_digest TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (skill_id, version_no)
);

CREATE UNIQUE INDEX idx_skv_single_active
    ON skill_versions(skill_id) WHERE status = 'active';

-- 版本级绑定：绑定指向具体 skill version；profile_version_id NULL = 全局。
CREATE TABLE skill_bindings_v2 (
    id TEXT PRIMARY KEY,
    skill_version_id TEXT NOT NULL REFERENCES skill_versions(id),
    profile_version_id TEXT REFERENCES agent_profile_versions(id),
    created_at TEXT NOT NULL,
    UNIQUE (skill_version_id, profile_version_id)
);

-- 3) Run 冻结 team ref（§3 不变量 8）；context/middleware refs 随 0037。
ALTER TABLE agent_runs ADD COLUMN team_version_id TEXT REFERENCES agent_team_versions(id);
