//! 多项目工作区（F01）：项目 CRUD、local_root 校验与摘要。
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};
use sg_store::{ids, outbox, timefmt, Error, Store};

#[derive(Debug, Clone, Serialize)]
pub struct Project {
    pub id: String,
    pub gitlab_instance: String,
    pub namespace: String,
    pub project: String,
    pub default_branch: String,
    pub name: String,
    pub local_root: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// local_root 安全校验：必须绝对路径、真实存在且不逃逸符号链接目标。
pub fn validate_local_root(local_root: &str) -> Result<PathBuf, Error> {
    let path = Path::new(local_root);
    if !path.is_absolute() {
        return Err(Error::Message(format!(
            "local_root 必须是绝对路径：{local_root}"
        )));
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| Error::Message(format!("local_root 不存在：{local_root}")))?;
    if !canonical.starts_with("/") {
        return Err(Error::Message("local_root 规范化失败".into()));
    }
    Ok(canonical)
}

/// 登记项目（幂等：同实例+ns+项目返回既有）。
pub fn register(
    store: &Store,
    gitlab_instance: &str,
    namespace: &str,
    project: &str,
    default_branch: &str,
    name: &str,
    local_root: &str,
) -> Result<Project, Error> {
    if gitlab_instance.is_empty() || namespace.is_empty() || project.is_empty() {
        return Err(Error::Message("instance/namespace/project required".into()));
    }
    let branch = if default_branch.is_empty() {
        "main"
    } else {
        default_branch
    };
    if !local_root.is_empty() {
        validate_local_root(local_root)?;
    }
    let existing = find_by_locator(store, gitlab_instance, namespace, project)?;
    if let Some(p) = existing {
        return Ok(p);
    }
    let id = ids::new_id("pj");
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, name, local_root, status, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,'not_ready',?8,?8)",
            rusqlite::params![id, gitlab_instance, namespace, project, branch, name, local_root, now],
        )?;
        Ok(())
    })?;
    outbox::emit(
        store,
        "project",
        &id,
        "project.created",
        json!({"namespace": namespace, "project": project}),
    )?;
    get(store, &id)
}

fn find_by_locator(
    store: &Store,
    instance: &str,
    namespace: &str,
    project: &str,
) -> Result<Option<Project>, Error> {
    // 注意不可在 with_conn 闭包内再调 get()（Mutex 不可重入，会死锁）。
    let id: Option<String> = store.with_conn(|conn| {
        conn.query_row(
            "SELECT id FROM projects WHERE gitlab_instance=?1 AND namespace=?2 AND project=?3",
            [instance, namespace, project],
            |r| r.get(0),
        )
        .optional()
        .map_err(Error::from)
    })?;
    match id {
        Some(id) => get(store, &id).map(Some),
        None => Ok(None),
    }
}

pub fn get(store: &Store, id: &str) -> Result<Project, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT id, gitlab_instance, namespace, project, default_branch, name, local_root, status, COALESCE(archived_at,''), created_at, COALESCE(updated_at,'')
             FROM projects WHERE id = ?1",
            [id],
            |r| {
                let archived: String = r.get(8)?;
                Ok(Project {
                    id: r.get(0)?,
                    gitlab_instance: r.get(1)?,
                    namespace: r.get(2)?,
                    project: r.get(3)?,
                    default_branch: r.get(4)?,
                    name: r.get(5)?,
                    local_root: r.get(6)?,
                    status: r.get(7)?,
                    archived_at: if archived.is_empty() { None } else { Some(archived) },
                    created_at: r.get(9)?,
                    updated_at: r.get(10)?,
                })
            },
        )
        .map_err(|_| Error::Message(format!("project {id} not found")))
    })
}

pub fn list(store: &Store, include_archived: bool) -> Result<Vec<Project>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, gitlab_instance, namespace, project, default_branch, name, local_root, status, COALESCE(archived_at,''), created_at, COALESCE(updated_at,'')
             FROM projects WHERE (?1 OR archived_at IS NULL) ORDER BY created_at",
        )?;
        let rows = stmt.query_map([include_archived], |r| {
            let archived: String = r.get(8)?;
            Ok(Project {
                id: r.get(0)?, gitlab_instance: r.get(1)?, namespace: r.get(2)?, project: r.get(3)?,
                default_branch: r.get(4)?, name: r.get(5)?, local_root: r.get(6)?, status: r.get(7)?,
                archived_at: if archived.is_empty() { None } else { Some(archived) },
                created_at: r.get(9)?, updated_at: r.get(10)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

pub fn update(
    store: &Store,
    id: &str,
    name: Option<&str>,
    local_root: Option<&str>,
    default_branch: Option<&str>,
    status: Option<&str>,
) -> Result<Project, Error> {
    if let Some(root) = local_root {
        if !root.is_empty() {
            validate_local_root(root)?;
        }
    }
    if let Some(s) = status {
        if !matches!(s, "not_ready" | "ready" | "error") {
            return Err(Error::Message(format!("invalid status {s}")));
        }
    }
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE projects SET
                name = COALESCE(?1, name),
                local_root = COALESCE(?2, local_root),
                default_branch = COALESCE(?3, default_branch),
                status = COALESCE(?4, status),
                updated_at = ?5
             WHERE id = ?6 AND archived_at IS NULL",
            rusqlite::params![name, local_root, default_branch, status, now, id],
        )?;
        Ok(())
    })?;
    get(store, id)
}

/// 可恢复归档。
pub fn archive(store: &Store, id: &str, archived: bool) -> Result<(), Error> {
    let now = timefmt::now();
    let changed = store.with_conn(|conn| {
        conn.execute(
            "UPDATE projects SET archived_at = CASE WHEN ?1 THEN ?2 ELSE NULL END, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![archived, now, id],
        )?;
        Ok(conn.changes())
    })?;
    if changed == 0 {
        return Err(Error::Message(format!("project {id} not found")));
    }
    Ok(())
}

/// F01 摘要：ready 状态、任务数、来源数、阻塞数。
pub fn summary(store: &Store, id: &str) -> Result<Value, Error> {
    let p = get(store, id)?;
    let workitems: i64 = store.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM workitems WHERE project_id=?1",
            [id],
            |r| r.get(0),
        )
        .map_err(Error::from)
    })?;
    let sources: i64 = store.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM knowledge_sources WHERE project_id=?1",
            [id],
            |r| r.get(0),
        )
        .map_err(Error::from)
    })?;
    let blocked: i64 = store.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM workitem_stages WHERE state IN ('blocked','awaiting_approval','failed','stale')
             AND workitem_id IN (SELECT id FROM workitems WHERE project_id=?1)",
            [id],
            |r| r.get(0),
        )
        .map_err(Error::from)
    })?;
    Ok(json!({
        "id": p.id,
        "name": p.name,
        "status": p.status,
        "workItemCount": workitems,
        "knowledgeSourceCount": sources,
        "blockedCount": blocked,
        "archived": p.archived_at.is_some(),
    }))
}

