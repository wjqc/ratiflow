-- 0062：关卡运行上下文绑定（gate_context）。
-- 每关 Run 启动时服务端固定装配"原始目标 + 验收标准 + 上游已批准产物（冻结修订）"，
-- meta JSON 进 object store，本表存指针：界面核对"本次已读取哪些上游交付物及版本"，
-- 也是产出核验的对照基准。一行一 Run，重复启动活动不覆盖（ON CONFLICT DO NOTHING）。
CREATE TABLE agent_run_gate_context (
    agent_run_id  TEXT PRIMARY KEY REFERENCES agent_runs(id),
    manifest_id   TEXT NOT NULL REFERENCES context_manifests(id),
    gate          TEXT NOT NULL,
    object_sha256 TEXT NOT NULL,
    bytes         INTEGER NOT NULL,
    created_at    TEXT NOT NULL
);

CREATE INDEX idx_agent_run_gate_context_manifest ON agent_run_gate_context(manifest_id);
