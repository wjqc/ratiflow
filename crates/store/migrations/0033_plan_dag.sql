-- 0033：Plan DAG（EvoFlow 方案 M2-01 / ADR-036 §6.2、ADR-037 §6.3/§6.4 / §7.3）。
-- 结构化计划是调度与验证权威（Markdown 只是投影）；无环由服务层 + 激活校验保证，
-- schema 不做环约束；attempt 幂等 = UNIQUE(task_id, attempt_no) + 单活跃 partial index。

-- 1) 计划版本：不可变；状态机
--    draft → awaiting_approval → approved → executing → completed
--    旁路 rejected/superseded/cancelled；executing → replan_required → superseded。
CREATE TABLE plan_revisions (
    id TEXT PRIMARY KEY,
    workitem_id TEXT NOT NULL REFERENCES workitems(id),
    stage_attempt_id TEXT NOT NULL REFERENCES stage_attempts(id),
    revision_no INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft','awaiting_approval','approved','executing','completed',
                          'rejected','superseded','cancelled','replan_required')),
    digest TEXT NOT NULL DEFAULT '',
    supersedes_id TEXT REFERENCES plan_revisions(id),
    approved_by TEXT,
    approved_at TEXT,
    markdown_digest TEXT,
    created_by TEXT NOT NULL DEFAULT 'local',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (stage_attempt_id, revision_no)
);

-- 2) 计划任务：定义无运行状态；task_key 为 revision 内稳定 ID。
CREATE TABLE plan_tasks (
    id TEXT PRIMARY KEY,
    plan_revision_id TEXT NOT NULL REFERENCES plan_revisions(id),
    task_key TEXT NOT NULL,
    kind TEXT NOT NULL
        CHECK (kind IN ('analysis','read','local_write','external_write','verification','merge')),
    title TEXT NOT NULL DEFAULT '',
    inputs_json TEXT NOT NULL DEFAULT '[]',
    expected_outputs_json TEXT NOT NULL DEFAULT '[]',
    acceptance_json TEXT NOT NULL DEFAULT '{}',
    effect_class TEXT NOT NULL
        CHECK (effect_class IN ('none','read','local_write','external_write','irreversible')),
    workspace_policy_version_id TEXT,
    team_role_key TEXT,
    max_attempts INTEGER NOT NULL DEFAULT 3,
    timeout_secs INTEGER NOT NULL DEFAULT 3600,
    budget_json TEXT NOT NULL DEFAULT '{}',
    reused_from_attempt_id TEXT,
    created_at TEXT NOT NULL,
    UNIQUE (plan_revision_id, task_key)
);

-- 3) 依赖边：只表达 depends_on。
CREATE TABLE plan_task_edges (
    id TEXT PRIMARY KEY,
    plan_revision_id TEXT NOT NULL REFERENCES plan_revisions(id),
    from_task_id TEXT NOT NULL REFERENCES plan_tasks(id),
    to_task_id TEXT NOT NULL REFERENCES plan_tasks(id),
    relation TEXT NOT NULL DEFAULT 'depends_on',
    created_at TEXT NOT NULL,
    UNIQUE (from_task_id, to_task_id, relation)
);

-- 4) 任务尝试：运行事实（§6.3 状态机）。
--    pending → ready → preparing_workspace → running
--    running → succeeded | failed | unknown | cancelled | awaiting_approval
--    failed/unknown → reconciliation_required → succeeded | failed | manual_action_required
CREATE TABLE plan_task_attempts (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES plan_tasks(id),
    attempt_no INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending','ready','preparing_workspace','running',
                         'succeeded','failed','unknown','cancelled',
                         'awaiting_approval','reconciliation_required',
                         'manual_action_required')),
    input_digest TEXT NOT NULL DEFAULT '',
    workspace_id TEXT,
    run_id TEXT,
    outcome_id TEXT,
    started_at TEXT,
    finished_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (task_id, attempt_no)
);

-- 单活跃：同一任务同时至多一个进行中的 attempt（幂等重放不产生并行执行）。
CREATE UNIQUE INDEX idx_pta_single_active
    ON plan_task_attempts(task_id)
    WHERE state IN ('pending','ready','preparing_workspace','running',
                    'awaiting_approval','reconciliation_required');

-- 5) 任务产物清单：成功必须生成 output manifest；写任务附 patch 工件与 diff 检查。
CREATE TABLE plan_task_outputs (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL UNIQUE REFERENCES plan_task_attempts(id),
    manifest_json TEXT NOT NULL,
    patch_object_sha256 TEXT,
    files_root_digest TEXT NOT NULL DEFAULT '',
    git_diff_check_clean INTEGER,
    created_at TEXT NOT NULL
);

-- 6) 重规划链：新 revision 取代旧 revision（不回写旧版本——事实只追加）。
CREATE TABLE replan_links (
    id TEXT PRIMARY KEY,
    from_revision_id TEXT NOT NULL REFERENCES plan_revisions(id),
    to_revision_id TEXT NOT NULL REFERENCES plan_revisions(id),
    root_task_keys_json TEXT NOT NULL DEFAULT '[]',
    reason TEXT NOT NULL DEFAULT '',
    created_by TEXT NOT NULL DEFAULT 'local',
    created_at TEXT NOT NULL,
    UNIQUE (from_revision_id, to_revision_id)
);

CREATE INDEX idx_plan_tasks_revision ON plan_tasks(plan_revision_id);
CREATE INDEX idx_plan_edges_to ON plan_task_edges(to_task_id);
CREATE INDEX idx_pta_task ON plan_task_attempts(task_id);
CREATE INDEX idx_pta_state ON plan_task_attempts(state);
