-- 0004_artifact: 工件、不可变修订、评审与基线
CREATE TABLE artifacts (
    id TEXT PRIMARY KEY,
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE revisions (
    id TEXT PRIMARY KEY,
    artifact_id TEXT NOT NULL REFERENCES artifacts(id),
    rev_no INTEGER NOT NULL,
    content_sha256 TEXT NOT NULL,
    size INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft','in_review','frozen','superseded')),
    created_by TEXT NOT NULL DEFAULT 'local',
    created_at TEXT NOT NULL,
    UNIQUE(artifact_id, rev_no)
);

CREATE TABLE reviews (
    id TEXT PRIMARY KEY,
    revision_id TEXT NOT NULL REFERENCES revisions(id),
    reviewer TEXT NOT NULL,
    verdict TEXT NOT NULL CHECK (verdict IN ('approved','rejected','changes_requested')),
    comment TEXT NOT NULL DEFAULT '',
    gitlab_mr_iid INTEGER,
    decided_at TEXT NOT NULL
);

CREATE TABLE baselines (
    id TEXT PRIMARY KEY,
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    gate TEXT NOT NULL,
    revision_map TEXT NOT NULL,
    inputs_sha256 TEXT NOT NULL,
    gitlab_commit_sha TEXT NOT NULL DEFAULT '',
    frozen_at TEXT NOT NULL,
    superseded_by TEXT
);

CREATE INDEX idx_artifacts_workitem ON artifacts(workitem_id, kind);
CREATE INDEX idx_revisions_artifact ON revisions(artifact_id, rev_no);
