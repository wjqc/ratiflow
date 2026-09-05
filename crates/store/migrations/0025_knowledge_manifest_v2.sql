-- 0024_knowledge_manifest_v2 (RFC《知识来源清单随仓库走》v1.0 §12)
-- 在 FK OFF 的 runner 契约内执行（§12.1）；本文件含 §12.2 闭包换表与 §12.3/§12.4 全部新表。
-- 注意：表内 UNIQUE 约束不支持 WHERE，manifest 唯一键必须用独立 partial index。

------------------------------------------------------------------
-- 步骤 2：knowledge_sources_new（新列/新 CHECK/独立 partial unique index）
------------------------------------------------------------------
CREATE TABLE knowledge_sources_new (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('repo_path','document','openapi','gitlab','rule','local_path')),
    name TEXT NOT NULL,
    locator TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    scan_state TEXT NOT NULL DEFAULT 'pending'
        CHECK (scan_state IN ('pending','scanning','indexed','partial_indexed','no_indexable_files','scan_failed','failed','disabled')),
    content_sha256 TEXT NOT NULL DEFAULT '',
    last_scanned_at TEXT,
    error TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    origin TEXT NOT NULL DEFAULT 'local' CHECK (origin IN ('manifest','local')),
    stable_id TEXT,
    identity_digest TEXT,
    manifest_sha256 TEXT,
    active_manifest_sha256 TEXT,
    indexed_manifest_sha256 TEXT,
    desired_input_revision TEXT,
    indexed_input_revision TEXT,
    input_revision_mode TEXT CHECK (input_revision_mode IN ('committed','worktree')),
    local_input_revision TEXT,
    local_indexed_revision TEXT,
    local_input_state TEXT CHECK (local_input_state IN ('current','unknown','stale')),
    present INTEGER NOT NULL DEFAULT 1,
    worktree_state TEXT NOT NULL DEFAULT '' CHECK (worktree_state IN ('','untracked','modified','committed')),
    publication_state TEXT NOT NULL DEFAULT 'unpublished'
        CHECK (publication_state IN ('unpublished','published','publish_unknown','revoked')),
    legacy_local INTEGER NOT NULL DEFAULT 0,
    UNIQUE (project_id, id),
    -- manifest 行身份字段必填；local 行 manifest 系列必空（§3/§12.4）
    CHECK (origin != 'manifest' OR (
        stable_id IS NOT NULL AND identity_digest IS NOT NULL
        AND manifest_sha256 IS NOT NULL AND active_manifest_sha256 IS NOT NULL
    )),
    -- legacy-local 允许存量 repo_path/document 以 local 身份存在（§12.4）
    CHECK (origin != 'local' OR stable_id IS NULL)
);
CREATE UNIQUE INDEX idx_src_manifest_stable
    ON knowledge_sources_new(project_id, stable_id) WHERE origin='manifest' AND present=1;
CREATE UNIQUE INDEX idx_src_new_locator ON knowledge_sources_new(project_id, kind, locator);

INSERT INTO knowledge_sources_new
    (id, project_id, kind, name, locator, enabled, scan_state, content_sha256,
     last_scanned_at, error, created_at, updated_at, origin, present, legacy_local)
SELECT id, project_id, kind, name, locator, enabled,
    CASE scan_state
        WHEN 'pending' THEN 'pending'
        WHEN 'scanning' THEN 'scanning'
        WHEN 'indexed' THEN 'indexed'
        WHEN 'failed' THEN 'failed'
        WHEN 'disabled' THEN 'disabled'
        ELSE 'failed'
    END,
    content_sha256, last_scanned_at, error, created_at, updated_at,
    'local', 1,
    CASE WHEN kind IN ('repo_path','document') THEN 1 ELSE 0 END
FROM knowledge_sources;

------------------------------------------------------------------
-- 步骤 3：child scratch/new 表（FK 指向 knowledge_sources_new）并复制
------------------------------------------------------------------
CREATE TABLE knowledge_chunks_new (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    object_sha256 TEXT NOT NULL,
    token_count INTEGER NOT NULL DEFAULT 0,
    metadata TEXT NOT NULL DEFAULT '{}',
    project_id TEXT NOT NULL,
    UNIQUE (source_id, ordinal, object_sha256),
    FOREIGN KEY (project_id, source_id) REFERENCES knowledge_sources_new(project_id, id)
);
INSERT INTO knowledge_chunks_new (id, source_id, ordinal, object_sha256, token_count, metadata, project_id)
SELECT c.id, c.source_id, c.ordinal, c.object_sha256, c.token_count, c.metadata, s.project_id
FROM knowledge_chunks c JOIN knowledge_sources s ON s.id = c.source_id;

