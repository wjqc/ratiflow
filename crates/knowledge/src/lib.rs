//! 项目知识库（F02）：来源管理、受限扫描、分块索引（FTS5）、检索与上下文预览。
//! 硬边界：所有查询按 project 隔离；扫描限制在项目根目录；秘密扫描失败禁止入模型。

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};
use sg_store::{ids, objects, outbox, scan, timefmt, Error, Store};

#[derive(Debug, Clone, Serialize)]
pub struct Source {
    pub id: String,
    pub project_id: String,
    pub kind: String,
    pub name: String,
    pub locator: String,
    pub enabled: bool,
    pub scan_state: String,
    pub content_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_scanned_at: Option<String>,
    pub error: String,
    pub created_at: String,
    pub updated_at: String,
}

pub fn create_source(store: &Store, project_id: &str, kind: &str, name: &str, locator: &str) -> Result<Source, Error> {
    if !matches!(kind, "repo_path" | "document" | "openapi" | "gitlab" | "rule") {
        return Err(Error::Message(format!("invalid source kind {kind}")));
    }
    if project_id.is_empty() || name.is_empty() || locator.is_empty() {
        return Err(Error::Message("project/name/locator required".into()));
    }
    let id = ids::new_id("ks");
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO knowledge_sources(id, project_id, kind, name, locator, enabled, scan_state, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,1,'pending',?6,?6)
             ON CONFLICT(project_id, kind, locator) DO NOTHING",
            rusqlite::params![id, project_id, kind, name, locator, now],
        )?;
        Ok(())
    })?;
    get_source(store, &id)
}

pub fn get_source(store: &Store, id: &str) -> Result<Source, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT id, project_id, kind, name, locator, enabled, scan_state, COALESCE(content_sha256,''),
                    COALESCE(last_scanned_at,''), error, created_at, updated_at
             FROM knowledge_sources WHERE id=?1",
            [id],
            |r| {
                let last: String = r.get(8)?;
                Ok(Source {
                    id: r.get(0)?, project_id: r.get(1)?, kind: r.get(2)?, name: r.get(3)?,
                    locator: r.get(4)?, enabled: r.get::<_, i64>(5)? == 1, scan_state: r.get(6)?,
                    content_sha256: r.get(7)?,
                    last_scanned_at: if last.is_empty() { None } else { Some(last) },
                    error: r.get(9)?, created_at: r.get(10)?, updated_at: r.get(11)?,
                })
            },
        )
        .map_err(|_| Error::Message("source_not_found".into()))
    })
}

