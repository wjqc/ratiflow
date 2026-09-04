//! 读路径：settings、列表、搜索、详情、FTS 投影重建（方案 §6.5/§7.1）。
use rusqlite::Connection;
use serde_json::{json, Value};

use sg_store::{objects, Store};

use crate::model::{merr, MemorySettings};

pub const ERR_TOKEN_NOT_FOUND: &str = "not_found";

fn row_to_settings(
    project_id: &str,
    feature_enabled: bool,
    r: (bool, String, i64, i64, i64, i64, String, String),
) -> MemorySettings {
    MemorySettings {
        project_id: project_id.to_string(),
        feature_enabled,
        enabled: r.0,
        capture_mode: r.1,
        max_entries: r.2,
        max_bytes: r.3,
        stale_after_days: r.4,
        revision: r.5,
        updated_at: r.6,
        updated_by: r.7,
    }
}

const SETTINGS_SELECT: &str = "SELECT enabled, capture_mode, max_entries, max_bytes, stale_after_days, revision, updated_at, updated_by FROM project_memory_settings WHERE project_id = ?1";

pub fn query_settings_row(
    conn: &Connection,
    project_id: &str,
    feature_enabled: bool,
) -> Result<MemorySettings, sg_store::Error> {
    let row = conn
        .query_row(SETTINGS_SELECT, [project_id], |r| {
            Ok((
                r.get::<_, i64>(0)? != 0,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
            ))
        })
        .map_err(sg_store::Error::from)?;
    Ok(row_to_settings(project_id, feature_enabled, row))
}

/// 项目是否存在（not_found 语义，MEM-010 项目隔离第一道门）。
pub fn require_project(conn: &Connection, project_id: &str) -> Result<(), sg_store::Error> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM projects WHERE id = ?1",
            [project_id],
            |r| r.get(0),
        )
        .map_err(sg_store::Error::from)?;
    if n == 0 {
        return Err(merr(
            ERR_TOKEN_NOT_FOUND,
            format!("项目 {project_id} 不存在"),
        ));
    }
    Ok(())
}

pub fn project_archived(conn: &Connection, project_id: &str) -> Result<bool, sg_store::Error> {
    conn.query_row(
        "SELECT archived_at FROM projects WHERE id = ?1",
        [project_id],
        |r| r.get::<_, Option<String>>(0),
    )
    .map(|v| v.is_some())
    .map_err(sg_store::Error::from)
}

/// 全局 rollout flag（方案 §6.1/§16.1）：默认 false；
/// 开发/测试/E2E 构建可用环境变量 SIXGATES_MEMORY_FEATURE 显式打开（生产不设置），
/// 未设置时读 app_settings（memory.featureEnabled，默认 false）。
pub fn feature_enabled(store: &Store) -> Result<bool, sg_store::Error> {
    if let Ok(v) = std::env::var("SIXGATES_MEMORY_FEATURE") {
        return Ok(v == "1" || v.eq_ignore_ascii_case("true"));
    }
    store.with_conn(|conn| {
        let v: Option<String> = conn
            .query_row(
                "SELECT value_json FROM app_settings WHERE scope='global' AND project_id='' AND key='memory.featureEnabled'",
                [],
                |r| r.get(0),
            )
            .ok();
        Ok(v.map(|s| s.contains("true")).unwrap_or(false))
    })
}

/// 项目策略：惰性建行（一项目一行，方案 §6.1），不存在项目报 not_found。
pub fn settings_get(store: &Store, project_id: &str) -> Result<MemorySettings, sg_store::Error> {
    let feature = feature_enabled(store)?;
    store.with_conn(|conn| {
        require_project(conn, project_id)?;
        conn.execute(
            "INSERT OR IGNORE INTO project_memory_settings(project_id, updated_at, updated_by)
             VALUES (?1, ?2, 'local')",
            rusqlite::params![project_id, sg_store::timefmt::now()],
        )?;
        query_settings_row(conn, project_id, feature)
    })
}

pub struct EntryRow {
    pub id: String,
    pub project_id: String,
    pub slug: String,
    pub kind: String,
    pub subject_key: String,
    pub status: String,
    pub current_revision_id: Option<String>,
    pub pinned: bool,
    pub valid_until: Option<String>,
    pub confirmed_at: Option<String>,
    pub confirmed_by: String,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
    pub purged_at: Option<String>,
}

