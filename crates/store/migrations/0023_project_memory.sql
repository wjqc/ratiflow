-- 0023_project_memory: 项目记忆 bounded context（ADR-032 / 实施方案 v1.0 §6）。
-- 权威元数据 = memory_entries/memory_revisions/memory_source_refs；
-- 权威正文 = app data content-addressed objects（memory_revisions.object_sha256）；
-- memory_fts 仅为可重建投影；context_manifest_memories 冻结 Run 实际采用的 revision；
-- capture/candidates 为 M4 候选沉淀；receipts 保证本地写幂等。
-- 全部业务变更、FTS、audit、outbox、receipt 必须在同一 with_tx_immediate 中提交（§6.8）。

-- 6.1 项目级记忆策略：一项目一行；默认关闭。全局 rollout flag 存 app_settings
--（scope='global', project_id='', key='memory.featureEnabled'，默认 false）。
CREATE TABLE project_memory_settings (
    project_id TEXT PRIMARY KEY REFERENCES projects(id),
    enabled INTEGER NOT NULL DEFAULT 0,
    capture_mode TEXT NOT NULL DEFAULT 'off' CHECK (capture_mode IN ('off','suggest')),
    max_entries INTEGER NOT NULL DEFAULT 8 CHECK (max_entries BETWEEN 1 AND 32),
    max_bytes INTEGER NOT NULL DEFAULT 12288 CHECK (max_bytes BETWEEN 1024 AND 65536),
    stale_after_days INTEGER NOT NULL DEFAULT 180 CHECK (stale_after_days BETWEEN 1 AND 3650),
    revision INTEGER NOT NULL DEFAULT 1,
    updated_at TEXT NOT NULL,
    updated_by TEXT NOT NULL DEFAULT ''
);

-- 6.2 记忆主档。current_revision_id 可空：entry 与首个 revision 在同一事务内先后落库。
CREATE TABLE memory_entries (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    slug TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('decision','convention','fact','lesson','preference')),
    subject_key TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL CHECK (status IN ('proposed','active','conflicted','archived','rejected','purged')),
    current_revision_id TEXT REFERENCES memory_revisions(id),
    pinned INTEGER NOT NULL DEFAULT 0,
    valid_until TEXT,
    confirmed_at TEXT,
    confirmed_by TEXT NOT NULL DEFAULT '',
    revision INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    archived_at TEXT,
    purged_at TEXT
);
CREATE UNIQUE INDEX idx_memory_entries_project_slug ON memory_entries(project_id, slug);
CREATE INDEX idx_memory_entries_list ON memory_entries(project_id, status, pinned, updated_at);
CREATE INDEX idx_memory_entries_subject ON memory_entries(project_id, subject_key, status);

-- 6.3 不可变修订。业务内容不可变；purge 只允许置空 object_sha256 并写 purged_at。
CREATE TABLE memory_revisions (
    id TEXT PRIMARY KEY,
    memory_id TEXT NOT NULL REFERENCES memory_entries(id),
    revision_no INTEGER NOT NULL,
    title TEXT NOT NULL,
    summary TEXT NOT NULL DEFAULT '',
    object_sha256 TEXT,
    content_sha256 TEXT NOT NULL,
    tags_json TEXT NOT NULL DEFAULT '[]',
    source_type TEXT NOT NULL CHECK (source_type IN ('manual','import','run','artifact','decision')),
    author_kind TEXT NOT NULL CHECK (author_kind IN ('user','agent','system')),
    author_id TEXT NOT NULL DEFAULT '',
    idempotency_key TEXT NOT NULL UNIQUE,
    request_fingerprint TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    purged_at TEXT,
    UNIQUE(memory_id, revision_no)
);

