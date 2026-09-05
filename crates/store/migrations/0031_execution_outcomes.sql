-- 0031：一等 ToolExecutionOutcome（EvoFlow 方案 M0-06 / §7.2 / §3 不变量 7）。
-- 背景：MCP 写超时曾以 Ok("tool_outcome_unknown: ...") 返回，却被统一标成 executed；
-- 执行不确定性必须是一等终态：unknown/indeterminate 不得落成 executed，不得透明重试。

-- 1) 执行结果事实表：一次提案至多一条 outcome（幂等键 = proposal_id）。
--    reconciliation 记录对账推进：unknown 默认 pending，查证后落到 confirmed_* /
--    manual_action_required；ok/failed 无需对账（none）。
CREATE TABLE tool_execution_outcomes (
    id TEXT PRIMARY KEY,
    proposal_id TEXT NOT NULL UNIQUE REFERENCES tool_proposals(id),
    outcome TEXT NOT NULL
        CHECK (outcome IN ('ok', 'failed', 'unknown', 'indeterminate')),
    reason TEXT NOT NULL DEFAULT '',
    reconciliation TEXT NOT NULL DEFAULT 'none'
        CHECK (reconciliation IN (
            'none', 'pending', 'confirmed_not_executed',
            'confirmed_executed', 'manual_action_required')),
    reconciled_at TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX idx_tool_outcomes_reconciliation
    ON tool_execution_outcomes(reconciliation);

-- 2) tool_proposals 重建：decision 扩 'unknown'/'indeterminate'
--    （SQLite 不能修改 CHECK 约束，只能重建换表；存量行原样保留，事实只追加）。
CREATE TABLE tool_proposals_v31 (
    id TEXT PRIMARY KEY,
    agent_run_id TEXT NOT NULL REFERENCES agent_runs(id),
    tool TEXT NOT NULL,
    arguments TEXT NOT NULL,
    risk TEXT NOT NULL CHECK (risk IN ('low', 'medium', 'high')),
    action_digest TEXT NOT NULL,
    requires_approval INTEGER NOT NULL,
    decision TEXT NOT NULL DEFAULT 'proposed'
        CHECK (decision IN (
            'proposed', 'approved', 'rejected', 'executed', 'exec_failed',
            'cancelled', 'unknown', 'indeterminate')),
    result TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);

INSERT INTO tool_proposals_v31
    (id, agent_run_id, tool, arguments, risk, action_digest,
     requires_approval, decision, result, created_at)
SELECT id, agent_run_id, tool, arguments, risk, action_digest,
       requires_approval, decision, result, created_at
FROM tool_proposals;

DROP TABLE tool_proposals;
ALTER TABLE tool_proposals_v31 RENAME TO tool_proposals;

-- 3) v3 纪元元数据（§7.1）：标记运行库纪元，供诊断与回退提示读取。
INSERT INTO app_meta(key, value) VALUES ('db_epoch', 'v3')
    ON CONFLICT(key) DO UPDATE SET value = 'v3', updated_at = CURRENT_TIMESTAMP;
