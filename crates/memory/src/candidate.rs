//! 候选裁决（方案 §13 M4 / MEM-020）：接受可编辑并转 active；拒绝永不注入、不再推荐。
use serde_json::{json, Value};

use sg_store::Store;

use crate::model::{merr, sha256_hex, CreateInput, DuplicateMode, SourceRefInput};
use crate::{mutation, repository};

/// 待确认/已裁决列表（项目作用域；默认 pending 优先）。
pub fn list(
    store: &Store,
    project_id: &str,
    status: Option<&str>,
    limit: i64,
) -> Result<Value, sg_store::Error> {
    let limit = limit.clamp(1, 200);
    let (project_id_owned, collected) = store.with_conn(|conn| {
        repository::require_project(conn, project_id)?;
        let status_filter = match status.filter(|s| !s.is_empty()) {
            Some(s) => {
                if !matches!(s, "pending" | "accepted" | "rejected") {
                    return Err(merr(
                        crate::model::err_tokens::INVALID_STATE,
                        format!("非法候选状态 {s}"),
                    ));
                }
                s.to_string()
            }
            None => "pending".to_string(),
        };
        let sql = format!(
            "SELECT c.id, c.job_id, c.kind, c.title, c.summary, c.status, c.created_at,
                    c.decision_at, c.accepted_memory_id, j.run_id, c.object_sha256
             FROM memory_candidates c
             JOIN memory_capture_jobs j ON j.id = c.job_id
             WHERE c.project_id = ?1 AND c.status = ?2
             ORDER BY c.created_at DESC LIMIT {limit}"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![project_id, status_filter], |r| {
            Ok((candidate_row(r)?, r.get::<_, String>(10)?))
        })?;
        let mut collected = Vec::new();
        for row in rows {
            collected.push(row?);
        }
        Ok((project_id.to_string(), collected))
    })?;
    // 正文在连接外逐条读取（接受前可编辑；连接互斥不可重入）。
    let mut items = Vec::new();
    for (mut item, object_sha) in collected {
        if !object_sha.is_empty() {
            match repository::read_body(store, &object_sha) {
                Ok(body) => {
                    item["body"] = json!(body);
                }
                Err(e) => {
                    item["bodyError"] = json!(e.to_string());
                }
            }
        }
        items.push(item);
    }
    Ok(json!({"projectId": project_id_owned, "items": items, "cursor": Value::Null}))
}

fn candidate_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "candidateId": r.get::<_, String>(0)?,
        "jobId": r.get::<_, String>(1)?,
        "kind": r.get::<_, String>(2)?,
        "title": r.get::<_, String>(3)?,
        "summary": r.get::<_, String>(4)?,
        "status": r.get::<_, String>(5)?,
        "createdAt": r.get::<_, String>(6)?,
        "decisionAt": r.get::<_, Option<String>>(7)?,
        "acceptedMemoryId": r.get::<_, Option<String>>(8)?,
        "runId": r.get::<_, String>(9)?,
    }))
}

