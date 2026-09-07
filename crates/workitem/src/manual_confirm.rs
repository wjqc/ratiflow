//! 结构化验收 manual_confirm 人工确认链（EvoFlow WP-7；表 0042 §4）。
//!
//! 确认单与审批主体分离：每行 = 一次人工确认事实，骑 approvals 链
//! （subject_type='gate_manual_confirm'，approval_id UNIQUE 回填；决定经
//! approval.decide 路由到 apply_approval_decision 单向落态）。
//! action_digest = sha256("manual_confirm|workitem|gate|attempt|element_digest")
//! 承载幂等：同 attempt 同元素重放返回既有确认单（UNIQUE 约束兜底）。
//! 身份绑定 = (workitem, gate, stage_attempt, acceptance_item_digest)：
//! digest 来自 acceptance::element_digest（canonical 形态哈希），A 项确认
//! 只放行 A 项；新 attempt（返工/重开）旧确认自然失效。
//! evaluator 只读 state='confirmed' 且 digest 精确匹配的行，绝不读其他来源。

use serde::Serialize;
use sg_policy::Risk;
use sg_store::{ids, timefmt, Error, Store};

#[derive(Debug, Clone, Serialize)]
pub struct ManualConfirmation {
    pub id: String,
    pub workitem_id: String,
    pub gate: String,
    pub stage_attempt_id: String,
    pub acceptance_item_digest: String,
    pub approval_id: String,
    pub state: String,
    pub confirmed_by: String,
    pub confirmed_at: String,
    pub created_at: String,
    pub updated_at: String,
    /// 审批面字段（JOIN approvals；confirmation 行自身不存决定人/理由）。
    pub approval_status: String,
    pub approval_reason: String,
}

const COLS: &str = "c.id, c.workitem_id, c.gate, c.stage_attempt_id, c.acceptance_item_digest,
        c.approval_id, c.state, c.confirmed_by, c.confirmed_at, c.created_at, c.updated_at,
        COALESCE(a.status,''), COALESCE(a.reason,'')";

fn row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<ManualConfirmation> {
    Ok(ManualConfirmation {
        id: r.get(0)?,
        workitem_id: r.get(1)?,
        gate: r.get(2)?,
        stage_attempt_id: r.get(3)?,
        acceptance_item_digest: r.get(4)?,
        approval_id: r.get(5)?,
        state: r.get(6)?,
        confirmed_by: r.get(7)?,
        confirmed_at: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
        approval_status: r.get(11)?,
        approval_reason: r.get(12)?,
    })
}

fn by_approval(
    conn: &rusqlite::Connection,
    approval_id: &str,
) -> Result<ManualConfirmation, Error> {
    conn.query_row(
        &format!(
            "SELECT {COLS} FROM gate_manual_confirmations c
             LEFT JOIN approvals a ON a.id = c.approval_id
             WHERE c.approval_id=?1"
        ),
        [approval_id],
        row_from,
    )
    .map_err(|_| {
        Error::Message(format!(
            "manual_confirmation_missing: approval {approval_id}"
        ))
    })
}

