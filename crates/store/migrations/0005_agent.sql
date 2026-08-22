-- 0005_agent: 上下文清单、Agent Run、检查点、工具提案与模型调用审计
CREATE TABLE context_manifests (
    id TEXT PRIMARY KEY,
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    scope TEXT NOT NULL,
    data_policy TEXT NOT NULL DEFAULT 'standard',
    created_at TEXT NOT NULL
);

CREATE TABLE agent_runs (
    id TEXT PRIMARY KEY,
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    task_id TEXT NOT NULL DEFAULT '',
    goal TEXT NOT NULL,
    input_baseline_sha TEXT NOT NULL,
    context_manifest_id TEXT NOT NULL REFERENCES context_manifests(id),
    tool_allowlist TEXT NOT NULL DEFAULT '[]',
    budget TEXT NOT NULL,
    policy_snapshot TEXT NOT NULL,
    idempotency_key TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued','running','paused','completed_execution','failed','cancelled')),
    result TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE agent_checkpoints (
    id TEXT PRIMARY KEY,
    agent_run_id TEXT NOT NULL REFERENCES agent_runs(id),
    seq INTEGER NOT NULL,
    state TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(agent_run_id, seq)
);

CREATE TABLE tool_proposals (
    id TEXT PRIMARY KEY,
    agent_run_id TEXT NOT NULL REFERENCES agent_runs(id),
    tool TEXT NOT NULL,
    arguments TEXT NOT NULL,
    risk TEXT NOT NULL CHECK (risk IN ('low','medium','high')),
    action_digest TEXT NOT NULL,
    requires_approval INTEGER NOT NULL,
    decision TEXT NOT NULL DEFAULT 'proposed'
        CHECK (decision IN ('proposed','approved','rejected','executed','exec_failed','cancelled')),
    result TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);

CREATE TABLE model_calls (
    id TEXT PRIMARY KEY,
    agent_run_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    tokens_in INTEGER NOT NULL DEFAULT 0,
    tokens_out INTEGER NOT NULL DEFAULT 0,
    cost_micros INTEGER NOT NULL DEFAULT 0,
    latency_ms INTEGER NOT NULL DEFAULT 0,
    redactions INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX idx_agent_runs_workitem ON agent_runs(workitem_id, created_at);
