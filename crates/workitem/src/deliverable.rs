//! 关卡交付物门禁（§交付物）：每关绑定工件类型，无冻结基线不可进入审批。
//! 内建映射与前端 KINDS 对齐：requirements↔prd、design↔tech_design、development↔code、
//! testing↔test、deployment↔deployment、verification↔verification。
use serde_json::{json, Value};

use crate::Gate;

/// 每关要求的交付物工件类型（内建映射）。
pub fn required_kind(gate: Gate) -> &'static str {
    match gate {
        Gate::Requirements => "prd",
        Gate::Design => "tech_design",
        Gate::Development => "code",
        Gate::Testing => "test",
        Gate::Deployment => "deployment",
        Gate::Verification => "verification",
    }
}

/// M1-06：交付物 kind 按实例定义解析（GateDefinition.deliverables 首项）；
/// 无实例/未声明时回退内建六关映射。自定义模板关卡不再受六值假设限制。
pub fn required_kind_for(
    store: &sg_store::Store,
    workitem_id: &str,
    gate_id: &str,
) -> Result<String, sg_store::Error> {
    if let Some(gates) = sg_workflow::instance::gates_for_workitem(store, workitem_id)? {
        if let Some(g) = gates.iter().find(|g| g.gate_id == gate_id) {
            if let Some(kind) = g.deliverables.first() {
                return Ok(kind.clone());
            }
        }
        if !gates.iter().any(|g| g.gate_id == gate_id) {
            return Err(sg_store::Error::Message(format!(
                "workflow_template_invalid: 关卡 {gate_id} 不在实例定义中"
            )));
        }
    }
    Gate::parse(gate_id)
        .map(|g| required_kind(g).to_string())
        .ok_or_else(|| {
            sg_store::Error::Message(format!(
                "workflow_template_invalid: 关卡 {gate_id} 无交付物定义"
            ))
        })
}

