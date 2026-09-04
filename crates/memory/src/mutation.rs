//! 写路径：手工 CRUD 与生命周期转换（方案 §8.1）。
//! 执行顺序固定：校验 → Secret scan → objects::put → BEGIN IMMEDIATE →
//! receipt 幂等 → CAS → revision/refs/FTS → audit+outbox+receipt 同事务提交。
//! 注意：事务闭包内禁止调用 store.with_conn/objects::*（Mutex 重入死锁），
//! 一切读对象/读旧值的操作在开事务前完成，事务内只操作 tx。
use rusqlite::Connection;
use serde_json::{json, Value};

use sg_store::{objects, scan, PutOptions, Store};

use crate::model::{
    byte_len, derive_slug, derive_summary, fingerprint, merr, normalize_tags, sha256_hex,
    subject_key, CreateInput, DuplicateMode, UpdateInput, KINDS, RELATIONS, SOURCE_KINDS,
};
use crate::repository;

const ACTOR_FALLBACK: &str = "local";

pub(crate) struct ReceiptRow {
    pub(crate) fingerprint: String,
    pub(crate) result: Value,
}

pub(crate) fn receipt_get(
    tx: &Connection,
    key: &str,
) -> Result<Option<ReceiptRow>, sg_store::Error> {
    let mut stmt = tx.prepare(
        "SELECT request_fingerprint, result_json FROM memory_mutation_receipts
         WHERE idempotency_key = ?1",
    )?;
    let mut rows = stmt.query_map([key], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    if let Some(Some(first)) = rows.next().map(|r| r.ok()) {
        return Ok(Some(ReceiptRow {
            fingerprint: first.0,
            result: serde_json::from_str(&first.1).unwrap_or(Value::Null),
        }));
    }
    Ok(None)
}

pub(crate) fn receipt_put(
    tx: &Connection,
    key: &str,
    project_id: &str,
    operation: &str,
    target_id: &str,
    fingerprint: &str,
    result: &Value,
) -> Result<(), sg_store::Error> {
    tx.execute(
        "INSERT INTO memory_mutation_receipts(
            idempotency_key, project_id, operation, target_id, request_fingerprint, result_json, completed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            key,
            project_id,
            operation,
            target_id,
            fingerprint,
            result.to_string(),
            sg_store::timefmt::now()
        ],
    )?;
    Ok(())
}

/// 审计 detail 白名单（§12.1）：不含 title/body/query/绝对路径。
fn audit_detail(
    status: &str,
    kind: &str,
    revision_no: i64,
    bytes: i64,
    content_sha: &str,
    source_count: i64,
    tags: &[String],
) -> Value {
    json!({
        "status": status,
        "kind": kind,
        "revisionNo": revision_no,
        "bytes": bytes,
        "contentShaPrefix": content_sha.chars().take(12).collect::<String>(),
        "sourceCount": source_count,
        "tagsCount": tags.len(),
    })
}

fn audit(
    tx: &Connection,
    actor: &str,
    action: &str,
    target_id: &str,
    detail: Value,
) -> Result<(), sg_store::Error> {
    sg_store::audit::append_at(tx, actor, action, "memory", target_id, detail)?;
    Ok(())
}

fn emit(
    tx: &Connection,
    event_type: &str,
    memory_id: &str,
    payload: Value,
) -> Result<(), sg_store::Error> {
    sg_store::outbox::emit_at(tx, "memory", memory_id, event_type, payload)?;
    Ok(())
}

fn validate_source_refs(refs: &[crate::model::SourceRefInput]) -> Result<(), sg_store::Error> {
    for r in refs {
        if !SOURCE_KINDS.contains(&r.source_kind.as_str()) {
            return Err(merr(
                crate::model::err_tokens::INVALID_STATE,
                format!("非法来源类型 {}", r.source_kind),
            ));
        }
        if !RELATIONS.contains(&r.relation.as_str()) {
            return Err(merr(
                crate::model::err_tokens::INVALID_STATE,
                format!("非法来源关系 {}", r.relation),
            ));
        }
    }
    Ok(())
}

