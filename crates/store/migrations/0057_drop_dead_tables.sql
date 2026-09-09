-- 0057_drop_dead_tables: 清理 8 张死表（死代码审计 2026-09-08，全量扫描结论；
-- 扫描语料经括号配平式 #[cfg(test)] 剥离二次复核，误判的 skill_bindings_v2 已撤下
-- —— 它被 skill.bindVersion RPC 生产路径引用，是活表）。
-- 判定口径：建表后从未被任何生产代码读写（word-boundary 全仓扫描），且无任何
-- 活表外键指向它们（含列级 REFERENCES，已逐一核实）。
-- [纯死表 7 张] 早期设计被后续机制整体取代，产品从未写入：
--   sessions / idempotency_keys        (0002_auth)      本地应用无 auth；幂等被 rpc_receipts(0041) 取代
--   workflows / workflow_steps         (0006_workflow)  旧工作流引擎，被模板版本化(0032)+stage_attempts 取代
--   trace_links                        (0007_gate)      追溯链设计未实现
--   deployment_steps                   (0008_delivery)  被 stage_attempts/delivery 路径取代
--   plan_task_outputs                  (0033_plan_dag)  plan DAG 输出未接线
-- [未接线功能表 1 张] 仅迁移测试写入（store_test M2-05 断言同步清理）：
--   run_interrupts                     (0034)  自治中断只建表未接产品；无子表引用，可安全删
-- [保留 1 张：workspace_policy_versions (0034)]
--   活表 task_workspaces / agent_runs 的 workspace_policy_version_id 列 REFERENCES 本表，
--   SQLite FK ON 下父表缺失会让子表任何 INSERT（含 NULL 值）报 no-such-table——
--   agent_runs 是运行启动必写的核心热表。删除本表必须同时重建两张活表，
--   风险与死表清理的收益不成比例，故保留为外键锚点（生产恒 NULL 列，零维护成本）。
-- 不在此列：app_settings / knowledge_default_settings（偏好迁移 2026-09-08 后为
-- prefstore 种子源，冻结但保留）；skill_bindings_v2（活表）；各 _new/_vNN 重建
-- 脚手架（事务内换名，终态不存在）。
-- 破坏性操作纪律：依赖迁移前自动快照回退（runner 硬 Gate，§12.1/A48）；
-- 历史库中若有旧版本残留行，随快照保全，不再迁移。

DROP TABLE IF EXISTS workflow_steps;
DROP TABLE IF EXISTS workflows;
DROP TABLE IF EXISTS deployment_steps;
DROP TABLE IF EXISTS plan_task_outputs;
DROP TABLE IF EXISTS trace_links;
DROP TABLE IF EXISTS idempotency_keys;
DROP TABLE IF EXISTS sessions;
DROP TABLE IF EXISTS run_interrupts;