/// 请求人工确认：element 必须是本关 acceptance 集合内的 manual_confirm 结构化项
/// （digest 成员校验——不可为集外元素制造确认事实）；绑定当前活跃 attempt。
/// 幂等：同 attempt 同元素重放返回既有确认单（不重复建审批）。
pub fn request(
    store: &Store,
    workitem_id: &str,
    gate: &str,
    element: &serde_json::Value,
    _requested_by: &str,
    reason: &str,
) -> Result<ManualConfirmation, Error> {
    let el = serde_json::from_value::<sg_workflow::acceptance::AcceptanceElement>(element.clone())
        .map_err(|e| Error::Message(format!("acceptance_schema_invalid: element 形状非法：{e}")))?;
    let verifier = match &el {
        sg_workflow::acceptance::AcceptanceElement::Structured(v) => v,
        sg_workflow::acceptance::AcceptanceElement::Display(_) => {
            return Err(Error::Message(
                "acceptance_schema_invalid: 仅 manual_confirm 结构化项可请求人工确认".into(),
            ))
        }
    };
    if verifier.verifier_name() != "manual_confirm" {
        return Err(Error::Message(
            "acceptance_schema_invalid: 仅 manual_confirm 项可请求人工确认".into(),
        ));
    }
    let item_digest = sg_workflow::acceptance::element_digest(&el);
    let gate_elements = sg_workflow::acceptance::elements_for_gate(store, workitem_id, gate)?;
    if !gate_elements
        .iter()
        .any(|ge| sg_workflow::acceptance::element_digest(ge) == item_digest)
    {
        return Err(Error::Message(format!(
            "acceptance_schema_invalid: element 不属于关 {gate} 的验收集合"
        )));
    }
    let attempt = crate::attempt::ensure_active(store, workitem_id, gate)?;
    // 放行 digest 同构：身份五元组哈希（WP-7）。
    let action_digest = {
        use sha2::{Digest, Sha256};
        format!(
            "sha256:{}",
            sg_store::ids::hex(&Sha256::digest(
                format!(
                    "manual_confirm|{workitem_id}|{gate}|{}|{item_digest}",
                    attempt.id
                )
                .as_bytes()
            ))
        )
    };
    // 幂等重放：同 attempt 同元素已有确认单 → 原样返回（含其审批状态）。
    let existing: Option<ManualConfirmation> = store.with_conn(|conn| {
        conn.query_row(
            &format!(
                "SELECT {COLS} FROM gate_manual_confirmations c
                 LEFT JOIN approvals a ON a.id = c.approval_id
                 WHERE c.action_digest=?1"
            ),
            [&action_digest],
            row_from,
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other.into()),
        })
    })?;
    if let Some(c) = existing {
        return Ok(c);
    }
    let id = ids::new_id("gmc");
    let now = timefmt::now();
    // 顺序：审批先行（approvals.action_digest UNIQUE 同键幂等），确认单随之携带
    // 真实 approval_id 落库（0042 该列 NOT NULL REFERENCES approvals，不可占位）。
    let approval = sg_policy::request_approval(
        store,
        "gate_manual_confirm",
        &id,
        &action_digest,
        Risk::Low,
        reason,
        0,
        Some(workitem_id),
        Some(&attempt.id),
    )?;
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO gate_manual_confirmations
             (id, workitem_id, gate, stage_attempt_id, acceptance_item_digest,
              approval_id, action_digest, state, confirmed_by, confirmed_at, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,'requested','','',?8,?8)",
            rusqlite::params![
                id,
                workitem_id,
                gate,
                attempt.id,
                item_digest,
                approval.id,
                action_digest,
                now
            ],
        )?;
        Ok(())
    })?;
    sg_store::outbox::emit(
        store,
        "workitem",
        workitem_id,
        "gate.manualConfirmation.requested",
        serde_json::json!({
            "gate": gate,
            "confirmationId": id,
            "approvalId": approval.id,
            "elementDigest": item_digest
        }),
    )?;
    store.with_conn(|conn| {
        conn.query_row(
            &format!(
                "SELECT {COLS} FROM gate_manual_confirmations c
                 LEFT JOIN approvals a ON a.id = c.approval_id WHERE c.id=?1"
            ),
            [&id],
            row_from,
        )
        .map_err(Error::from)
    })
}

