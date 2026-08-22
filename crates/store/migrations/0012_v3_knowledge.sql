-- 0012_v3_knowledge: 项目知识来源与分块索引（F02），FTS5 检索
CREATE TABLE knowledge_sources (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    kind TEXT NOT NULL CHECK (kind IN ('repo_path','document','openapi','gitlab','rule')),
    name TEXT NOT NULL,
    locator TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    scan_state TEXT NOT NULL DEFAULT 'pending'
        CHECK (scan_state IN ('pending','scanning','indexed','failed','disabled')),
    content_sha256 TEXT NOT NULL DEFAULT '',
    last_scanned_at TEXT,
    error TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(project_id, kind, locator)
);

CREATE TABLE knowledge_chunks (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES knowledge_sources(id),
    ordinal INTEGER NOT NULL,
    object_sha256 TEXT NOT NULL,
    token_count INTEGER NOT NULL DEFAULT 0,
    metadata TEXT NOT NULL DEFAULT '{}',
    UNIQUE(source_id, ordinal, object_sha256)
);

CREATE VIRTUAL TABLE knowledge_fts USING fts5(
    chunk_id UNINDEXED,
    source_id UNINDEXED,
    project_id UNINDEXED,
    body
);
