-- 0008_delivery: 部署状态机与步骤
CREATE TABLE deployments (
    id TEXT PRIMARY KEY,
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    target TEXT NOT NULL,
    image_digest TEXT NOT NULL DEFAULT '',
    plan TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'draft'
        CHECK (state IN ('draft','awaiting_approval','approved','deploying','awaiting_verification',
                         'verified','verification_failed','deploy_failed','rolling_back',
                         'rolled_back','rollback_failed')),
    action_digest TEXT NOT NULL DEFAULT '',
    idempotency_key TEXT NOT NULL DEFAULT '',
    result TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE deployment_steps (
    id TEXT PRIMARY KEY,
    deployment_id TEXT NOT NULL REFERENCES deployments(id),
    seq INTEGER NOT NULL,
    name TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending','intent','done','failed','skipped')),
    result TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL,
    UNIQUE(deployment_id, seq)
);