/// 条目主档（含项目二次校验；MEM-010）。
pub fn entry_row(
    conn: &Connection,
    project_id: &str,
    memory_id: &str,
) -> Result<EntryRow, sg_store::Error> {
    conn.query_row(
        "SELECT id, project_id, slug, kind, subject_key, status, current_revision_id, pinned,
                valid_until, confirmed_at, confirmed_by, revision, created_at, updated_at,
                archived_at, purged_at
         FROM memory_entries WHERE id = ?1 AND project_id = ?2",
        rusqlite::params![memory_id, project_id],
        |r| {
            Ok(EntryRow {
                id: r.get(0)?,
                project_id: r.get(1)?,
                slug: r.get(2)?,
                kind: r.get(3)?,
                subject_key: r.get(4)?,
                status: r.get(5)?,
                current_revision_id: r.get(6)?,
                pinned: r.get::<_, i64>(7)? != 0,
                valid_until: r.get(8)?,
                confirmed_at: r.get(9)?,
                confirmed_by: r.get(10)?,
                revision: r.get(11)?,
                created_at: r.get(12)?,
                updated_at: r.get(13)?,
                archived_at: r.get(14)?,
                purged_at: r.get(15)?,
            })
        },
    )
    .map_err(|_| merr(ERR_TOKEN_NOT_FOUND, format!("记忆 {memory_id} 不存在")))
}

/// 列表：状态/类型过滤 + 稳定 cursor；query 走 FTS 命中集合；置顶优先。
pub fn list(
    store: &Store,
    project_id: &str,
    query: Option<&str>,
    statuses: Option<&[String]>,
    kinds: Option<&[String]>,
    cursor: Option<&str>,
    limit: i64,
) -> Result<Value, sg_store::Error> {
    let limit = limit.clamp(1, 200);
    store.with_conn(|conn| {
        require_project(conn, project_id)?;
        let ids_filter: Option<Vec<String>> = match query.map(str::trim).filter(|q| !q.is_empty()) {
            Some(q) => Some(search_ids(conn, project_id, q, 500)?),
            None => None,
        };
        if let Some(ids) = &ids_filter {
            if ids.is_empty() {
                return Ok(json!({"projectId": project_id, "items": [], "counts": counts(conn, project_id)?, "cursor": Value::Null}));
            }
        }

        let mut sql = String::from(
            "SELECT e.id, e.slug, e.kind, e.status, e.pinned, e.revision, e.updated_at,
                    r.revision_no, r.title, r.summary, r.tags_json,
                    (SELECT COUNT(*) FROM memory_source_refs s WHERE s.revision_id = r.id)
             FROM memory_entries e
             JOIN memory_revisions r ON r.id = e.current_revision_id
             WHERE e.project_id = ?1",
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(project_id.to_string())];
        let statuses_in: Vec<String> = match statuses {
            Some(list) if !list.is_empty() => list.to_vec(),
            _ => [
                "proposed",
                "active",
                "conflicted",
                "archived",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        };
        sql.push_str(&format!(
            " AND e.status IN ({})",
            vec!["?"; statuses_in.len()].join(",")
        ));
        for s in &statuses_in {
            params.push(Box::new(s.clone()));
        }
        if let Some(kinds) = kinds {
            if !kinds.is_empty() {
                sql.push_str(&format!(
                    " AND e.kind IN ({})",
                    vec!["?"; kinds.len()].join(",")
                ));
                for k in kinds {
                    params.push(Box::new(k.clone()));
                }
            }
        }
        if let Some(ids) = &ids_filter {
            sql.push_str(&format!(
                " AND e.id IN ({})",
                vec!["?"; ids.len()].join(",")
            ));
            for id in ids {
                params.push(Box::new(id.clone()));
            }
        }
        if let Some(cur) = cursor {
            if let Some((at, id)) = parse_cursor(cur) {
                sql.push_str(" AND (e.updated_at < ? OR (e.updated_at = ? AND e.id > ?))");
                params.push(Box::new(at.clone()));
                params.push(Box::new(at));
                params.push(Box::new(id));
            }
        }
        sql.push_str(&format!(
            " ORDER BY e.pinned DESC, e.updated_at DESC, e.id ASC LIMIT {limit}"
        ));
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
            let tags_json: String = r.get(10)?;
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "slug": r.get::<_, String>(1)?,
                "kind": r.get::<_, String>(2)?,
                "status": r.get::<_, String>(3)?,
                "pinned": r.get::<_, i64>(4)? != 0,
                "revision": r.get::<_, i64>(5)?,
                "updatedAt": r.get::<_, String>(6)?,
                "revisionNo": r.get::<_, i64>(7)?,
                "title": r.get::<_, String>(8)?,
                "summary": r.get::<_, String>(9)?,
                "tags": serde_json::from_str::<Vec<String>>(&tags_json).unwrap_or_default(),
                "sourceCount": r.get::<_, i64>(11)?,
            }))
        })?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        let next_cursor = items
            .last()
            .and_then(|it: &Value| {
                let at = it["updatedAt"].as_str()?.to_string();
                let id = it["id"].as_str()?.to_string();
                Some(format!("{at}|{id}"))
            })
            .map(Value::String)
            .unwrap_or(Value::Null);
        let counts = counts(conn, project_id)?;
        Ok(json!({
            "projectId": project_id,
            "items": items,
            "counts": counts,
            "cursor": next_cursor,
        }))
    })
}

