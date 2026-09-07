//! B7 结构化验收评估器（RDWS 实施计划 v1.4 WP-7，关 P2-11）。
//!
//! 结构化 acceptance 元素映射为逐关 evaluator 输入：每个元素独立判定 pass/fail
//! （unknown 进 fail 方向——门禁 fail-closed）；字符串元素仅展示不进判定。
//! kill switch（flag=0）时结构化强制项一律 failed_inputs 记
//! acceptance_evaluator_unavailable（放行被阻，绝不静默降级回旧六输入放行）。
//! manual_confirm 只读 gate_manual_confirmations 的 confirmed 事实（与当前
//! attempt + 验收项 digest 匹配）；确认链的请求/决定在 manual_confirm.rs。

use crate::gate::{EvaluateInputs, GateResult, InputState};
use sg_store::{Error, Store};
use sg_workflow::acceptance::{AcceptanceElement, AcceptanceVerifier};

/// 单元素判定结果。
#[derive(Debug)]
pub struct ElementOutcome {
    pub verifier: &'static str,
    pub detail: String,
    pub state: InputState,
}

/// 结构化元素评估（纯读 + 门禁语义 fail-closed）。
pub fn evaluate_elements(
    store: &Store,
    inputs: &EvaluateInputs,
    elements: &[AcceptanceElement],
) -> Result<Vec<ElementOutcome>, Error> {
    let mut out = Vec::new();
    for el in elements {
        let AcceptanceElement::Structured(v) = el else {
            continue; // 展示形态不进判定
        };
        let state = if !sg_workflow::acceptance::enabled() {
            // kill switch：机器契约无 evaluator 可用 → 放行被阻（不降级）。
            InputState::Fail
        } else {
            evaluate_verifier(
                store,
                inputs,
                v,
                &sg_workflow::acceptance::element_digest(el),
            )?
        };
        out.push(ElementOutcome {
            verifier: v.verifier_name(),
            detail: detail_of(v),
            state,
        });
    }
    Ok(out)
}

fn detail_of(v: &AcceptanceVerifier) -> String {
    match v {
        AcceptanceVerifier::ArtifactFrozen { artifact_kind } => format!("kind={artifact_kind}"),
        AcceptanceVerifier::TextNonempty { artifact_kind } => format!("kind={artifact_kind}"),
        AcceptanceVerifier::EvidenceVerified {
            evidence_kind,
            min_count,
        } => format!("kind={evidence_kind},min={min_count}"),
        AcceptanceVerifier::CoverageComplete { scope } => format!("scope={scope}"),
        AcceptanceVerifier::DigestMatch { expected_from } => format!("from={expected_from}"),
        AcceptanceVerifier::ManualConfirm { .. } => "manual".into(),
    }
}