/// Secret 高风险扫描（fail-closed；不提供普通放行开关，§10.1）。
fn require_clean_body(body: &str) -> Result<(), sg_store::Error> {
    let findings = scan::scan(body.as_bytes());
    if scan::has_high_risk(&findings) {
        return Err(merr(
            crate::model::err_tokens::SECRET,
            "正文命中高风险秘密，已拒绝写入",
        ));
    }
    Ok(())
}

/// 项目状态门：存在 + 未归档（MEM-027）。仅接收 &Connection，可在事务内使用。
fn require_project_writable(conn: &Connection, project_id: &str) -> Result<(), sg_store::Error> {
    repository::require_project(conn, project_id)?;
    if repository::project_archived(conn, project_id)? {
        return Err(merr(
            crate::model::err_tokens::DISABLED,
            "项目已归档，记忆写操作不可用",
        ));
    }
    Ok(())
}

fn unique_slug(conn: &Connection, project_id: &str, base: &str) -> Result<String, sg_store::Error> {
    let mut candidate = base.to_string();
    let mut n = 1;
    loop {
        let taken: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_entries WHERE project_id = ?1 AND slug = ?2",
            rusqlite::params![project_id, candidate],
            |r| r.get(0),
        )?;
        if taken == 0 {
            return Ok(candidate);
        }
        n += 1;
        candidate = format!("{base}-{n}");
    }
}

/// 幂等收据命中：同 key 同 fingerprint 返回原结果；异内容 MEMORY_CONFLICT（MEM-005/006）。
pub(crate) fn check_receipt(
    tx: &Connection,
    key: &str,
    fingerprint: &str,
) -> Result<Option<Value>, sg_store::Error> {
    if let Some(row) = receipt_get(tx, key)? {
        if row.fingerprint == fingerprint {
            return Ok(Some(row.result));
        }
        return Err(merr(
            crate::model::err_tokens::CONFLICT,
            "相同幂等键对应不同请求内容",
        ));
    }
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn insert_revision_and_refs(
    tx: &Connection,
    memory_id: &str,
    revision_id: &str,
    revision_no: i64,
    title: &str,
    summary: &str,
    object_sha: &str,
    content_sha: &str,
    tags: &[String],
    source_type: &str,
    actor: &str,
    idempotency_key: &str,
    request_fingerprint: &str,
    source_refs: &[crate::model::SourceRefInput],
) -> Result<(), sg_store::Error> {
    tx.execute(
        "INSERT INTO memory_revisions(
            id, memory_id, revision_no, title, summary, object_sha256, content_sha256,
            tags_json, source_type, author_kind, author_id, idempotency_key,
            request_fingerprint, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'user', ?10, ?11, ?12, ?13)",
        rusqlite::params![
            revision_id,
            memory_id,
            revision_no,
            title,
            summary,
            object_sha,
            content_sha,
            serde_json::to_string(&tags).unwrap_or_else(|_| "[]".into()),
            source_type,
            actor,
            idempotency_key,
            request_fingerprint,
            sg_store::timefmt::now()
        ],
    )?;
    let mut refs = source_refs.to_vec();
    if refs.is_empty() {
        // 无来源 active 不允许（MEM-018）：手工创建补 manual ref。
        refs.push(crate::model::SourceRefInput {
            source_kind: "manual".into(),
            source_id: actor.to_string(),
            locator: "S12 项目记忆页手工创建".into(),
            source_digest: String::new(),
            relation: "derived_from".into(),
        });
    }
    for (i, r) in refs.iter().enumerate() {
        tx.execute(
            "INSERT INTO memory_source_refs(
                revision_id, ordinal, project_id, source_kind, source_id, locator, source_digest, relation
             ) SELECT ?1, ?2, e.project_id, ?3, ?4, ?5, ?6, ?7 FROM memory_entries e WHERE e.id = ?8",
            rusqlite::params![
                revision_id,
                i as i64,
                r.source_kind,
                r.source_id,
                r.locator,
                r.source_digest,
                r.relation,
                memory_id
            ],
        )?;
    }
    Ok(())
}