fn parse_cursor(cur: &str) -> Option<(String, String)> {
    let (at, id) = cur.split_once('|')?;
    if at.is_empty() || id.is_empty() {
        return None;
    }
    Some((at.to_string(), id.to_string()))
}

pub fn counts(conn: &Connection, project_id: &str) -> Result<Value, sg_store::Error> {
    let mut out = serde_json::Map::new();
    for status in crate::model::STATUSES {
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_entries WHERE project_id = ?1 AND status = ?2",
            rusqlite::params![project_id, status],
            |r| r.get(0),
        )?;
        out.insert(status.to_string(), json!(n));
    }
    Ok(Value::Object(out))
}

/// 用户检索词项清洗：剔除 FTS 语法字符；空 query 返回空（调用方回落最近列表）。
pub fn sanitize_terms(query: &str, max_terms: usize) -> Vec<String> {
    query
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| !matches!(c, '"' | '*' | '(' | ')' | ':' | '\'' | '%' | '-'))
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .take(max_terms)
        .collect()
}

/// FTS 命中的 memory_id 集合（trigram ≥3 字 MATCH / 短词 LIKE 兜底，用户输入不进 FTS 语法层）。
pub fn search_ids(
    conn: &Connection,
    project_id: &str,
    query: &str,
    limit: i64,
) -> Result<Vec<String>, sg_store::Error> {
    let terms = sanitize_terms(query, 8);
    if terms.is_empty() {
        return Ok(vec![]);
    }
    let mut conditions = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(project_id.to_string())];
    for (i, term) in terms.iter().enumerate() {
        conditions.push(format!(
            "(title LIKE ?{n} OR tags LIKE ?{n} OR body LIKE ?{n})",
            n = i + 2
        ));
        params.push(Box::new(format!("%{term}%")));
    }
    let sql = format!(
        "SELECT DISTINCT memory_id FROM memory_fts
         WHERE project_id = ?1 AND ({}) LIMIT {limit}",
        conditions.join(" AND ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
        r.get::<_, String>(0)
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// 显式搜索：管理检索投影（active/archived/conflicted 均可命中，purged 已不在 FTS）。
pub fn search(
    store: &Store,
    project_id: &str,
    query: &str,
    kinds: Option<&[String]>,
    limit: i64,
) -> Result<Value, sg_store::Error> {
    let limit = limit.clamp(1, 100);
    store.with_conn(|conn| {
        require_project(conn, project_id)?;
        if sanitize_terms(query, 8).is_empty() {
            return Ok(json!({"projectId": project_id, "items": []}));
        }
        let ids = search_ids(conn, project_id, query, limit * 4)?;
        if ids.is_empty() {
            return Ok(json!({"projectId": project_id, "items": []}));
        }
        let mut kind_sql = String::new();
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(project_id.to_string())];
        if let Some(kinds) = kinds {
            if !kinds.is_empty() {
                kind_sql = format!(" AND e.kind IN ({})", vec!["?"; kinds.len()].join(","));
                for k in kinds {
                    params.push(Box::new(k.clone()));
                }
            }
        }
        let placeholders = vec!["?"; ids.len()].join(",");
        for id in &ids {
            params.push(Box::new(id.clone()));
        }
        let sql = format!(
            "SELECT e.id, r.id, r.title, e.status, e.kind, r.summary
             FROM memory_entries e
             JOIN memory_revisions r ON r.id = e.current_revision_id
             WHERE e.project_id = ?1{kind_sql} AND e.id IN ({placeholders})
             ORDER BY e.updated_at DESC LIMIT {limit}"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
            Ok(json!({
                "memoryId": r.get::<_, String>(0)?,
                "revisionId": r.get::<_, String>(1)?,
                "title": r.get::<_, String>(2)?,
                "status": r.get::<_, String>(3)?,
                "kind": r.get::<_, String>(4)?,
                "summary": r.get::<_, String>(5)?,
            }))
        })?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(json!({"projectId": project_id, "items": items}))
    })
}

