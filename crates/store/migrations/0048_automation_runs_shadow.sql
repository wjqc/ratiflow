-- 0048：automation_runs.status CHECK 增补（WP-12 shadow policy；CHECK 变更走表
-- 重建，0032/0044 先例）：增 'shadowed'（shadow tick 只产建议）、'shadow_fallback'
-- （误报率触发自动回 shadow）、'deduped'（receipt 去重，既往走应用层未落账的形态
-- 一并收编）；automations.shadow_mode 列已在 0043 就位。

CREATE TABLE automation_runs_v48 (
    id TEXT PRIMARY KEY,
    automation_id TEXT NOT NULL REFERENCES automations(id),
    scheduled_for TEXT NOT NULL,
    receipt TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'fired'
        CHECK (status IN ('fired','intent_created','skipped_misfire','skipped_overlap',
                          'blocked_no_grant','failed','shadowed','shadow_fallback','deduped')),
    run_intent_id TEXT,
    note TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    UNIQUE (automation_id, scheduled_for)
);

INSERT INTO automation_runs_v48
    (id, automation_id, scheduled_for, receipt, status, run_intent_id, note, created_at)
SELECT id, automation_id, scheduled_for, receipt, status, run_intent_id, note, created_at
FROM automation_runs;

DROP TABLE automation_runs;
ALTER TABLE automation_runs_v48 RENAME TO automation_runs;

CREATE INDEX idx_aruns_automation ON automation_runs(automation_id, created_at);
