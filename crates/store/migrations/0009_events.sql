-- 0009_events: 事件 outbox（SSE 回放）、追加式审计日志、内容寻址对象登记
CREATE TABLE events_outbox (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    aggregate_type TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    type TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL,
    dispatched_at TEXT
);

CREATE TABLE audit_log (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    target_type TEXT NOT NULL,
    target_id TEXT NOT NULL,
    detail TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL
);

CREATE TABLE objects (
    sha256 TEXT PRIMARY KEY,
    size INTEGER NOT NULL,
    content_type TEXT NOT NULL,
    secret_findings TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL
);
