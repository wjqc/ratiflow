-- 0059_run_defaults: 规范技能化接入——run 显式技能面 + WorkItem 任务级默认运行面。
-- （1）agent_run_skills：run 启动时显式选择的技能版本冻结证据（任务输入 "/" 快捷命令）。
--     注入优先级由服务层 enforced：run 显式 > workitem 默认 > 全局 enabled。
-- （2）workitems 任务级默认两列（创建/更新时冻结版本，EV-002 同款语义：
--     后续技能或 profile 升级不影响既有任务）：
--     default_profile_version_id（"@" 指派 Agent，选路四级中的 workitem_default 档）
--     default_skill_version_ids（JSON 数组，"/" 选择的默认技能面）。
-- 两列生产恒可为 NULL/空数组（未指派 → 走既有项目/全局绑定与全局技能面），零回填。

CREATE TABLE agent_run_skills (
    agent_run_id TEXT NOT NULL REFERENCES agent_runs(id),
    skill_version_id TEXT NOT NULL REFERENCES skill_versions(id),
    ordinal INTEGER NOT NULL,
    bytes INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    PRIMARY KEY (agent_run_id, skill_version_id)
);

CREATE INDEX idx_agent_run_skills_version ON agent_run_skills(skill_version_id);

ALTER TABLE workitems ADD COLUMN default_profile_version_id TEXT REFERENCES agent_profile_versions(id);
ALTER TABLE workitems ADD COLUMN default_skill_version_ids TEXT NOT NULL DEFAULT '[]';
