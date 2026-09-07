//! fast-track 快通道（EvoFlow WP-8；策略 schema 见 sg-workflow template）。
//!
//! 流程：提案方提交六因素受控枚举（全真才受理）→ 建议落 WP-8a shadow 表
//! （source=fast_track，**不自动执行**）→ 人工经 automation.decideSuggestion
//! 决定；accepted 后缩减在域内生效——
//! - 活动缩减：活跃 attempt 中 skippable_activities 的 pending 活动标记 skipped；
//! - 交付物豁免：waived_deliverables 以替代证据（verified 证据 kind 匹配）替代，
//!   替代证据缺失照实不满足（deliverable 门禁 fail-closed）；
//! - reduced_approval 仅声明可缩减关卡层人工复核，Tool 风险/Policy 强制审批
//!   永不在豁免面（策略 schema 无此声明位）。
//!
//! 身份幂等：suggestion_digest = sha256(fast_track|workitem|gate|attempt|canonical factors)。

use serde_json::json;
use sg_store::{Error, Store};
use sg_workflow::template::{FastTrackFactors, FastTrackPolicy, WaivedDeliverable};

fn gate_policy(
    store: &Store,
    workitem_id: &str,
    gate: &str,
) -> Result<sg_workflow::template::GateDefinition, Error> {
    let instance = sg_workflow::instance::for_workitem(store, workitem_id)?.ok_or_else(|| {
        Error::Message(format!(
            "fast_track_invalid: 工作项 {workitem_id} 无工作流实例"
        ))
    })?;
    let defs = sg_workflow::template::definitions_via_store(store, &instance.template_version_id)?;
    defs.into_iter()
        .find(|d| d.gate_id == gate)
        .ok_or_else(|| Error::Message(format!("fast_track_invalid: 未知关 {gate}")))
}

/// 六因素评估 + 建议落 shadow。策略未声明 / 枚举非全真 → 拒（不产生建议）。
/// 同 attempt 同因素重放 → 返回既有建议（digest 幂等）。
pub fn evaluate_and_suggest(
    store: &Store,
    workitem_id: &str,
    gate: &str,
    factors: &FastTrackFactors,
) -> Result<sg_workflow::shadow::Suggestion, Error> {
    let def = gate_policy(store, workitem_id, gate)?;
    if def.fast_track_policy.is_none() {
        return Err(Error::Message(format!(
            "fast_track_forbidden: 关 {gate} 未声明 fast-track 策略"
        )));
    }
    if !factors.all_true() {
        return Err(Error::Message(
            "fast_track_factors_not_all_true: 六因素须全真方可生成快通道建议".into(),
        ));
    }
    let attempt = crate::attempt::ensure_active(store, workitem_id, gate)?;
    let canonical = serde_json::to_string(factors).unwrap_or_default();
    let suggestion_digest = {
        use sha2::{Digest, Sha256};
        format!(
            "sha256:{}",
            sg_store::ids::hex(&Sha256::digest(
                format!("fast_track|{workitem_id}|{gate}|{}|{canonical}", attempt.id).as_bytes()
            ))
        )
    };
    // 幂等重放：同 attempt 同因素已有建议 → 原样返回。
    if let Some(existing) = existing_by_digest(store, &suggestion_digest)? {
        return Ok(existing);
    }
    let hypothetical = format!("sha256:{}", {
        use sha2::{Digest, Sha256};
        sg_store::ids::hex(&Sha256::digest(
            format!("fast_track_apply|{workitem_id}|{gate}|{}", attempt.id).as_bytes(),
        ))
    });
    sg_workflow::shadow::record(
        store,
        &sg_workflow::shadow::SuggestionInput {
            source: "fast_track",
            automation_id: None,
            workitem_id: Some(workitem_id),
            suggestion_type: "gate_fast_track",
            suggestion_digest: &suggestion_digest,
            content: json!({"gate": gate, "factors": factors}),
            hypothetical_action_digest: &hypothetical,
            policy_version: "ft1",
            model: "",
            prompt_version: "",
        },
    )
}

fn existing_by_digest(
    store: &Store,
    digest: &str,
) -> Result<Option<sg_workflow::shadow::Suggestion>, Error> {
    // 查 id 与 get 分两段：with_conn 内不得嵌套 shadow::get（Mutex 不可重入）。
    let id: Option<String> = store.with_conn(|conn| {
        match conn.query_row(
            "SELECT id FROM shadow_suggestions WHERE suggestion_digest=?1",
            [digest],
            |r| r.get::<_, String>(0),
        ) {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(other) => Err(other.into()),
        }
    })?;
    match id {
        Some(id) => Ok(Some(sg_workflow::shadow::get(store, &id)?)),
        None => Ok(None),
    }
}

/// 该关已采纳（decision=accepted）的 fast-track 建议与策略；未采纳 → None。
fn accepted(
    store: &Store,
    workitem_id: &str,
    gate: &str,
) -> Result<Option<FastTrackPolicy>, Error> {
    for s in sg_workflow::shadow::decided_suggestions(store, "fast_track", workitem_id, "accepted")?
    {
        if s.content.get("gate").and_then(|g| g.as_str()) == Some(gate) {
            let def = gate_policy(store, workitem_id, gate)?;
            if let Some(policy) = def.fast_track_policy {
                return Ok(Some(policy));
            }
        }
    }
    Ok(None)
}

