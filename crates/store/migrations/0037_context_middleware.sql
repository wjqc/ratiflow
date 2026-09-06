-- 0037：Context Policy 与 Middleware Profile 版本化
-- （EvoFlow 方案 M4-05 / ADR-038 §6.10/§6.11）。
-- 最终工具集 = Template/Gate/Task/Profile/Autonomy 交集（服务端派生，客户端只可收紧）；
-- middleware 仅内建、配置只调顺序与参数，security 项顺序约束由服务层校验。

-- 1) Context Policy 版本：按 key（如 per-gate/全局）+ 不可变版本。
CREATE TABLE context_policy_versions (
    id TEXT PRIMARY KEY,
    key TEXT NOT NULL,
    version_no INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft','active','deprecated')),
    gate_id TEXT,
    sources_json TEXT NOT NULL DEFAULT '[]',
    allowed_tools_json TEXT NOT NULL DEFAULT '[]',
    compaction_json TEXT NOT NULL DEFAULT '{}',
    content_digest TEXT NOT NULL,
    created_by TEXT NOT NULL DEFAULT 'local',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (key, version_no)
);

CREATE UNIQUE INDEX idx_cpv_single_active
    ON context_policy_versions(key) WHERE status = 'active';

-- 2) Middleware Profile 版本：有序内建 hook 步骤（只调顺序与参数）。
CREATE TABLE middleware_profile_versions (
    id TEXT PRIMARY KEY,
    key TEXT NOT NULL,
    version_no INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft','active','deprecated')),
    steps_json TEXT NOT NULL DEFAULT '[]',
    content_digest TEXT NOT NULL,
    created_by TEXT NOT NULL DEFAULT 'local',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (key, version_no)
);

CREATE UNIQUE INDEX idx_mpv_single_active
    ON middleware_profile_versions(key) WHERE status = 'active';

-- 3) Run 冻结 refs（M2 从 0036 顺延的 context/middleware 两列）。
ALTER TABLE agent_runs ADD COLUMN context_policy_version_id TEXT REFERENCES context_policy_versions(id);
ALTER TABLE agent_runs ADD COLUMN middleware_profile_version_id TEXT REFERENCES middleware_profile_versions(id);