/// 裁决：accept（可编辑内容）→ 正式 active entry；reject → 记录且不再推荐。
pub fn decide(
    store: &Store,
    project_id: &str,
    candidate_id: &str,
    decision: &str,
    edited_content: Option<&str>,
    edited_title: Option<&str>,
    idempotency_key: &str,
) -> Result<Value, sg_store::Error> {
    if !matches!(decision, "accept" | "reject") {
        return Err(merr(
            crate::model::err_tokens::INVALID_STATE,
            format!("非法裁决 {decision}（仅 accept/reject）"),
        ));
    }
    let candidate = store.with_conn(|conn| {
        repository::require_project(conn, project_id)?;
        conn.query_row(
            "SELECT c.id, c.job_id, c.kind, c.title, c.summary, c.status, c.object_sha256,
                    c.content_sha256, j.run_id
             FROM memory_candidates c
             JOIN memory_capture_jobs j ON j.id = c.job_id
             WHERE c.id = ?1 AND c.project_id = ?2",
            rusqlite::params![candidate_id, project_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, String>(8)?,
                ))
            },
        )
        .map_err(|_| sg_store::Error::Message("not_found: candidate".into()))
    })?;
    let (_id, _job, kind, title, _summary, status, object_sha, content_sha, run_id) = candidate;

    let fp = crate::model::fingerprint(&serde_json::json!({
        "candidateId": candidate_id,
        "decision": decision,
        "editedBody": edited_content,
        "editedTitle": edited_title,
        "operation": "memory.candidateDecide",
        "projectId": project_id,
    }));

    // 幂等：同 key 同指纹重放。
    if let Some(previous) = store.with_conn(|conn| mutation::receipt_get(conn, idempotency_key))? {
        if previous.fingerprint == fp {
            return Ok(previous.result);
        }
        return Err(merr(
            crate::model::err_tokens::CONFLICT,
            "相同幂等键对应不同请求内容",
        ));
    }

    if status != "pending" {
        return Err(merr(
            crate::model::err_tokens::INVALID_STATE,
            format!("候选已裁决（{status}）"),
        ));
    }

    let now = sg_store::timefmt::now();
    let result = if decision == "reject" {
        store.with_tx_immediate(|tx| {
            tx.execute(
                "UPDATE memory_candidates SET status='rejected', decision_by='local', decision_at=?2
                 WHERE id=?1 AND status='pending'",
                rusqlite::params![candidate_id, now],
            )?;
            sg_store::audit::append_at(
                tx,
                "local",
                "memory.candidate.reject",
                "memory",
                candidate_id,
                json!({"candidateId": candidate_id, "runId": run_id}),
            )?;
            sg_store::outbox::emit_at(
                tx,
                "memory",
                candidate_id,
                "memory.candidate_rejected",
                json!({"candidateId": candidate_id, "projectId": project_id}),
            )?;
            Ok(json!({"candidateId": candidate_id, "status": "rejected"}))
        })
    } else {
        // 接受：正文 = 编辑内容或候选对象正文；写正式 active entry（用户确认即生效）。
        let body: String = match edited_content.filter(|c| !c.trim().is_empty()) {
            Some(c) => c.to_string(),
            None => {
                if object_sha.is_empty() {
                    return Err(merr(
                        crate::model::err_tokens::OBJECT_MISSING,
                        "候选缺少正文对象",
                    ));
                }
                repository::read_body(store, &object_sha)?
            }
        };
        let input = CreateInput {
            project_id: project_id.to_string(),
            title: edited_title
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(String::from)
                .unwrap_or(title),
            kind,
            body,
            summary: None,
            tags: vec![],
            source_refs: vec![SourceRefInput {
                source_kind: "run".into(),
                source_id: run_id.clone(),
                locator: format!("候选沉淀（job 幂等指纹 {content_sha} 前缀）"),
                source_digest: content_sha.clone(),
                relation: "derived_from".into(),
            }],
            target_status: "active",
            actor: "local".into(),
            // create 的收据用派生键，原始键留给 candidateDecide 收据。
            idempotency_key: format!("{idempotency_key}::accept"),
            on_duplicate: DuplicateMode::Reject,
        };
        let created = mutation::create(store, &input)?;
        let memory_id = created["memoryId"].as_str().unwrap_or_default().to_string();
        store.with_tx_immediate(|tx| {
            let changed = tx.execute(
                "UPDATE memory_candidates SET status='accepted', decision_by='local', decision_at=?2,
                        accepted_memory_id=?3
                 WHERE id=?1 AND status='pending'",
                rusqlite::params![candidate_id, now, memory_id],
            )?;
            if changed == 0 {
                return Err(merr(
                    crate::model::err_tokens::INVALID_STATE,
                    "候选已裁决",
                ));
            }
            sg_store::audit::append_at(
                tx,
                "local",
                "memory.candidate.accept",
                "memory",
                candidate_id,
                json!({"candidateId": candidate_id, "memoryId": memory_id, "runId": run_id}),
            )?;
            sg_store::outbox::emit_at(
                tx,
                "memory",
                candidate_id,
                "memory.candidate_accepted",
                json!({"candidateId": candidate_id, "projectId": project_id, "memoryId": memory_id}),
            )?;
            Ok(json!({"candidateId": candidate_id, "status": "accepted", "memoryId": memory_id, "entryStatus": created["status"].clone()}))
        })
    }?;

    // 收据（裁决幂等）。
    store.with_conn(|conn| {
        mutation::receipt_put(
            conn,
            idempotency_key,
            project_id,
            "memory.candidateDecide",
            candidate_id,
            &crate::model::fingerprint(&serde_json::json!({
                "candidateId": candidate_id, "decision": decision, "operation": "memory.candidateDecide",
            })),
            &result,
        )
    })?;
    Ok(result)
}

/// 允许的候选状态集合（decoder 侧一致性辅助）。
pub const CANDIDATE_STATUSES: &[&str] = &["pending", "accepted", "rejected"];

/// 内容哈希（供测试与去重校验）。
pub fn content_hash(body: &str) -> String {
    sha256_hex(body.as_bytes())
}
