-- 0049：B10 知识验证事实（EvoFlow WP-13；本机 SQLite 权威，manifest 只声明策略）。
-- 编号注：计划原定 0047 已被 workitem_search 占用，顺延至此。
-- 幂等：同 (stable_id, 被验证内容版本, outcome, verifier) 至多一条 receipt
--（重复人工复核原样返回既有行）；verified_input_digest = 被验证的 manifest
-- contentSha256——内容变更后旧验证立即不匹配（freshness 派生见 sg-knowledge）。

CREATE TABLE knowledge_verification_receipts (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    stable_id TEXT NOT NULL,
    verified_input_digest TEXT NOT NULL,
    verifier TEXT NOT NULL,
    policy_version TEXT NOT NULL DEFAULT '',
    verified_at TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('pass','fail','unknown')),
    evidence_ref TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    UNIQUE(stable_id, verified_input_digest, outcome, verifier)
);

CREATE INDEX idx_kvr_stable ON knowledge_verification_receipts(stable_id, verified_at DESC);
