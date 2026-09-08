-- 0053：rework recovery v2（审计《Ratiflow_RDWS_v1.4实施审计与整改方案_v1.0》§6 0053 / §7 P0-4；
-- 权威规格 v1.4 §WP-9 A1）。
-- 只追加事实，禁止 downgrade 删除。
--
-- 1) rework_operations 补恢复协议列（v1.4 §WP-9 权威 schema）：
--    - progress 游标（prepared → step_a_committed → step_b_committed）；
--    - pre_state_digest：发起时全关 state+指针+活跃 attempt 的 digest
--      （存量回填自 current_state_digest，两列此后同值维护）；
--    - post_step_a_digest：Step A 事务末同口径 digest，但**排除 target 关的
--      attempt 行**（Step B 会写 entry_snapshot_id 与 preparing→prepared——
--      部分 Step B 后恢复不误判）；
--    - step_b_object_sha256：Step B 对象写的内容摘要（重放校验）；
--    - recovery_attempts：恢复尝试计数。
--    存量：completed → step_b_committed（事实已终态）；其余 → prepared
--    （digest 可从 current_state_digest 重建，走恢复链重验）。
-- 2) baselines 纠偏：superseded_by 语义 = 后继 baseline id（artifact 域），
--    历史上被 rework 写入了 rework id（审计 §3 RDWS-015）。可确定归属的行
--    （superseded_by ∈ rework_operations.id）迁到新列 invalidated_by_rework_id
--    并清空原值；迁移前后 manifest 落 audit_log。无法确定归属的行不动
--    （'pending' 为 artifact 冻结中标记、其余为真实后继 id——非 rework 污染）。
-- 3) 读取面：latest_baseline 过滤 superseded_by IS NULL AND invalidated_by_rework_id
--    IS NULL（sg-artifact 单一收口点）。

ALTER TABLE rework_operations ADD COLUMN progress TEXT NOT NULL DEFAULT 'prepared'
    CHECK (progress IN ('prepared','step_a_committed','step_b_committed'));
ALTER TABLE rework_operations ADD COLUMN pre_state_digest TEXT NOT NULL DEFAULT '';
ALTER TABLE rework_operations ADD COLUMN post_step_a_digest TEXT NOT NULL DEFAULT '';
ALTER TABLE rework_operations ADD COLUMN step_b_object_sha256 TEXT NOT NULL DEFAULT '';
ALTER TABLE rework_operations ADD COLUMN recovery_attempts INTEGER NOT NULL DEFAULT 0;

UPDATE rework_operations SET pre_state_digest = current_state_digest;
UPDATE rework_operations SET progress = 'step_b_committed' WHERE state = 'completed';

CREATE INDEX idx_rework_recovery ON rework_operations(state, progress);

ALTER TABLE baselines ADD COLUMN invalidated_by_rework_id TEXT;

-- 迁移 manifest（迁移动作与受影响行数落审计；行清单以 detail JSON 记录）。
INSERT INTO audit_log(actor, action, target_type, target_id, detail, created_at)
SELECT 'system', 'rework_recovery_v2_manifest', 'migration', '0053',
       json_object('movedToInvalidated', COUNT(*), 'baselineIds',
                   json_group_array(id)),
       CURRENT_TIMESTAMP
FROM baselines WHERE superseded_by IN (SELECT id FROM rework_operations);

UPDATE baselines SET invalidated_by_rework_id = superseded_by, superseded_by = NULL
 WHERE superseded_by IN (SELECT id FROM rework_operations);

CREATE INDEX idx_baselines_invalidated ON baselines(workitem_id, gate)
    WHERE invalidated_by_rework_id IS NOT NULL;

-- 4) per-gate 活跃唯一索引重建：活跃 = 未后继 AND 未被 rework 失效
--    （与读取面双列过滤一致——失效行释放活跃位，重做链可重新冻结；
--    0017 先例的 partial index 形态）。
DROP INDEX IF EXISTS idx_baselines_active_per_gate;
CREATE UNIQUE INDEX idx_baselines_active_per_gate ON baselines(workitem_id, gate)
    WHERE superseded_by IS NULL AND invalidated_by_rework_id IS NULL;