fn sync_fts(
    tx: &Connection,
    memory_id: &str,
    project_id: &str,
    revision_id: &str,
    title: &str,
    tags: &[String],
    body: &str,
) -> Result<(), sg_store::Error> {
    tx.execute("DELETE FROM memory_fts WHERE memory_id = ?1", [memory_id])?;
    tx.execute(
        "INSERT INTO memory_fts(memory_id, revision_id, project_id, title, tags, body)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            memory_id,
            revision_id,
            project_id,
            title,
            tags.join(" "),
            body
        ],
    )?;
    Ok(())
}

/// 与 active/conflicted 同主题的条目：同内容 → duplicate 信息；异内容 → 冲突集合（§8.3）。
fn subject_conflicts(
    tx: &Connection,
    project_id: &str,
    subj: &str,
    content_sha: &str,
    exclude_id: Option<&str>,
) -> Result<(Option<String>, Vec<String>), sg_store::Error> {
    let mut stmt = tx.prepare(
        "SELECT e.id, r.content_sha256 FROM memory_entries e
         JOIN memory_revisions r ON r.id = e.current_revision_id
         WHERE e.project_id = ?1 AND e.subject_key = ?2 AND e.status IN ('active','conflicted')
           AND (?3 = '' OR e.id != ?3)",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![project_id, subj, exclude_id.unwrap_or("")],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    )?;
    let mut duplicate_of = None;
    let mut conflicting = Vec::new();
    for row in rows {
        let (id, sha): (String, String) = row?;
        if sha == content_sha && duplicate_of.is_none() {
            duplicate_of = Some(id);
        } else {
            conflicting.push(id);
        }
    }
    Ok((duplicate_of, conflicting))
}

/// 创建（手工 → active；导入 → proposed）。返回 {memoryId, revisionId, revisionNo, status}。
pub fn create(store: &Store, input: &CreateInput) -> Result<Value, sg_store::Error> {
    if !KINDS.contains(&input.kind.as_str()) {
        return Err(merr(
            crate::model::err_tokens::INVALID_STATE,
            format!("非法记忆类型 {}", input.kind),
        ));
    }
    if input.title.trim().is_empty() || input.body.trim().is_empty() {
        return Err(merr(
            crate::model::err_tokens::INVALID_STATE,
            "标题与正文不得为空",
        ));
    }
    validate_source_refs(&input.source_refs)?;

    let settings = repository::settings_get(store, &input.project_id)?;
    if byte_len(&input.body) > settings.max_bytes {
        return Err(merr(
            crate::model::err_tokens::QUOTA,
            format!(
                "正文 {} 字节超过单条上限 {} 字节",
                byte_len(&input.body),
                settings.max_bytes
            ),
        ));
    }
    require_clean_body(&input.body)?;

    let tags = normalize_tags(&input.tags);
    let summary = derive_summary(&input.body, input.summary.as_deref());
    let subj = subject_key(&input.title);
    let content_sha = sha256_hex(input.body.as_bytes());
    let fp = fingerprint(&json!({
        "body": input.body,
        "kind": input.kind,
        "operation": "memory.create",
        "projectId": input.project_id,
        "sourceRefs": input.source_refs,
        "status": input.target_status,
        "summary": summary,
        "tags": tags,
        "title": input.title.trim(),
    }));

    // 对象先写（§8.1）；DB 事务失败由 ref-scan GC 补偿孤儿对象。
    let mut body_reader = input.body.as_bytes();
    let info = objects::put(
        store,
        &mut body_reader,
        PutOptions {
            max_bytes: settings.max_bytes,
            allow_secrets: false,
        },
    )?;

    let actor = if input.actor.trim().is_empty() {
        ACTOR_FALLBACK
    } else {
        input.actor.trim()
    };
    let memory_id = sg_store::ids::new_id("mem");
    let revision_id = sg_store::ids::new_id("memr");

    store.with_tx_immediate(|tx| {
        require_project_writable(tx, &input.project_id)?;
        if let Some(previous) = check_receipt(tx, &input.idempotency_key, &fp)? {
            return Ok(previous);
        }

        let slug_base = derive_slug(&input.title);
        let slug = unique_slug(tx, &input.project_id, &slug_base)?;

        let (duplicate_of, conflicting) =
            subject_conflicts(tx, &input.project_id, &subj, &content_sha, None)?;
        if let Some(dup) = duplicate_of {
            return match input.on_duplicate {
                DuplicateMode::Reject => Err(merr(
                    crate::model::err_tokens::CONFLICT,
                    format!("同主题已存在相同内容（{dup}）"),
                )),
                DuplicateMode::Skip => Ok(json!({ "skippedDuplicateOf": dup })),
            };
        }
        let final_status: &str = if conflicting.is_empty() {
            input.target_status
        } else {
            "conflicted"
        };
        for id in &conflicting {
            tx.execute(
                "UPDATE memory_entries SET status='conflicted', updated_at=?2 WHERE id=?1",
                rusqlite::params![id, sg_store::timefmt::now()],
            )?;
        }

        let now = sg_store::timefmt::now();
        let (confirmed_at, confirmed_by): (Option<String>, String) = if final_status == "active" {
            (Some(now.clone()), actor.to_string())
        } else {
            (None, String::new())
        };
        tx.execute(
            "INSERT INTO memory_entries(
                id, project_id, slug, kind, subject_key, status, pinned, confirmed_at, confirmed_by, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, ?9, ?9)",
            rusqlite::params![
                memory_id,
                input.project_id,
                slug,
                input.kind,
                subj,
                final_status,
                confirmed_at,
                confirmed_by,
                now
            ],
        )?;
        insert_revision_and_refs(
            tx,
            &memory_id,
            &revision_id,
            1,
            input.title.trim(),
            &summary,
            &info.sha256,
            &content_sha,
            &tags,
            if input.source_refs.is_empty() { "manual" } else { input.source_refs[0].source_kind.as_str() },
            actor,
            &input.idempotency_key,
            &fp,
            &input.source_refs,
        )?;
        tx.execute(
            "UPDATE memory_entries SET current_revision_id = ?2 WHERE id = ?1",
            rusqlite::params![memory_id, revision_id],
        )?;
        sync_fts(tx, &memory_id, &input.project_id, &revision_id, input.title.trim(), &tags, &input.body)?;

        let detail = audit_detail(
            final_status,
            &input.kind,
            1,
            byte_len(&input.body),
            &content_sha,
            input.source_refs.len().max(1) as i64,
            &tags,
        );
        audit(tx, actor, "memory.create", &memory_id, detail)?;
        emit(
            tx,
            "memory.created",
            &memory_id,
            json!({"id": memory_id, "projectId": input.project_id, "kind": input.kind, "status": final_status, "bytes": byte_len(&input.body)}),
        )?;
        let result = json!({
            "memoryId": memory_id,
            "revisionId": revision_id,
            "revisionNo": 1,
            "status": final_status,
            "slug": slug,
        });
        receipt_put(tx, &input.idempotency_key, &input.project_id, "memory.create", &memory_id, &fp, &result)?;
        Ok(result)
    })
}

