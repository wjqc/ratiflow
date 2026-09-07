-- 0046：A1 跨关返工（EvoFlow WP-9；flag RATIFLOW_REWORK，默认 0）。
-- 编号注：计划原定 0045 已被并行落地的 skill_market_sources 占用，顺延 0046。
-- 权威=rework_operations（对齐 rollback_operations 模式，补时间列）；
-- 失效范围=rework_affected_facts 统一登记表（读取面据此过滤，空表=逐字等价回退）。

CREATE TABLE rework_operations (
    id TEXT PRIMARY KEY,
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    from_gate TEXT NOT NULL,
    target_gate TEXT NOT NULL,
    from_attempt_id TEXT REFERENCES stage_attempts(id),
    reason_code TEXT NOT NULL
        CHECK (reason_code IN ('requirement_unclear','design_defect','implementation_defect','regression','other')),
    note TEXT NOT NULL DEFAULT '',
    current_state_digest TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN (
        'previewed', 'awaiting_approval', 'executing', 'completed',
        'blocked', 'failed', 'cancelled')),
    blocked_reason TEXT NOT NULL DEFAULT '',
    approval_id TEXT UNIQUE REFERENCES approvals(id),
    action_digest TEXT NOT NULL UNIQUE,
    requested_by TEXT NOT NULL DEFAULT '',
    decided_by TEXT,
    decided_at TEXT,
    completed_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_rework_wi ON rework_operations(workitem_id, created_at DESC);
CREATE INDEX idx_rework_state ON rework_operations(state);

-- 统一失效登记表（八类事实；读取面排除/过滤据此）。
CREATE TABLE rework_affected_facts (
    id TEXT PRIMARY KEY,
    rework_operation_id TEXT NOT NULL REFERENCES rework_operations(id),
    fact_kind TEXT NOT NULL CHECK (fact_kind IN (
        'gate_result','stage_attempt','output_package','release_request',
        'approval','baseline','plan_attempt','passport')),
    fact_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(rework_operation_id, fact_kind, fact_id)
);

CREATE INDEX idx_rework_facts_kind ON rework_affected_facts(fact_kind, fact_id);
