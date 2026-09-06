-- 0038：Durable Trace、usage 诚实语义与 read model checkpoint
-- （EvoFlow 方案 M5-01 / ADR-039；原 §7.2 0037 顺延至此，见状态区裁定 8）。
-- span 只存结构化元数据与 digest，不存 prompt/secret/reasoning 正文。

-- 1) trace_spans：trace_id/span_id/parent_span_id 表达
--    workflow → plan → task → run → model/gateway/tool/middleware 层级。
CREATE TABLE trace_spans (
    id TEXT PRIMARY KEY,
    trace_id TEXT NOT NULL,
    span_id TEXT NOT NULL,
    parent_span_id TEXT,
    kind TEXT NOT NULL
        CHECK (kind IN ('workflow','plan','task','run','model','gateway','tool','middleware')),
    workitem_id TEXT REFERENCES workitems(id),
    plan_revision_id TEXT REFERENCES plan_revisions(id),
    task_attempt_id TEXT REFERENCES plan_task_attempts(id),
    run_id TEXT REFERENCES agent_runs(id),
    name TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'ok'
        CHECK (status IN ('ok','failed','unknown','running')),
    attrs_json TEXT NOT NULL DEFAULT '{}',
    attrs_sha256 TEXT NOT NULL DEFAULT '',
    started_at TEXT NOT NULL,
    finished_at TEXT,
    UNIQUE (span_id)
);

CREATE INDEX idx_spans_trace ON trace_spans(trace_id);
CREATE INDEX idx_spans_workitem ON trace_spans(workitem_id, started_at);

-- 2) usage 诚实语义：Provider 未提供的字段为 NULL（不填 0 冒充）。
ALTER TABLE model_turns ADD COLUMN cache_read_tokens INTEGER;
ALTER TABLE model_turns ADD COLUMN cache_write_tokens INTEGER;
ALTER TABLE model_turns ADD COLUMN usage_estimated INTEGER NOT NULL DEFAULT 0;
ALTER TABLE model_turns ADD COLUMN cost_state TEXT NOT NULL DEFAULT 'unknown'
    CHECK (cost_state IN ('unknown','estimated','priced'));

-- 3) read model checkpoint：驾驶舱从 durable facts 重建（UI 断线恢复）。
CREATE TABLE read_model_checkpoints (
    id TEXT PRIMARY KEY,
    workitem_id TEXT NOT NULL UNIQUE REFERENCES workitems(id),
    checkpoint_json TEXT NOT NULL,
    facts_sha256 TEXT NOT NULL,
    built_at TEXT NOT NULL
);
