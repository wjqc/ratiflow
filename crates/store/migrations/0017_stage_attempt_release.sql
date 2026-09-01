-- 0017_stage_attempt_release: ADR-030 M2 人工放行。
-- StageAttempt/activity/输出包/关卡放行请求；approvals 单事务安全重建
-- （subject_type 增 gate_release/rollback，status 增 changes_requested，增 workitem/attempt 作用域列）；
-- baselines 修正为 per-gate（修复按 WorkItem 全局 supersede 的 P0 缺陷）。
-- 迁移纪律（蓝图 §11.3）：additive 优先；approvals 重建在本事务内 copy→rename，
-- 行数一致性由 INSERT..SELECT 原子性保证；回退路径为迁移前自动快照（migration.rs）。

CREATE TABLE stage_attempts (
    id TEXT PRIMARY KEY,
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    gate TEXT NOT NULL,
    attempt_no INTEGER NOT NULL,
    branch_no INTEGER NOT NULL DEFAULT 1,
    state TEXT NOT NULL CHECK (state IN (
        'preparing', 'prepared', 'running', 'review_ready', 'awaiting_user_approval',
        'approved', 'changes_requested', 'rejected', 'superseded', 'rolled_back',
        'failed', 'cancelled')),
    -- entry_snapshot_id：0018 落 state_snapshots 后由应用校验提交态非空（蓝图 §4.1）。
    entry_snapshot_id TEXT NOT NULL DEFAULT '',
    input_package_sha256 TEXT NOT NULL DEFAULT '',
    active_output_package_id TEXT REFERENCES stage_output_packages(id),
    predecessor_attempt_id TEXT REFERENCES stage_attempts(id),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(workitem_id, gate, attempt_no)
);
CREATE INDEX idx_attempts_wi ON stage_attempts(workitem_id, gate, created_at DESC);
CREATE INDEX idx_attempts_state ON stage_attempts(state);
-- 每个 WorkItem 同一时刻至多一个活跃 attempt（蓝图 §5.2 单活跃约束）。
-- preparing 不占名额：快照失败即停留 preparing（不可执行，SG-RBK-001），不阻塞重试。
CREATE UNIQUE INDEX idx_attempts_single_active ON stage_attempts(workitem_id)
    WHERE state IN ('prepared', 'running', 'review_ready',
                    'awaiting_user_approval', 'changes_requested');

CREATE TABLE stage_activities (
    id TEXT PRIMARY KEY,
    stage_attempt_id TEXT NOT NULL REFERENCES stage_attempts(id),
    activity_key TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    required_capabilities_json TEXT NOT NULL DEFAULT '[]',
    output_contract_sha256 TEXT NOT NULL DEFAULT '',
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'running', 'done', 'skipped', 'failed')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(stage_attempt_id, activity_key)
);

CREATE TABLE stage_output_packages (
    id TEXT PRIMARY KEY,
    stage_attempt_id TEXT NOT NULL REFERENCES stage_attempts(id),
    package_no INTEGER NOT NULL,
    -- 清单正文进 objects；库内只存哈希与 digest。
    manifest_object_sha256 TEXT NOT NULL,
    digest TEXT NOT NULL,
    gate_evaluation_id TEXT NOT NULL REFERENCES gate_results(id),
    trace_coverage_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    UNIQUE(stage_attempt_id, package_no),
    UNIQUE(stage_attempt_id, digest)
);

CREATE TABLE stage_output_items (
    package_id TEXT NOT NULL REFERENCES stage_output_packages(id),
    node_id TEXT NOT NULL REFERENCES provenance_nodes(id),
    -- artifact_revision / evidence / requirement_item 等谱系节点角色。
    role TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    PRIMARY KEY (package_id, node_id, role)
);
CREATE INDEX idx_output_items_node ON stage_output_items(node_id);

CREATE TABLE gate_release_requests (
    id TEXT PRIMARY KEY,
    stage_attempt_id TEXT NOT NULL REFERENCES stage_attempts(id),
    output_package_id TEXT NOT NULL REFERENCES stage_output_packages(id),
    -- 与 approvals 同事务内回填（先建请求后建审批，避免循环插入依赖）。
    approval_id TEXT UNIQUE REFERENCES approvals(id),
    -- digest 按蓝图公式内容派生；全局唯一会阻止"要求修改后原样重提"。
    -- 真实不变量 = 每 attempt 同一 digest 至多一条 pending（部分唯一索引承载）。
    release_digest TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN (
        'pending', 'approved', 'rejected', 'changes_requested', 'expired', 'superseded')),
    created_at TEXT NOT NULL,
    decided_at TEXT
);
CREATE INDEX idx_release_attempt ON gate_release_requests(stage_attempt_id, state);
CREATE UNIQUE INDEX idx_release_pending_digest ON gate_release_requests(stage_attempt_id, release_digest)
    WHERE state = 'pending';

-- approvals 安全重建：扩 subject_type/status 作用域（蓝图 §5.2）。
CREATE TABLE approvals_new (
    id TEXT PRIMARY KEY,
    subject_type TEXT NOT NULL CHECK (subject_type IN (
        'tool_proposal', 'deployment', 'baseline', 'risk', 'gate_release', 'rollback')),
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
INSERT INTO approvals_new(
        id, subject_type, subject_id, workitem_id, stage_attempt_id, action_digest, risk,
        status, requested_by, decided_by, decided_at, expires_at, reason, created_at)
    SELECT id, subject_type, subject_id, NULL, NULL, action_digest, risk,
           status, requested_by, decided_by, decided_at, expires_at, reason, created_at
    FROM approvals;
DROP TABLE approvals;
ALTER TABLE approvals_new RENAME TO approvals;
CREATE INDEX idx_approvals_subject ON approvals(subject_type, subject_id);
CREATE INDEX idx_approvals_wi_status ON approvals(workitem_id, status);

-- baselines 修正（蓝图 §5.3）：增 attempt 绑定列 + per-gate 活跃唯一。
ALTER TABLE baselines ADD COLUMN stage_attempt_id TEXT REFERENCES stage_attempts(id);
CREATE UNIQUE INDEX idx_baselines_active_per_gate ON baselines(workitem_id, gate)
    WHERE superseded_by IS NULL;
