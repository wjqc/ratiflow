-- 0014_v3_context_items: Context Manifest 明细（F04）：来源/片段/附件与选用理由
CREATE TABLE context_manifest_items (
    manifest_id TEXT NOT NULL REFERENCES context_manifests(id),
    source_id TEXT REFERENCES knowledge_sources(id),
    attachment_id TEXT REFERENCES attachments(id),
    object_sha256 TEXT NOT NULL,
    purpose TEXT NOT NULL,
    included INTEGER NOT NULL,
    reason TEXT NOT NULL DEFAULT '',
    ordinal INTEGER NOT NULL,
    PRIMARY KEY (manifest_id, ordinal)
);