/// 交付物满足状态（供 request_release 前置与 gate.deliverableStatus RPC 复用）。
/// 满足 = 工件存在 + 最新有效修订已冻结进本关当前基线 + 正文非空（size>0）。
pub fn status(
    store: &sg_store::Store,
    workitem_id: &str,
    gate_id: &str,
) -> Result<Value, sg_store::Error> {
    let kind = required_kind_for(store, workitem_id, gate_id)?;
    let mut out = json!({
        "workItemId": workitem_id,
        "gate": gate_id,
        "requiredKind": kind,
        "satisfied": false,
        "missing": "artifact_absent",
        "artifactId": Value::Null,
        "artifactTitle": Value::Null,
        "revisionId": Value::Null,
        "revisionNo": Value::Null,
        "baselineId": Value::Null,
    });

    let artifact: Option<(String, String)> = store.with_conn(|conn| {
        Ok(conn
            .query_row(
                "SELECT id, title FROM artifacts WHERE workitem_id=?1 AND kind=?2 ORDER BY created_at DESC LIMIT 1",
                rusqlite::params![workitem_id, kind],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .ok())
    })?;
    let Some((artifact_id, title)) = artifact else {
        return Ok(out);
    };
    out["artifactId"] = json!(artifact_id);
    out["artifactTitle"] = json!(title);
    out["missing"] = json!("revision_absent");

    let revision: Option<(String, i64, i64)> = store.with_conn(|conn| {
        Ok(conn
            .query_row(
                "SELECT id, rev_no, size FROM revisions WHERE artifact_id=?1 AND status <> 'superseded' ORDER BY rev_no DESC LIMIT 1",
                [&artifact_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)),
            )
            .ok())
    })?;
    let Some((revision_id, rev_no, size)) = revision else {
        return Ok(out);
    };
    out["revisionId"] = json!(revision_id);
    out["revisionNo"] = json!(rev_no);

    let baseline: Option<(String, String)> = store.with_conn(|conn| {
        Ok(conn
            .query_row(
                "SELECT id, revision_map FROM baselines WHERE workitem_id=?1 AND gate=?2 AND superseded_by IS NULL ORDER BY frozen_at DESC LIMIT 1",
                rusqlite::params![workitem_id, gate_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .ok())
    })?;
    let Some((baseline_id, revision_map)) = baseline else {
        out["missing"] = json!("not_frozen");
        return Ok(out);
    };
    out["baselineId"] = json!(baseline_id);
    let map: Value = serde_json::from_str(&revision_map).unwrap_or(Value::Null);
    let Some(frozen_rev) = map
        .get(&artifact_id)
        .and_then(|v| v.as_str())
        .map(String::from)
    else {
        out["missing"] = json!("not_frozen");
        return Ok(out);
    };
    if frozen_rev != revision_id {
        // 最新有效修订尚未冻结（冻结后又产生了新草稿）。
        out["missing"] = json!("not_frozen");
        return Ok(out);
    }
    if size <= 0 {
        out["missing"] = json!("empty_content");
        return Ok(out);
    }
    out["satisfied"] = json!(true);
    out["missing"] = Value::Null;
    Ok(out)
}

/// request_release 前置：不满足即 deliverable_missing（fail-closed）。
pub fn require_for_release(
    store: &sg_store::Store,
    workitem_id: &str,
    gate_id: &str,
) -> Result<Value, sg_store::Error> {
    let s = status(store, workitem_id, gate_id)?;
    if s["satisfied"] == json!(true) {
        return Ok(s);
    }
    let kind = required_kind_for(store, workitem_id, gate_id)?;
    Err(sg_store::Error::Message(format!(
        "deliverable_missing: {} 关缺少交付物 {}（{}）",
        gate_id,
        kind,
        s["missing"].as_str().unwrap_or("unknown"),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sg_store::{ids, timefmt, Store};

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-dlv-{}-{}",
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

    #[test]
    fn status_tracks_artifact_draft_and_frozen() {
        let store = setup();
        let wi = crate::create(&store, "pj", "交付物门禁", "", None, &[]).unwrap();
        let workitem_id = wi.id.as_str();
        let gate = Gate::Requirements;

        // 1) 无工件：artifact_absent。
        let s = status(&store, workitem_id, gate.as_str()).unwrap();
        assert_eq!(s["satisfied"], json!(false));
        assert_eq!(s["missing"], json!("artifact_absent"));

        // 2) 有工件有草稿：revision_absent → not_frozen。
        let art = sg_artifact::create_artifact(&store, workitem_id, "prd", "PRD").unwrap();
        let draft = sg_artifact::create_draft(&store, &art.id, "# PRD\n").unwrap();
        let s = status(&store, workitem_id, gate.as_str()).unwrap();
        assert_eq!(s["missing"], json!("not_frozen"));

        // 3) 冻结该草稿 → 满足（正文非空）。
        sg_artifact::add_review(&store, &draft.id, "r", "approved", "", None).unwrap();
        sg_artifact::freeze(
            &store,
            workitem_id,
            gate.as_str(),
            std::slice::from_ref(&draft.id),
            "",
            "",
        )
        .unwrap();
        let s = status(&store, workitem_id, gate.as_str()).unwrap();
        assert_eq!(s["satisfied"], json!(true));
        assert_eq!(s["revisionId"], json!(draft.id));
        assert!(s["baselineId"].as_str().unwrap().starts_with("base"));

        // 4) 冻结后新增草稿（未冻结）→ 退回 not_frozen。
        sg_artifact::create_draft(&store, &art.id, "# PRD v2\n").unwrap();
        let s = status(&store, workitem_id, gate.as_str()).unwrap();
        assert_eq!(s["missing"], json!("not_frozen"));
        assert_eq!(s["satisfied"], json!(false));

        // 5) require_for_release：缺失关报 deliverable_missing。
        let err = require_for_release(&store, workitem_id, "testing").unwrap_err();
        assert!(err.to_string().contains("deliverable_missing"), "{err}");
    }
}
