-- 0011_v3_projects: 多项目工作区（F01）
ALTER TABLE projects ADD COLUMN name TEXT NOT NULL DEFAULT '';
ALTER TABLE projects ADD COLUMN local_root TEXT NOT NULL DEFAULT '';
ALTER TABLE projects ADD COLUMN status TEXT NOT NULL DEFAULT 'not_ready'
    CHECK (status IN ('not_ready','ready','error'));
ALTER TABLE projects ADD COLUMN archived_at TEXT;
ALTER TABLE projects ADD COLUMN updated_at TEXT NOT NULL DEFAULT '';