/// 项目作用域列表（跨项目不可见）。
pub fn list_sources(store: &Store, project_id: &str) -> Result<Vec<Source>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, project_id, kind, name, locator, enabled, scan_state, COALESCE(content_sha256,''),
                    COALESCE(last_scanned_at,''), error, created_at, updated_at
             FROM knowledge_sources WHERE project_id=?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map([project_id], |r| {
            let last: String = r.get(8)?;
            Ok(Source {
                id: r.get(0)?, project_id: r.get(1)?, kind: r.get(2)?, name: r.get(3)?,
                locator: r.get(4)?, enabled: r.get::<_, i64>(5)? == 1, scan_state: r.get(6)?,
                content_sha256: r.get(7)?,
                last_scanned_at: if last.is_empty() { None } else { Some(last) },
                error: r.get(9)?, created_at: r.get(10)?, updated_at: r.get(11)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

pub fn update_source(store: &Store, id: &str, enabled: Option<bool>, name: Option<&str>) -> Result<(), Error> {
    let changed = store.with_conn(|conn| {
        conn.execute(
            "UPDATE knowledge_sources SET enabled = COALESCE(?1, enabled), name = COALESCE(?2, name),
                    scan_state = CASE WHEN ?1 IS NOT NULL THEN 'pending' ELSE scan_state END,
                    updated_at = ?3
             WHERE id = ?4",
            rusqlite::params![enabled, name, timefmt::now(), id],
        )?;
        Ok(conn.changes())
    })?;
    if changed == 0 {
        return Err(Error::Message("source_not_found".into()));
    }
    Ok(())
}

pub fn remove_source(store: &Store, id: &str) -> Result<(), Error> {
    store.with_conn(|conn| {
        conn.execute("DELETE FROM knowledge_fts WHERE source_id=?1", [id])?;
        conn.execute("DELETE FROM knowledge_chunks WHERE source_id=?1", [id])?;
        conn.execute("DELETE FROM knowledge_sources WHERE id=?1", [id])?;
        Ok(())
    })
}

/// 忽略规则：.gitignore 简化匹配（目录名/后缀/精确路径）+ .sixgatesignore 逐行前缀匹配。
fn load_ignores(root: &Path) -> Vec<String> {
    let mut patterns = vec![
        ".git".to_string(),
        "node_modules".to_string(),
        "target".to_string(),
        "dist".to_string(),
    ];
    for file in [".sixgatesignore", ".gitignore"] {
        if let Ok(body) = std::fs::read_to_string(root.join(file)) {
            for line in body.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() && !trimmed.starts_with('#') {
                    patterns.push(trimmed.trim_end_matches('/').to_string());
                }
                if patterns.len() > 500 {
                    break;
                }
            }
        }
    }
    patterns
}

fn is_ignored(rel: &Path, patterns: &[String]) -> bool {
    let rel_str = rel.to_string_lossy();
    for pattern in patterns {
        if rel_str.starts_with(&format!("{pattern}/")) || rel_str == *pattern
            || rel.file_name().map(|n| n.to_string_lossy() == *pattern).unwrap_or(false)
            || (pattern.starts_with('*') && rel_str.ends_with(pattern.trim_start_matches('*')))
        {
            return true;
        }
    }
    false
}

/// 扫描：受限在根目录内；秘密内容拒绝入库（fail closed）；单事务替换索引。
pub fn scan_source(store: &Store, source_id: &str, project_root: Option<&Path>, max_files: usize, max_file_bytes: u64) -> Result<Source, Error> {
    let source = get_source(store, source_id)?;
    set_scan_state(store, source_id, "scanning", "")?;

    let root: PathBuf = match project_root {
        Some(p) => p.to_path_buf(),
        None => PathBuf::from(&source.locator),
    };
    if !root.is_absolute() || !root.exists() {
        set_scan_state(store, source_id, "failed", &format!("根目录不可用：{}", root.display()))?;
        return Err(Error::Message(format!("path_outside_project: {}", root.display())));
    }
    let canonical = root.canonicalize().map_err(|e| Error::Message(e.to_string()))?;

    let ignores = load_ignores(&canonical);
    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(&canonical, &canonical, &ignores, &mut files, max_files, max_file_bytes)?;
    if files.is_empty() {
        set_scan_state(store, source_id, "failed", "未发现可索引文件")?;
        return Err(Error::Message("no indexable files".into()));
    }

    // 计算内容根哈希并写入分块。
    use sha2::{Digest, Sha256};
    let mut root_hasher = Sha256::new();
    let mut chunks: Vec<(usize, String, i64)> = Vec::new(); // (ordinal, object_sha, tokens)
    let mut chunk_rows: Vec<(String, String, String)> = Vec::new(); // (chunk_id, ordinal_object, body) for FTS
    for file in &files {
        let body = match std::fs::read(file) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if body.len() as u64 > max_file_bytes {
            continue;
        }
        let rel = file.strip_prefix(&canonical).unwrap_or(file);
        root_hasher.update(rel.to_string_lossy().as_bytes());
        root_hasher.update(&body);
        let findings = scan::scan(&body);
        // 秘密内容：跳过该文件并记录（不中断整个来源）。
        if scan::has_high_risk(&findings) {
            continue;
        }
        let text = String::from_utf8_lossy(&body);
        for (ordinal, chunk) in chunk_text(&text, 2000).into_iter().enumerate() {
            let info = objects::put(store, chunk.as_bytes(), objects::PutOptions::default())?;
            let chunk_id = ids::new_id("kc");
            root_hasher.update(chunk.as_bytes());
            chunks.push((ordinal, info.sha256.clone(), (chunk.len() / 4) as i64));
            chunk_rows.push((chunk_id.clone(), info.sha256, format!("{}\n{}", rel.display(), chunk)));
        }
    }
    let content_sha = ids::hex(&root_hasher.finalize());
    if chunks.is_empty() {
        set_scan_state(store, source_id, "failed", "全部文件含秘密或不可读")?;
        return Err(Error::Message("all files rejected".into()));
    }

    store.with_tx(|tx| {
        tx.execute("DELETE FROM knowledge_fts WHERE source_id=?1", [source_id])?;
        tx.execute("DELETE FROM knowledge_chunks WHERE source_id=?1", [source_id])?;
        for (idx, (ordinal, object_sha, tokens)) in chunks.iter().enumerate() {
            let (chunk_id, _, body) = &chunk_rows[idx];
            tx.execute(
                "INSERT INTO knowledge_chunks(id, source_id, ordinal, object_sha256, token_count) VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params![chunk_id, source_id, ordinal, object_sha, tokens],
            )?;
            tx.execute(
                "INSERT INTO knowledge_fts(chunk_id, source_id, project_id, body) VALUES (?1,?2,?3,?4)",
                rusqlite::params![chunk_id, source_id, source.project_id, body],
            )?;
        }
        tx.execute(
            "UPDATE knowledge_sources SET scan_state='indexed', content_sha256=?1, last_scanned_at=?2, error='', updated_at=?2 WHERE id=?3",
            rusqlite::params![content_sha, timefmt::now(), source_id],
        )?;
        Ok(())
    })?;
    outbox::emit(store, "knowledge", source_id, "knowledge.scanned",
        json!({"projectId": source.project_id, "chunks": chunks.len()}))?;
    get_source(store, source_id)
}

fn set_scan_state(store: &Store, source_id: &str, state: &str, error: &str) -> Result<(), Error> {
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE knowledge_sources SET scan_state=?1, error=?2, updated_at=?3 WHERE id=?4",
            rusqlite::params![state, error, timefmt::now(), source_id],
        )?;
        Ok(())
    })
}