CREATE TABLE context_manifest_items_new (
    manifest_id TEXT NOT NULL REFERENCES context_manifests(id),
    source_id TEXT REFERENCES knowledge_sources_new(id),
    attachment_id TEXT REFERENCES attachments(id),
    object_sha256 TEXT NOT NULL,
    purpose TEXT NOT NULL,
    included INTEGER NOT NULL,
    reason TEXT NOT NULL DEFAULT '',
    ordinal INTEGER NOT NULL,
    PRIMARY KEY (manifest_id, ordinal)
);
INSERT INTO context_manifest_items_new
    (manifest_id, source_id, attachment_id, object_sha256, purpose, included, reason, ordinal)
SELECT manifest_id, source_id, attachment_id, object_sha256, purpose, included, reason, ordinal
FROM context_manifest_items;

------------------------------------------------------------------
-- 步骤 4/5：删旧 child、删旧 parent
------------------------------------------------------------------
DROP TABLE knowledge_chunks;
DROP TABLE context_manifest_items;
DROP TABLE knowledge_sources;

------------------------------------------------------------------
-- 步骤 6：parent new 换正名
------------------------------------------------------------------
ALTER TABLE knowledge_sources_new RENAME TO knowledge_sources;

------------------------------------------------------------------
-- 步骤 7：正式名重建 child（完整 FK/索引），回灌，删 scratch
-- （knowledge_generations 家族先建，供 chunks 组合 FK 引用）
------------------------------------------------------------------
CREATE TABLE knowledge_generations (
    id                TEXT PRIMARY KEY,
    project_id        TEXT NOT NULL,
    idempotency_key   TEXT NOT NULL,
    source_set_digest TEXT NOT NULL,
    status            TEXT NOT NULL CHECK (status IN ('pending','building','ready','active','failed','superseded','abandoned')),
    payload_available INTEGER NOT NULL DEFAULT 1,
    lease_owner TEXT,
    lease_expires_at TEXT,
    chunker_version   TEXT NOT NULL,
    error             TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    activated_at TEXT,
    abandoned_at TEXT,
    gc_eligible_at TEXT,
    UNIQUE (project_id, idempotency_key),
    UNIQUE (project_id, id),
    CHECK (status != 'active' OR activated_at IS NOT NULL)
);
CREATE UNIQUE INDEX idx_gen_single_active ON knowledge_generations(project_id) WHERE status='active';

CREATE TABLE knowledge_generation_sources (
    generation_id  TEXT NOT NULL,
    project_id     TEXT NOT NULL,
    source_id      TEXT NOT NULL,
    stable_id      TEXT NOT NULL,
    manifest_sha256 TEXT NOT NULL,
    input_revision TEXT NOT NULL,
    enabled INTEGER NOT NULL,
    present INTEGER NOT NULL,
    PRIMARY KEY (generation_id, stable_id),
    FOREIGN KEY (project_id, generation_id) REFERENCES knowledge_generations(project_id, id),
    FOREIGN KEY (project_id, source_id) REFERENCES knowledge_sources(project_id, id)
);

CREATE TABLE knowledge_generation_active (
    project_id    TEXT NOT NULL,
    generation_id TEXT NOT NULL,
    PRIMARY KEY (project_id),
    FOREIGN KEY (project_id, generation_id) REFERENCES knowledge_generations(project_id, id)
);

CREATE TABLE knowledge_generation_retention (
    generation_id  TEXT PRIMARY KEY REFERENCES knowledge_generations(id),
    project_id     TEXT NOT NULL,
    retained_until TEXT NOT NULL
);

CREATE TABLE knowledge_generation_activation_history (
    id             TEXT PRIMARY KEY,
    project_id     TEXT NOT NULL,
    previous_generation_id TEXT,
    previous_generation_key TEXT NOT NULL,
    current_generation_id  TEXT NOT NULL,
    current_generation_key  TEXT NOT NULL,
    reason         TEXT NOT NULL CHECK (reason IN ('activate','rollback','supersede')),
    actor          TEXT NOT NULL,
    source_set_digest TEXT NOT NULL,
    created_at     TEXT NOT NULL,
    FOREIGN KEY (project_id, previous_generation_id) REFERENCES knowledge_generations(project_id, id) ON DELETE SET NULL,
    FOREIGN KEY (project_id, current_generation_id)  REFERENCES knowledge_generations(project_id, id)
);

