//! 关卡交付物门禁（§交付物）：每关绑定工件类型集合，无冻结基线不可进入审批。
//! 配置化（评审后续）：每关要求的所有 deliverable kind 都必须满足（工件存在 +
//! 最新有效修订冻结进本关基线 + 正文非空）；内建六关映射仅作无实例时的回退。
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

/// 每关要求的全部交付物 kind：实例定义 deliverables 全量（可多个）；
/// 无实例时回退内建六关映射（单值）。关不在实例定义中即报错（fail-closed）。
pub fn required_kinds_for(
    store: &sg_store::Store,
    workitem_id: &str,
    gate_id: &str,
) -> Result<Vec<String>, sg_store::Error> {
    if let Some(gates) = sg_workflow::instance::gates_for_workitem(store, workitem_id)? {
        if let Some(g) = gates.iter().find(|g| g.gate_id == gate_id) {
            return Ok(g.deliverables.clone());
        }
        return Err(sg_store::Error::Message(format!(
            "workflow_template_invalid: 关卡 {gate_id} 不在实例定义中"
        )));
    }
    Gate::parse(gate_id)
        .map(|g| vec![required_kind(g).to_string()])
        .ok_or_else(|| {
            sg_store::Error::Message(format!(
                "workflow_template_invalid: 关卡 {gate_id} 无交付物定义"
            ))
        })
}

