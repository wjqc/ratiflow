-- 0032：数据化工作流模板（EvoFlow 方案 M1-01 / ADR-036）。
-- 模板三层：workflow_templates（逻辑身份）→ workflow_template_versions（不可变版本）
-- → workflow_instances（WorkItem 创建时冻结）+ workflow_instance_gates（运行投影）。
-- 同时移除 workitems.current_gate 与 workitem_stages.gate 的六值 CHECK
-- （gate_id 是模板版本内稳定字符串，动态校验移到服务层——schema 不再假设固定六关）。

-- 1) workitems 重建：去 current_gate 六值 CHECK（列集与 0003+0021 完全一致）。
CREATE TABLE workitems_v32 (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    gitlab_issue_iid INTEGER,
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    labels TEXT NOT NULL DEFAULT '[]',
    current_gate TEXT NOT NULL DEFAULT 'requirements',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    archived_at TEXT
);

INSERT INTO workitems_v32
    (id, project_id, gitlab_issue_iid, title, description, labels, current_gate, created_at, updated_at, archived_at)
SELECT id, project_id, gitlab_issue_iid, title, description, labels, current_gate, created_at, updated_at, archived_at
FROM workitems;

DROP TABLE workitems;
ALTER TABLE workitems_v32 RENAME TO workitems;
CREATE INDEX idx_workitems_project ON workitems(project_id, created_at);

-- 2) workitem_stages 重建：去 gate 六值 CHECK（运行时状态投影按实例 gate_id 记录）。
CREATE TABLE workitem_stages_v32 (
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    gate TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'not_started'
        CHECK (state IN ('not_started','running','blocked','awaiting_approval','passed','failed','cancelled','stale')),
    input_baseline_sha TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL,
    PRIMARY KEY (workitem_id, gate)
);

INSERT INTO workitem_stages_v32 (workitem_id, gate, state, input_baseline_sha, updated_at)
SELECT workitem_id, gate, state, input_baseline_sha, updated_at FROM workitem_stages;

DROP TABLE workitem_stages;
ALTER TABLE workitem_stages_v32 RENAME TO workitem_stages;

-- 3) 模板域五表。
CREATE TABLE workflow_templates (
    id TEXT PRIMARY KEY,
    key TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE workflow_template_versions (
    id TEXT PRIMARY KEY,
    template_id TEXT NOT NULL REFERENCES workflow_templates(id),
    version_no INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft','active','deprecated')),
    content_digest TEXT NOT NULL,
    created_by TEXT NOT NULL DEFAULT 'local',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (template_id, version_no)
);

-- 单 active：一个模板至多一个激活版本（partial unique index）。
CREATE UNIQUE INDEX idx_wfv_single_active
    ON workflow_template_versions(template_id) WHERE status = 'active';

CREATE TABLE workflow_gate_definitions (
    id TEXT PRIMARY KEY,
    version_id TEXT NOT NULL REFERENCES workflow_template_versions(id),
    gate_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    title TEXT NOT NULL,
    purpose TEXT NOT NULL DEFAULT '',
    deliverables_json TEXT NOT NULL DEFAULT '[]',
    context_policy_ref TEXT,
    team_policy_ref TEXT,
    workspace_policy_ref TEXT,
    created_at TEXT NOT NULL,
    UNIQUE (version_id, gate_id),
    UNIQUE (version_id, ordinal)
);

CREATE TABLE workflow_instances (
    id TEXT PRIMARY KEY,
    workitem_id TEXT NOT NULL UNIQUE REFERENCES workitems(id),
    template_version_id TEXT NOT NULL REFERENCES workflow_template_versions(id),
    current_gate_id TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'active'
        CHECK (state IN ('active','migrated','archived')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE workflow_instance_gates (
    instance_id TEXT NOT NULL REFERENCES workflow_instances(id),
    gate_definition_id TEXT NOT NULL REFERENCES workflow_gate_definitions(id),
    state TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (instance_id, gate_definition_id)
);

CREATE INDEX idx_wi_gates_definition ON workflow_instance_gates(gate_definition_id);

-- 4) 内置 six-gate-default@1：激活版本，gate 定义与既有六关语义逐一对齐。
--    content_digest 常量 = sha256("v3|" + 六定义行按序拼接)，行格式见 sg-workflow template.rs
--    （v3 = WP-7：acceptance 槽为逐元素 canonical 形态；内置模板 acceptance 空、ref 空，
--      故行内容与 v2 一致、仅前缀升级；v2 = v1 基础上追加 acceptance 与三个 policy ref）。
INSERT INTO workflow_templates (id, key, name, created_at, updated_at)
VALUES ('wtpl_six_gate_default', 'six-gate-default', '默认六关',
        strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'));

INSERT INTO workflow_template_versions (id, template_id, version_no, status, content_digest, created_by, created_at, updated_at)
VALUES ('wfv_six_gate_default_1', 'wtpl_six_gate_default', 1, 'active',
        'f6cd29b6ec7dc47902911a0add31941b0d1ebd101f95ee904fdbda71a7f87d53',
        'migration', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'));

INSERT INTO workflow_gate_definitions
    (id, version_id, gate_id, ordinal, title, purpose, deliverables_json, created_at)
VALUES
    ('wgd_sgd1_requirements', 'wfv_six_gate_default_1', 'requirements', 1, '需求关',   '澄清需求并冻结 PRD 与需求项', '["prd"]',          strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    ('wgd_sgd1_design',       'wfv_six_gate_default_1', 'design',       2, '设计关',   '产出技术方案并冻结基线',       '["tech_design"]',  strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    ('wgd_sgd1_development',  'wfv_six_gate_default_1', 'development',  3, '开发关',   '实现代码并完成自测',           '["code"]',         strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    ('wgd_sgd1_testing',      'wfv_six_gate_default_1', 'testing',      4, '测试关',   '执行端到端与质量验证',         '["test"]',         strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    ('wgd_sgd1_deployment',   'wfv_six_gate_default_1', 'deployment',   5, '部署关',   '准备发布并验证环境',           '["deployment"]',   strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    ('wgd_sgd1_verification', 'wfv_six_gate_default_1', 'verification', 6, '验证关',   '完成验收与交付确认',           '["verification"]', strftime('%Y-%m-%dT%H:%M:%fZ','now'));

-- 5) 存量 WorkItem backfill：全部冻结为默认六关 v1（EV-001），投影各关当前状态。
INSERT INTO workflow_instances (id, workitem_id, template_version_id, current_gate_id, state, created_at, updated_at)
SELECT 'winst_' || w.id, w.id, 'wfv_six_gate_default_1', w.current_gate, 'active',
       strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')
FROM workitems w;

INSERT INTO workflow_instance_gates (instance_id, gate_definition_id, state, updated_at)
SELECT 'winst_' || s.workitem_id, gd.id, s.state, s.updated_at
FROM workitem_stages s
JOIN workflow_gate_definitions gd
  ON gd.version_id = 'wfv_six_gate_default_1' AND gd.gate_id = s.gate;
