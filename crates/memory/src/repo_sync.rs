//! 记忆随仓库走（团队共享，2026-09-05 决策——推翻 ADR-032 §16「记忆不入库」，仅此一项）：
//! `<repo>/memory/<slug>.md`（frontmatter 元数据 + 正文全文）为权威文件，SQLite 为可重建投影。
//! 写路径：mutation 成功后由 core 调 [`persist_entry`] 落盘 / [`remove_entry`] 删文件；
//! 读路径：[`sync_from_repo`] 以文件为 desired 集合做 diff（新建/换版/删 absent）。
//! 修订历史只在本地（文件只存当前版）；proposed 草稿 origin='local' 不落仓库，确认后升级 repo。

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use sg_store::{ids, objects, timefmt, Store};

use crate::model::{normalize_tags, sha256_hex};
use crate::mutation::sync_fts;
use crate::repository;

/// 记忆目录（仓库根下）。
pub const MEMORY_DIR: &str = "memory";

/// 项目本地仓库根（projects.local_root；canonicalize 失败/未登记返回 None）。
fn project_root(store: &Store, project_id: &str) -> Option<PathBuf> {
    let raw: String = store
        .with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT COALESCE(local_root,'') FROM projects WHERE id=?1",
                    [project_id],
                    |r| r.get(0),
                )
                .unwrap_or_default())
        })
        .unwrap_or_default();
    if raw.is_empty() {
        return None;
    }
    PathBuf::from(&raw).canonicalize().ok()
}

/// frontmatter 序列化（字段序固定——文件 diff 友好）。
fn frontmatter(
    slug: &str,
    kind: &str,
    status: &str,
    subject_key: &str,
    tags: &[String],
    content_sha256: &str,
    revision_no: i64,
    updated_at: &str,
    confirmed_by: &str,
) -> String {
    let mut out = String::from("---\n");
    out.push_str(&format!("slug: {slug}\n"));
    out.push_str(&format!("kind: {kind}\n"));
    out.push_str(&format!("status: {status}\n"));
    out.push_str(&format!("subjectKey: {subject_key}\n"));
    if !tags.is_empty() {
        out.push_str("tags:\n");
        for t in tags {
            out.push_str(&format!("  - {t}\n"));
        }
    }
    out.push_str(&format!("contentSha256: {content_sha256}\n"));
    out.push_str(&format!("revisionNo: {revision_no}\n"));
    out.push_str(&format!("updatedAt: {updated_at}\n"));
    if !confirmed_by.is_empty() {
        out.push_str(&format!("confirmedBy: {confirmed_by}\n"));
    }
    out.push_str("---\n");
    out
}

/// frontmatter + 正文 解析（宽松：缺字段回退默认；正文 CRLF→LF）。
struct MemoryFile {
    slug: String,
    kind: String,
    status: String,
    subject_key: String,
    tags: Vec<String>,
    revision_no: i64,
    body: String,
}

fn parse_memory_file(raw: &str) -> Option<MemoryFile> {
    let rest = raw.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    let (fm, body) = rest.split_at(end + 1);
    let body = body.trim_start_matches("\n---\n").replace("\r\n", "\n");
    let mut slug = String::new();
    let mut kind = String::from("fact");
    let mut status = String::from("active");
    let mut subject_key = String::new();
    let mut tags: Vec<String> = Vec::new();
    let mut revision_no = 1_i64;
    let mut in_tags = false;
    for line in fm.lines() {
        if let Some(t) = line.trim().strip_prefix("- ") {
            if in_tags {
                tags.push(t.trim().to_string());
                continue;
            }
        }
        in_tags = false;
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "slug" => slug = v.to_string(),
            "kind" => kind = v.to_string(),
            "status" => status = v.to_string(),
            "subjectKey" => subject_key = v.to_string(),
            "revisionNo" => revision_no = v.parse().unwrap_or(1),
            "tags" => in_tags = true,
            _ => {}
        }
    }
    if slug.is_empty() || body.trim().is_empty() {
        return None;
    }
    if subject_key.is_empty() {
        subject_key = crate::model::subject_key(&slug);
    }
    Some(MemoryFile {
        slug,
        kind,
        status,
        subject_key,
        tags,
        revision_no,
        body,
    })
}