/// 当前修订快照：(revision_no, title, summary, object_sha, content_sha, tags_json)。
type CurrentRevision = (i64, String, String, Option<String>, String, String);

/// 当前修订读取（开事务前；避免事务内触碰 store）。
fn current_revision(store: &Store, revision_id: &str) -> Result<CurrentRevision, sg_store::Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT revision_no, title, summary, object_sha256, content_sha256, tags_json
             FROM memory_revisions WHERE id = ?1",
            [revision_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            },
        )
        .map_err(sg_store::Error::from)
    })
}

/// 更新：新 revision + 移动 current pointer（旧 revision 不可变，MEM-004）。
pub fn update(store: &Store, input: &UpdateInput) -> Result<Value, sg_store::Error> {
    let new_body_raw = input.body.clone().unwrap_or_default();
    if input
        .title
        .as_deref()
        .map(str::trim)
        .map(str::is_empty)
        .unwrap_or(false)
    {
        return Err(merr(
            crate::model::err_tokens::INVALID_STATE,
            "标题不得为空",
        ));
    }
    if !new_body_raw.is_empty() {
        require_clean_body(&new_body_raw)?;
    }
    let tags_override = input.tags.as_ref().map(|t| normalize_tags(t));

    let settings = repository::settings_get(store, &input.project_id)?;
    if !new_body_raw.is_empty() && byte_len(&new_body_raw) > settings.max_bytes {
        return Err(merr(
            crate::model::err_tokens::QUOTA,
            "正文超过单条字节上限",
        ));
    }

    // 开事务前读旧值并确定新内容（事务闭包内禁止触碰 store）。
    let entry_pre =
        store.with_conn(|conn| repository::entry_row(conn, &input.project_id, &input.memory_id))?;
    let current = current_revision(
        store,
        entry_pre.current_revision_id.as_deref().unwrap_or(""),
    )?;
    let new_title = input
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(String::from)
        .unwrap_or_else(|| current.1.clone());
    let new_body = if new_body_raw.is_empty() {
        // 正文未变更：读取旧正文以保持 FTS 完整。
        repository::read_body(store, current.3.as_deref().unwrap_or_default())?
    } else {
        new_body_raw.clone()
    };
    let new_summary = derive_summary(&new_body, input.summary.as_deref());
    let new_tags: Vec<String> =
        tags_override.unwrap_or_else(|| serde_json::from_str(&current.5).unwrap_or_default());
    let new_content_sha = sha256_hex(new_body.as_bytes());

    let fp = fingerprint(&json!({
        "expectedRevision": input.expected_revision,
        "memoryId": input.memory_id,
        "operation": "memory.update",
        "projectId": input.project_id,
        "summary": new_summary,
        "tags": new_tags,
        "title": new_title,
        "updateBody": new_body,
    }));

    // 正文变更时先写对象（失败无副作用；孤儿由 GC 清理）。
    let object_info = if new_body_raw.is_empty() {
        None
    } else {
        let mut reader = new_body_raw.as_bytes();
        Some(objects::put(
            store,
            &mut reader,
            PutOptions {
                max_bytes: settings.max_bytes,
                allow_secrets: false,
            },
        )?)
    };
    let new_object_sha = object_info
        .as_ref()
        .map(|o| o.sha256.clone())
        .unwrap_or_else(|| current.3.clone().unwrap_or_default());

    let actor = if input.actor.trim().is_empty() {
        ACTOR_FALLBACK
    } else {
        input.actor.trim()
    };
    let revision_id = sg_store::ids::new_id("memr");

    store.with_tx_immediate(|tx| {
        require_project_writable(tx, &input.project_id)?;
        if let Some(previous) = check_receipt(tx, &input.idempotency_key, &fp)? {
            return Ok(previous);
        }
        let entry = repository::entry_row(tx, &input.project_id, &input.memory_id)?;
        if !matches!(entry.status.as_str(), "proposed" | "active" | "conflicted") {
            return Err(merr(
                crate::model::err_tokens::INVALID_STATE,
                format!("状态 {} 不允许更新（归档条目请先恢复）", entry.status),
            ));
        }
        if entry.revision != input.expected_revision {
            return Err(merr(
                crate::model::err_tokens::CONFLICT,
                format!(
                    "revision 冲突：期望 {}，实际 {}",
                    input.expected_revision, entry.revision
                ),
            ));
        }
        // 同主题冲突再评估（§8.3）：更新本身不改变 conflicted 状态，裁决走 restore/archive。
        let (_dup, _conflicting) = subject_conflicts(
            tx,
            &input.project_id,
            &entry.subject_key,
            &new_content_sha,
            Some(&entry.id),
        )?;

        let next_no = current.0 + 1;
        insert_revision_and_refs(
            tx,
            &entry.id,
            &revision_id,
            next_no,
            &new_title,
            &new_summary,
            &new_object_sha,
            &new_content_sha,
            &new_tags,
            "manual",
            actor,
            &input.idempotency_key,
            &fp,
            &[],
        )?;
        let changed = tx.execute(
            "UPDATE memory_entries SET current_revision_id = ?2, revision = revision + 1, updated_at = ?3
             WHERE id = ?1 AND revision = ?4",
            rusqlite::params![entry.id, revision_id, sg_store::timefmt::now(), input.expected_revision],
        )?;
        if changed == 0 {
            return Err(merr(
                crate::model::err_tokens::CONFLICT,
                "revision CAS 更新未命中",
            ));
        }
        sync_fts(tx, &entry.id, &entry.project_id, &revision_id, &new_title, &new_tags, &new_body)?;

        audit(
            tx,
            actor,
            "memory.update",
            &entry.id,
            audit_detail(&entry.status, &entry.kind, next_no, byte_len(&new_body), &new_content_sha, 1, &new_tags),
        )?;
        emit(
            tx,
            "memory.updated",
            &entry.id,
            json!({"id": entry.id, "projectId": entry.project_id, "revisionNo": next_no, "bytes": byte_len(&new_body)}),
        )?;
        let result = json!({
            "memoryId": entry.id,
            "revisionId": revision_id,
            "revisionNo": next_no,
            "status": entry.status,
        });
        receipt_put(tx, &input.idempotency_key, &input.project_id, "memory.update", &entry.id, &fp, &result)?;
        Ok(result)
    })
}

