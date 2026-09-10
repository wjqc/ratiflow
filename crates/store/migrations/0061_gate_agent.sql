-- 0061：关卡展示元数据收编入模板（前端 GATE_SUBS/GATE_REQUIREMENTS/GATE_AGENT
-- 三张六关硬编码映射删除，工作台/新建任务统一读模板配置）。
-- 1) workflow_gate_definitions 加 agent 列（本关默认执行 Agent 展示名）。
ALTER TABLE workflow_gate_definitions ADD COLUMN agent TEXT NOT NULL DEFAULT '';

-- 2) 内置 six-gate-default 升版 v2：补齐 purpose/acceptance/agent 展示文案
--    （与此前前端六关硬编码映射逐字一致），v1 → deprecated、v2 → active。
--    content_digest 常量 = sha256("v5|" + 六定义行按序拼接)，行格式见 sg-workflow template.rs
--    （v5 = 追加 agent 槽；v4 及以前的存量激活版本不重算）。
UPDATE workflow_template_versions
   SET status = 'deprecated', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
 WHERE id = 'wfv_six_gate_default_1';

INSERT INTO workflow_template_versions
    (id, template_id, version_no, status, content_digest, created_by, created_at, updated_at)
VALUES
    ('wfv_six_gate_default_2', 'wtpl_six_gate_default', 2, 'active',
     '40ce379996c8219d7166315422aa9e427b152be51e9ef75ebcc29f86108a702c',
     'migration', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'));

INSERT INTO workflow_gate_definitions
    (id, version_id, gate_id, ordinal, title, purpose, agent, deliverables_json, acceptance_json, created_at)
VALUES
    ('wgd_sgd2_requirements', 'wfv_six_gate_default_2', 'requirements', 1, '需求关', '需求澄清与 PRD', '需求分析 Agent', '["prd"]',
     '["完成需求澄清与确认","完成 PRD 并存储到知识库","通过评审并放行"]', strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    ('wgd_sgd2_design',       'wfv_six_gate_default_2', 'design',       2, '设计关', '产品与技术方案', '方案设计 Agent', '["tech_design"]',
     '["完成技术方案设计","方案评审通过并冻结基线","通过评审并放行"]', strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    ('wgd_sgd2_development',  'wfv_six_gate_default_2', 'development',  3, '开发关', '编码实现与自测', '开发实施 Agent', '["code"]',
     '["完成编码实现与自测","通过代码评审","通过评审并放行"]', strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    ('wgd_sgd2_testing',      'wfv_six_gate_default_2', 'testing',      4, '测试关', '集成测试与质量验证', '质量校验 Agent', '["test"]',
     '["完成集成测试","缺陷清零或达成豁免","通过评审并放行"]', strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    ('wgd_sgd2_deployment',   'wfv_six_gate_default_2', 'deployment',   5, '部署关', '发布与环境准备', '部署执行 Agent', '["deployment"]',
     '["完成发布与环境准备","部署到目标环境","通过评审并放行"]', strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    ('wgd_sgd2_verification', 'wfv_six_gate_default_2', 'verification', 6, '验证关', '验收与交付确认', '验收确认 Agent', '["verification"]',
     '["完成验收确认","交付物归档","通过验收并放行"]', strftime('%Y-%m-%dT%H:%M:%fZ','now'));

-- 既有 workflow_instances 仍冻结 v1（版本不可变语义）；新 WorkItem 冻结 v2。
-- 需要老实例换版的用 workflow.migrate（显式迁移，不自动改写历史）。
