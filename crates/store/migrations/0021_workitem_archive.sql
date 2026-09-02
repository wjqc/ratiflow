-- 0021_workitem_archive: workitems 增加归档时间戳（可空）。
-- 归档语义（非删除）：archived_at 非空即从默认列表隐藏，置 NULL 恢复；
-- 阶段/文档/审批等子表不受影响，与 projects.archived_at 的移除/恢复模型一致。

ALTER TABLE workitems ADD COLUMN archived_at TEXT;
