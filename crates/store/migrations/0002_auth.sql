-- 0002_auth: 本机会话、项目与凭据引用、API 幂等键
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    label TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);

CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    gitlab_instance TEXT NOT NULL,
    namespace TEXT NOT NULL,
    project TEXT NOT NULL,
    default_branch TEXT NOT NULL DEFAULT 'main',
    created_at TEXT NOT NULL,
    UNIQUE(gitlab_instance, namespace, project)
);

-- 只保存 Keychain/env 引用名，永不保存明文
CREATE TABLE credential_refs (
    id TEXT PRIMARY KEY,
    project_id TEXT REFERENCES projects(id),
    kind TEXT NOT NULL CHECK (kind IN ('gitlab','model','ssh')),
    ref TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(project_id, kind)
);

CREATE TABLE idempotency_keys (
    key TEXT PRIMARY KEY,
    request_hash TEXT NOT NULL,
    response_status INTEGER NOT NULL,
    response_body TEXT NOT NULL,
    created_at TEXT NOT NULL
);
