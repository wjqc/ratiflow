-- 0054：knowledge verification v2（审计《Ratiflow_RDWS_v1.4实施审计与整改方案_v1.0》§6 0054 / §7 P0-6；
-- 权威规格 v1.4 §WP-13 B10 / RDWS-019）。
--
-- 0049 的三个语义缺陷（审计 §3 RDWS-019「语义反向」）：
--   1) 查询仅按 stable_id 无项目作用域——跨项目相同 stable id 串数据；
--   2) freshness 取「最近 outcome=pass」——新 fail/unknown 被旧 pass 遮蔽；
--   3) 幂等键含 outcome——同一操作改判产生并行事实，且排序用 rowid（跨备份语义漂移）。
-- 修复原则（审计 §6）：**新建 v2 表而不是原地猜测修复**。
--
-- v2 权威语义：
--   - 作用域：project_id + source_id（FK knowledge_sources）+ stable_id（manifest 域身份）；
--   - 锚点：verified_input_revision + input_revision_mode 四态
--     （committed/worktree/content_hash/remote_version，服务端按 source kind 解析，
--      RPC 不接受 revision）；
--   - 操作幂等：verification_op_id UNIQUE（跨 transport key 的领域操作键）；
--     每次 verify 是一个事件（重新 pass 是新事实，时间戳推进后即为最新）——
--     0049 的 (…,outcome,verifier) 域键会使改判无法翻面，废除；
--   - 排序：最后一条权威 receipt 按 (verified_at, id)——rowid 不参与语义。
--
-- 0049 存量：manifest 文件与 source 投影行在应用层数据（迁移 SQL 内不可解析
-- stable_id → source 行映射），**不猜测迁移**——旧表保留为只读历史事实，
-- v2 从空开始，存量源要求重新验证（审计：无法唯一解析即 legacy/unverified）。

CREATE TABLE knowledge_verifications_v2 (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    source_id TEXT NOT NULL REFERENCES knowledge_sources(id),
    stable_id TEXT NOT NULL,
    verified_input_revision TEXT NOT NULL,
    input_revision_mode TEXT NOT NULL
        CHECK (input_revision_mode IN ('committed','worktree','content_hash','remote_version')),
    verified_input_digest TEXT NOT NULL DEFAULT '',
    outcome TEXT NOT NULL CHECK (outcome IN ('pass','fail','unknown')),
    verifier TEXT NOT NULL,
    verification_op_id TEXT NOT NULL UNIQUE,
    policy_version TEXT NOT NULL,
    verified_at TEXT NOT NULL,
    evidence_ref TEXT NOT NULL DEFAULT '',
    legacy INTEGER NOT NULL DEFAULT 0 CHECK (legacy IN (0,1)),
    created_at TEXT NOT NULL
);

-- 当前 revision 的权威 receipt 查询路径（freshness：按 (verified_at,id) 取最后一条）。
CREATE INDEX idx_knowledge_verifications_current
    ON knowledge_verifications_v2(project_id, stable_id, verified_input_revision,
                                  input_revision_mode, verified_at, id);

-- 迁移 manifest：存量行数与处置方式落审计（不猜测迁移的证据）。
INSERT INTO audit_log(actor, action, target_type, target_id, detail, created_at)
SELECT 'system', 'knowledge_verification_v2_manifest', 'migration', '0054',
       json_object('legacyTablePreserved', 'knowledge_verification_receipts',
                   'legacyRows', (SELECT COUNT(*) FROM knowledge_verification_receipts),
                   'policy', 're-verification required; legacy table read-only'),
       CURRENT_TIMESTAMP;
