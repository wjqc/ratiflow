-- 0039：自动化调度与 Goal（EvoFlow 方案 M6-01 / ADR-037 §6.5、ADR-039 §6.13）。
-- Automation = durable schedule + RunIntent + TriggerReceipt：只产 intent 不旁路
-- （预算/Context/Policy/Autonomy/Quota 检查在 intent 消费侧）；receipt 幂等
-- （同 automation+scheduled_for 唯一）承接重复启动与时钟跳变（M6 退出标准）。

CREATE TABLE automations (
    id TEXT PRIMARY KEY,
    key TEXT NOT NULL UNIQUE,
    project_id TEXT REFERENCES projects(id),
    workitem_id TEXT REFERENCES workitems(id),
    intent_json TEXT NOT NULL DEFAULT '{}',
    interval_secs INTEGER NOT NULL CHECK (interval_secs BETWEEN 1 AND 31536000),
    next_fire_at TEXT NOT NULL,
    misfire_policy TEXT NOT NULL DEFAULT 'skip'
        CHECK (misfire_policy IN ('skip','run_once','catch_up_one')),
    overlap_policy TEXT NOT NULL DEFAULT 'skip'
        CHECK (overlap_policy IN ('skip','queue_one')),
    autonomy_grant_id TEXT REFERENCES autonomy_grants(id),
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active','paused','disabled')),
    revision INTEGER NOT NULL DEFAULT 1,
    created_by TEXT NOT NULL DEFAULT 'local',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_automations_due ON automations(status, next_fire_at);

-- 触发回执：同 (automation_id, scheduled_for) 唯一——重复启动/时钟跳变去重锚点。
CREATE TABLE automation_runs (
    id TEXT PRIMARY KEY,
    automation_id TEXT NOT NULL REFERENCES automations(id),
    scheduled_for TEXT NOT NULL,
    receipt TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'fired'
        CHECK (status IN ('fired','intent_created','skipped_misfire','skipped_overlap',
                          'blocked_no_grant','failed')),
    run_intent_id TEXT,
    note TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    UNIQUE (automation_id, scheduled_for)
);

CREATE INDEX idx_aruns_automation ON automation_runs(automation_id, created_at);

-- 通知 outbox（桌面通知首期；Feishu/WeCom 为后续受控 Integration）。
CREATE TABLE notification_outbox (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL
        CHECK (kind IN ('approval_required','goal_paused','automation_blocked',
                        'automation_failed','unknown_detected')),
    workitem_id TEXT REFERENCES workitems(id),
    automation_id TEXT REFERENCES automations(id),
    payload_json TEXT NOT NULL DEFAULT '{}',
    delivered INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);

CREATE INDEX idx_notifications_undelivered ON notification_outbox(delivered, created_at);
