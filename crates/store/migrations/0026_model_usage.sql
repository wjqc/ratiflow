-- 0026_model_usage: 缓存与 reasoning 观测（ADR-033 / Codex 能力差距方案 M4）。
-- additive 只增；token 计量列不携带任何 reasoning 正文。
ALTER TABLE model_calls ADD COLUMN cached_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE model_calls ADD COLUMN reasoning_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE model_turns ADD COLUMN prompt_cache_key TEXT NOT NULL DEFAULT '';
CREATE INDEX idx_model_calls_run_cached ON model_calls(agent_run_id, cached_tokens);