-- 6.4 来源引用：至少一条 ref 才可成为 active；手工创建写 manual ref。
CREATE TABLE memory_source_refs (
    revision_id TEXT NOT NULL REFERENCES memory_revisions(id),
    ordinal INTEGER NOT NULL,
    project_id TEXT NOT NULL,
    source_kind TEXT NOT NULL CHECK (source_kind IN ('manual','run','workitem','artifact','evidence','requirement','import')),
    source_id TEXT NOT NULL DEFAULT '',
    locator TEXT NOT NULL DEFAULT '',
    source_digest TEXT NOT NULL DEFAULT '',
    relation TEXT NOT NULL DEFAULT 'derived_from' CHECK (relation IN ('derived_from','summarizes','corrects')),
    PRIMARY KEY (revision_id, ordinal)
);

-- 6.5 检索投影：保留所有非 purged entry 的 current revision；purge 立即移除；
-- 可由 rebuild_index 逐条复核 object hash 后重建。
CREATE VIRTUAL TABLE memory_fts USING fts5(
    memory_id UNINDEXED,
    revision_id UNINDEXED,
    project_id UNINDEXED,
    title,
    tags,
    body,
    tokenize='trigram'
);

-- 6.6 Run 冻结证据：实际采用的 revision 与 included/excluded 理由。
CREATE TABLE context_manifest_memories (
    manifest_id TEXT NOT NULL REFERENCES context_manifests(id),
    ordinal INTEGER NOT NULL,
    project_id TEXT NOT NULL,
    memory_id TEXT NOT NULL REFERENCES memory_entries(id),
    revision_id TEXT NOT NULL REFERENCES memory_revisions(id),
    included INTEGER NOT NULL,
    reason TEXT NOT NULL CHECK (reason IN ('matched','pinned','over_budget','stale','conflicted','disabled','purged')),
    score REAL NOT NULL DEFAULT 0,
    bytes INTEGER NOT NULL DEFAULT 0,
    token_estimate INTEGER NOT NULL DEFAULT 0,
    selected_at TEXT NOT NULL,
    PRIMARY KEY (manifest_id, ordinal)
);
CREATE INDEX idx_manifest_memories_memory ON context_manifest_memories(memory_id);

-- 6.7 候选捕获（M4）：Provider 不确定结果必须落 unknown，不透明重试。
CREATE TABLE memory_capture_jobs (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    run_id TEXT NOT NULL DEFAULT '',
    source_digest TEXT NOT NULL DEFAULT '',
    prompt_schema_version INTEGER NOT NULL DEFAULT 1,
    model_profile_id TEXT NOT NULL DEFAULT '',
    model_revision INTEGER NOT NULL DEFAULT 0,
    summary_json TEXT NOT NULL DEFAULT '{}',
    idempotency_key TEXT NOT NULL UNIQUE,
    request_fingerprint TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','in_flight','succeeded','failed','unknown','cancelled')),
    provider_request_id TEXT NOT NULL DEFAULT '',
    attempt_count INTEGER NOT NULL DEFAULT 0,
    error_code TEXT NOT NULL DEFAULT '',
    started_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    finished_at TEXT
);

CREATE TABLE memory_candidates (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL REFERENCES memory_capture_jobs(id),
    project_id TEXT NOT NULL REFERENCES projects(id),
    kind TEXT NOT NULL CHECK (kind IN ('decision','convention','fact','lesson','preference')),
    subject_key TEXT NOT NULL DEFAULT '',
    title TEXT NOT NULL,
    summary TEXT NOT NULL DEFAULT '',
    object_sha256 TEXT NOT NULL DEFAULT '',
    content_sha256 TEXT NOT NULL DEFAULT '',
    request_fingerprint TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','accepted','rejected')),
    decision_by TEXT NOT NULL DEFAULT '',
    decision_at TEXT,
    accepted_memory_id TEXT REFERENCES memory_entries(id),
    created_at TEXT NOT NULL
);
CREATE INDEX idx_memory_candidates_project_status ON memory_candidates(project_id, status);

-- 6.8 本地写幂等收据：同 key 同 fingerprint 重放返回原结果；异内容 MEMORY_CONFLICT。
CREATE TABLE memory_mutation_receipts (
    idempotency_key TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    operation TEXT NOT NULL,
    target_id TEXT NOT NULL DEFAULT '',
    request_fingerprint TEXT NOT NULL,
    result_json TEXT NOT NULL,
    completed_at TEXT NOT NULL
);