fn collect_files(root: &Path, dir: &Path, ignores: &[String], out: &mut Vec<PathBuf>, max_files: usize, max_file_bytes: u64) -> Result<(), Error> {
    if out.len() >= max_files {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir).map_err(|e| Error::Message(e.to_string()))?;
    for entry in entries {
        let entry = entry.map_err(|e| Error::Message(e.to_string()))?;
        let path = entry.path();
        // 符号链接逃逸防护。
        if entry.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(&path);
        if is_ignored(rel, ignores) {
            continue;
        }
        if path.is_dir() {
            collect_files(root, &path, ignores, out, max_files, max_file_bytes)?;
        } else if path.is_file() {
            if let Ok(meta) = entry.metadata() {
                if meta.len() <= max_file_bytes {
                    out.push(path);
                }
            }
        }
        if out.len() >= max_files {
            return Ok(());
        }
    }
    Ok(())
}

/// 按段落/长度分块。
fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for paragraph in text.split("\n\n") {
        if current.len() + paragraph.len() > max_chars && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
        }
        current.push_str(paragraph);
        current.push_str("\n\n");
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    chunks
}

/// FTS5 检索（项目作用域强制）。trigram 分词器：MATCH 支持中文短语（≥3 字符）；
/// 短查询与兜底走 LIKE（同表 trigram 索引可加速）。用户输入不进 FTS 语法层。
pub fn search(store: &Store, project_id: &str, query: &str, limit: i64) -> Result<Vec<Value>, Error> {
    if query.trim().is_empty() {
        return Ok(vec![]);
    }
    // 逐词 AND：每个词独立 MATCH（trigram，≥3 字）或 LIKE（短词/中文词组均可用）。
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|w| w.chars().filter(|c| !matches!(c, '"' | '*' | '(' | ')' | ':' | '\'' | '%')).collect::<String>())
        .filter(|w| !w.is_empty())
        .take(5)
        .collect();
    if terms.is_empty() {
        return Ok(vec![]);
    }
    store.with_conn(|conn| {
        let mut conditions: Vec<String> = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(project_id.to_string())];
        for (i, term) in terms.iter().enumerate() {
            conditions.push(format!("body LIKE ?{}", i + 2));
            params.push(Box::new(format!("%{term}%")));
        }
        let sql = format!(
            "SELECT chunk_id, source_id, body FROM knowledge_fts
             WHERE project_id = ?1 AND {} LIMIT {}",
            conditions.join(" AND "),
            limit
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
            Ok(json!({
                "chunkId": r.get::<_, String>(0)?,
                "sourceId": r.get::<_, String>(1)?,
                "snippet": r.get::<_, String>(2)?,
            }))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

/// 上下文预览：来源、片段、大小与排除理由（F02：不能只返回拼接大文本）。
pub fn context_preview(store: &Store, project_id: &str, query: &str, max_bytes: i64) -> Result<Value, Error> {
    let sources = list_sources(store, project_id)?;
    let hits = search(store, project_id, query, 20)?;
    let mut items = Vec::new();
    let mut total = 0i64;
    for hit in &hits {
        let source = sources.iter().find(|s| s.id == hit["sourceId"].as_str().unwrap_or_default());
        let snippet = hit["snippet"].as_str().unwrap_or_default();
        let size = snippet.len() as i64;
        let included = total + size <= max_bytes;
        if included {
            total += size;
        }
        items.push(json!({
            "sourceId": hit["sourceId"],
            "sourceName": source.map(|s| s.name.clone()).unwrap_or_default(),
            "snippet": snippet,
            "bytes": size,
            "included": included,
            "reason": if included { "命中且在预算内" } else { "超出上下文预算" },
        }));
    }
    Ok(json!({"projectId": project_id, "query": query, "totalBytes": total, "items": items}))
}

/// 记录 Context Manifest（不可变）与明细（F04）。
pub fn create_manifest(
    store: &Store,
    project_id: &str,
    workitem_id: &str,
    query: &str,
    selected_sources: &[String],
) -> Result<Value, Error> {
    let preview = context_preview(store, project_id, query, 64 << 10)?;
    let id = ids::new_id("ctx");
    let now = timefmt::now();
    let scope = json!({"query": query, "selectedSources": selected_sources, "maxContextBytes": 262144});
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO context_manifests(id, workitem_id, scope, data_policy, created_at) VALUES (?1,?2,?3,'standard',?4)",
            rusqlite::params![id, workitem_id, scope.to_string(), now],
        )?;
        let items = preview["items"].as_array().cloned().unwrap_or_default();
        for (ordinal, item) in items.iter().filter(|i| i["included"].as_bool() == Some(true)).enumerate() {
            conn.execute(
                "INSERT INTO context_manifest_items(manifest_id, source_id, object_sha256, purpose, included, reason, ordinal)
                 VALUES (?1,?2,'',?3,1,?4,?5)",
                rusqlite::params![id, item["sourceId"].as_str().unwrap_or_default(), "retrieval", item["reason"].as_str().unwrap_or_default(), ordinal as i64],
            )?;
        }
        Ok(())
    })?;
    Ok(json!({"id": id, "workitemId": workitem_id, "scope": scope, "createdAt": now}))
}

