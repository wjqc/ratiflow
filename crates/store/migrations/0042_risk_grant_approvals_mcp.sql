-- 0042：RDWS 实施计划 v1.4 §1.6 批次——风险模型/Grant 计量/审批增强/MCP 沙箱与导入的
-- 统一 schema 落点。消费方按 WP 分批接入（WP-1 ledger / WP-2 ToolId / WP-3 send_phase /
-- WP-4 imports / WP-6 审批 digest / WP-7 manual confirm）。
-- 纪律：CHECK 变更走表重建（0034 先例），加列走 ALTER（0040 先例）。

-- 1) approvals 重建（WP-6/WP-7/WP-8/WP-9）：subject_type 扩五枚举 + 双 digest 列。
--    impact_digest = ActionDigest+影响面绑定（WP-6）；scope_facts_digest = incomplete/unknown
--    审批的全图摘要（WP-6）；digest_schema_version 标记 digest 公式版本（未知版本 decide
--    时 fail-closed）。存量行默认空串 = legacy 跳过比对，不进自动放行。
CREATE TABLE approvals_v42 (
    id TEXT PRIMARY KEY,
    subject_type TEXT NOT NULL CHECK (subject_type IN (
        'tool_proposal', 'deployment', 'baseline', 'risk', 'gate_release', 'rollback',
        'plan_revision', 'autonomy_grant',
        'mcp_import_probe', 'mcp_import_activate', 'gate_manual_confirm', 'gate_skip', 'rework')),
    subject_id TEXT NOT NULL,
    workitem_id TEXT REFERENCES workitems(id),
    stage_attempt_id TEXT REFERENCES stage_attempts(id),
    action_digest TEXT NOT NULL,
    impact_digest TEXT NOT NULL DEFAULT '',
    scope_facts_digest TEXT NOT NULL DEFAULT '',
    digest_schema_version INTEGER NOT NULL DEFAULT 0,
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

INSERT INTO approvals_v42
    (id, subject_type, subject_id, workitem_id, stage_attempt_id, action_digest,
     risk, status, requested_by, decided_by, decided_at, expires_at, reason, created_at)
SELECT id, subject_type, subject_id, workitem_id, stage_attempt_id, action_digest,
       risk, status, requested_by, decided_by, decided_at, expires_at, reason, created_at
FROM approvals;

DROP TABLE approvals;
ALTER TABLE approvals_v42 RENAME TO approvals;
-- 重建查询索引（DROP TABLE 连带删除 0017/0034 所建；先例均随重建补回）。
CREATE INDEX idx_approvals_subject ON approvals(subject_type, subject_id);
CREATE INDEX idx_approvals_wi_status ON approvals(workitem_id, status);

-- 2) tool_proposals 加列（WP-1 rationale/confidence、WP-2 tool_provider、WP-3 send_phase）。
ALTER TABLE tool_proposals ADD COLUMN rationale TEXT NOT NULL DEFAULT '';
ALTER TABLE tool_proposals ADD COLUMN confidence_json TEXT NOT NULL DEFAULT '';
ALTER TABLE tool_proposals ADD COLUMN tool_provider TEXT NOT NULL DEFAULT 'builtin';
ALTER TABLE tool_proposals ADD COLUMN send_phase TEXT NOT NULL DEFAULT 'not_sent'
    CHECK (send_phase IN (
        'not_sent', 'send_intent_persisted', 'request_flushed',
        'response_received', 'shutdown_after_response'));
ALTER TABLE tool_proposals ADD COLUMN provider_call_id TEXT NOT NULL DEFAULT '';
ALTER TABLE tool_proposals ADD COLUMN provider_evidence_json TEXT NOT NULL DEFAULT '';

-- 存量分类回填（WP-2 ToolId 契约）：旧 mcp__server__tool 前缀 → mcp，其余显式 builtin。
UPDATE tool_proposals SET tool_provider='mcp' WHERE tool LIKE 'mcp__%';
UPDATE tool_proposals SET tool_provider='builtin' WHERE tool_provider <> 'mcp';

