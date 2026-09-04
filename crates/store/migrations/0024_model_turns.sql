-- 0024_model_turns: 模型交互轮次观测（ADR-033 / Codex 能力差距方案 M0）。
-- additive 只增；仅记录协议、用量、延迟与终态，不存 prompt/response 正文。
CREATE TABLE model_turns (
    id TEXT PRIMARY KEY,
    agent_run_id TEXT NOT NULL,
    turn_seq INTEGER NOT NULL,
    protocol TEXT NOT NULL DEFAULT 'legacy_json',
    capability_digest TEXT NOT NULL DEFAULT '',
    provider TEXT NOT NULL DEFAULT '',
    model TEXT NOT NULL DEFAULT '',
    tokens_in INTEGER NOT NULL DEFAULT 0,
    tokens_out INTEGER NOT NULL DEFAULT 0,
    cached_tokens INTEGER NOT NULL DEFAULT 0,
    reasoning_tokens INTEGER NOT NULL DEFAULT 0,
    ttft_ms INTEGER,
    total_ms INTEGER NOT NULL DEFAULT 0,
    finish_reason TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'ok' CHECK (status IN ('ok','failed','cancelled','unknown')),
    error_code TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);

CREATE INDEX idx_model_turns_run ON model_turns(agent_run_id, turn_seq);
CREATE INDEX idx_model_turns_created ON model_turns(created_at);