/// 正文读取：object 缺失/哈希不符 fail-closed（MEM-026 / §7.4）。
pub fn read_body(store: &Store, object_sha256: &str) -> Result<String, sg_store::Error> {
    let bytes = objects::open(store, object_sha256).map_err(|_| {
        merr(
            crate::model::err_tokens::OBJECT_MISSING,
            format!("正文对象 {object_sha256} 缺失"),
        )
    })?;
    let actual = crate::model::sha256_hex(&bytes);
    if actual != object_sha256 {
        return Err(merr(
            crate::model::err_tokens::OBJECT_MISSING,
            format!("正文对象哈希不符：期望 {object_sha256}，实际 {actual}"),
        ));
    }
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

/// 详情：metadata + 正文/墓碑 + 来源 + 版本史 + 使用记录。
pub fn detail(
    store: &Store,
    project_id: &str,
    memory_id: &str,
    revision_id: Option<&str>,
) -> Result<Value, sg_store::Error> {
    store.with_conn(|conn| {
        require_project(conn, project_id)?;
        let entry = entry_row(conn, project_id, memory_id)?;
        let rev_id = revision_id
            .map(String::from)
            .or_else(|| entry.current_revision_id.clone())
            .ok_or_else(|| merr(crate::model::err_tokens::INVALID_STATE, "条目缺少当前修订"))?;
        let rev = conn
            .query_row(
                "SELECT id, revision_no, title, summary, object_sha256, content_sha256, tags_json,
                        source_type, author_kind, author_id, created_at, purged_at
                 FROM memory_revisions WHERE id = ?1 AND memory_id = ?2",
                rusqlite::params![rev_id, memory_id],
                |r| {
                    Ok(json!({
                        "revisionId": r.get::<_, String>(0)?,
                        "revisionNo": r.get::<_, i64>(1)?,
                        "title": r.get::<_, String>(2)?,
                        "summary": r.get::<_, String>(3)?,
                        "objectSha256": r.get::<_, Option<String>>(4)?,
                        "contentSha256": r.get::<_, String>(5)?,
                        "tags": serde_json::from_str::<Vec<String>>(&r.get::<_, String>(6)?).unwrap_or_default(),
                        "sourceType": r.get::<_, String>(7)?,
                        "authorKind": r.get::<_, String>(8)?,
                        "authorId": r.get::<_, String>(9)?,
                        "createdAt": r.get::<_, String>(10)?,
                        "purgedAt": r.get::<_, Option<String>>(11)?,
                    }))
                },
            )
            .map_err(|_| merr(ERR_TOKEN_NOT_FOUND, "修订不存在"))?;

        let purged = entry.status == "purged";
        let (body, body_state): (Value, &str) = if purged {
            (Value::Null, "purged")
        } else {
            let sha = rev["objectSha256"].as_str().ok_or_else(|| {
                merr(
                    crate::model::err_tokens::OBJECT_MISSING,
                    "当前修订缺少正文对象",
                )
            })?;
            (Value::String(read_body(store, sha)?), "available")
        };

        let mut sources = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT source_kind, source_id, locator, source_digest, relation
             FROM memory_source_refs WHERE revision_id = ?1 ORDER BY ordinal",
        )?;
        let rows = stmt.query_map([&rev_id], |r| {
            Ok(json!({
                "sourceKind": r.get::<_, String>(0)?,
                "sourceId": r.get::<_, String>(1)?,
                "locator": r.get::<_, String>(2)?,
                "sourceDigest": r.get::<_, String>(3)?,
                "relation": r.get::<_, String>(4)?,
            }))
        })?;
        for row in rows {
            sources.push(row?);
        }

        let mut revisions = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT revision_no, title, content_sha256, created_at, purged_at
             FROM memory_revisions WHERE memory_id = ?1 ORDER BY revision_no",
        )?;
        let rows = stmt.query_map([memory_id], |r| {
            Ok(json!({
                "revisionNo": r.get::<_, i64>(0)?,
                "title": r.get::<_, String>(1)?,
                "contentSha256": r.get::<_, String>(2)?,
                "createdAt": r.get::<_, String>(3)?,
                "purgedAt": r.get::<_, Option<String>>(4)?,
            }))
        })?;
        for row in rows {
            revisions.push(row?);
        }

        let mut usage = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT m.manifest_id, m.revision_id, m.selected_at
             FROM context_manifest_memories m
             WHERE m.memory_id = ?1 ORDER BY m.selected_at DESC LIMIT 20",
        )?;
        let rows = stmt.query_map([memory_id], |r| {
            let rev: String = r.get(1)?;
            let rev_no: i64 = conn
                .query_row(
                    "SELECT revision_no FROM memory_revisions WHERE id = ?1",
                    [&rev],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            Ok(json!({
                "manifestId": r.get::<_, String>(0)?,
                "revisionNo": rev_no,
                "selectedAt": r.get::<_, String>(2)?,
            }))
        })?;
        for row in rows {
            usage.push(row?);
        }

        Ok(json!({
            "id": entry.id,
            "projectId": entry.project_id,
            "slug": entry.slug,
            "kind": entry.kind,
            "status": entry.status,
            "pinned": entry.pinned,
            "validUntil": entry.valid_until,
            "confirmedAt": entry.confirmed_at,
            "confirmedBy": entry.confirmed_by,
            "revisionNo": rev["revisionNo"],
            "revisionId": rev["revisionId"],
            "revision": entry.revision,
            "title": rev["title"],
            "summary": rev["summary"],
            "bodyState": body_state,
            "body": body,
            "objectSha256": rev["objectSha256"],
            "contentSha256": rev["contentSha256"],
            "tags": rev["tags"],
            "sourceType": rev["sourceType"],
            "sources": sources,
            "revisions": revisions,
            "usage": usage,
            "createdAt": entry.created_at,
            "updatedAt": entry.updated_at,
            "archivedAt": entry.archived_at,
            "purgedAt": entry.purged_at,
        }))
    })
}

