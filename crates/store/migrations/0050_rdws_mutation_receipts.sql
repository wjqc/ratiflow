-- 0050：RDWS mutation receipt 收口（审计《Ratiflow_RDWS_v1.4实施审计与整改方案_v1.0》§6/§7 P0-1）。
-- rpc_receipts 表（lease 三态：指纹门/CAS 认领/completed envelope 重放）已由 0040 建立，
-- 本迁移不新建第二套 receipt 表；只为"先查后插、无数据库唯一约束"的伪幂等补上领域唯一键，
-- 使领域不变量落在 schema 而非代码顺序上：
--   1) gate_skip 审批：同 (subject_type, action_digest) 只允许一条审批行。
--      requestSkip 的 action_digest = sha256(gate_skip|workitem|gate|waiver|排序后证据 ids)，
--      确定性派生；重放/竞态由唯一约束兜底。partial index 只作用于 gate_skip，
--      其余 subject_type（gate_release/tool_proposal/rework 等）不受影响。
--   2) shadow 建议：suggestion_digest 全局唯一。fast_track 与 automation 的摘要均带
--      来源前缀（fast_track|…/automation_shadow|…），跨来源不碰撞；重放读既有行。
-- 领域 operation 表（gate_skip_requests/waivers、rework 恢复列、knowledge v2、
-- run_intents）由 0051～0055 追加；本迁移只加索引，禁止 downgrade 删除事实。
-- 前置数据说明：两条路径历史上均经单连接串行 + 先查后插，存量不应存在重复行；
-- 若迁移时命中重复，创建索引将失败并列出冲突行——按 §10.3 保留事实进入人工处置，
-- 不静默去重。

CREATE UNIQUE INDEX IF NOT EXISTS idx_approvals_gate_skip_digest
    ON approvals(subject_type, action_digest)
    WHERE subject_type = 'gate_skip';

CREATE UNIQUE INDEX IF NOT EXISTS idx_shadow_suggestions_digest
    ON shadow_suggestions(suggestion_digest);
