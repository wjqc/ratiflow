-- 0030_skills_agent：技能绑定 Agent（agent_profiles.id）；NULL = 全局（所有 Run 注入）。
-- 绑定后仅该 Agent 的 Run 注入此技能；Agent 删除不级联（绑定失效按全局不注入处理）。
ALTER TABLE skills ADD COLUMN agent_profile_id TEXT REFERENCES agent_profiles(id);
