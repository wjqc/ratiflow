-- 0041：schema 约束修复（缺陷审计 2026-09-07）+ receipt lease 三态
-- （RDWS 实施计划 v1.4 §1.3 最终结构）。
-- 1) rpc_receipts 重建：
--    - 主键 (method, idem_key)——跨方法复用 key 不再串台返回无关响应；
--    - request_fingerprint = sha256(canonical params 去除 idempotencyKey)——同 key 异参重放
--      在执行前被拒（rpc_receipt_fingerprint_mismatch），不返回无关响应；
--    - owner/lease_revision/lease_expires_at/lease_state —— 认领、retryable_failed→in_flight、
--      过期接管都必须条件 UPDATE 命中 1 行才获得执行权（CAS）；
--    - lease_state：in_flight（执行者持有）/ retryable_failed（transient 错误释放执行语义）/
--      completed（成功或 deterministic 错误终态，错误 envelope 可重放）；
--    - response_json 同时承载错误 envelope（空串 = 执行中占位）。
--    存量行迁移语义：response_json 非空 → completed（保持可重放）；空占位（旧 crash 占位，
--    无 owner 事实）→ retryable_failed（owner=''，可被下一次请求 CAS 认领，不永久阻塞）。
--    request_fingerprint=''（legacy）= 跳过指纹比对，维持旧重放行为。
CREATE TABLE rpc_receipts_v41 (
    method TEXT NOT NULL,
    idem_key TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL DEFAULT '',
    owner TEXT NOT NULL DEFAULT '',
    lease_revision INTEGER NOT NULL DEFAULT 1,
    lease_expires_at TEXT,
    lease_state TEXT NOT NULL CHECK(lease_state IN ('in_flight','retryable_failed','completed')),
    response_json TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (method, idem_key)
);
INSERT INTO rpc_receipts_v41
    (method, idem_key, request_fingerprint, owner, lease_revision, lease_state,
     response_json, created_at, updated_at)
SELECT method, idem_key, '', '', 1,
       CASE WHEN response_json != '' THEN 'completed' ELSE 'retryable_failed' END,
       response_json, created_at, created_at
FROM rpc_receipts;
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
