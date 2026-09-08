-- 0051：shadow automation policy v2（审计《Ratiflow_RDWS_v1.4实施审计与整改方案_v1.0》§6 0051 / §7 P0-2/P0-5）。
-- 只追加事实，禁止 downgrade 删除。本迁移只落 schema 与存量回填；writer/consumer
-- 语义（CAS 回退、两窗迟滞、cooldown、RunIntent）随 P0-5 激活。
--
-- 1) shadow_suggestions v2 重建（CHECK 变更走表重建，0034 先例）：
--    - scope_key：fast_track→workitem_id / automation→automation_id（CHECK 强制成对）；
--    - input_state_digest：服务端派生的输入状态摘要（P0-2 权威事实冻结）；
--    - expires_at：建议有效期（过期由领域钩子/worker 写 expired 决定，不改建议行）；
--    - legacy：存量行无法证明 scope/input digest → 标记只读，不参与 live 门槛；
--    - UNIQUE(source, scope_key, suggestion_digest) 取代 0050 的全局 digest 唯一索引
--      （表重建后旧索引随旧表消失；同 scope 内 digest 仍唯一，跨 scope 允许）。
-- 2) automations 补 shadow policy 状态列（P0-5 消费）。
-- 3) automation_policy_transitions：模式切换事实，唯一键防同快照重复切换/重复通知。
-- 4) notification_outbox 幂等键：空串（存量/无键）不参与唯一约束。

CREATE TABLE shadow_suggestions_v2 (
    id TEXT PRIMARY KEY,
    source TEXT NOT NULL CHECK (source IN ('fast_track','automation')),
    automation_id TEXT REFERENCES automations(id),
    workitem_id TEXT REFERENCES workitems(id),
    scope_key TEXT NOT NULL,
    suggestion_type TEXT NOT NULL,
    suggestion_digest TEXT NOT NULL,
    content_json TEXT NOT NULL DEFAULT '{}',
    hypothetical_action_digest TEXT NOT NULL DEFAULT '',
    policy_version TEXT NOT NULL DEFAULT '',
    input_state_digest TEXT NOT NULL DEFAULT '',
    expires_at TEXT,
    legacy INTEGER NOT NULL DEFAULT 0 CHECK (legacy IN (0,1)),
    model TEXT NOT NULL DEFAULT '',
    prompt_version TEXT NOT NULL DEFAULT '',
    generated_at TEXT NOT NULL,
    CHECK ((source='fast_track' AND workitem_id IS NOT NULL AND automation_id IS NULL
            AND scope_key=workitem_id)
        OR (source='automation' AND automation_id IS NOT NULL
            AND scope_key=automation_id)),
    UNIQUE(source, scope_key, suggestion_digest)
);

INSERT INTO shadow_suggestions_v2
    (id, source, automation_id, workitem_id, scope_key, suggestion_type, suggestion_digest,
     content_json, hypothetical_action_digest, policy_version, input_state_digest, expires_at,
     legacy, model, prompt_version, generated_at)
SELECT id, source, automation_id, workitem_id,
       COALESCE(workitem_id, automation_id, ''),
       suggestion_type, suggestion_digest, content_json, hypothetical_action_digest,
       policy_version, '', NULL, 1, model, prompt_version, generated_at
FROM shadow_suggestions;

DROP TABLE shadow_suggestions;
ALTER TABLE shadow_suggestions_v2 RENAME TO shadow_suggestions;

CREATE INDEX idx_shadow_suggestions_source ON shadow_suggestions(source, generated_at);
CREATE INDEX idx_shadow_suggestions_automation ON shadow_suggestions(automation_id);
CREATE INDEX idx_shadow_suggestions_scope ON shadow_suggestions(source, scope_key, generated_at);

ALTER TABLE automations ADD COLUMN cooldown_started_at TEXT NOT NULL DEFAULT '';
ALTER TABLE automations ADD COLUMN bad_window_streak INTEGER NOT NULL DEFAULT 0;
ALTER TABLE automations ADD COLUMN last_metric_snapshot_digest TEXT NOT NULL DEFAULT '';

CREATE TABLE automation_policy_transitions (
    id TEXT PRIMARY KEY,
    automation_id TEXT NOT NULL REFERENCES automations(id),
    metric_snapshot_digest TEXT NOT NULL,
    target_mode INTEGER NOT NULL CHECK (target_mode IN (0,1)),
    expected_revision INTEGER NOT NULL,
    review_coverage REAL NOT NULL,
    sample_size INTEGER NOT NULL,
    false_positives INTEGER NOT NULL,
    window_days INTEGER NOT NULL,
    policy_version TEXT NOT NULL,
    actor TEXT NOT NULL CHECK (actor IN ('system','user')),
    created_at TEXT NOT NULL,
    UNIQUE(automation_id, metric_snapshot_digest, target_mode)
);

ALTER TABLE notification_outbox ADD COLUMN idempotency_key TEXT NOT NULL DEFAULT '';
CREATE UNIQUE INDEX idx_notification_outbox_idem
    ON notification_outbox(idempotency_key)
    WHERE idempotency_key != '';
