-- 0019_agent_profiles: ADR-030 M4 阶段 Agent。
-- AgentProfile 版本化冻结（persona/SOP/能力/工具/输出 schema/模型路由/预算/健康策略）；
-- 阶段 activity 绑定与选路记录；agent_runs 增加关卡绑定列（''=非关卡运行的诚实哨兵，
-- 应用层校验关卡运行必须四列齐备——蓝图 SG-AGT-007 客户端不可伪造，装配在服务端）。

CREATE TABLE agent_profiles (
    id TEXT PRIMARY KEY,
    project_id TEXT REFERENCES projects(id), -- NULL = 全局
    name TEXT NOT NULL,
    adapter_kind TEXT NOT NULL CHECK (adapter_kind IN ('local_harness', 'external_agent')),
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_profiles_project ON agent_profiles(project_id);

CREATE TABLE agent_profile_versions (
    id TEXT PRIMARY KEY,
    profile_id TEXT NOT NULL REFERENCES agent_profiles(id),
    version_no INTEGER NOT NULL,
    persona_object_sha256 TEXT NOT NULL DEFAULT '',
    sop_object_sha256 TEXT NOT NULL DEFAULT '',
    capabilities_json TEXT NOT NULL DEFAULT '[]',
    tool_policy_json TEXT NOT NULL DEFAULT '{}',
    output_schema_sha256 TEXT NOT NULL DEFAULT '',
    model_route_json TEXT NOT NULL DEFAULT '{}',
    budget_json TEXT NOT NULL DEFAULT '{}',
    health_policy_json TEXT NOT NULL DEFAULT '{}',
    content_digest TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(profile_id, version_no),
    UNIQUE(profile_id, content_digest)
);

CREATE TABLE stage_agent_bindings (
    id TEXT PRIMARY KEY,
    project_id TEXT REFERENCES projects(id), -- NULL = 全局
    gate TEXT NOT NULL,
    activity_key TEXT NOT NULL,
    profile_version_id TEXT NOT NULL REFERENCES agent_profile_versions(id),
    fallback_mode TEXT NOT NULL DEFAULT 'generic' CHECK (fallback_mode IN ('generic', 'fail_closed')),
    priority INTEGER NOT NULL DEFAULT 0,
    enabled INTEGER NOT NULL DEFAULT 1,
    revision INTEGER NOT NULL DEFAULT 1,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_bindings_lookup ON stage_agent_bindings(project_id, gate, activity_key, priority);

CREATE TABLE agent_selections (
    id TEXT PRIMARY KEY,
    -- 引用 stage_activities(id)；由应用层保证（选路仅经 stage.startActivity 写入，
    -- 单测/预览使用合成 id，与 agent_runs 哨兵列同一约定）。
    stage_activity_id TEXT NOT NULL,
    requested_profile_version_id TEXT REFERENCES agent_profile_versions(id),
    resolved_profile_version_id TEXT NOT NULL REFERENCES agent_profile_versions(id),
    -- task_override / project_binding / global_binding / builtin_generic
    source_scope TEXT NOT NULL,
    fallback_used INTEGER NOT NULL DEFAULT 0,
    reason_code TEXT NOT NULL DEFAULT '',
    candidate_report_json TEXT NOT NULL DEFAULT '[]',
    selection_digest TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_selections_activity ON agent_selections(stage_activity_id);

ALTER TABLE agent_runs ADD COLUMN stage_attempt_id TEXT NOT NULL DEFAULT '';
ALTER TABLE agent_runs ADD COLUMN stage_activity_id TEXT NOT NULL DEFAULT '';
ALTER TABLE agent_runs ADD COLUMN agent_selection_id TEXT NOT NULL DEFAULT '';
ALTER TABLE agent_runs ADD COLUMN input_snapshot_id TEXT NOT NULL DEFAULT '';