/// 建议被采纳后应用活动缩减：活跃 attempt 中 skippable_activities 的
/// pending 活动 → skipped（running/done 等已有事实不改）。
/// 返回是否发生了应用（未采纳 → false，幂等）。
pub fn apply_if_accepted(store: &Store, workitem_id: &str, gate: &str) -> Result<bool, Error> {
    let Some(policy) = accepted(store, workitem_id, gate)? else {
        return Ok(false);
    };
    if policy.skippable_activities.is_empty() {
        return Ok(true); // 无活动缩减项；豁免面在交付物检查时消费
    }
    let attempt = crate::attempt::ensure_active(store, workitem_id, gate)?;
    store.with_conn(|conn| {
        for key in &policy.skippable_activities {
            conn.execute(
                "UPDATE stage_activities SET state='skipped', updated_at=?2
                 WHERE stage_attempt_id=?1 AND activity_key=?3 AND state='pending'",
                rusqlite::params![attempt.id, sg_store::timefmt::now(), key],
            )?;
        }
        Ok(())
    })?;
    Ok(true)
}

/// 交付物豁免清单（仅当该关存在已采纳的 fast-track 建议；否则空 = 无豁免）。
pub fn waived_deliverables(
    store: &Store,
    workitem_id: &str,
    gate: &str,
) -> Result<Vec<WaivedDeliverable>, Error> {
    Ok(accepted(store, workitem_id, gate)?
        .map(|p| p.waived_deliverables)
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sg_store::{ids, timefmt};

    fn setup() -> Store {
        let dir =
            std::env::temp_dir().join(format!("sg-ft-{}-{}", std::process::id(), ids::new_id("t")));
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

    fn workitem_with_ft_gate(store: &Store) -> String {
        let t = sg_workflow::template::create_template(store, "ft-tpl", "快通道模板").unwrap();
        let v = sg_workflow::template::create_version(
            store,
            &t.id,
            &[sg_workflow::template::GateDefInput {
                gate_id: "build".into(),
                title: "构建关".into(),
                purpose: String::new(),
                deliverables: vec!["code".into()],
                acceptance: vec![],
                context_policy_ref: None,
                team_policy_ref: None,
                workspace_policy_ref: None,
                skip_policy: None,
                fast_track_policy: Some(sg_workflow::template::FastTrackPolicy {
                    skippable_activities: vec!["execution".into()],
                    waived_deliverables: vec![sg_workflow::template::WaivedDeliverable {
                        kind: "code".into(),
                        substitute_evidence_kind: "manual".into(),
                    }],
                    reduced_approval: true,
                }),
            }],
            "tester",
        )
        .unwrap();
        sg_workflow::template::activate(store, &v.id).unwrap();
        crate::create_with_template(store, "pj", "快通道任务", "", None, &[], Some("ft-tpl"))
            .unwrap()
            .id
    }

    fn factors_all_true() -> FastTrackFactors {
        FastTrackFactors {
            no_protected_path: true,
            api_schema_unchanged: true,
            effect_class_read_only: true,
            provenance_complete: true,
            test_evidence_present: true,
        }
    }

    #[test]
    fn evaluate_suggest_and_apply_flow() {
        let store = setup();
        let wi = workitem_with_ft_gate(&store);
        // 枚举非全真拒绝（不产生建议）。
        let mut partial = factors_all_true();
        partial.api_schema_unchanged = false;
        assert!(evaluate_and_suggest(&store, &wi, "build", &partial).is_err());
        // 全真 → 建议落 shadow（source=fast_track）；重放幂等返回同一建议。
        let s1 = evaluate_and_suggest(&store, &wi, "build", &factors_all_true()).unwrap();
        assert_eq!(s1.source, "fast_track");
        assert_eq!(s1.content["gate"], json!("build"));
        let replay = evaluate_and_suggest(&store, &wi, "build", &factors_all_true()).unwrap();
        assert_eq!(replay.id, s1.id);
        // 未采纳：应用返回 false、无豁免。
        assert!(!apply_if_accepted(&store, &wi, "build").unwrap());
        assert!(waived_deliverables(&store, &wi, "build")
            .unwrap()
            .is_empty());
        // 采纳 → 应用 true；execution 活动（唯一 pending 活动）被标记 skipped。
        sg_workflow::shadow::decide(&store, &s1.id, "accepted", "owner", "采纳").unwrap();
        assert!(apply_if_accepted(&store, &wi, "build").unwrap());
        assert!(apply_if_accepted(&store, &wi, "build").unwrap(), "幂等");
        let attempt = crate::attempt::ensure_active(&store, &wi, "build").unwrap();
        let acts = crate::attempt::activities(&store, &attempt.id).unwrap();
        assert!(
            acts.iter()
                .all(|a| a.activity_key == "execution" && a.state == "skipped"),
            "{acts:?}"
        );
        // 豁免清单生效。
        let waived = waived_deliverables(&store, &wi, "build").unwrap();
        assert_eq!(waived.len(), 1);
        assert_eq!(waived[0].substitute_evidence_kind, "manual");
    }

    #[test]
    fn policy_undeclared_gate_rejected() {
        let store = setup();
        let wi = crate::create_with_template(&store, "pj", "无策略任务", "", None, &[], None)
            .unwrap()
            .id;
        let err = evaluate_and_suggest(&store, &wi, "requirements", &factors_all_true())
            .unwrap_err()
            .to_string();
        assert!(err.contains("fast_track_forbidden"), "{err}");
        let _ = workitem_with_ft_gate(&store); // 模板独立存在，不影响默认实例
    }
}
