-- 0040：门禁数据化收口 + RPC 幂等回执（EvoFlow 评审 P0-4 / P1 修复）。
-- 1) 每关验收策略 acceptance_json：随版本冻结、参与 content_digest v2；
--    空 = 沿用通用六输入门禁基线（存量行默认 '[]'）。
-- 2) rpc_receipts：显式 idempotencyKey 的 mutation 回执（key 唯一）——
--    重放返回首次响应而非重复执行（workflowTemplate/plan 域写路径消费）。

ALTER TABLE workflow_gate_definitions
    ADD COLUMN acceptance_json TEXT NOT NULL DEFAULT '[]';

CREATE TABLE rpc_receipts (
    idem_key TEXT PRIMARY KEY,
    method TEXT NOT NULL,
    response_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX idx_rpc_receipts_method ON rpc_receipts(method, created_at);