CREATE TABLE knowledge_chunks (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    object_sha256 TEXT NOT NULL,
    token_count INTEGER NOT NULL DEFAULT 0,
    metadata TEXT NOT NULL DEFAULT '{}',
    generation_id TEXT,
    project_id TEXT NOT NULL,
    UNIQUE (source_id, ordinal, object_sha256),
    FOREIGN KEY (project_id, source_id) REFERENCES knowledge_sources(project_id, id),
    FOREIGN KEY (project_id, generation_id) REFERENCES knowledge_generations(project_id, id)
);
INSERT INTO knowledge_chunks (id, source_id, ordinal, object_sha256, token_count, metadata, generation_id, project_id)
SELECT id, source_id, ordinal, object_sha256, token_count, metadata, NULL, project_id
FROM knowledge_chunks_new;
DROP TABLE knowledge_chunks_new;

CREATE TABLE context_manifest_items (
    manifest_id TEXT NOT NULL REFERENCES context_manifests(id),
    source_id TEXT REFERENCES knowledge_sources(id),
    attachment_id TEXT REFERENCES attachments(id),
    object_sha256 TEXT NOT NULL,
    purpose TEXT NOT NULL,
    included INTEGER NOT NULL,
    reason TEXT NOT NULL DEFAULT '',
    ordinal INTEGER NOT NULL,
    PRIMARY KEY (manifest_id, ordinal)
);
INSERT INTO context_manifest_items
    (manifest_id, source_id, attachment_id, object_sha256, purpose, included, reason, ordinal)
SELECT manifest_id, source_id, attachment_id, object_sha256, purpose, included, reason, ordinal
FROM context_manifest_items_new;
DROP TABLE context_manifest_items_new;

------------------------------------------------------------------
-- §8：Context blocks（最终 prompt-ready 冻结）+ replay_status + 迁移 job
------------------------------------------------------------------
ALTER TABLE context_manifests ADD COLUMN replay_status TEXT NOT NULL DEFAULT 'legacy_pending'
    CHECK (replay_status IN ('legacy_pending','frozen','migrated_reconstructed','legacy_unverifiable'));

CREATE TABLE context_manifest_blocks (
    manifest_id   TEXT NOT NULL REFERENCES context_manifests(id),
    ordinal       INTEGER NOT NULL,
    role          TEXT NOT NULL CHECK (role IN ('knowledge_block','instruction_layer')),
    object_sha256 TEXT NOT NULL,
    bytes         INTEGER NOT NULL,
    PRIMARY KEY (manifest_id, ordinal)
);

CREATE TABLE context_manifest_item_sources (
    manifest_id    TEXT NOT NULL,
    block_ordinal  INTEGER NOT NULL,
    source_ordinal INTEGER NOT NULL,
    source_key     TEXT NOT NULL
        CHECK (length(source_key) > 2
               AND (substr(source_key, 1, 2) = 'm:' OR substr(source_key, 1, 2) = 'l:')),
    source_id      TEXT NOT NULL REFERENCES knowledge_sources(id),
    chunk_shas     TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (manifest_id, block_ordinal, source_ordinal),
    FOREIGN KEY (manifest_id, block_ordinal) REFERENCES context_manifest_blocks(manifest_id, ordinal)
);

CREATE TABLE context_migration_jobs (
    manifest_id  TEXT PRIMARY KEY REFERENCES context_manifests(id),
    status       TEXT NOT NULL CHECK (status IN ('pending','running','frozen','legacy_unverifiable','failed')),
    lease_owner  TEXT,
    lease_expires_at TEXT,
    attempt      INTEGER NOT NULL DEFAULT 0,
    error        TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT,
    finished_at TEXT
);

------------------------------------------------------------------
-- §6.4：durable receipt
------------------------------------------------------------------
CREATE TABLE knowledge_ops (
    op_id           TEXT PRIMARY KEY,
    project_id      TEXT NOT NULL REFERENCES projects(id),
    stable_id       TEXT NOT NULL,
    op              TEXT NOT NULL CHECK (op IN ('create','update','remove')),
    request_fingerprint TEXT NOT NULL,
    expected_manifest_sha256 TEXT,
    result_manifest_sha256   TEXT,
    status          TEXT NOT NULL CHECK (status IN ('intent','done_file','done_db','failed')),
    response        TEXT,
    error           TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT,
    finished_at TEXT
);
CREATE INDEX idx_ops_cleanup ON knowledge_ops(project_id, status, finished_at);
