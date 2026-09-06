-- 0034：自治授权、工作区策略与任务工作区（EvoFlow 方案 M2-05 / ADR-037 §6.5/§6.7 / §7.2/§7.3）。
-- AutonomyGrant 限范围/限时/限额；WorkspacePolicyVersion 声明化四策略；
-- TaskWorkspace 每 attempt 唯一（并行可归因）。approvals 扩 plan_revision/autonomy_grant
-- 作用域（计划批准与授权批准走既有审批链，不旁路）。

-- 1) 自治授权（§6.5）：绑定 scope/digest/限额/时限/撤销。
CREATE TABLE autonomy_grants (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL DEFAULT 'local',
    project_id TEXT REFERENCES projects(id),
    workitem_id TEXT REFERENCES workitems(id),
    gate_id TEXT,
    plan_digest TEXT NOT NULL DEFAULT '',
    allowed_tools_json TEXT NOT NULL DEFAULT '[]',
    allowed_risks_json TEXT NOT NULL DEFAULT '[]',
    limits_json TEXT NOT NULL DEFAULT '{}',
    allow_gate_release INTEGER NOT NULL DEFAULT 0,
    approval_digest TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active','revoked','expired','exhausted')),
    granted_at TEXT NOT NULL,
    expires_at TEXT,
    revoked_at TEXT,
    revoked_reason TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_grants_workitem ON autonomy_grants(workitem_id, status);

-- 2) 工作区策略版本（§6.7）：声明化四策略 + 最低沙箱 + 网络与并发上限 + digest。
CREATE TABLE workspace_policy_versions (
    id TEXT PRIMARY KEY,
    strategy TEXT NOT NULL
        CHECK (strategy IN ('readonly_snapshot','task_worktree','shared_single_writer','container')),
    sandbox_minimum TEXT NOT NULL
        CHECK (sandbox_minimum IN ('kernel_restricted','docker')),
    network TEXT NOT NULL DEFAULT 'off' CHECK (network IN ('off','allowlist')),
    read_roots_json TEXT NOT NULL DEFAULT '[]',
    write_roots_json TEXT NOT NULL DEFAULT '[]',
    max_parallel_readers INTEGER NOT NULL DEFAULT 4,
    max_parallel_writers INTEGER NOT NULL DEFAULT 3,
    merge_strategy TEXT NOT NULL DEFAULT 'deterministic_task_key_order',
    cleanup_json TEXT NOT NULL DEFAULT '{}',
    limits_json TEXT NOT NULL DEFAULT '{}',
    digest TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('draft','active','deprecated')),
    created_by TEXT NOT NULL DEFAULT 'local',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE UNIQUE INDEX idx_wsp_single_active
    ON workspace_policy_versions(id) WHERE status = 'active';

-- 3) 任务工作区（§6.3/§6.7）：每写 TaskAttempt 独立 worktree；唯一 + 状态机。
--    path 唯一（两个写 task 绝不共享路径——EV-007）。
CREATE TABLE task_workspaces (
    id TEXT PRIMARY KEY,
    task_attempt_id TEXT NOT NULL UNIQUE REFERENCES plan_task_attempts(id),
    workspace_policy_version_id TEXT REFERENCES workspace_policy_versions(id),
    path TEXT NOT NULL UNIQUE,
    base_head TEXT NOT NULL DEFAULT '',
    workspace_digest_before TEXT NOT NULL DEFAULT '',
    workspace_digest_after TEXT NOT NULL DEFAULT '',
    state TEXT NOT NULL DEFAULT 'preparing'
        CHECK (state IN ('preparing','ready','in_use','merged','merge_conflict',
                         'retained','cleaned','failed')),
    merge_receipt TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_task_ws_state ON task_workspaces(state);

-- 4) run_interrupts（§6.11 Clarification）：durable 中断事实，Run 用既有 paused 状态。
CREATE TABLE run_interrupts (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES agent_runs(id),
    kind TEXT NOT NULL
        CHECK (kind IN ('clarification','awaiting_approval','autonomy_boundary')),
    question TEXT NOT NULL DEFAULT '',
    resolution TEXT,
    resolved_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_run_interrupts ON run_interrupts(run_id, resolved_at);

-- 5) agent_runs 冻结 refs（§3 不变量 8）：additive 列。
ALTER TABLE agent_runs ADD COLUMN plan_revision_id TEXT REFERENCES plan_revisions(id);
ALTER TABLE agent_runs ADD COLUMN plan_task_attempt_id TEXT REFERENCES plan_task_attempts(id);
ALTER TABLE agent_runs ADD COLUMN workspace_policy_version_id TEXT REFERENCES workspace_policy_versions(id);
ALTER TABLE agent_runs ADD COLUMN autonomy_grant_id TEXT REFERENCES autonomy_grants(id);
ALTER TABLE agent_runs ADD COLUMN phase TEXT NOT NULL DEFAULT 'execution'
    CHECK (phase IN ('planning','execution','reconciliation'));

-- 6) approvals 扩作用域：plan_revision（计划批准）/ autonomy_grant（授权批准）。
--    CHECK 不可改 → 闭包重建（列集与 0017 完全一致，仅扩枚举）。
CREATE TABLE approvals_v34 (
    id TEXT PRIMARY KEY,
    subject_type TEXT NOT NULL CHECK (subject_type IN (
        'tool_proposal', 'deployment', 'baseline', 'risk', 'gate_release', 'rollback',
        'plan_revision', 'autonomy_grant')),
    subject_id TEXT NOT NULL,
    workitem_id TEXT REFERENCES workitems(id),
    stage_attempt_id TEXT REFERENCES stage_attempts(id),
    action_digest TEXT NOT NULL,
    risk TEXT NOT NULL CHECK (risk IN ('low', 'medium', 'high')),
    status TEXT NOT NULL DEFAULT 'requested'
        CHECK (status IN ('requested', 'approved', 'rejected', 'expired', 'changes_requested')),
    requested_by TEXT NOT NULL DEFAULT 'local',
    decided_by TEXT,
    decided_at TEXT,
    expires_at TEXT NOT NULL,
    reason TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);

INSERT INTO approvals_v34
    (id, subject_type, subject_id, workitem_id, stage_attempt_id, action_digest, risk,
     status, requested_by, decided_by, decided_at, expires_at, reason, created_at)
SELECT id, subject_type, subject_id, workitem_id, stage_attempt_id, action_digest, risk,
       status, requested_by, decided_by, decided_at, expires_at, reason, created_at
FROM approvals;

DROP TABLE approvals;
ALTER TABLE approvals_v34 RENAME TO approvals;
CREATE INDEX idx_approvals_subject ON approvals(subject_type, subject_id);
CREATE INDEX idx_approvals_wi_status ON approvals(workitem_id, status);