fn evaluate_verifier(
    store: &Store,
    inputs: &EvaluateInputs,
    v: &AcceptanceVerifier,
    element_digest: &str,
) -> Result<InputState, Error> {
    match v {
        // 关基线已冻结指定 kind 的交付物：active 基线 revision_map 经 artifacts
        // 解析 kind，命中即 pass；无基线/无该 kind → fail（明确缺口，非 unknown）。
        AcceptanceVerifier::ArtifactFrozen { artifact_kind } => {
            let Some(base) =
                sg_artifact::latest_baseline(store, &inputs.workitem_id, &inputs.gate)?
            else {
                return Ok(InputState::Fail);
            };
            let map = base.revision_map;
            let hit: Option<String> = store.with_conn(|c| {
                let mut stmt = c.prepare("SELECT a.kind FROM artifacts a WHERE a.id=?1")?;
                for (artifact_id, _rev) in map
                    .as_object()
                    .map(|m| m.iter().collect::<Vec<_>>())
                    .unwrap_or_default()
                {
                    if let Ok(kind) = stmt.query_row([artifact_id], |r| r.get::<_, String>(0)) {
                        if kind == *artifact_kind {
                            return Ok(Some(artifact_id.clone()));
                        }
                    }
                }
                Ok(None)
            })?;
            Ok(if hit.is_some() {
                InputState::Pass
            } else {
                InputState::Fail
            })
        }
        // 冻结交付物正文非空：取该 kind 的冻结 revision 内容。
        AcceptanceVerifier::TextNonempty { artifact_kind } => {
            let Some(base) =
                sg_artifact::latest_baseline(store, &inputs.workitem_id, &inputs.gate)?
            else {
                return Ok(InputState::Fail);
            };
            let rev_id: Option<String> = store.with_conn(|c| {
                let mut stmt = c.prepare("SELECT kind FROM artifacts WHERE id=?1")?;
                for (artifact_id, rev) in base
                    .revision_map
                    .as_object()
                    .map(|m| m.iter().collect::<Vec<_>>())
                    .unwrap_or_default()
                {
                    if let Ok(kind) = stmt.query_row([artifact_id], |r| r.get::<_, String>(0)) {
                        if kind == *artifact_kind {
                            return Ok(rev.as_str().map(String::from));
                        }
                    }
                }
                Ok(None)
            })?;
            let Some(rev_id) = rev_id else {
                return Ok(InputState::Fail);
            };
            let content = sg_artifact::revision_content(store, &rev_id)?;
            Ok(if content.iter().any(|b| !b.is_ascii_whitespace()) {
                InputState::Pass
            } else {
                InputState::Fail
            })
        }
        // 已核验证据 ≥ min_count。
        AcceptanceVerifier::EvidenceVerified {
            evidence_kind,
            min_count,
        } => {
            let evidences = sg_evidence::list(store, &inputs.workitem_id, Some(&inputs.gate))?;
            let n = evidences
                .iter()
                .filter(|e| e.verified && &e.kind == evidence_kind)
                .count() as u32;
            Ok(if n >= *min_count {
                InputState::Pass
            } else {
                InputState::Fail
            })
        }
        // 需求覆盖完整：最新修订逐条 covered（satisfies/implements 有边）。
        AcceptanceVerifier::CoverageComplete { .. } => {
            let Some(rev) = crate::requirements::latest_revision_id(store, &inputs.workitem_id)?
            else {
                return Ok(InputState::Fail);
            };
            let coverage = sg_provenance::coverage(store, &inputs.workitem_id, &rev)?;
            let items = coverage
                .get("items")
                .and_then(|i| i.as_array())
                .cloned()
                .unwrap_or_default();
            if items.is_empty() {
                return Ok(InputState::Fail);
            }
            let all_covered = items.iter().all(|i| {
                matches!(
                    i.get("status").and_then(|s| s.as_str()),
                    Some("covered") | Some("verified")
                )
            });
            Ok(if all_covered {
                InputState::Pass
            } else {
                InputState::Fail
            })
        }
        // 摘要匹配：baseline=active 基线未漂移；approval=无待决放行审批被 digest
        // 失效（pending 放行存在时其绑定仍有效）；policy=无 pending 阻塞审批。
        AcceptanceVerifier::DigestMatch { expected_from } => match expected_from.as_str() {
            "baseline" => {
                let Some(base) =
                    sg_artifact::latest_baseline(store, &inputs.workitem_id, &inputs.gate)?
                else {
                    return Ok(InputState::Fail);
                };
                Ok(if sg_artifact::is_baseline_current(store, &base.id)? {
                    InputState::Pass
                } else {
                    InputState::Fail
                })
            }
            "approval" => {
                let drifted: i64 = store.with_conn(|c| {
                    Ok(c.query_row(
                        "SELECT COUNT(*) FROM approvals
                         WHERE subject_type='gate_release' AND workitem_id=?1
                           AND status='requested' AND reason LIKE '%approval_drift%'",
                        [&inputs.workitem_id],
                        |r| r.get(0),
                    )
                    .unwrap_or(0))
                })?;
                Ok(if drifted == 0 {
                    InputState::Pass
                } else {
                    InputState::Fail
                })
            }
            _ => {
                // policy：无 pending 阻塞审批即策略面稳定。
                Ok(
                    if sg_policy::pending_blocking_count(store, &inputs.workitem_id)? == 0 {
                        InputState::Pass
                    } else {
                        InputState::Fail
                    },
                )
            }
        },
        // 人工确认事实：当前活跃 attempt + 本验收项 digest 精确匹配的 confirmed 行
        //（digest 绑定验收项身份——A 项的确认不能放行 B 项）。
        AcceptanceVerifier::ManualConfirm { .. } => {
            let attempt = crate::attempt::ensure_active(store, &inputs.workitem_id, &inputs.gate)?;
            let confirmed: i64 = store.with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM gate_manual_confirmations
                     WHERE workitem_id=?1 AND gate=?2 AND stage_attempt_id=?3
                       AND acceptance_item_digest=?4 AND state='confirmed'",
                    rusqlite::params![inputs.workitem_id, inputs.gate, attempt.id, element_digest],
                    |r| r.get(0),
                )
                .unwrap_or(0))
            })?;
            Ok(if confirmed > 0 {
                InputState::Pass
            } else {
                InputState::Fail
            })
        }
    }
}

/// 合并进 GateResult：结构化项失败以 `acceptance:<verifier>(:<detail>)[:unavailable]`
/// 记入 failed_inputs（六输入之外追加；passed 重算）。
pub fn merge_into_result(result: &mut GateResult, outcomes: &[ElementOutcome]) {
    for o in outcomes {
        if o.state != InputState::Pass {
            let unavailable = !sg_workflow::acceptance::enabled() && o.state == InputState::Fail;
            let token = if unavailable {
                format!(
                    "acceptance_evaluator_unavailable:{}({})",
                    o.verifier, o.detail
                )
            } else {
                format!("acceptance:{}({})", o.verifier, o.detail)
            };
            result.failed_inputs.push(token);
        }
    }
    result.passed = result.failed_inputs.is_empty();
}