/// 审批决定落态（approval.decide 的 gate_manual_confirm 路由；单向 requested →
/// confirmed/rejected；重放同态返回既有行，反向/越态拒绝）。
pub fn apply_approval_decision(
    store: &Store,
    approval_id: &str,
    decision: &str,
    decided_by: &str,
) -> Result<ManualConfirmation, Error> {
    let state = match decision {
        "approved" => "confirmed",
        "rejected" => "rejected",
        other => {
            return Err(Error::Message(format!(
                "manual_confirmation_invalid_decision: {other}"
            )))
        }
    };
    let now = timefmt::now();
    store.with_conn(|conn| {
        let existing = by_approval(conn, approval_id)?;
        if existing.state == state {
            return Ok(existing); // 幂等重放
        }
        if existing.state != "requested" {
            return Err(Error::Message(
                "manual_confirmation_state_changed: 确认单已决定（不可改判）".into(),
            ));
        }
        conn.execute(
            "UPDATE gate_manual_confirmations
             SET state=?1, confirmed_by=?2, confirmed_at=?3, updated_at=?3
             WHERE approval_id=?4 AND state='requested'",
            rusqlite::params![state, decided_by, now, approval_id],
        )?;
        by_approval(conn, approval_id)
    })?;
    let updated = store.with_conn(|conn| by_approval(conn, approval_id))?;
    sg_store::outbox::emit(
        store,
        "workitem",
        &updated.workitem_id,
        "gate.manualConfirmation.decided",
        serde_json::json!({
            "gate": updated.gate,
            "confirmationId": updated.id,
            "approvalId": updated.approval_id,
            "elementDigest": updated.acceptance_item_digest,
            "state": updated.state
        }),
    )?;
    Ok(updated)
}

