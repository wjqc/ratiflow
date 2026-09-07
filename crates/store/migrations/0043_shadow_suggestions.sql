-- 0043：通用 suggestion/observation 基础设施（EvoFlow WP-8a，供 WP-8/10/12 消费）。
-- 三表修正 v1.1 单表自相矛盾（「不可变但 decision UPDATE」）与「无法写初始建议」：
--   建议不可变（无决定字段）→ 决定 append-only（一建议一终局，PK=suggestion_id）
--   → 复核 append-only（误报率分子来源：decision∈{rejected,expired} 且 false_positive=1）。

CREATE TABLE shadow_suggestions (
    id TEXT PRIMARY KEY,
    source TEXT NOT NULL CHECK (source IN ('fast_track','automation')),
    automation_id TEXT REFERENCES automations(id),
    workitem_id TEXT REFERENCES workitems(id),
    suggestion_type TEXT NOT NULL,
    suggestion_digest TEXT NOT NULL,
    content_json TEXT NOT NULL DEFAULT '{}',
    hypothetical_action_digest TEXT NOT NULL DEFAULT '',
    policy_version TEXT NOT NULL DEFAULT '',
    model TEXT NOT NULL DEFAULT '',
    prompt_version TEXT NOT NULL DEFAULT '',
    generated_at TEXT NOT NULL
);

CREATE INDEX idx_shadow_suggestions_source ON shadow_suggestions(source, generated_at);
CREATE INDEX idx_shadow_suggestions_automation ON shadow_suggestions(automation_id);

CREATE TABLE shadow_decisions (
    suggestion_id TEXT PRIMARY KEY REFERENCES shadow_suggestions(id),
    decision TEXT NOT NULL CHECK (decision IN ('accepted','rejected','ignored','expired')),
    decided_by TEXT NOT NULL,
    decided_at TEXT NOT NULL,
    note TEXT NOT NULL DEFAULT ''
);

CREATE TABLE shadow_reviews (
    id TEXT PRIMARY KEY,
    suggestion_id TEXT NOT NULL REFERENCES shadow_decisions(suggestion_id),
    false_positive INTEGER NOT NULL CHECK (false_positive IN (0,1)),
    reviewer TEXT NOT NULL,
    note TEXT NOT NULL DEFAULT '',
    reviewed_at TEXT NOT NULL
);

CREATE INDEX idx_shadow_reviews_suggestion ON shadow_reviews(suggestion_id);

-- automations 加列（WP-12 shadow policy 消费；本 WP 仅建列，tick 语义随 WP-12 激活）。
ALTER TABLE automations ADD COLUMN shadow_mode INTEGER NOT NULL DEFAULT 1;