/// 置顶/取消置顶：仅影响排序权重（MEM-007），metadata revision +1。
pub fn pin(
    store: &Store,
    project_id: &str,
    memory_id: &str,
    pinned: bool,
    expected_revision: i64,
    actor: &str,
    idempotency_key: &str,
) -> Result<Value, sg_store::Error> {
    let fp = fingerprint(&json!({
        "expectedRevision": expected_revision,
        "memoryId": memory_id,
        "operation": "memory.pin",
        "pinned": pinned,
        "projectId": project_id,
    }));
    let actor = if actor.trim().is_empty() {
        ACTOR_FALLBACK
    } else {
        actor.trim()
    };
    store.with_tx_immediate(|tx| {
        require_project_writable(tx, project_id)?;
        if let Some(previous) = check_receipt(tx, idempotency_key, &fp)? {
            return Ok(previous);
        }
        let entry = repository::entry_row(tx, project_id, memory_id)?;
        if entry.revision != expected_revision {
            return Err(merr(crate::model::err_tokens::CONFLICT, "revision 冲突"));
        }
        if entry.status == "purged" {
            return Err(merr(
                crate::model::err_tokens::INVALID_STATE,
                "已清除条目不可置顶",
            ));
        }
        let changed = tx.execute(
            "UPDATE memory_entries SET pinned = ?2, revision = revision + 1, updated_at = ?3
             WHERE id = ?1 AND revision = ?4",
            rusqlite::params![
                entry.id,
                pinned as i64,
                sg_store::timefmt::now(),
                expected_revision
            ],
        )?;
        if changed == 0 {
            return Err(merr(
                crate::model::err_tokens::CONFLICT,
                "revision CAS 更新未命中",
            ));
        }
        audit(
            tx,
            actor,
            "memory.pin",
            &entry.id,
            json!({"pinned": pinned, "revision": expected_revision + 1}),
        )?;
        emit(
            tx,
            "memory.pinned",
            &entry.id,
            json!({"id": entry.id, "projectId": project_id, "pinned": pinned}),
        )?;
        let result =
            json!({"memoryId": entry.id, "pinned": pinned, "revision": expected_revision + 1});
        receipt_put(
            tx,
            idempotency_key,
            project_id,
            "memory.pin",
            &entry.id,
            &fp,
            &result,
        )?;
        Ok(result)
    })
}

