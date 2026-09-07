-- 0041：schema 约束修复（缺陷审计 2026-09-07）。
-- 1) rpc_receipts 主键从全局 idem_key 改为 (method, idem_key)——跨方法复用 key
--    不再串台返回无关响应；response_json 同时承载错误 envelope（空串 = 执行中占位）。
CREATE TABLE rpc_receipts_v41 (
    method TEXT NOT NULL,
    idem_key TEXT NOT NULL,
    response_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (method, idem_key)
);
INSERT INTO rpc_receipts_v41 (method, idem_key, response_json, created_at)
    SELECT method, idem_key, response_json, created_at FROM rpc_receipts;
DROP TABLE rpc_receipts;
ALTER TABLE rpc_receipts_v41 RENAME TO rpc_receipts;
CREATE INDEX idx_rpc_receipts_method ON rpc_receipts(method, created_at);

-- 2) workspace_policy_versions 单 active：原唯一索引建在主键 id 上是空约束，可插入
--    任意多行 active。改为对常量列 status 建部分唯一索引（全表至多一行 active）。
--    先把存量多行 active 收敛为最新一行 active、其余 deprecated，避免建索引失败。
UPDATE workspace_policy_versions SET status = 'deprecated', updated_at = created_at
WHERE status = 'active' AND id NOT IN (
    SELECT id FROM workspace_policy_versions WHERE status = 'active'
    ORDER BY created_at DESC, id DESC LIMIT 1
);
DROP INDEX IF EXISTS idx_wsp_single_active;
CREATE UNIQUE INDEX idx_wsp_single_active
    ON workspace_policy_versions(status) WHERE status = 'active';

-- 3) model_turns 序号唯一：MAX+1 取号在双开进程下会撞号，观测序列错乱。
--    先按 (run, seq) 去重（保留首写行），再建唯一索引承载约束。
DELETE FROM model_turns WHERE rowid NOT IN (
    SELECT MIN(rowid) FROM model_turns GROUP BY agent_run_id, turn_seq
);
CREATE UNIQUE INDEX idx_model_turns_run_seq ON model_turns(agent_run_id, turn_seq);
