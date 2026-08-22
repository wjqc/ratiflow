-- 0013_v3_attachments: 多模态附件（F03）
CREATE TABLE attachments (
    id TEXT PRIMARY KEY,
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    kind TEXT NOT NULL CHECK (kind IN ('document','image')),
    filename TEXT NOT NULL,
    content_type TEXT NOT NULL,
    object_sha256 TEXT NOT NULL,
    size INTEGER NOT NULL,
    parse_state TEXT NOT NULL DEFAULT 'pending'
        CHECK (parse_state IN ('pending','parsed','failed','vision_unsupported')),
    extracted_object_sha256 TEXT,
    error TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);

CREATE INDEX idx_attachments_workitem ON attachments(workitem_id, created_at);
