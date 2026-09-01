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
    let result = evaluate(inputs);
    record(store, inputs, &result)?;
    Ok(result)
}

/// 最近一次计算的 gate_results 行 id（放行事务绑定 GateEvaluation 用）。
pub fn latest_id(store: &Store, workitem_id: &str, gate: &str) -> Result<Option<String>, Error> {
    store.with_conn(|conn| {
        let id = conn
            .query_row(
                "SELECT id FROM gate_results WHERE workitem_id=?1 AND gate=?2 ORDER BY computed_at DESC LIMIT 1",
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
             WHERE workitem_id=?1 AND gate=?2 ORDER BY computed_at DESC LIMIT 1",
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
