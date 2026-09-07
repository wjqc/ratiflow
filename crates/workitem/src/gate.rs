//! 门禁引擎（手册 §3.2 / FR-PLT-009 行为等价）：
//! 任何输入 unknown/缺失/超时/未运行时不得通过。

use serde::{Deserialize, Serialize};
use sg_store::{ids, Error, Store};

pub const GATE_INPUTS: [&str; 6] = [
    "required_artifacts_frozen",
    "required_checks_passed",
    "approvals_valid",
    "evidence_complete",
    "no_blocking_risk",
    "inputs_current",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputState {
    Pass,
    Fail,
    Unknown,
}

impl InputState {
    pub fn as_str(&self) -> &'static str {
        match self {
            InputState::Pass => "pass",
            InputState::Fail => "fail",
            InputState::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluateInputs {
    pub workitem_id: String,
    pub gate: String,
    pub required_artifacts_frozen: InputState,
    pub required_checks_passed: InputState,
    pub approvals_valid: InputState,
    pub evidence_complete: InputState,
    pub no_blocking_risk: InputState,
    pub inputs_current: InputState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateResult {
    pub gate: String,
    pub passed: bool,
    pub failed_inputs: Vec<String>,
    pub computed_at: String,
}

/// 纯函数：六输入全 pass 才通过。
pub fn evaluate(inputs: &EvaluateInputs) -> GateResult {
    let checks = [
        (
            "required_artifacts_frozen",
            inputs.required_artifacts_frozen,
        ),
        ("required_checks_passed", inputs.required_checks_passed),
        ("approvals_valid", inputs.approvals_valid),
        ("evidence_complete", inputs.evidence_complete),
        ("no_blocking_risk", inputs.no_blocking_risk),
        ("inputs_current", inputs.inputs_current),
    ];
    let failed: Vec<String> = checks
        .iter()
        .filter(|(_, state)| *state != InputState::Pass)
        .map(|(name, _)| name.to_string())
        .collect();
    GateResult {
        gate: inputs.gate.clone(),
        passed: failed.is_empty(),
        failed_inputs: failed,
        computed_at: sg_store::now(),
    }
}

/// 记录一次门禁计算。
pub fn record(store: &Store, inputs: &EvaluateInputs, result: &GateResult) -> Result<(), Error> {
    let id = ids::new_id("gate");
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO gate_results(id, workitem_id, gate, inputs, result, computed_at)
             VALUES (?1,?2,?3,?4,?5,?6)",
            rusqlite::params![
                id,
                inputs.workitem_id,
                inputs.gate,
                serde_json::to_string(inputs).unwrap_or_default(),
                serde_json::to_string(result).unwrap_or_default(),
                result.computed_at
            ],
        )?;
        Ok(())
    })?;
    sg_store::outbox::emit(
        store,
        "workitem",
        &inputs.workitem_id,
        "gate.evaluated",
        serde_json::json!({"gate": inputs.gate, "passed": result.passed, "failedInputs": result.failed_inputs}),
    )?;
    Ok(())
}

pub fn evaluate_and_record(store: &Store, inputs: &EvaluateInputs) -> Result<GateResult, Error> {
    let mut result = evaluate(inputs);
    // WP-7：结构化 acceptance 逐项判定，以 `acceptance:*`/`acceptance_evaluator_unavailable:*`
    // 追加进 failed_inputs（六输入之外）。自由文本元素不进判定；关无结构化项时
    // 行为与旧六输入完全一致。evaluator 内部 fail-closed（kill switch=0 → 放行被阻）。
    let elements =
        sg_workflow::acceptance::elements_for_gate(store, &inputs.workitem_id, &inputs.gate)?;
    if elements.iter().any(|el| {
        matches!(
            el,
            sg_workflow::acceptance::AcceptanceElement::Structured(_)
        )
    }) {
        let outcomes = crate::acceptance_eval::evaluate_elements(store, inputs, &elements)?;
        crate::acceptance_eval::merge_into_result(&mut result, &outcomes);
    }
    record(store, inputs, &result)?;
    Ok(result)
}

/// 评估输入构建（单一事实源）：dispatch 的 evaluate 与放行的"评估新鲜度重查"
/// 必须用同一函数，否则存储的 inputs 与重算结果不可比。
pub fn build_inputs(store: &Store, workitem_id: &str, gate: &str) -> Result<EvaluateInputs, Error> {
    let mut inputs = EvaluateInputs {
        workitem_id: workitem_id.into(),
        gate: gate.into(),
        required_artifacts_frozen: InputState::Unknown,
        required_checks_passed: InputState::Unknown,
        approvals_valid: InputState::Unknown,
        evidence_complete: InputState::Unknown,
        no_blocking_risk: InputState::Pass,
        inputs_current: InputState::Unknown,
    };
    // M2 per-gate baseline：各关基线独立 active（蓝图 §5.3）；M1-04：gate_id 实例字符串直传。
    if let Some(base) = sg_artifact::latest_baseline(store, workitem_id, gate)? {
        if sg_artifact::is_baseline_current(store, &base.id)? {
            inputs.required_artifacts_frozen = InputState::Pass;
            inputs.inputs_current = InputState::Pass;
        } else {
            inputs.required_artifacts_frozen = InputState::Fail;
            inputs.inputs_current = InputState::Fail;
        }
    }
    let evidences = sg_evidence::list(store, workitem_id, Some(gate))?;
    if !evidences.is_empty() {
        inputs.evidence_complete = if evidences.iter().all(|e| e.verified) {
            InputState::Pass
        } else {
            InputState::Fail
        };
        inputs.required_checks_passed = InputState::Pass;
    }
    // AC-SW-05：审批按 WorkItem 作用域；gate_release/rollback 不阻塞技术评估。
    if sg_policy::pending_blocking_count(store, workitem_id)? == 0 {
        inputs.approvals_valid = InputState::Pass;
    } else {
        inputs.no_blocking_risk = InputState::Fail;
    }
    Ok(inputs)
}

/// 最近一次计算的完整行：行 id + 存储 inputs（canonical JSON）+ 结果。
pub fn latest_full(
    store: &Store,
    workitem_id: &str,
    gate: &str,
) -> Result<Option<(String, String, GateResult)>, Error> {
    store.with_conn(|conn| {
        let row = conn
            .query_row(
                "SELECT id, inputs, result FROM gate_results
                 WHERE workitem_id=?1 AND gate=?2
                 ORDER BY computed_at DESC, rowid DESC LIMIT 1",
                [workitem_id, gate],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .ok();
        Ok(row.map(|(id, inputs, result)| {
            let parsed: GateResult = serde_json::from_str(&result).unwrap_or(GateResult {
                gate: gate.into(),
                passed: false,
                failed_inputs: vec![],
                computed_at: String::new(),
            });
            (id, inputs, parsed)
        }))
    })
}

/// 最近一次计算的 gate_results 行 id（放行事务绑定 GateEvaluation 用）。
pub fn latest_id(store: &Store, workitem_id: &str, gate: &str) -> Result<Option<String>, Error> {
    store.with_conn(|conn| {
        let id = conn
            .query_row(
                "SELECT id FROM gate_results WHERE workitem_id=?1 AND gate=?2
                 ORDER BY computed_at DESC, rowid DESC LIMIT 1",
                [workitem_id, gate],
                |r| r.get::<_, String>(0),
            )
            .ok();
        Ok(id)
    })
}

/// 最近一次计算。
pub fn latest(store: &Store, workitem_id: &str, gate: &str) -> Result<Option<GateResult>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT result, computed_at FROM gate_results
             WHERE workitem_id=?1 AND gate=?2
             ORDER BY computed_at DESC, rowid DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map([workitem_id, gate], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        match rows.next() {
            Some(Ok((body, computed_at))) => {
                let mut result: GateResult =
                    serde_json::from_str(&body).map_err(|e| Error::Message(e.to_string()))?;
                result.computed_at = computed_at;
                Ok(Some(result))
            }
            _ => Ok(None),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(states: InputState) -> EvaluateInputs {
        EvaluateInputs {
            workitem_id: "wi".into(),
            gate: "testing".into(),
            required_artifacts_frozen: states,
            required_checks_passed: states,
            approvals_valid: states,
            evidence_complete: states,
            no_blocking_risk: states,
            inputs_current: states,
        }
    }

    #[test]
    fn all_pass_passes() {
        assert!(evaluate(&inputs(InputState::Pass)).passed);
    }

    #[test]
    fn unknown_never_passes() {
        let mut i = inputs(InputState::Pass);
        i.required_checks_passed = InputState::Unknown;
        let r = evaluate(&i);
        assert!(!r.passed);
        assert_eq!(r.failed_inputs, vec!["required_checks_passed"]);
    }

    #[test]
    fn multiple_failures_listed() {
        let mut i = inputs(InputState::Pass);
        i.required_checks_passed = InputState::Fail;
        i.evidence_complete = InputState::Unknown;
        let r = evaluate(&i);
        assert_eq!(r.failed_inputs.len(), 2);
    }
}
