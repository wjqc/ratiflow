-- 0044：skip / fast-track 拆分（EvoFlow WP-8；digest v4 先行落列）。
-- 1) 护照 outcome（v1.1 的 passed:true,skipped:true 作废）：显式结果四态，
--    存量行默认 passed（兼容）。完成谓词 = outcome ∈ {passed, skipped_with_waiver}；
--    skipped 关照实输出 passed:false + outcome:'skipped_with_waiver'（旧消费者
--    保守视为未全通过 = fail-closed 方向，取舍见模板层文档）。
ALTER TABLE passport_gates ADD COLUMN outcome TEXT NOT NULL DEFAULT 'passed'
    CHECK (outcome IN ('passed','skipped_with_waiver','failed','unknown'));
ALTER TABLE passport_gates ADD COLUMN waiver_approval_id TEXT NOT NULL DEFAULT '';

-- 2) 关卡级 skip / fast-track 策略（随版本冻结、参与 digest v4；'{}' = 禁/缺省）。
--    skip_policy：{mode: forbidden|manual_approval}；fast_track_policy：
--    {skippable_activities[], waived_deliverables[], reduced_approval}（schema 严格，
--    未知字段创建即拒——reduced_approval 永不豁免 Tool 风险/Policy 强制审批）。
ALTER TABLE workflow_gate_definitions ADD COLUMN skip_policy_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE workflow_gate_definitions ADD COLUMN fast_track_policy_json TEXT NOT NULL DEFAULT '{}';

-- 3) workitem_stages 重建：state CHECK 增 'skipped'（WP-8 新终态；CHECK 变更走
--    表重建，0032 先例。无独立索引需重建）。
CREATE TABLE workitem_stages_v44 (
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    gate TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'not_started'
        CHECK (state IN ('not_started','running','blocked','awaiting_approval','passed','skipped','failed','cancelled','stale')),
    input_baseline_sha TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL,
    PRIMARY KEY (workitem_id, gate)
);

INSERT INTO workitem_stages_v44 (workitem_id, gate, state, input_baseline_sha, updated_at)
SELECT workitem_id, gate, state, input_baseline_sha, updated_at FROM workitem_stages;

DROP TABLE workitem_stages;
ALTER TABLE workitem_stages_v44 RENAME TO workitem_stages;
