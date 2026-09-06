//! Durable Trace span（EvoFlow 方案 M5-02 / ADR-039）：
//! workflow → plan → task → run → model/gateway/tool/middleware 层级；
//! span 只存结构化元数据与 digest（attrs sha256），不存 prompt/secret/reasoning 正文。
//! 事实只追加：UI 断线后可从 spans + 既有 durable 表完整重建调用链。

use serde::Serialize;
use sg_store::{ids, Error, Store};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct SpanInput<'a> {
    pub trace_id: &'a str,
    pub parent_span_id: Option<&'a str>,
    pub kind: &'a str,
    pub workitem_id: &'a str,
    pub plan_revision_id: Option<&'a str>,
    pub task_attempt_id: Option<&'a str>,
    pub run_id: Option<&'a str>,
    pub name: &'a str,
    pub status: &'a str,
    pub attrs: &'a serde_json::Value,
    pub started_at: &'a str,
    pub finished_at: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpanRecord {
    pub id: String,
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub kind: String,
    pub name: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
}

const KINDS: [&str; 8] = [
    "workflow",
    "plan",
    "task",
    "run",
    "model",
    "gateway",
    "tool",
    "middleware",
];

/// 追加 span（attrs 只留 digest；正文不落库）。
pub fn emit_span(store: &Store, input: SpanInput) -> Result<String, Error> {
    if !KINDS.contains(&input.kind) {
        return Err(Error::Message(format!(
            "trace_span_invalid: 非法 kind {}",
            input.kind
        )));
    }
    let span_id = ids::new_id("spn");
    let attrs_json = serde_json::to_string(input.attrs).unwrap_or_default();
    let attrs_sha = ids::hex(&Sha256::digest(attrs_json.as_bytes()));
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO trace_spans(id, trace_id, span_id, parent_span_id, kind, workitem_id,
                plan_revision_id, task_attempt_id, run_id, name, status, attrs_json, attrs_sha256,
                started_at, finished_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            rusqlite::params![
                ids::new_id("tsp"),
                input.trace_id,
                span_id,
                input.parent_span_id,
                input.kind,
                input.workitem_id,
                input.plan_revision_id,
                input.task_attempt_id,
                input.run_id,
                input.name,
                input.status,
                attrs_json,
                attrs_sha,
                input.started_at,
                input.finished_at,
            ],
        )?;
        Ok(())
    })?;
    Ok(span_id)
}

/// 按 workitem 取调用链（时间序；span 图重建面）。
pub fn spans_for_workitem(store: &Store, workitem_id: &str) -> Result<Vec<SpanRecord>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, trace_id, span_id, parent_span_id, kind, name, status, started_at, finished_at
             FROM trace_spans WHERE workitem_id=?1 ORDER BY started_at, id",
        )?;
        let rows = stmt.query_map([workitem_id], |r| {
            Ok(SpanRecord {
                id: r.get(0)?,
                trace_id: r.get(1)?,
                span_id: r.get(2)?,
                parent_span_id: r.get(3)?,
                kind: r.get(4)?,
                name: r.get(5)?,
                status: r.get(6)?,
                started_at: r.get(7)?,
                finished_at: r.get(8)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-trace-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        store
            .with_conn(|c| {
                c.execute_batch(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj','u','n','p','main','t');
                    INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                     VALUES ('wi','pj','t','','[]','requirements','t','t');",
                )
                .map_err(Error::from)?;
                Ok(())
            })
            .unwrap();
        store
    }

    #[test]
    fn spans_append_and_rebuild_chain() {
        let store = setup();
        let root = emit_span(
            &store,
            SpanInput {
                trace_id: "tr1",
                parent_span_id: None,
                kind: "workflow",
                workitem_id: "wi",
                plan_revision_id: None,
                task_attempt_id: None,
                run_id: None,
                name: "plan.start",
                status: "ok",
                attrs: &serde_json::json!({"planRevisionId": "pr1"}),
                started_at: "t1",
                finished_at: Some("t2"),
            },
        )
        .unwrap();
        let child = emit_span(
            &store,
            SpanInput {
                trace_id: "tr1",
                parent_span_id: Some(&root),
                kind: "task",
                workitem_id: "wi",
                plan_revision_id: None,
                task_attempt_id: None,
                run_id: None,
                name: "task.succeeded",
                status: "ok",
                attrs: &serde_json::json!({"outputDigest": "od"}),
                started_at: "t2",
                finished_at: Some("t3"),
            },
        )
        .unwrap();
        let spans = spans_for_workitem(&store, "wi").unwrap();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].span_id, root);
        assert_eq!(spans[1].parent_span_id.as_deref(), Some(root.as_str()));
        assert_eq!(spans[1].span_id, child);
        // 非法 kind 拒绝。
        assert!(emit_span(
            &store,
            SpanInput {
                trace_id: "tr1",
                parent_span_id: None,
                kind: "quantum",
                workitem_id: "wi",
                plan_revision_id: None,
                task_attempt_id: None,
                run_id: None,
                name: "x",
                status: "ok",
                attrs: &serde_json::json!({}),
                started_at: "t",
                finished_at: None,
            },
        )
        .is_err());
    }
}