/// 确认单列表（可按关过滤；新→旧）。
pub fn list(
    store: &Store,
    workitem_id: &str,
    gate: Option<&str>,
) -> Result<Vec<ManualConfirmation>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLS} FROM gate_manual_confirmations c
             LEFT JOIN approvals a ON a.id = c.approval_id
             WHERE c.workitem_id=?1 AND (?2='' OR c.gate=?2)
             ORDER BY c.created_at DESC, c.rowid DESC"
        ))?;
        let rows = stmt.query_map(rusqlite::params![workitem_id, gate.unwrap_or("")], row_from)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-gmc-{}-{}",
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

    fn workitem_with_manual_gate(store: &Store) -> String {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(sg_workflow::acceptance::FLAG, "1");
        let t = sg_workflow::template::create_template(store, "gmc-wi", "人工确认关").unwrap();
        let v = sg_workflow::template::create_version(
            store,
            &t.id,
            &[sg_workflow::template::GateDefInput {
                gate_id: "confirm".into(),
                title: "确认关".into(),
                purpose: String::new(),
                deliverables: vec!["verification".into()],
                acceptance: vec![
                    json!({"verifier": "manual_confirm", "confirmation_subject": "gate_manual_confirm", "confirm_role": "user"}),
                    json!({"verifier": "evidence_verified", "evidence_kind": "test_report", "min_count": 1}),
                ],
                context_policy_ref: None,
                team_policy_ref: None,
                workspace_policy_ref: None,
                skip_policy: None,
                fast_track_policy: None,
            }],
            "tester",
        )
        .unwrap();
        sg_workflow::template::activate(store, &v.id).unwrap();
        std::env::remove_var(sg_workflow::acceptance::FLAG);
        let wi =
            crate::create_with_template(store, "pj", "人工确认任务", "", None, &[], Some("gmc-wi"))
                .unwrap();
        wi.id
    }

    fn manual_element() -> serde_json::Value {
        json!({"verifier": "manual_confirm", "confirmation_subject": "gate_manual_confirm", "confirm_role": "user"})
    }

    #[test]
    fn request_decide_binds_element_identity() {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let store = setup();
        let wi = workitem_with_manual_gate(&store);
        // 评估期 = 功能开启的运行时（flag 关闭时结构化项按设计一律 unavailable→Fail）。
        std::env::set_var(sg_workflow::acceptance::FLAG, "1");
        // 请求 → requested + 审批挂起；同元素重放幂等返回同一确认单。
        let c = request(
            &store,
            &wi,
            "confirm",
            &manual_element(),
            "agent",
            "请人工复核",
        )
        .unwrap();
        assert_eq!(c.state, "requested");
        assert_eq!(c.approval_status, "requested");
        let replay = request(&store, &wi, "confirm", &manual_element(), "agent", "").unwrap();
        assert_eq!(replay.id, c.id);
        // canonical 身份：多余字段不改变 digest → 幂等重放同一确认单（不重复建审批）。
        let extra_field = json!({"verifier": "manual_confirm", "confirmation_subject": "gate_manual_confirm", "confirm_role": "user", "extra": 1});
        let fk = request(&store, &wi, "confirm", &extra_field, "agent", "").unwrap();
        assert_eq!(fk.id, c.id);
        // 非 manual_confirm 结构化项拒绝（不可为其他 verifier 造人工确认事实）。
        assert!(request(
            &store,
            &wi,
            "confirm",
            &json!({"verifier": "artifact_frozen", "artifact_kind": "prd"}),
            "agent",
            ""
        )
        .is_err());
        // 审批决定确认 → confirmed；重复/越态决定冲突。
        let d = apply_approval_decision(&store, &c.approval_id, "approved", "user").unwrap();
        assert_eq!(d.state, "confirmed");
        assert_eq!(
            apply_approval_decision(&store, &c.approval_id, "approved", "user")
                .unwrap()
                .state,
            "confirmed"
        );
        // evaluator：digest 精确匹配 → manual_confirm 项 pass；
        // 同关另一结构化项（evidence_verified）不受该确认影响。
        let inputs = crate::gate::build_inputs(&store, &wi, "confirm").unwrap();
        let inputs = crate::gate::EvaluateInputs {
            required_artifacts_frozen: crate::gate::InputState::Pass,
            required_checks_passed: crate::gate::InputState::Pass,
            approvals_valid: crate::gate::InputState::Pass,
            evidence_complete: crate::gate::InputState::Pass,
            no_blocking_risk: crate::gate::InputState::Pass,
            inputs_current: crate::gate::InputState::Pass,
            ..inputs
        };
        let elements = sg_workflow::acceptance::elements_for_gate(&store, &wi, "confirm").unwrap();
        let outcomes =
            crate::acceptance_eval::evaluate_elements(&store, &inputs, &elements).unwrap();
        let manual = outcomes
            .iter()
            .find(|o| o.verifier == "manual_confirm")
            .unwrap();
        assert_eq!(manual.state, crate::gate::InputState::Pass, "{manual:?}");
        let evidence = outcomes
            .iter()
            .find(|o| o.verifier == "evidence_verified")
            .unwrap();
        assert_eq!(
            evidence.state,
            crate::gate::InputState::Fail,
            "{evidence:?}"
        );
        // 终态后重放：同 attempt 同元素返回既有确认单（confirmed 保持），不可反向改判。
        let replay2 = request(&store, &wi, "confirm", &manual_element(), "agent", "").unwrap();
        assert_eq!(replay2.id, c.id);
        assert!(apply_approval_decision(&store, &c.approval_id, "rejected", "user").is_err());
        // 拒绝路径（第二个工作项）：rejected 不放行，且不可改判。
        let wi2 = crate::create_with_template(
            &store,
            "pj",
            "拒绝路径任务",
            "",
            None,
            &[],
            Some("gmc-wi"),
        )
        .unwrap()
        .id;
        let c2 = request(&store, &wi2, "confirm", &manual_element(), "agent", "").unwrap();
        let d2 = apply_approval_decision(&store, &c2.approval_id, "rejected", "user").unwrap();
        assert_eq!(d2.state, "rejected");
        assert!(apply_approval_decision(&store, &c2.approval_id, "approved", "user").is_err());
        let inputs2 = crate::gate::build_inputs(&store, &wi2, "confirm").unwrap();
        let inputs2 = crate::gate::EvaluateInputs {
            required_artifacts_frozen: crate::gate::InputState::Pass,
            required_checks_passed: crate::gate::InputState::Pass,
            approvals_valid: crate::gate::InputState::Pass,
            evidence_complete: crate::gate::InputState::Pass,
            no_blocking_risk: crate::gate::InputState::Pass,
            inputs_current: crate::gate::InputState::Pass,
            ..inputs2
        };
        let elements2 =
            sg_workflow::acceptance::elements_for_gate(&store, &wi2, "confirm").unwrap();
        let outcomes2 =
            crate::acceptance_eval::evaluate_elements(&store, &inputs2, &elements2).unwrap();
        let manual2 = outcomes2
            .iter()
            .find(|o| o.verifier == "manual_confirm")
            .unwrap();
        assert_eq!(manual2.state, crate::gate::InputState::Fail, "{manual2:?}");
        std::env::remove_var(sg_workflow::acceptance::FLAG);
    }
}
