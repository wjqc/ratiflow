-- 0055：durable run intents（审计《Ratiflow_RDWS_v1.4实施审计与整改方案_v1.0》§6 0055 / §7 P0-5；
-- 权威规格 v1.4 §WP-12 live 消费链）。
--
-- WP-12 的 live 触发只落 outbox 事件（automation.triggered），无 durable 消费链——
-- 崩溃/重启后意图丢失，也无法对账「意图 → Run → 终态」。0055 落权威意图表：
--   - 冻结面：intent_json / policy_snapshot / context_digest / grant_digest（消费时
--     不读漂移的当前配置）；
--   - 幂等：idempotency_key UNIQUE（= ri|automation|scheduled_for）；
--   - 消费链状态机：pending → claimed（owner/lease CAS）→ consumed（回填 run_id）；
--     transient 失败回 pending（attempt 上限后 unknown）；租约过期 → unknown；
--     deterministic 失败 → cancelled；
--   - consumer 只调用既有 agent.start 装配入口（内部 tx-safe，不直接调
--     Provider/Tool，不推进 Gate）。
-- automation_runs.run_intent_id 补 FK（0048 已有列但无约束；CHECK 不变走表重建）。

CREATE TABLE run_intents (
    id TEXT PRIMARY KEY,
    source TEXT NOT NULL CHECK (source IN ('automation')),
    automation_id TEXT NOT NULL REFERENCES automations(id),
    workitem_id TEXT REFERENCES workitems(id),
    intent_json TEXT NOT NULL,
    policy_snapshot TEXT NOT NULL DEFAULT '',
    context_digest TEXT NOT NULL DEFAULT '',
    grant_digest TEXT NOT NULL DEFAULT '',
    idempotency_key TEXT NOT NULL UNIQUE,
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending','claimed','consumed','unknown',
                         'reconciliation_required','manual_action_required','cancelled')),
    attempt INTEGER NOT NULL DEFAULT 0,
    claimed_by TEXT NOT NULL DEFAULT '',
    lease_expires_at TEXT,
    run_id TEXT,
    blocked_reason TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_run_intents_state ON run_intents(state, created_at);
CREATE INDEX idx_run_intents_automation ON run_intents(automation_id, created_at);

-- automation_runs 重建挂 FK（列集与 0048 完全一致；存量行原样保留）。
CREATE TABLE automation_runs_v55 (
    id TEXT PRIMARY KEY,
    automation_id TEXT NOT NULL REFERENCES automations(id),
    scheduled_for TEXT NOT NULL,
    receipt TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'fired'
        CHECK (status IN ('fired','intent_created','skipped_misfire','skipped_overlap',
                          'blocked_no_grant','failed','shadowed','shadow_fallback','deduped')),
    run_intent_id TEXT REFERENCES run_intents(id),
    note TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    UNIQUE (automation_id, scheduled_for)
);

INSERT INTO automation_runs_v55
    (id, automation_id, scheduled_for, receipt, status, run_intent_id, note, created_at)
SELECT id, automation_id, scheduled_for, receipt, status,
       NULLIF(run_intent_id, ''), note, created_at
FROM automation_runs;

DROP TABLE automation_runs;
ALTER TABLE automation_runs_v55 RENAME TO automation_runs;
CREATE INDEX idx_aruns_automation ON automation_runs(automation_id, created_at);