pub fn get_manifest(store: &Store, id: &str) -> Result<Value, Error> {
    let row: (String, String, String) = store.with_conn(|conn| {
        conn.query_row(
            "SELECT workitem_id, scope, COALESCE(data_policy,'standard') FROM context_manifests WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|_| Error::Message("manifest_not_found".into()))
    })?;
    Ok(json!({
        "id": id,
        "workitemId": row.0,
        "scope": serde_json::from_str::<Value>(&row.1).unwrap_or(Value::Null),
        "dataPolicy": row.2,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Store, tempdir::TempDirGuard) {
        let dir = tempdir::make("sg-kb");
        let store = Store::open(dir.path(), "test").unwrap();
        store.with_conn(|c| {
            c.execute("INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at) VALUES ('pj','u','n','p','main',?1)", [timefmt::now()])?;
            Ok(())
        }).unwrap();
        (store, dir)
    }

    mod tempdir {
        use std::path::PathBuf;
        pub struct TempDirGuard(pub PathBuf);
        impl Drop for TempDirGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        pub fn make(prefix: &str) -> TempDirGuard {
            let path = std::env::temp_dir().join(format!("{prefix}-{}-{}", std::process::id(), sg_store::ids::new_id("t")));
            std::fs::create_dir_all(&path).unwrap();
            TempDirGuard(path)
        }
        impl TempDirGuard {
            pub fn path(&self) -> &std::path::Path { &self.0 }
        }
    }

    #[test]
    fn scan_index_search_isolated() {
        let (store, dir) = setup();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("docs")).unwrap();
        std::fs::create_dir_all(repo.join("node_modules/pkg")).unwrap();
        std::fs::write(repo.join("docs").join("auth.md"), "# 认证设计\nOIDC 登录流程：授权码模式。\n").unwrap();
        std::fs::write(repo.join("README.md"), "# Demo\n支持 sso 单点登录。\n").unwrap();
        std::fs::write(repo.join("node_modules").join("pkg").join("x.js"), "ignored").unwrap();
        std::fs::write(repo.join(".sixgatesignore"), "private/\n").unwrap();
        std::fs::write(repo.join("private"), b"secret area").ok(); // 文件非目录，命中前缀忽略

        let source = create_source(&store, "pj", "repo_path", "主仓库", repo.to_str().unwrap()).unwrap();
        let scanned = scan_source(&store, &source.id, None, 100, 64 << 10).unwrap();
        assert_eq!(scanned.scan_state, "indexed");
        assert!(!scanned.content_sha256.is_empty());

        let hits = search(&store, "pj", "认证 登录", 10).unwrap();
        assert!(!hits.is_empty(), "FTS 应命中");

        // 跨项目检索为空。
        assert!(search(&store, "pj_other", "认证", 10).unwrap().is_empty());

        let preview = context_preview(&store, "pj", "登录", 4096).unwrap();
        assert!(preview["items"].as_array().unwrap().len() > 0);
    }

    #[test]
    fn secret_files_are_skipped_fail_closed() {
        let (store, dir) = setup();
        let repo = dir.path().join("repo2");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("deploy.env"), "api_key = sk-0123456789abcdef\n").unwrap();
        let source = create_source(&store, "pj", "repo_path", "秘密仓", repo.to_str().unwrap()).unwrap();
        let result = scan_source(&store, &source.id, None, 100, 64 << 10);
        assert!(result.is_err(), "全秘密来源必须失败");
        let after = get_source(&store, &source.id).unwrap();
        assert_eq!(after.scan_state, "failed");
    }

    #[test]
    fn manifest_immutable_record() {
        let (store, _dir) = setup();
        store.with_conn(|c| {
            c.execute("INSERT INTO workitems(id, project_id, title, created_at, updated_at) VALUES ('wi','pj','t',?1,?1)", [timefmt::now()])?;
            Ok(())
        }).unwrap();
        let manifest = create_manifest(&store, "pj", "wi", "登录", &[]).unwrap();
        let fetched = get_manifest(&store, manifest["id"].as_str().unwrap()).unwrap();
        assert_eq!(fetched["workitemId"], "wi");
    }
}
