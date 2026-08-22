-- 0003_workitem: 工作项与六关阶段状态
CREATE TABLE workitems (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    gitlab_issue_iid INTEGER,
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    labels TEXT NOT NULL DEFAULT '[]',
    current_gate TEXT NOT NULL DEFAULT 'requirements'
        CHECK (current_gate IN ('requirements','design','development','testing','deployment','verification')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE workitem_stages (
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    gate TEXT NOT NULL CHECK (gate IN ('requirements','design','development','testing','deployment','verification')),
    state TEXT NOT NULL DEFAULT 'not_started'
        CHECK (state IN ('not_started','running','blocked','awaiting_approval','passed','failed','cancelled','stale')),
    input_baseline_sha TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL,
    PRIMARY KEY (workitem_id, gate)
);

CREATE INDEX idx_workitems_project ON workitems(project_id, created_at);
