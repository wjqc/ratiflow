-- 0016_provenance: 谱系底座（ADR-030 M1）。
-- 需求事实（文档/不可变修订/需求项）+ 不可变谱系节点与边。
-- 全部只追加：修订/条目/节点/边不 UPDATE 事实列；current 投影由应用层查询派生。

CREATE TABLE requirement_documents (
    id TEXT PRIMARY KEY,
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    -- inline=文字创建 / document=文档导入 / issue=GitLab Issue / legacy_import=旧 docs 目录回填
    source_kind TEXT NOT NULL
        CHECK (source_kind IN ('inline', 'document', 'issue', 'legacy_import')),
    source_ref TEXT NOT NULL DEFAULT '',
    title TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);
CREATE INDEX idx_req_docs_wi ON requirement_documents(workitem_id, created_at);

CREATE TABLE requirement_revisions (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES requirement_documents(id),
    revision_no INTEGER NOT NULL,
    -- 正文进 objects；库里只存哈希。
    object_sha256 TEXT NOT NULL,
    content_sha256 TEXT NOT NULL,
    created_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    supersedes_revision_id TEXT REFERENCES requirement_revisions(id),
    UNIQUE(document_id, revision_no),
    -- 同文档同内容幂等：重复导入不产生新修订。
    UNIQUE(document_id, content_sha256)
);

CREATE TABLE requirement_items (
    id TEXT PRIMARY KEY,
    revision_id TEXT NOT NULL REFERENCES requirement_revisions(id),
    requirement_key TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    body_sha256 TEXT NOT NULL DEFAULT '',
    anchor_json TEXT NOT NULL DEFAULT '{}',
    acceptance_json TEXT NOT NULL DEFAULT '[]',
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'removed', 'waived')),
    created_at TEXT NOT NULL,
    UNIQUE(revision_id, requirement_key)
);
CREATE INDEX idx_req_items_rev ON requirement_items(revision_id, requirement_key);
CREATE INDEX idx_req_items_key ON requirement_items(requirement_key);

CREATE TABLE provenance_nodes (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL DEFAULT '',
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    node_type TEXT NOT NULL CHECK (node_type IN (
        'requirement_revision', 'requirement_item', 'stage_attempt', 'activity',
        'agent_run', 'tool_proposal', 'artifact_revision', 'commit', 'mr',
        'pipeline', 'test_report', 'evidence', 'deployment', 'approval',
        'snapshot', 'passport')),
    entity_id TEXT NOT NULL,
    content_digest TEXT NOT NULL DEFAULT '',
    -- legacy 回填 = unverified；治理豁免 = waived。
    verification_state TEXT NOT NULL DEFAULT 'verified'
        CHECK (verification_state IN ('verified', 'unverified', 'waived')),
    created_at TEXT NOT NULL,
    UNIQUE(node_type, entity_id)
);
CREATE INDEX idx_prov_nodes_wi ON provenance_nodes(workitem_id);
CREATE INDEX idx_prov_nodes_entity ON provenance_nodes(node_type, entity_id);

CREATE TABLE provenance_edges (
    id TEXT PRIMARY KEY,
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    from_node_id TEXT NOT NULL REFERENCES provenance_nodes(id),
    relation TEXT NOT NULL CHECK (relation IN (
        'derived_from', 'satisfies', 'implements', 'verifies', 'produced_by',
        'uses', 'approves', 'supersedes', 'restores', 'waives',
        'part_of', 'executes', 'validates')),
    to_node_id TEXT NOT NULL REFERENCES provenance_nodes(id),
    -- M1 无 stage_attempts（0017 落表）；'' 表示不绑定 attempt，届时由应用层校验。
    stage_attempt_id TEXT NOT NULL DEFAULT '',
    created_by_run_id TEXT NOT NULL DEFAULT '',
    -- digest = sha256(from|relation|to|attempt|run) 的规范化拼接，防篡改可对账。
    edge_digest TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(from_node_id, relation, to_node_id, stage_attempt_id)
);
CREATE INDEX idx_prov_edges_from ON provenance_edges(from_node_id);
CREATE INDEX idx_prov_edges_to ON provenance_edges(to_node_id);
CREATE INDEX idx_prov_edges_wi ON provenance_edges(workitem_id, stage_attempt_id);