/// 原子写（tmp+fsync+rename；knowledge manifest 同模式）。
fn atomic_write(path: &Path, content: &str) -> Result<(), sg_store::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| sg_store::Error::Message(format!("memory dir: {e}")))?;
    }
    let tmp = path.with_extension("md.tmp");
    let mut f = std::fs::File::create(&tmp)
        .map_err(|e| sg_store::Error::Message(format!("memory tmp: {e}")))?;
    f.write_all(content.as_bytes())
        .map_err(|e| sg_store::Error::Message(format!("memory write: {e}")))?;
    f.sync_all().map_err(|e| sg_store::Error::Message(format!("fsync: {e}")))?;
    std::fs::rename(&tmp, path).map_err(|e| sg_store::Error::Message(format!("memory rename: {e}")))?;
    Ok(())
}

/// 当前条目快照（文件内容来源）。
struct Snapshot {
    slug: String,
    kind: String,
    status: String,
    subject_key: String,
    origin: String,
    revision_no: i64,
    title: String,
    content_sha256: String,
    object_sha256: Option<String>,
    tags_json: String,
    confirmed_by: String,
    updated_at: String,
}

fn snapshot(store: &Store, project_id: &str, memory_id: &str) -> Result<Option<Snapshot>, sg_store::Error> {
    store
        .with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT e.slug, e.kind, e.status, e.subject_key, e.origin, r.revision_no, r.title,
                            r.content_sha256, r.object_sha256, r.tags_json, COALESCE(e.confirmed_by,''), e.updated_at
                     FROM memory_entries e JOIN memory_revisions r ON r.id = e.current_revision_id
                     WHERE e.project_id=?1 AND e.id=?2",
                    rusqlite::params![project_id, memory_id],
                    |row| {
                        Ok(Snapshot {
                            slug: row.get(0)?,
                            kind: row.get(1)?,
                            status: row.get(2)?,
                            subject_key: row.get(3)?,
                            origin: row.get(4)?,
                            revision_no: row.get(5)?,
                            title: row.get(6)?,
                            content_sha256: row.get(7)?,
                            object_sha256: row.get(8)?,
                            tags_json: row.get(9)?,
                            confirmed_by: row.get(10)?,
                            updated_at: row.get(11)?,
                        })
                    },
                )
                .ok())
        })
}

/// mutation 成功后落盘（core 编排调用）：origin='repo' 且非 purged/rejected 的条目写文件；
/// proposed/local 与无 local_root 的项目跳过（返回 skipped 原因，不报错——诚实降级）。
pub fn persist_entry(store: &Store, project_id: &str, memory_id: &str) -> Value {
    let Some(root) = project_root(store, project_id) else {
        return json!({"persisted": false, "reason": "project_root_missing"});
    };
    let snap = match snapshot(store, project_id, memory_id) {
        Ok(Some(v)) => v,
        _ => return json!({"persisted": false, "reason": "entry_missing"}),
    };
    if snap.origin != "repo" || matches!(snap.status.as_str(), "purged" | "rejected") {
        return json!({"persisted": false, "reason": format!("origin={}/status={}", snap.origin, snap.status)});
    }
    let body = match snap.object_sha256.as_deref().map(|sha| repository::read_body(store, sha)) {
        Some(Ok(body)) => body,
        _ => return json!({"persisted": false, "reason": "body_object_missing"}),
    };
    let tags: Vec<String> = serde_json::from_str(&snap.tags_json).unwrap_or_default();
    let content = format!(
        "{}\n# {}\n\n{}\n",
        frontmatter(
            &snap.slug,
            &snap.kind,
            &snap.status,
            &snap.subject_key,
            &tags,
            &snap.content_sha256,
            snap.revision_no,
            &snap.updated_at,
            &snap.confirmed_by,
        ),
        snap.title,
        body.trim_end()
    );
    let path = root.join(MEMORY_DIR).join(format!("{}.md", snap.slug));
    if let Err(e) = atomic_write(&path, &content) {
        return json!({"persisted": false, "reason": e.to_string()});
    }
    json!({"persisted": true, "path": path.to_string_lossy()})
}