/// 生命周期转换公共体（archive/restore）。
#[allow(clippy::too_many_arguments)]
fn transition(
    store: &Store,
    project_id: &str,
    memory_id: &str,
    expected_revision: i64,
    actor: &str,
    idempotency_key: &str,
    operation: &str,
    event: &str,
) -> Result<Value, sg_store::Error> {
    let fp = fingerprint(&json!({
        "expectedRevision": expected_revision,
        "memoryId": memory_id,
        "operation": operation,
        "projectId": project_id,
    }));
    let actor = if actor.trim().is_empty() {
        ACTOR_FALLBACK
    } else {
        actor.trim()
    };
    store.with_tx_immediate(|tx| {
        require_project_writable(tx, project_id)?;
        if let Some(previous) = check_receipt(tx, idempotency_key, &fp)? {
            return Ok(previous);
        }
        let entry = repository::entry_row(tx, project_id, memory_id)?;
        if entry.revision != expected_revision {
            return Err(merr(
                crate::model::err_tokens::CONFLICT,
                format!(
                    "revision 冲突：期望 {}，实际 {}",
                    expected_revision, entry.revision
                ),
            ));
        }
        let next_status: &str = if operation == "memory.archive" {
            match entry.status.as_str() {
                "proposed" | "active" | "conflicted" => "archived",
                other => {
                    return Err(merr(
                        crate::model::err_tokens::INVALID_STATE,
                        format!("状态 {other} 不允许归档"),
                    ))
                }
            }
        } else {
            match entry.status.as_str() {
                "archived" | "conflicted" => {
                    // 恢复时重评同主题冲突（§8.3：用户裁决后回到 active/conflicted）。
                    let sha = entry
                        .current_revision_id
                        .as_deref()
                        .map(|rev_id| {
                            tx.query_row(
                                "SELECT content_sha256 FROM memory_revisions WHERE id = ?1",
                                [rev_id],
                                |r| r.get::<_, String>(0),
                            )
                            .unwrap_or_default()
                        })
                        .unwrap_or_default();
                    let (_dup, conflicting) = subject_conflicts(
                        tx,
                        project_id,
                        &entry.subject_key,
                        &sha,
                        Some(&entry.id),
                    )?;
                    if conflicting.is_empty() {
                        "active"
                    } else {
                        "conflicted"
                    }
                }
                other => {
                    return Err(merr(
                        crate::model::err_tokens::INVALID_STATE,
                        format!("状态 {other} 不允许恢复"),
                    ))
                }
            }
        };

        let now = sg_store::timefmt::now();
        let changed = if operation == "memory.archive" {
            tx.execute(
                "UPDATE memory_entries SET status=?2, archived_at=?3, revision=revision+1, updated_at=?3
                 WHERE id=?1 AND revision=?4",
                rusqlite::params![entry.id, next_status, now, expected_revision],
            )?
        } else {
            tx.execute(
                "UPDATE memory_entries SET status=?2, archived_at=NULL,
                        confirmed_at=COALESCE(NULLIF(confirmed_at,''),?3),
                        revision=revision+1, updated_at=?3
                 WHERE id=?1 AND revision=?4",
                rusqlite::params![entry.id, next_status, now, expected_revision],
            )?
        };
        if changed == 0 {
            return Err(merr(crate::model::err_tokens::CONFLICT, "revision CAS 更新未命中"));
        }

        audit(
            tx,
            actor,
            operation,
            &entry.id,
            json!({"from": entry.status, "to": next_status, "revision": expected_revision + 1}),
        )?;
        emit(
            tx,
            event,
            &entry.id,
            json!({"id": entry.id, "projectId": project_id, "status": next_status}),
        )?;
        let result = json!({"memoryId": entry.id, "status": next_status, "revision": expected_revision + 1});
        receipt_put(tx, idempotency_key, project_id, operation, &entry.id, &fp, &result)?;
        Ok(result)
    })
}

pub fn archive(
    store: &Store,
    project_id: &str,
    memory_id: &str,
    expected_revision: i64,
    actor: &str,
    idempotency_key: &str,
) -> Result<Value, sg_store::Error> {
    transition(
        store,
        project_id,
        memory_id,
        expected_revision,
        actor,
        idempotency_key,
        "memory.archive",
        "memory.archived",
    )
}

pub fn restore(
    store: &Store,
    project_id: &str,
    memory_id: &str,
    expected_revision: i64,
    actor: &str,
    idempotency_key: &str,
) -> Result<Value, sg_store::Error> {
    transition(
        store,
        project_id,
        memory_id,
        expected_revision,
        actor,
        idempotency_key,
        "memory.restore",
        "memory.restored",
    )
}
