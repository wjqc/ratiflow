-- 0007_gate: 门禁计算结果、审批、追踪链接、证据与通关文牒
CREATE TABLE gate_results (
    id TEXT PRIMARY KEY,
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    gate TEXT NOT NULL,
    inputs TEXT NOT NULL,
    result TEXT NOT NULL,
    computed_at TEXT NOT NULL
);

CREATE TABLE approvals (
    id TEXT PRIMARY KEY,
    subject_type TEXT NOT NULL CHECK (subject_type IN ('tool_proposal','deployment','baseline','risk')),
    subject_id TEXT NOT NULL,
    action_digest TEXT NOT NULL,
    risk TEXT NOT NULL CHECK (risk IN ('low','medium','high')),
    status TEXT NOT NULL DEFAULT 'requested'
        CHECK (status IN ('requested','approved','rejected','expired')),
    requested_by TEXT NOT NULL DEFAULT 'local',
    decided_by TEXT,
    decided_at TEXT,
    expires_at TEXT NOT NULL,
    reason TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);

CREATE TABLE trace_links (
    id TEXT PRIMARY KEY,
    from_type TEXT NOT NULL,
    from_id TEXT NOT NULL,
    relation TEXT NOT NULL
        CHECK (relation IN ('satisfies','implements','verifies','builds','deploys','validates')),
    to_type TEXT NOT NULL,
    to_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(from_type, from_id, relation, to_type, to_id)
);

CREATE TABLE evidences (
    id TEXT PRIMARY KEY,
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    gate TEXT NOT NULL,
    kind TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    object_sha256 TEXT NOT NULL DEFAULT '',
    payload TEXT NOT NULL DEFAULT '{}',
    source TEXT NOT NULL DEFAULT 'local',
    verified INTEGER NOT NULL DEFAULT 0,
    verified_at TEXT,
    verified_by TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE passports (
    id TEXT PRIMARY KEY,
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    object_sha256 TEXT NOT NULL,
    inputs_sha256 TEXT NOT NULL,
    shared_summary TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    UNIQUE(workitem_id, inputs_sha256)
);

CREATE INDEX idx_evidences_workitem ON evidences(workitem_id, gate);
CREATE INDEX idx_approvals_subject ON approvals(subject_type, subject_id);