-- 3) tool_execution_outcomes 加列（WP-3 调用阶段状态机 + Provider 证据）。
ALTER TABLE tool_execution_outcomes ADD COLUMN send_phase TEXT NOT NULL DEFAULT 'not_sent'
    CHECK (send_phase IN (
        'not_sent', 'send_intent_persisted', 'request_flushed',
        'response_received', 'shutdown_after_response'));
ALTER TABLE tool_execution_outcomes ADD COLUMN provider_evidence_json TEXT NOT NULL DEFAULT '';

-- 4) gate_manual_confirmations（WP-7 手工确认链）：confirmation 与 approval 主体分离，
--    evaluator 只读 confirmed 事实；action_digest UNIQUE 承载领域幂等。
CREATE TABLE gate_manual_confirmations (
    id TEXT PRIMARY KEY,
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    gate TEXT NOT NULL,
    stage_attempt_id TEXT NOT NULL REFERENCES stage_attempts(id),
    acceptance_item_digest TEXT NOT NULL,
    approval_id TEXT NOT NULL UNIQUE REFERENCES approvals(id),
    action_digest TEXT NOT NULL UNIQUE,
    state TEXT NOT NULL CHECK (state IN ('requested', 'confirmed', 'rejected', 'expired')),
    confirmed_by TEXT,
    confirmed_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- 5) grant_usage_ledger（WP-1 Grant 计量）：consumption_key 粒度的 reserve/settle——
--    模型每 model_call、工具每 proposal 各自独立行，一个 Run 多次消费互不撞键；
--    UNIQUE(grant_id, run_id, dimension, consumption_key) 承载领域幂等。
CREATE TABLE grant_usage_ledger (
    id TEXT PRIMARY KEY,
    grant_id TEXT NOT NULL REFERENCES autonomy_grants(id),
    run_id TEXT NOT NULL REFERENCES agent_runs(id),
    dimension TEXT NOT NULL
        CHECK (dimension IN ('model_calls', 'tokens_in', 'tokens_out', 'reasoning_tokens',
                             'tool_calls', 'cost_micros')),
    consumption_key TEXT NOT NULL,
    reserved_amount INTEGER NOT NULL CHECK (reserved_amount >= 0),
    settled_amount INTEGER CHECK (settled_amount >= 0),
    reservation_evidence_json TEXT NOT NULL DEFAULT '{}',
    settlement_evidence_json TEXT,
    state TEXT NOT NULL
        CHECK (state IN ('reserved', 'settled', 'reconciliation_required', 'manual_action_required')),
    reserved_at TEXT NOT NULL,
    settled_at TEXT,
    UNIQUE(grant_id, run_id, dimension, consumption_key)
);

CREATE INDEX idx_grant_usage_grant_state ON grant_usage_ledger(grant_id, state);
CREATE INDEX idx_grant_usage_run_state ON grant_usage_ledger(run_id, state);

-- 6) mcp_repo_imports（WP-4 Git 仓库导入）：八态状态机 + 内容冻结五元组 +
--    worker durable claim（owner/lease/attempt）。UNIQUE(repo_url,pinned_sha,manifest_digest)
--    ——同 ref 漂移 = 新行新候选，旧 active 不动。
CREATE TABLE mcp_repo_imports (
    id TEXT PRIMARY KEY,
    repo_url TEXT NOT NULL,
    ref_name TEXT NOT NULL,
    pinned_sha TEXT NOT NULL,
    manifest_digest TEXT NOT NULL,
    entrypoint_json TEXT NOT NULL DEFAULT '',
    schema_candidates_json TEXT NOT NULL DEFAULT '',
    content_freeze_json TEXT NOT NULL DEFAULT '',
    progress_cursor TEXT NOT NULL DEFAULT '',
    server_id TEXT,
    worker_owner TEXT NOT NULL DEFAULT '',
    worker_lease_expires_at TEXT,
    worker_attempt_no INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL CHECK (status IN (
        'imported', 'awaiting_probe_approval', 'probing', 'schema_candidate',
        'awaiting_activation', 'active', 'failed', 'unknown', 'revoked')),
    error TEXT NOT NULL DEFAULT '',
    created_by TEXT NOT NULL DEFAULT 'local',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(repo_url, pinned_sha, manifest_digest)
);
