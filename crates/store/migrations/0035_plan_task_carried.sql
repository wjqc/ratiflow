-- 0035：plan_tasks 携带标记（EvoFlow 方案 M3-02 / ADR-037 §6.4 实现裁定）。
-- replan 携带/同键替换的任务 = 重执行已批准工作：孤立写检查豁免需跨
-- create/submit/start 三处重校验生效，故落列持久化（不只传参）。
-- 原 §7.2 中 0035（agent_teams/skills v2）顺延为 0036+，见状态区裁定记录。
ALTER TABLE plan_tasks ADD COLUMN carried_from_old INTEGER NOT NULL DEFAULT 0;
