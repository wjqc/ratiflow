//! 需求事实域（ADR-030 M1）：文档 → 不可变修订 → 需求项。
//! 正文进 objects，库内只存哈希；同文档同内容幂等（不产生新修订）。
//! 每次导入同步注册谱系节点/边：item -derived_from→ revision，
//! 新修订 -supersedes→ 上一修订；legacy 回填一律 unverified。

use serde::Serialize;
use sg_provenance::{node_type, relation, EdgeInput, NodeInput};
use sg_store::{ids, objects, outbox, timefmt, Error, Store};
use sha2::{Digest, Sha256};

use crate::docs;

#[derive(Debug, Clone, Serialize)]
pub struct RequirementDocument {
    pub id: String,
    pub workitem_id: String,
    pub source_kind: String,
    pub source_ref: String,
    pub title: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RequirementRevision {
    pub id: String,
    pub document_id: String,
    pub revision_no: i64,
    pub object_sha256: String,
    pub content_sha256: String,
    pub created_by: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes_revision_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RequirementItem {
    pub id: String,
    pub revision_id: String,
    pub requirement_key: String,
    pub title: String,
    pub body_sha256: String,
    pub anchor_json: String,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportResult {
    pub document: RequirementDocument,
    pub revision: RequirementRevision,
    pub items: Vec<RequirementItem>,
    /// 同内容重复导入时为 true（返回既有修订，不追加）。
    pub deduplicated: bool,
}

pub struct ParsedItem {
    pub requirement_key: String,
    pub title: String,
    pub body: String,
    pub line: usize,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    ids::hex(&hasher.finalize())
}

/// 确定性需求项解析：Markdown 列表行（含任务勾选框）为条目；
/// 无列表时整篇回退为单条目（legacy 导入场景，unverified 标记诚实性由调用方保证）。
/// 条目文本中反引号包裹的 `REQ-xxx` 优先作为 key，否则按顺序生成 REQ-001…。
pub fn parse_items(content: &str) -> Vec<ParsedItem> {
    let mut items: Vec<ParsedItem> = Vec::new();
    let mut auto_key = 0usize;
    for (idx, raw) in content.lines().enumerate() {
        let trimmed = raw.trim_start();
        let body = trimmed
            .strip_prefix("- [ ] ")
            .or_else(|| trimmed.strip_prefix("- [x] "))
            .or_else(|| trimmed.strip_prefix("- [X] "))
            .or_else(|| trimmed.strip_prefix("- "))
            .or_else(|| trimmed.strip_prefix("* "))
            .map(str::trim);
        let Some(body) = body else { continue };
        if body.is_empty() {
            continue;
        }
        auto_key += 1;
        let key = extract_key(body).unwrap_or_else(|| format!("REQ-{auto_key:03}"));
        let title: String = body.chars().take(80).collect();
        items.push(ParsedItem {
            requirement_key: key,
            title,
            body: body.to_string(),
            line: idx + 1,
        });
    }
    if items.is_empty() {
        let fallback = content.trim();
        if !fallback.is_empty() {
            let title: String = fallback.chars().take(80).collect();
            items.push(ParsedItem {
                requirement_key: "REQ-001".into(),
                title,
                body: fallback.to_string(),
                line: 1,
            });
        }
    }
    items
}

fn extract_key(body: &str) -> Option<String> {
    let start = body.find('`')? + 1;
    let rest = &body[start..];
    let end = rest.find('`')?;
    let candidate = &rest[..end];
    let upper: String = candidate.to_uppercase();
    if upper.starts_with("REQ-")
        && candidate[4..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
        && candidate.len() > 4
    {
        Some(upper)
    } else {
        None
    }
}

fn find_document(
    store: &Store,
    workitem_id: &str,
    source_ref: &str,
) -> Result<Option<RequirementDocument>, Error> {
    store.with_conn(|conn| {
        let doc = conn
            .query_row(
                "SELECT id, workitem_id, source_kind, source_ref, title, created_at
                 FROM requirement_documents WHERE workitem_id=?1 AND source_ref=?2",
                [workitem_id, source_ref],
                |r| {
                    Ok(RequirementDocument {
                        id: r.get(0)?,
                        workitem_id: r.get(1)?,
                        source_kind: r.get(2)?,
                        source_ref: r.get(3)?,
                        title: r.get(4)?,
                        created_at: r.get(5)?,
                    })
                },
            )
            .ok();
        Ok(doc)
    })
}

fn latest_revision(store: &Store, document_id: &str) -> Result<Option<RequirementRevision>, Error> {
    store.with_conn(|conn| {
        let rev = conn
            .query_row(
                "SELECT id, document_id, revision_no, object_sha256, content_sha256, created_by, created_at, supersedes_revision_id
                 FROM requirement_revisions WHERE document_id=?1 ORDER BY revision_no DESC LIMIT 1",
                [document_id],
                row_revision,
            )
            .ok();
        Ok(rev)
    })
}

fn row_revision(r: &rusqlite::Row<'_>) -> rusqlite::Result<RequirementRevision> {
    Ok(RequirementRevision {
        id: r.get(0)?,
        document_id: r.get(1)?,
        revision_no: r.get(2)?,
        object_sha256: r.get(3)?,
        content_sha256: r.get(4)?,
        created_by: r.get(5)?,
        created_at: r.get(6)?,
        supersedes_revision_id: r.get(7)?,
    })
}

/// 导入一个需求修订（verified 语义由调用方决定：用户导入 verified，legacy 回填 unverified）。
#[allow(clippy::too_many_arguments)]
pub fn import_revision(
    store: &Store,
    workitem_id: &str,
    filename: &str,
    content: &str,
    source_kind: &str,
    created_by: &str,
    verification_state: &str,
) -> Result<ImportResult, Error> {
    if workitem_id.is_empty() || content.trim().is_empty() {
        return Err(Error::Message("workitem and content required".into()));
    }
    crate::get(store, workitem_id)?;

    let document = match find_document(store, workitem_id, filename)? {
        Some(doc) => doc,
        None => {
            let id = ids::new_id("rd");
            let title = docs::title_from_document(filename, content);
            store.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO requirement_documents(id, workitem_id, source_kind, source_ref, title, created_at)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                    rusqlite::params![id, workitem_id, source_kind, filename, title, timefmt::now()],
                )?;
                Ok(())
            })?;
            find_document(store, workitem_id, filename)?
                .ok_or_else(|| Error::Message("trace_incomplete: 需求文档创建后不可见".into()))?
        }
    };

    // 同文档同内容幂等。
    let content_sha = sha256_hex(content.as_bytes());
    if let Some(existing) = store.with_conn(|conn| {
        let rev = conn
            .query_row(
                "SELECT id, document_id, revision_no, object_sha256, content_sha256, created_by, created_at, supersedes_revision_id
                 FROM requirement_revisions WHERE document_id=?1 AND content_sha256=?2",
                [&document.id, &content_sha],
                row_revision,
            )
            .ok();
        Ok(rev)
    })? {
        let items = items_of_revision(store, &existing.id)?;
        return Ok(ImportResult {
            document,
            revision: existing,
            items,
            deduplicated: true,
        });
    }

    let info = objects::put(store, content.as_bytes(), objects::PutOptions::default())
        .map_err(|e| Error::Message(format!("store requirement: {e}")))?;
    let previous = latest_revision(store, &document.id)?;
    let revision_no = previous.as_ref().map(|r| r.revision_no + 1).unwrap_or(1);
    let id = ids::new_id("rr");
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO requirement_revisions(id, document_id, revision_no, object_sha256, content_sha256, created_by, created_at, supersedes_revision_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![
                id,
                document.id,
                revision_no,
                info.sha256,
                content_sha,
                created_by,
                timefmt::now(),
                previous.as_ref().map(|r| r.id.clone())
            ],
        )?;
        Ok(())
    })?;

    let parsed = parse_items(content);
    let mut items = Vec::new();
    for item in &parsed {
        let item_id = ids::new_id("ri");
        let body_sha = sha256_hex(item.body.as_bytes());
        let anchor = serde_json::json!({"file": filename, "line": item.line}).to_string();
        store.with_conn(|conn| {
            conn.execute(
                "INSERT INTO requirement_items(id, revision_id, requirement_key, title, body_sha256, anchor_json, acceptance_json, status, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,'[]','active',?7)",
                rusqlite::params![item_id, id, item.requirement_key, item.title, body_sha, anchor, timefmt::now()],
            )?;
            Ok(())
        })?;
        items.push(RequirementItem {
            id: item_id,
            revision_id: id.clone(),
            requirement_key: item.requirement_key.clone(),
            title: item.title.clone(),
            body_sha256: body_sha,
            anchor_json: anchor,
            status: "active".into(),
            created_at: timefmt::now(),
        });
    }

    // 谱系：修订节点 + 条目节点 + item -derived_from→ revision + supersedes 链。
    sg_provenance::register_node(
        store,
        &NodeInput {
            project_id: "",
            workitem_id,
            node_type: node_type::REQUIREMENT_REVISION,
            entity_id: &id,
            content_digest: &content_sha,
            verification_state,
        },
    )?;
    for item in &items {
        sg_provenance::register_node(
            store,
            &NodeInput {
                project_id: "",
                workitem_id,
                node_type: node_type::REQUIREMENT_ITEM,
                entity_id: &item.id,
                content_digest: &item.body_sha256,
                verification_state,
            },
        )?;
        sg_provenance::add_edge(
            store,
            &EdgeInput {
                workitem_id,
                from_node_type: node_type::REQUIREMENT_ITEM,
                from_entity_id: &item.id,
                relation: relation::DERIVED_FROM,
                to_node_type: node_type::REQUIREMENT_REVISION,
                to_entity_id: &id,
                stage_attempt_id: "",
                created_by_run_id: "",
            },
        )?;
    }
    if let Some(prev) = &previous {
        sg_provenance::add_edge(
            store,
            &EdgeInput {
                workitem_id,
                from_node_type: node_type::REQUIREMENT_REVISION,
                from_entity_id: &id,
                relation: relation::SUPERSEDES,
                to_node_type: node_type::REQUIREMENT_REVISION,
                to_entity_id: &prev.id,
                stage_attempt_id: "",
                created_by_run_id: "",
            },
        )?;
    }

    let revision = RequirementRevision {
        id: id.clone(),
        document_id: document.id.clone(),
        revision_no,
        object_sha256: info.sha256,
        content_sha256: content_sha,
        created_by: created_by.into(),
        created_at: timefmt::now(),
        supersedes_revision_id: previous.map(|r| r.id),
    };
    outbox::emit(
        store,
        "workitem",
        workitem_id,
        "requirement.revision_imported",
        serde_json::json!({
            "workitemId": workitem_id,
            "revisionId": revision.id,
            "revisionNo": revision.revision_no,
            "itemCount": items.len(),
            "sourceKind": source_kind,
            "verificationState": verification_state,
        }),
    )?;
    Ok(ImportResult {
        document,
        revision,
        items,
        deduplicated: false,
    })
}

