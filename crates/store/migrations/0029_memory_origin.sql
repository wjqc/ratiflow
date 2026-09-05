-- 0029_memory_origin：记忆随仓库走（团队共享，2026-09-05 决策——推翻 ADR-032 §16「记忆不入库」裁定，仅此一项）。
-- origin='repo'：条目权威文件在 <repo>/memory/<slug>.md，SQLite 为可重建投影；
-- origin='local'：proposed 草稿与未接受候选，不落仓库，确认后升级 repo。
-- additive 只增；存量行全部视为 repo（首次 sync 时无文件的行按 absent 删除前，先由本地写路径补写文件）。
ALTER TABLE memory_entries ADD COLUMN origin TEXT NOT NULL DEFAULT 'repo' CHECK (origin IN ('repo','local'));

CREATE INDEX idx_memory_entries_origin ON memory_entries(project_id, origin, status);
