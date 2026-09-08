-- 0052：gate skip operations v2（审计《Ratiflow_RDWS_v1.4实施审计与整改方案_v1.0》§6 0052 / §7 P0-3；
-- 权威规格 v1.4 §WP-8「Skip 操作权威」+「权威豁免事实」）。
-- 只追加事实，禁止 downgrade 删除。本迁移落表与存量投影迁移；writer 语义随 P0-3 激活。
--
-- 1) gate_skip_requests：跳关 operation 状态机（progress 承载两步执行的崩溃恢复游标）。
--    Step A（DB 事务）：requested→executing/progress=stage_advanced——当前活跃 attempt→superseded、
--    stage→skipped、指针推进、创建下一关 preparing attempt，同事务写 audit/outbox。
--    Step B（可恢复对象写）：先 CAS put 下一关 entry snapshot 对象（幂等，内容寻址），
--    再单事务绑定 snapshot 行 + attempt.entry_snapshot_id/state=prepared +
--    progress=next_attempt_ready + request=completed。失败 → blocked 保留 progress，
--    gate.resumeSkip 按 progress 幂等恢复。
--    action_digest 绑 (workitem|template_version|gate|attempt|current_state_digest|waiver|
--    替代证据内容摘要)——不仅绑 evidence id。
-- 2) gate_fast_track_waivers：权威豁免事实（采纳建议 ≠ 应用豁免——两动作分离）。
--    effective_required = 模板 deliverables − active waivers 的 kind 集；
--    revoke 使 (workitem,gate) 的 pending 放行失效（安全收紧通道不受 flag 关闭影响）。
-- 3) 存量迁移：已 approved 的 gate_skip 审批 → legacy_completed 只读投影
--    （digest 列无法重建，置空——绝不冒充 v2 可恢复 operation）；
--    pending 的 gate_skip 审批 → requested 投影（可经 decideSkip 作废，不可批准——
--    空 current_state_digest 上批准一律 gate_skip_state_changed，请重新发起 v2 请求）。

CREATE TABLE gate_skip_requests (
    id TEXT PRIMARY KEY,
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    gate TEXT NOT NULL,
    stage_attempt_id TEXT NOT NULL REFERENCES stage_attempts(id),
    template_version_id TEXT NOT NULL,
    current_state_digest TEXT NOT NULL DEFAULT '',
    pre_state_digest TEXT NOT NULL DEFAULT '',
    post_step_a_digest TEXT NOT NULL DEFAULT '',
    waiver TEXT NOT NULL,
    substitute_evidence_json TEXT NOT NULL DEFAULT '[]',
    substitute_evidence_digest TEXT NOT NULL DEFAULT '',
    approval_id TEXT UNIQUE REFERENCES approvals(id),
    action_digest TEXT NOT NULL UNIQUE,
    next_attempt_id TEXT,
    progress TEXT NOT NULL DEFAULT 'prepared'
        CHECK (progress IN ('prepared','stage_advanced','next_attempt_ready')),
    state TEXT NOT NULL DEFAULT 'requested'
        CHECK (state IN ('requested','executing','completed','blocked','rejected','expired',
                         'legacy_completed')),
    blocked_reason TEXT NOT NULL DEFAULT '',
    requested_by TEXT NOT NULL DEFAULT 'local',
    decided_by TEXT NOT NULL DEFAULT '',
    decided_at TEXT,
    completed_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_gate_skip_requests_wi ON gate_skip_requests(workitem_id, gate, created_at);

CREATE TABLE gate_fast_track_waivers (
    id TEXT PRIMARY KEY,
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    gate TEXT NOT NULL,
    stage_attempt_id TEXT NOT NULL REFERENCES stage_attempts(id),
    policy_digest TEXT NOT NULL,
    waived_kind TEXT NOT NULL,
    substitute_evidence_id TEXT NOT NULL,
    substitute_evidence_digest TEXT NOT NULL,
    rationale TEXT NOT NULL,
    suggestion_id TEXT REFERENCES shadow_suggestions(id),
    action_digest TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL CHECK (status IN ('active','revoked')),
    revoked_at TEXT,
    revoked_reason TEXT,
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX idx_gate_fast_track_waivers_scope
    ON gate_fast_track_waivers(workitem_id, gate, status);

-- 存量 approved gate_skip → legacy_completed 只读投影（approval_id 反查关联）。
INSERT INTO gate_skip_requests
    (id, workitem_id, gate, stage_attempt_id, template_version_id, current_state_digest,
     pre_state_digest, waiver, substitute_evidence_json, substitute_evidence_digest,
     approval_id, action_digest, progress, state, requested_by, decided_by, decided_at,
     completed_at, created_at, updated_at)
SELECT 'gsk_legacy_' || a.id,
       a.workitem_id,
       COALESCE((SELECT gate FROM stage_attempts s WHERE s.id = a.stage_attempt_id), ''),
       COALESCE(a.stage_attempt_id, ''),
       '',
       '', '', COALESCE(a.reason, ''), '[]', '',
       a.id, a.action_digest, 'prepared', 'legacy_completed',
       a.requested_by, COALESCE(a.decided_by,''), COALESCE(a.decided_at,''),
       COALESCE(a.decided_at,''), a.created_at, COALESCE(a.decided_at, a.created_at)
FROM approvals a
WHERE a.subject_type = 'gate_skip' AND a.status = 'approved';

-- 存量 pending gate_skip → requested 投影（仅可作废；批准一律拒绝——digest 无法重建）。
INSERT INTO gate_skip_requests
    (id, workitem_id, gate, stage_attempt_id, template_version_id, current_state_digest,
     pre_state_digest, waiver, substitute_evidence_json, substitute_evidence_digest,
     approval_id, action_digest, progress, state, requested_by, created_at, updated_at)
SELECT 'gsk_legacy_' || a.id,
       a.workitem_id,
       COALESCE((SELECT gate FROM stage_attempts s WHERE s.id = a.stage_attempt_id), ''),
       COALESCE(a.stage_attempt_id, ''),
       '',
       '', '', COALESCE(a.reason, ''), '[]', '',
       a.id, a.action_digest, 'prepared', 'requested',
       a.requested_by, a.created_at, a.created_at
FROM approvals a
WHERE a.subject_type = 'gate_skip' AND a.status = 'requested';
