-- 0015_v3_settings: 设置域（ZCode 手册 §5）
-- app_settings：全局键 project_id=''；项目覆盖键带 project_id；仅非秘密值；revision 乐观锁。
CREATE TABLE app_settings (
    key TEXT NOT NULL,
    scope TEXT NOT NULL DEFAULT 'global',
    project_id TEXT NOT NULL DEFAULT '',
    value_json TEXT NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1,
    updated_at TEXT NOT NULL,
    updated_by TEXT NOT NULL DEFAULT 'local',
    PRIMARY KEY (scope, project_id, key)
);

-- credential_refs：v2 的旧结构（project_id/kind/ref）保留为 legacy；新结构只存 Keychain 定位引用。
ALTER TABLE credential_refs RENAME TO credential_refs_legacy;
CREATE TABLE credential_refs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('gitlab_token','model_api_key','ssh_key','generic_secret')),
    provider TEXT NOT NULL DEFAULT '',
    keychain_service TEXT NOT NULL,
    keychain_account TEXT NOT NULL,
    scope TEXT NOT NULL DEFAULT 'global',
    project_id TEXT NOT NULL DEFAULT '',
    expires_at TEXT,
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active','missing','expired','error')),
    last_verified_at TEXT,
    revision INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- model_profiles：managed_source='env' 为只读导入。
CREATE TABLE model_profiles (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    provider_kind TEXT NOT NULL CHECK (provider_kind IN ('openai_compatible','fake')),
    base_url TEXT NOT NULL DEFAULT '',
    credential_ref_id TEXT REFERENCES credential_refs(id),
    default_model TEXT NOT NULL DEFAULT '',
    capabilities_json TEXT NOT NULL DEFAULT '{}',
    limits_json TEXT NOT NULL DEFAULT '{}',
    data_policy_json TEXT NOT NULL DEFAULT '{}',
    managed_source TEXT,
    status TEXT NOT NULL DEFAULT 'configured'
        CHECK (status IN ('configured','testing','ready','degraded','error','managed_read_only')),
    last_tested_at TEXT,
    revision INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- model_routes：scope + task_kind 决定 primary/fallback。
CREATE TABLE model_routes (
    id TEXT NOT NULL,
    scope TEXT NOT NULL DEFAULT 'global',
    task_kind TEXT NOT NULL,
    primary_profile_id TEXT NOT NULL REFERENCES model_profiles(id),
    fallback_json TEXT NOT NULL DEFAULT '[]',
    budget_json TEXT NOT NULL DEFAULT '{}',
    revision INTEGER NOT NULL DEFAULT 1,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (scope, task_kind)
);

-- gitlab_profiles
CREATE TABLE gitlab_profiles (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    base_url TEXT NOT NULL,
    display_name TEXT NOT NULL DEFAULT '',
    credential_ref_id TEXT REFERENCES credential_refs(id),
    capabilities_json TEXT NOT NULL DEFAULT '{}',
    managed_source TEXT,
    status TEXT NOT NULL DEFAULT 'configured'
        CHECK (status IN ('configured','testing','ready','degraded','error','managed_read_only')),
    last_tested_at TEXT,
    revision INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- ssh_targets：指纹保存/首用确认状态。
CREATE TABLE ssh_targets (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    host TEXT NOT NULL,
    port INTEGER NOT NULL DEFAULT 22,
    username TEXT NOT NULL,
    remote_dir TEXT NOT NULL DEFAULT '',
    credential_ref_id TEXT REFERENCES credential_refs(id),
    jump_host TEXT NOT NULL DEFAULT '',
    fingerprint TEXT NOT NULL DEFAULT '',
    fingerprint_status TEXT NOT NULL DEFAULT 'unverified'
        CHECK (fingerprint_status IN ('unverified','accepted','changed')),
    allowed_commands_json TEXT NOT NULL DEFAULT '[]',
    project_id TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'configured'
        CHECK (status IN ('configured','testing','ready','degraded','error')),
    last_tested_at TEXT,
    revision INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- knowledge_default_settings：单行 per scope（global/项目覆盖）。
CREATE TABLE knowledge_default_settings (
    scope TEXT NOT NULL DEFAULT 'global',
    project_id TEXT NOT NULL DEFAULT '',
    settings_json TEXT NOT NULL DEFAULT '{}',
    revision INTEGER NOT NULL DEFAULT 1,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (scope, project_id)
);

-- tool_policies：工具策略持久化（含项目覆盖）。
CREATE TABLE tool_policies (
    tool_id TEXT NOT NULL,
    project_id TEXT NOT NULL DEFAULT '',
    enabled INTEGER NOT NULL DEFAULT 1,
    risk TEXT NOT NULL DEFAULT 'medium' CHECK (risk IN ('low','medium','high')),
    requires_approval INTEGER NOT NULL DEFAULT 0,
    network TEXT NOT NULL DEFAULT 'deny' CHECK (network IN ('deny','allow')),
    revision INTEGER NOT NULL DEFAULT 1,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (tool_id, project_id)
);

-- execution_profiles
CREATE TABLE execution_profiles (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    mode TEXT NOT NULL DEFAULT 'safe_restricted'
        CHECK (mode IN ('docker','safe_restricted','unsafe_explicit','disabled')),
    limits_json TEXT NOT NULL DEFAULT '{}',
    revision INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- backup_records：manifest/digest/校验状态。
CREATE TABLE backup_records (
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL,
    format_version INTEGER NOT NULL DEFAULT 1,
    schema_version INTEGER NOT NULL,
    size_bytes INTEGER NOT NULL DEFAULT 0,
    digest TEXT NOT NULL DEFAULT '',
    verified INTEGER NOT NULL DEFAULT 0,
    problems_json TEXT NOT NULL DEFAULT '[]',
    status TEXT NOT NULL DEFAULT 'created'
        CHECK (status IN ('created','verifying','verified','corrupt','incompatible','restoring','restored','failed')),
    manifest_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- operations：长操作可观察（备份/扫描/连接测试/导出）。
CREATE TABLE operations (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'running'
        CHECK (status IN ('running','succeeded','failed','cancelled')),
    progress_json TEXT NOT NULL DEFAULT '{}',
    cancellable INTEGER NOT NULL DEFAULT 0,
    result_json TEXT,
    started_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- audit_log 扩列（只增不改）：结构化审计。
ALTER TABLE audit_log ADD COLUMN actor_kind TEXT NOT NULL DEFAULT 'user';
ALTER TABLE audit_log ADD COLUMN result TEXT NOT NULL DEFAULT 'success';
ALTER TABLE audit_log ADD COLUMN correlation_id TEXT;
ALTER TABLE audit_log ADD COLUMN trace_id TEXT;
ALTER TABLE audit_log ADD COLUMN project_id TEXT;
ALTER TABLE audit_log ADD COLUMN before_summary TEXT;
ALTER TABLE audit_log ADD COLUMN after_summary TEXT;
ALTER TABLE audit_log ADD COLUMN metadata_redacted INTEGER NOT NULL DEFAULT 0;

-- env 导入为只读 Profile 的元数据标记（导入逻辑在 service 层，幂等）。
CREATE TABLE settings_managed_imports (
    source TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    imported_at TEXT NOT NULL,
    PRIMARY KEY (source, resource_type, resource_id)
);