trait OptionalRow {
    fn optional(self) -> Result<Option<String>, rusqlite::Error>;
}

impl OptionalRow for Result<String, rusqlite::Error> {
    fn optional(self) -> Result<Option<String>, rusqlite::Error> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// 根目录检查（S10）：git、权限、技术栈、.sixgates、blockers。
pub fn inspect_root(path: &str) -> Result<serde_json::Value, Error> {
    let root = std::path::Path::new(path);
    if !root.is_absolute() {
        return Err(Error::Message("必须提供绝对路径".into()));
    }
    let canonical = root
        .canonicalize()
        .map_err(|e| Error::Message(format!("路径不可达：{e}")))?;

    let mut blockers = Vec::new();
    let is_git = canonical.join(".git").exists();
    if !is_git {
        blockers.push(serde_json::json!({"id": "not_a_git_repo", "severity": "warning", "detail": "未发现 .git；版本控制能力受限"}));
    }

    let readable = std::fs::read_dir(&canonical).is_ok();
    let writable = std::fs::write(canonical.join(".sixgates-probe"), b"1").is_ok();
    if writable {
        let _ = std::fs::remove_file(canonical.join(".sixgates-probe"));
    }
    if !readable {
        blockers.push(serde_json::json!({"id": "not_readable", "severity": "blocking"}));
    }
    if !writable {
        blockers.push(serde_json::json!({"id": "not_writable", "severity": "blocking", "detail": "需要写权限建立 worktree/缓存"}));
    }

    let markers = [
        ("go.mod", "go"),
        ("Cargo.toml", "rust"),
        ("package.json", "node"),
        ("pom.xml", "java-maven"),
        ("build.gradle", "java-gradle"),
        ("pyproject.toml", "python"),
        ("composer.json", "php"),
    ];
    let stacks: Vec<&str> = markers
        .iter()
        .filter(|(f, _)| canonical.join(f).exists())
        .map(|(_, s)| *s)
        .collect();

    Ok(serde_json::json!({
        "path": canonical.to_string_lossy(),
        "isGitRepo": is_git,
        "readable": readable,
        "writable": writable,
        "stacks": stacks,
        "hasSixgatesDir": canonical.join(".sixgates").exists(),
        "blockers": blockers,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-proj-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Store::open(&dir, "test").unwrap()
    }

    #[test]
    fn register_idempotent_and_list() {
        let s = store();
        let p1 = register(&s, "https://gitlab.test", "team", "demo", "", "演示", "").unwrap();
        let p2 = register(
            &s,
            "https://gitlab.test",
            "team",
            "demo",
            "main",
            "改名",
            "",
        )
        .unwrap();
        assert_eq!(p1.id, p2.id);
        assert_eq!(list(&s, false).unwrap().len(), 1);
    }

    #[test]
    fn local_root_validation() {
        assert!(validate_local_root("relative/path").is_err());
        assert!(validate_local_root("/definitely/not/exist/xyz").is_err());
        assert!(validate_local_root("/tmp").is_ok());
    }

    #[test]
    fn archive_and_summary() {
        let s = store();
        let p = register(&s, "u", "n", "p", "", "", "").unwrap();
        summary(&s, &p.id).unwrap();
        archive(&s, &p.id, true).unwrap();
        assert!(list(&s, false).unwrap().is_empty());
        assert_eq!(list(&s, true).unwrap().len(), 1);
        archive(&s, &p.id, false).unwrap();
        assert_eq!(list(&s, false).unwrap().len(), 1);
    }

    #[test]
    fn update_rejects_bad_status() {
        let s = store();
        let p = register(&s, "u", "n", "p", "", "", "").unwrap();
        assert!(update(&s, &p.id, None, None, None, Some("bogus")).is_err());
        update(&s, &p.id, Some("新名"), None, None, Some("ready")).unwrap();
        assert_eq!(get(&s, &p.id).unwrap().name, "新名");
    }
}
