-- 0006_workflow: 持久化状态机（工作流与步骤 intent 恢复）
CREATE TABLE workflows (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    definition_version INTEGER NOT NULL,
    workitem_id TEXT REFERENCES workitems(id),
    status TEXT NOT NULL DEFAULT 'running'
        CHECK (status IN ('running','succeeded','failed','cancelled')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE workflow_steps (
    id TEXT PRIMARY KEY,
    workflow_id TEXT NOT NULL REFERENCES workflows(id),
    seq INTEGER NOT NULL,
    name TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending','intent','done','failed','skipped')),
    action_digest TEXT NOT NULL DEFAULT '',
    idempotency_key TEXT NOT NULL DEFAULT '',
    input_baseline TEXT NOT NULL DEFAULT '',
    policy_snapshot TEXT NOT NULL DEFAULT '',
    result TEXT NOT NULL DEFAULT '',
    attempts INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL,
    UNIQUE(workflow_id, seq)
);