/// 单 kind 满足状态：工件存在 + 最新有效修订已冻结进本关当前基线 + 正文非空。
fn kind_status(
    store: &sg_store::Store,
    workitem_id: &str,
    gate_id: &str,
    kind: &str,
) -> Result<Value, sg_store::Error> {
    let mut out = json!({
        "kind": kind,
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

/// 交付物满足状态（供 request_release 前置与 gate.deliverableStatus RPC 复用）。
/// 满足 = 本关声明的全部 kind 各自满足（工件 + 最新有效修订冻结进基线 + 正文非空）。
/// 顶层字段保持向后兼容（requiredKind/缺失身份字段 = 首 kind），新增
/// requiredKinds 全量与 entries 逐 kind 明细。
pub fn status(
    store: &sg_store::Store,
    workitem_id: &str,
    gate_id: &str,
) -> Result<Value, sg_store::Error> {
    let kinds = required_kinds_for(store, workitem_id, gate_id)?;
    let entries = kinds
        .iter()
        .map(|k| kind_status(store, workitem_id, gate_id, k))
        .collect::<Result<Vec<Value>, _>>()?;
    let satisfied = entries.iter().all(|e| e["satisfied"] == json!(true));
    let first_unsatisfied = entries.iter().find(|e| e["satisfied"] != json!(true));
    let missing = first_unsatisfied
        .map(|e| e["missing"].clone())
        .unwrap_or(Value::Null);
    let first = entries.first().cloned().unwrap_or(Value::Null);
    Ok(json!({
        "workItemId": workitem_id,
        "gate": gate_id,
        "requiredKind": kinds.first().cloned().unwrap_or_default(),
        "requiredKinds": kinds,
        "satisfied": satisfied,
        "missing": missing,
        "entries": entries,
        "artifactId": first["artifactId"].clone(),
        "artifactTitle": first["artifactTitle"].clone(),
        "revisionId": first["revisionId"].clone(),
        "revisionNo": first["revisionNo"].clone(),
        "baselineId": first["baselineId"].clone(),
    }))
}

/// request_release 前置：不满足即 deliverable_missing（fail-closed，
/// 指明第一个缺失的 kind 与原因）。
pub fn require_for_release(
    store: &sg_store::Store,
    workitem_id: &str,
    gate_id: &str,
) -> Result<Value, sg_store::Error> {
    let s = status(store, workitem_id, gate_id)?;
    if s["satisfied"] == json!(true) {
        return Ok(s);
    }
    let kinds: Vec<String> = serde_json::from_value(s["requiredKinds"].clone()).unwrap_or_default();
    let failing = s["entries"]
        .as_array()
        .and_then(|a| {
            a.iter()
                .find(|e| e["satisfied"] != json!(true))
                .and_then(|e| e["kind"].as_str().map(String::from))
        })
        .or_else(|| kinds.first().cloned())
        .unwrap_or_default();
    Err(sg_store::Error::Message(format!(
        "deliverable_missing: {} 关缺少交付物 {}（{}）",
        gate_id,
        failing,
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

    /// 配置化多交付物：实例声明多个 kind 时全部满足才放行，缺失者按名指明。
    #[test]
    fn multi_kind_deliverables_all_required() {
        let store = setup();
        let t =
            sg_workflow::template::create_template(&store, "multi-dlv", "多交付物模板").unwrap();
        let v = sg_workflow::template::create_version(
            &store,
            &t.id,
            &[sg_workflow::template::GateDefInput {
                gate_id: "build".into(),
                title: "构建关".into(),
                purpose: String::new(),
                deliverables: vec!["spec".into(), "patch".into()],
                acceptance: vec![],
                context_policy_ref: None,
                team_policy_ref: None,
                workspace_policy_ref: None,
            }],
            "tester",
        )
        .unwrap();
        sg_workflow::template::activate(&store, &v.id).unwrap();
        let wi = crate::create_with_template(
            &store,
            "pj",
            "多交付物任务",
            "",
            None,
            &[],
            Some("multi-dlv"),
        )
        .unwrap();

        // 两 kind 全缺失；requiredKinds 全量透出。
        let s = status(&store, &wi.id, "build").unwrap();
        assert_eq!(
            s["requiredKind"],
            json!("spec"),
            "requiredKind=首 kind（兼容）"
        );
        assert_eq!(
            serde_json::from_value::<Vec<String>>(s["requiredKinds"].clone()).unwrap(),
            vec!["spec".to_string(), "patch".to_string()]
        );
        assert_eq!(s["satisfied"], json!(false));

        // 只满足 spec → 整体不满足，缺失指向 patch。
        let spec = sg_artifact::create_artifact(&store, &wi.id, "spec", "规格").unwrap();
        let spec_rev = sg_artifact::create_draft(&store, &spec.id, "# spec\n").unwrap();
        sg_artifact::add_review(&store, &spec_rev.id, "r", "approved", "", None).unwrap();
        sg_artifact::freeze(&store, &wi.id, "build", std::slice::from_ref(&spec_rev.id), "", "").unwrap();
        let s = status(&store, &wi.id, "build").unwrap();
        assert_eq!(s["satisfied"], json!(false), "patch 缺失则不放行");
        let err = require_for_release(&store, &wi.id, "build").unwrap_err();
        assert!(
            err.to_string().contains("deliverable_missing") && err.to_string().contains("patch"),
            "{err}"
        );

        // patch 也备齐；spec 旧修订已冻结不可复用，重冻需全部 kind 出新修订
        //（基线 = 本关完整快照，每次 freeze 整体替换）→ 全部满足。
        let patch = sg_artifact::create_artifact(&store, &wi.id, "patch", "补丁").unwrap();
        let patch_rev = sg_artifact::create_draft(&store, &patch.id, "# patch\n").unwrap();
        sg_artifact::add_review(&store, &patch_rev.id, "r", "approved", "", None).unwrap();
        let spec_rev2 = sg_artifact::create_draft(&store, &spec.id, "# spec v2\n").unwrap();
        sg_artifact::add_review(&store, &spec_rev2.id, "r", "approved", "", None).unwrap();
        sg_artifact::freeze(
            &store,
            &wi.id,
            "build",
            &[spec_rev2.id, patch_rev.id],
            "",
            "",
        )
        .unwrap();
        let s = status(&store, &wi.id, "build").unwrap();
        assert_eq!(s["satisfied"], json!(true), "全部 kind 冻结后满足");
        let entries = s["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e["satisfied"] == json!(true)));
    }
}