struct FtsRow {
    memory_id: String,
    project_id: String,
    revision_id: String,
    title: String,
    tags: String,
    object_sha: String,
}

/// FTS 投影重建：逐条复核 object hash；损坏对象跳过并如实上报（方案 §6.5）。
pub fn rebuild_index(store: &Store, project_id: Option<&str>) -> Result<Value, sg_store::Error> {
    let rows: Vec<FtsRow> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT e.id, e.project_id, r.id, r.title, r.tags_json, r.object_sha256
             FROM memory_entries e JOIN memory_revisions r ON r.id = e.current_revision_id
             WHERE e.status != 'purged' AND (?1 = '' OR e.project_id = ?1)",
        )?;
        let rows = stmt.query_map(rusqlite::params![project_id.unwrap_or("")], |r| {
            Ok(FtsRow {
                memory_id: r.get(0)?,
                project_id: r.get(1)?,
                revision_id: r.get(2)?,
                title: r.get(3)?,
                tags: r.get(4)?,
                object_sha: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;

    let mut corrupt: Vec<Value> = Vec::new();
    let mut rebuilt = 0i64;
    for row in &rows {
        if row.object_sha.is_empty() {
            corrupt.push(json!({"memoryId": row.memory_id, "reason": "missing_object"}));
            continue;
        }
        match read_body(store, &row.object_sha) {
            Ok(body) => {
                store.with_tx(|tx| {
                    tx.execute(
                        "DELETE FROM memory_fts WHERE memory_id = ?1",
                        [&row.memory_id],
                    )?;
                    tx.execute(
                        "INSERT INTO memory_fts(memory_id, revision_id, project_id, title, tags, body)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        rusqlite::params![
                            row.memory_id,
                            row.revision_id,
                            row.project_id,
                            row.title,
                            row.tags,
                            body
                        ],
                    )?;
                    Ok(())
                })?;
                rebuilt += 1;
            }
            Err(_) => corrupt.push(json!({"memoryId": row.memory_id, "reason": "hash_mismatch"})),
        }
    }
    Ok(json!({"rebuilt": rebuilt, "corrupt": corrupt}))
}