/// purge/删除后移除仓库文件（幂等：不存在即成功）。
pub fn remove_entry(store: &Store, project_id: &str, slug: &str) -> Value {
    let Some(root) = project_root(store, project_id) else {
        return json!({"removed": false, "reason": "project_root_missing"});
    };
    let path = root.join(MEMORY_DIR).join(format!("{slug}.md"));
    match std::fs::remove_file(&path) {
        Ok(()) => json!({"removed": true}),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({"removed": true}),
        Err(e) => json!({"removed": false, "reason": e.to_string()}),
    }
}

/// 从仓库同步：`memory/*.md` 为 desired——新建缺失、contentSha 变则新修订、absent 删本地 repo 行。
/// local 行不受影响；统计如实返回。单文件解析失败跳过并计数（不阻断整体）。
pub fn sync_from_repo(store: &Store, project_id: &str) -> Result<Value, sg_store::Error> {
    let Some(root) = project_root(store, project_id) else {
        return Ok(json!({"synced": false, "reason": "project_root_missing", "created": 0, "updated": 0, "removed": 0, "skipped": 0}));
    };
    let dir = root.join(MEMORY_DIR);
    let mut files: Vec<(String, MemoryFile)> = Vec::new();
    let mut skipped = 0usize;
    if dir.is_dir() {
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .map_err(|e| sg_store::Error::Message(format!("memory dir read: {e}")))?
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(".md"))
            .collect();
        names.sort();
        for name in names {
            let raw = match std::fs::read_to_string(dir.join(&name)) {
                Ok(v) => v,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            match parse_memory_file(&raw) {
                Some(mf) => files.push((name, mf)),
                None => skipped += 1,
            }
        }
    }

    // 现有 repo 行：slug → (id, content_sha, status)。
    let existing: Vec<(String, String, String, String)> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT e.slug, e.id, r.content_sha256, e.status
             FROM memory_entries e JOIN memory_revisions r ON r.id = e.current_revision_id
             WHERE e.project_id=?1 AND e.origin='repo'",
        )?;
        let rows = stmt.query_map([project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;
    let mut by_slug: std::collections::HashMap<String, (String, String, String)> = existing
        .into_iter()
        .map(|(slug, id, sha, status)| (slug, (id, sha, status)))
        .collect();

    let mut created = 0usize;
    let mut updated = 0usize;
    for (_, mf) in &files {
        let content_sha = sha256_hex(mf.body.as_bytes());
        // 秘密扫描 fail-closed：仓库文件进入本地索引前同样过闸（防绕过 UI 的提交）。
        if sg_store::scan::has_high_risk(&sg_store::scan::scan(mf.body.as_bytes())) {
            skipped += 1;
            continue;
        }
        match by_slug.remove(&mf.slug) {
            None => {
                create_from_file(store, project_id, mf, &content_sha)?;
                created += 1;
            }
            Some((memory_id, local_sha, local_status)) => {
                if local_sha != content_sha {
                    add_revision_from_file(store, project_id, &memory_id, mf, &content_sha)?;
                    updated += 1;
                } else if local_status != mf.status {
                    store.with_conn(|conn| {
                        conn.execute(
                            "UPDATE memory_entries SET status=?2, updated_at=?3 WHERE id=?1",
                            rusqlite::params![memory_id, mf.status, timefmt::now()],
                        )?;
                        Ok(())
                    })?;
                    updated += 1;
                }
            }
        }
    }
    // absent 文件 → 删除本地 repo 行（含修订与 FTS；对象留待 GC）。
    let mut removed = 0usize;
    for (slug, (memory_id, _, _)) in by_slug {
        store.with_tx_immediate(|tx| {
            tx.execute("DELETE FROM memory_fts WHERE memory_id=?1", [&memory_id])?;
            tx.execute(
                "DELETE FROM memory_source_refs WHERE revision_id IN (SELECT id FROM memory_revisions WHERE memory_id=?1)",
                [&memory_id],
            )?;
            // entries.current_revision_id → revisions 环形引用：先解除指针再删子删父。
            tx.execute(
                "UPDATE memory_entries SET current_revision_id=NULL WHERE id=?1",
                [&memory_id],
            )?;
            tx.execute("DELETE FROM memory_revisions WHERE memory_id=?1", [&memory_id])?;
            tx.execute(
                "DELETE FROM memory_entries WHERE id=?1 AND project_id=?2 AND origin='repo'",
                rusqlite::params![memory_id, project_id],
            )?;
            let _ = slug;
            Ok(())
        })?;
        removed += 1;
    }
    Ok(json!({
        "synced": true,
        "created": created,
        "updated": updated,
        "removed": removed,
        "skipped": skipped,
    }))
}

/// 文件 → 新 entry（直接 SQL；系统 actor='repo-sync'，审计一条）。
fn create_from_file(
    store: &Store,
    project_id: &str,
    mf: &MemoryFile,
    content_sha: &str,
) -> Result<(), sg_store::Error> {
    let settings = repository::settings_get(store, project_id)?;
    let mut reader = mf.body.as_bytes();
    let info = objects::put(
        store,
        &mut reader,
        objects::PutOptions {
            max_bytes: settings.max_bytes,
            allow_secrets: false,
        },
    )?;
    let memory_id = ids::new_id("mem");
    let revision_id = ids::new_id("memr");
    let now = timefmt::now();
    let tags = normalize_tags(&mf.tags);
    let (confirmed_at, confirmed_by) = if mf.status == "active" {
        (Some(now.clone()), "repo-sync".to_string())
    } else {
        (None, String::new())
    };
    store.with_tx_immediate(|tx| {
        // slug 可能与 local 行撞名（撞名后缀机制只保写路径）；撞名时给同步条目加后缀。
        let mut slug = mf.slug.clone();
        let mut n = 1;
        loop {
            let taken: i64 = tx.query_row(
                "SELECT COUNT(*) FROM memory_entries WHERE project_id=?1 AND slug=?2",
                rusqlite::params![project_id, slug],
                |r| r.get(0),
            )?;
            if taken == 0 {
                break;
            }
            n += 1;
            slug = format!("{}-{n}", mf.slug);
        }
        tx.execute(
            "INSERT INTO memory_entries(id, project_id, slug, kind, subject_key, status, pinned, confirmed_at, confirmed_by, created_at, updated_at, origin)
             VALUES (?1,?2,?3,?4,?5,?6,0,?7,?8,?9,?9,'repo')",
            rusqlite::params![
                memory_id,
                project_id,
                slug,
                mf.kind,
                mf.subject_key,
                mf.status,
                confirmed_at,
                confirmed_by,
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO memory_revisions(id, memory_id, revision_no, title, summary, object_sha256, content_sha256, tags_json, source_type, author_kind, author_id, idempotency_key, created_at)
             VALUES (?1,?2,?3,?4,'',?5,?6,?7,'import','system','repo-sync',?8,?9)",
            rusqlite::params![
                revision_id,
                memory_id,
                mf.revision_no.max(1),
                mf.slug.replace(['-', '_'], " "),
                info.sha256,
                content_sha,
                serde_json::to_string(&tags).unwrap_or_else(|_| "[]".into()),
                format!("repo-sync-{revision_id}"),
                now
            ],
        )?;
        tx.execute(
            "UPDATE memory_entries SET current_revision_id=?2 WHERE id=?1",
            rusqlite::params![memory_id, revision_id],
        )?;
        sync_fts(tx, &memory_id, project_id, &revision_id, &mf.slug, &tags, &mf.body)?;
        let _ = sg_store::audit::append_at(
            tx,
            "repo-sync",
            "memory.repo_created",
            "memory",
            &memory_id,
            serde_json::json!({"projectId": project_id, "slug": slug, "status": mf.status}),
        );
        Ok(())
    })
}

/// 文件内容变化 → 新修订（仓库为准；本地旧内容留在修订链历史）。
fn add_revision_from_file(
    store: &Store,
    project_id: &str,
    memory_id: &str,
    mf: &MemoryFile,
    content_sha: &str,
) -> Result<(), sg_store::Error> {
    let settings = repository::settings_get(store, project_id)?;
    let mut reader = mf.body.as_bytes();
    let info = objects::put(
        store,
        &mut reader,
        objects::PutOptions {
            max_bytes: settings.max_bytes,
            allow_secrets: false,
        },
    )?;
    let revision_id = ids::new_id("memr");
    let now = timefmt::now();
    let tags = normalize_tags(&mf.tags);
    store.with_tx_immediate(|tx| {
        let next_no: i64 = tx.query_row(
            "SELECT COALESCE(MAX(revision_no),0)+1 FROM memory_revisions WHERE memory_id=?1",
            [memory_id],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO memory_revisions(id, memory_id, revision_no, title, summary, object_sha256, content_sha256, tags_json, source_type, author_kind, author_id, idempotency_key, created_at)
             VALUES (?1,?2,?3,?4,'',?5,?6,?7,'import','system','repo-sync',?8,?9)",
            rusqlite::params![
                revision_id,
                memory_id,
                next_no,
                mf.slug.replace(['-', '_'], " "),
                info.sha256,
                content_sha,
                serde_json::to_string(&tags).unwrap_or_else(|_| "[]".into()),
                format!("repo-sync-{revision_id}"),
                now
            ],
        )?;
        tx.execute(
            "UPDATE memory_entries SET current_revision_id=?2, status=?3, updated_at=?4 WHERE id=?1",
            rusqlite::params![memory_id, revision_id, mf.status, now],
        )?;
        sync_fts(tx, memory_id, project_id, &revision_id, &mf.slug, &tags, &mf.body)?;
        let _ = sg_store::audit::append_at(
            tx,
            "repo-sync",
            "memory.repo_updated",
            "memory",
            memory_id,
            serde_json::json!({"projectId": project_id, "revisionNo": next_no}),
        );
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mutation;
use crate::CreateInput;
    use sg_store::ids;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn setup(name: &str) -> (Store, PathBuf, Tmp) {
        let root = std::env::temp_dir().join(format!("sg-memrepo-{}-{}", name, ids::new_id("t")));
        std::fs::create_dir_all(&root).unwrap();
        let t = Tmp(root.clone());
        let store = Store::open(&root.join("data"), "test").unwrap();
        // git init（committed 语义不需要，本地目录即可）+ 项目登记 local_root。
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, local_root, created_at)
                     VALUES ('pj','u','n','p','main',?1,?2)",
                    rusqlite::params![root.join("repo").to_string_lossy(), timefmt::now()],
                )?;
                Ok(())
            })
            .unwrap();
        std::fs::create_dir_all(root.join("repo").join(MEMORY_DIR)).unwrap();
        (store, root.join("repo"), t)
    }

    fn create_active(store: &Store, title: &str, body: &str) -> Value {
        mutation::create(
            store,
            &CreateInput {
                project_id: "pj".into(),
                title: title.into(),
                body: body.into(),
                kind: "decision".into(),
                tags: vec![],
                summary: None,
                target_status: "active",
                source_refs: vec![],
                on_duplicate: crate::DuplicateMode::Skip,
                actor: "tester".into(),
                idempotency_key: ids::new_id("k"),
            },
        )
        .unwrap()
    }

    /// 写路径：create 后 persist 落文件；文件内容含 frontmatter 与正文，可被解析回路。
    #[test]
    fn persist_writes_parseable_file() {
        let (store, repo, _t) = setup("persist");
        let created = create_active(&store, "部署健康检查", "部署前检查端点。");
        let slug = created["slug"].as_str().unwrap();
        let mid = created["memoryId"].as_str().unwrap();

        let out = persist_entry(&store, "pj", mid);
        assert_eq!(out["persisted"], json!(true), "{out}");
        let raw = std::fs::read_to_string(repo.join(MEMORY_DIR).join(format!("{slug}.md"))).unwrap();
        assert!(raw.contains("slug: "));
        assert!(raw.contains("status: active"));
        assert!(raw.contains("部署前检查端点。"));
        // 去掉 frontmatter 的正文可解析回来。
        let body = raw.split("\n---\n").nth(1).unwrap();
        assert!(body.contains("部署前检查端点。"));
    }

    /// 同步：手改文件 → sync 产生新修订；删文件 → 行删除；local 行不受影响。
    #[test]
    fn sync_updates_and_removes() {
        let (store, repo, _t) = setup("sync");
        let created = create_active(&store, "发布流程", "先跑流水线。");
        let mid = created["memoryId"].as_str().unwrap();
        let slug = created["slug"].as_str().unwrap();
        assert_eq!(persist_entry(&store, "pj", mid)["persisted"], json!(true));

        // 手改文件（模拟队友 git pull 后的新内容）。
        let path = repo.join(MEMORY_DIR).join(format!("{slug}.md"));
        let raw = std::fs::read_to_string(&path).unwrap();
        let edited = raw.replace("先跑流水线。", "先跑流水线，再验证灰度。");
        std::fs::write(&path, edited).unwrap();

        let out = sync_from_repo(&store, "pj").unwrap();
        assert_eq!(out["updated"], json!(1), "{out}");
        // 列表读回新正文。
        let hits = crate::repository::search(&store, "pj", "灰度", None, 10).unwrap();
        assert!(hits["items"].as_array().map(|a| !a.is_empty()).unwrap_or(false), "同步后 FTS 命中新内容: {hits}");

        // 删文件 → 同步删除本地行。
        std::fs::remove_file(&path).unwrap();
        let out = sync_from_repo(&store, "pj").unwrap();
        assert_eq!(out["removed"], json!(1), "{out}");
        let n: i64 = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM memory_entries WHERE project_id='pj'",
                    [],
                    |r| r.get(0),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(n, 0);
    }

    /// proposed 不落仓库；无 local_root 项目诚实降级。
    #[test]
    fn proposed_stays_local_and_missing_root_degrades() {
        let (store, _repo, _t) = setup("proposed");
        let p = mutation::create(
            &store,
            &CreateInput {
                project_id: "pj".into(),
                title: "草稿".into(),
                body: "待确认内容".into(),
                kind: "fact".into(),
                tags: vec![],
                summary: None,
                target_status: "proposed",
                source_refs: vec![],
                on_duplicate: crate::DuplicateMode::Skip,
                actor: "tester".into(),
                idempotency_key: ids::new_id("k"),
            },
        )
        .unwrap();
        let out = persist_entry(&store, "pj", p["memoryId"].as_str().unwrap());
        assert_eq!(out["persisted"], json!(false), "{out}");
        assert!(out["reason"].as_str().unwrap().starts_with("origin=local"));

        // 无 local_root 项目：同步返回 synced=false 不报错。
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj2','u','n2','p2','main',?1)",
                    [timefmt::now()],
                )?;
                Ok(())
            })
            .unwrap();
        let out = sync_from_repo(&store, "pj2").unwrap();
        assert_eq!(out["synced"], json!(false));
    }
}
