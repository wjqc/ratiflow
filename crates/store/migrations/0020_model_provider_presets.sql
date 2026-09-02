-- 0020_model_provider_presets: 放开 model_profiles.provider_kind，接入内置供应商预设
-- （智谱 GLM / DeepSeek，OpenAI 兼容端点；契约 modelProvider.presets）。
-- SQLite 不能修改 CHECK：按官方表重建流程执行（rename → 建新表 → 回填 → 删旧）。
-- 迁移事务内外键开启（PRAGMA foreign_keys 在事务内不可变更），
-- 先后 rename 父子两表再重建，保证任一步都不产生孤引用。

ALTER TABLE model_profiles RENAME TO model_profiles_0020_old;
ALTER TABLE model_routes RENAME TO model_routes_0020_old;

CREATE TABLE model_profiles (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    provider_kind TEXT NOT NULL CHECK (provider_kind IN ('openai_compatible','zhipu','deepseek','fake')),
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

INSERT INTO model_profiles (id, name, provider_kind, base_url, credential_ref_id, default_model,
    capabilities_json, limits_json, data_policy_json, managed_source, status, last_tested_at, revision, created_at, updated_at)
SELECT id, name, provider_kind, base_url, credential_ref_id, default_model,
    capabilities_json, limits_json, data_policy_json, managed_source, status, last_tested_at, revision, created_at, updated_at
FROM model_profiles_0020_old;

INSERT INTO model_routes (id, scope, task_kind, primary_profile_id, fallback_json, budget_json, revision, updated_at)
SELECT id, scope, task_kind, primary_profile_id, fallback_json, budget_json, revision, updated_at
FROM model_routes_0020_old;

DROP TABLE model_routes_0020_old;
DROP TABLE model_profiles_0020_old;
