-- 0018_stage_snapshot_rollback: ADR-030 M3 关前快照与业务回滚。
-- 快照=三清单（控制面/工作区/外部引用）canonical hash 合成 root_digest；资源明细带可逆性分级。
-- 回滚操作只追加：preview→approval→decide→executing→completed/blocked；历史永不删除（SG-RBK-004）。

CREATE TABLE state_snapshots (
    id TEXT PRIMARY KEY,
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    stage_attempt_id TEXT NOT NULL REFERENCES stage_attempts(id),
    -- stage_entry=关前快照 / safety=回滚执行前保险快照 / rollback_target=预览指向的目标
    kind TEXT NOT NULL CHECK (kind IN ('stage_entry', 'safety', 'rollback_target')),
    control_manifest_sha256 TEXT NOT NULL,
    workspace_manifest_sha256 TEXT NOT NULL,
    external_manifest_sha256 TEXT NOT NULL,
    root_digest TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(stage_attempt_id, kind, root_digest)
);
CREATE INDEX idx_snapshots_wi ON state_snapshots(workitem_id, created_at DESC);

CREATE TABLE snapshot_resources (
    snapshot_id TEXT NOT NULL REFERENCES state_snapshots(id),
    resource_type TEXT NOT NULL,
    resource_key TEXT NOT NULL,
    version_ref TEXT NOT NULL DEFAULT '',
    object_sha256 TEXT NOT NULL DEFAULT '',
    -- logical_restore=指针可恢复 / compensatable=需补偿可验证 / manual=人工 / irreversible=不可逆
    reversibility TEXT NOT NULL CHECK (reversibility IN ('logical_restore', 'compensatable', 'manual', 'irreversible')),
    metadata_json TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY (snapshot_id, resource_type, resource_key)
);
CREATE INDEX idx_snapshot_resources_type ON snapshot_resources(resource_type);

CREATE TABLE rollback_operations (
    id TEXT PRIMARY KEY,
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    source_attempt_id TEXT NOT NULL REFERENCES stage_attempts(id),
    target_snapshot_id TEXT NOT NULL REFERENCES state_snapshots(id),
    safety_snapshot_id TEXT REFERENCES state_snapshots(id),
    approval_id TEXT UNIQUE REFERENCES approvals(id),
    impact_manifest_sha256 TEXT NOT NULL,
    action_digest TEXT NOT NULL UNIQUE,
    state TEXT NOT NULL CHECK (state IN (
        'previewed', 'awaiting_approval', 'executing', 'completed',
        'blocked', 'failed', 'cancelled')),
    blocked_reason TEXT NOT NULL DEFAULT '',
    requested_by TEXT NOT NULL DEFAULT '',
    decided_by TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_rollback_wi ON rollback_operations(workitem_id, created_at DESC);
CREATE INDEX idx_rollback_state ON rollback_operations(state);