/// 按需求 key 定位条目 id（在修订内）。
pub fn item_id_by_key(
    store: &Store,
    revision_id: &str,
    requirement_key: &str,
) -> Result<Option<String>, Error> {
    store.with_conn(|conn| {
        let id = conn
            .query_row(
                "SELECT id FROM requirement_items WHERE revision_id=?1 AND requirement_key=?2",
                [revision_id, requirement_key],
                |r| r.get::<_, String>(0),
            )
            .ok();
        Ok(id)
    })
}

fn items_of_revision(store: &Store, revision_id: &str) -> Result<Vec<RequirementItem>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, revision_id, requirement_key, title, body_sha256, anchor_json, status, created_at
             FROM requirement_items WHERE revision_id=?1 ORDER BY requirement_key",
        )?;
        let rows = stmt.query_map([revision_id], |r| {
            Ok(RequirementItem {
                id: r.get(0)?,
                revision_id: r.get(1)?,
                requirement_key: r.get(2)?,
                title: r.get(3)?,
                body_sha256: r.get(4)?,
                anchor_json: r.get(5)?,
                status: r.get(6)?,
                created_at: r.get(7)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

pub fn items(store: &Store, revision_id: &str) -> Result<Vec<RequirementItem>, Error> {
    items_of_revision(store, revision_id)
}

pub fn documents(store: &Store, workitem_id: &str) -> Result<Vec<RequirementDocument>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, workitem_id, source_kind, source_ref, title, created_at
             FROM requirement_documents WHERE workitem_id=?1 ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([workitem_id], |r| {
            Ok(RequirementDocument {
                id: r.get(0)?,
                workitem_id: r.get(1)?,
                source_kind: r.get(2)?,
                source_ref: r.get(3)?,
                title: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

/// 文档及其全部修订（revision_no 升序）。
pub fn revisions(
    store: &Store,
    workitem_id: &str,
) -> Result<Vec<(RequirementDocument, Vec<RequirementRevision>)>, Error> {
    let docs = documents(store, workitem_id)?;
    let mut out = Vec::new();
    for doc in docs {
        let revs: Vec<RequirementRevision> = store.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, document_id, revision_no, object_sha256, content_sha256, created_by, created_at, supersedes_revision_id
                 FROM requirement_revisions WHERE document_id=?1 ORDER BY revision_no",
            )?;
            let rows = stmt.query_map([&doc.id], row_revision)?;
            let mut revs = Vec::new();
            for row in rows {
                revs.push(row?);
            }
            Ok(revs)
        })?;
        out.push((doc, revs));
    }
    Ok(out)
}

/// 各文档最新修订中 created_at 最新的一个（coverage 默认目标）。
pub fn latest_revision_id(store: &Store, workitem_id: &str) -> Result<Option<String>, Error> {
    store.with_conn(|conn| {
        let id = conn
            .query_row(
                "SELECT r.id FROM requirement_revisions r
                 JOIN requirement_documents d ON d.id = r.document_id
                 WHERE d.workitem_id=?1 ORDER BY r.created_at DESC, r.revision_no DESC LIMIT 1",
                [workitem_id],
                |r| r.get::<_, String>(0),
            )
            .ok();
        Ok(id)
    })
}

/// legacy 回填（蓝图 §11.3-4）：为尚无需求文档的 WorkItem 生成 synthetic 修订，
/// 内容来自 docs/{workItemId}/ 文件（逐文件建档）或 title+description 兜底；
/// 节点一律 unverified——历史无法证明的链路不得伪装成已验证。幂等：有文档即跳过。
pub fn backfill_legacy(store: &Store) -> Result<usize, Error> {
    let workitems: Vec<(String, String, String)> = store.with_conn(|conn| {
        let mut stmt =
            conn.prepare("SELECT id, title, description FROM workitems ORDER BY created_at")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;
    let mut count = 0usize;
    for (workitem_id, title, description) in workitems {
        if !documents(store, &workitem_id)?.is_empty() {
            continue;
        }
        let files = docs::list(store, &workitem_id)?;
        if files.is_empty() {
            let content = format!("# {title}\n\n{description}\n");
            import_revision(
                store,
                &workitem_id,
                "requirement.md",
                &content,
                "legacy_import",
                "legacy-import",
                "unverified",
            )?;
        } else {
            for file in &files {
                let content = docs::read(store, &workitem_id, file).unwrap_or_default();
                if content.trim().is_empty() {
                    continue;
                }
                import_revision(
                    store,
                    &workitem_id,
                    file,
                    &content,
                    "legacy_import",
                    "legacy-import",
                    "unverified",
                )?;
            }
        }
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sg_store::Store;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-req-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj','u','n','p','main',?1)",
                    [timefmt::now()],
                )?;
                Ok(())
            })
            .unwrap();
        store
    }

    const REQUIREMENT_MD: &str = "# 支付对账\n\n目标：商户对账零差异。\n\n- [ ] 支持日终自动对账 `REQ-ACC-01`\n- 导出差异报告\n- [x] 手动触发重算\n";

    #[test]
    fn parse_items_extracts_list_and_explicit_keys() {
        let items = parse_items(REQUIREMENT_MD);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].requirement_key, "REQ-ACC-01");
        assert_eq!(items[1].requirement_key, "REQ-002");
        assert_eq!(items[1].body, "导出差异报告");
        assert_eq!(items[2].requirement_key, "REQ-003");
        // 无列表回退单条目。
        let fallback = parse_items("# 只有段落\n\n正文一段。\n");
        assert_eq!(fallback.len(), 1);
        assert_eq!(fallback[0].requirement_key, "REQ-001");
    }

    #[test]
    fn import_creates_document_revision_items_and_nodes() {
        let s = setup();
        let wi = crate::create(&s, "pj", "对账", "", None, &[]).unwrap();
        let result = import_revision(
            &s,
            &wi.id,
            "requirement.md",
            REQUIREMENT_MD,
            "document",
            "local-user",
            "verified",
        )
        .unwrap();
        assert_eq!(result.revision.revision_no, 1);
        assert_eq!(result.items.len(), 3);
        assert!(!result.deduplicated);
        // 修订节点 + 3 条目节点；item -derived_from→ revision。
        assert!(
            sg_provenance::find_node(&s, node_type::REQUIREMENT_REVISION, &result.revision.id)
                .unwrap()
                .is_some()
        );
        let item_node =
            sg_provenance::find_node(&s, node_type::REQUIREMENT_ITEM, &result.items[0].id)
                .unwrap()
                .unwrap();
        assert_eq!(item_node.verification_state, "verified");
        let lineage = sg_provenance::lineage(&s, &item_node.id, "down", 3).unwrap();
        assert_eq!(lineage.edges.len(), 1);
        assert_eq!(lineage.edges[0].relation, "derived_from");
    }

    #[test]
    fn same_content_is_deduplicated_new_content_supersedes() {
        let s = setup();
        let wi = crate::create(&s, "pj", "对账", "", None, &[]).unwrap();
        let first = import_revision(
            &s,
            &wi.id,
            "requirement.md",
            REQUIREMENT_MD,
            "document",
            "u",
            "verified",
        )
        .unwrap();
        let again = import_revision(
            &s,
            &wi.id,
            "requirement.md",
            REQUIREMENT_MD,
            "document",
            "u",
            "verified",
        )
        .unwrap();
        assert!(again.deduplicated);
        assert_eq!(again.revision.id, first.revision.id);
        // 内容变化 → 新修订 + supersedes 边。
        let second = import_revision(
            &s,
            &wi.id,
            "requirement.md",
            &format!("{REQUIREMENT_MD}\n- 新增支付渠道\n"),
            "document",
            "u",
            "verified",
        )
        .unwrap();
        assert_eq!(second.revision.revision_no, 2);
        assert_eq!(
            second.revision.supersedes_revision_id.as_deref(),
            Some(first.revision.id.as_str())
        );
        let supersede = sg_provenance::find_edge(
            &s,
            &sg_provenance::find_node(&s, node_type::REQUIREMENT_REVISION, &second.revision.id)
                .unwrap()
                .unwrap()
                .id,
            "supersedes",
            &sg_provenance::find_node(&s, node_type::REQUIREMENT_REVISION, &first.revision.id)
                .unwrap()
                .unwrap()
                .id,
            "",
        )
        .unwrap();
        assert!(supersede.is_some());
    }

    #[test]
    fn backfill_is_unverified_and_idempotent() {
        let s = setup();
        let wi = crate::create(&s, "pj", "历史任务", "旧需求描述", None, &[]).unwrap();
        docs::save(
            &s,
            &wi.id,
            "requirement.md",
            "# 历史需求\n\n- 支持单点登录\n",
        )
        .unwrap();
        let count = backfill_legacy(&s).unwrap();
        assert_eq!(count, 1);
        // 幂等：已有文档的 WorkItem 不重复回填。
        assert_eq!(backfill_legacy(&s).unwrap(), 0);
        let revs = revisions(&s, &wi.id).unwrap();
        assert_eq!(revs.len(), 1);
        assert_eq!(revs[0].0.source_kind, "legacy_import");
        assert_eq!(revs[0].1.len(), 1);
        let node = sg_provenance::find_node(&s, node_type::REQUIREMENT_REVISION, &revs[0].1[0].id)
            .unwrap()
            .unwrap();
        assert_eq!(node.verification_state, "unverified");
        // 无 docs 的 WorkItem 也生成 synthetic 修订。
        let bare = crate::create(&s, "pj", "无文档任务", "描述兜底", None, &[]).unwrap();
        backfill_legacy(&s).unwrap();
        let bare_revs = revisions(&s, &bare.id).unwrap();
        assert_eq!(bare_revs.len(), 1);
        assert_eq!(bare_revs[0].1[0].revision_no, 1);
    }
}
