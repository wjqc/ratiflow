//! 关卡放行事务（ADR-030 M2 / 蓝图 §4.2）。
//! evaluate 只计算；requestRelease 冻结 StageOutputPackage + digest 并创建 gate_release 审批；
//! decideRelease 重查 digest/有效期/身份后原子推进：批准后 attempt 置 approved、
//! current_gate 前移、下一关 attempt 创建（幂等可续）；拒绝/要求修改只记录决定，不创建下一关。
//! Agent、renderer 均不能替代此事务；workitem.setStage 已从协议移除。

use serde::Serialize;
use sg_policy::Risk;
use sg_provenance::{node_type, relation, EdgeInput, NodeInput};
use sg_store::{ids, objects, outbox, timefmt, Error, Store};
use sha2::{Digest, Sha256};

use crate::attempt::{self, StageAttempt};
use crate::gate;
use crate::Gate;

#[derive(Debug, Clone, Serialize)]
pub struct ReleaseRequest {
    pub id: String,
    pub stage_attempt_id: String,
    pub output_package_id: String,
    pub approval_id: Option<String>,
    pub release_digest: String,
    pub state: String,
    pub created_at: String,
    pub decided_at: Option<String>,
}

const RR_COLUMNS: &str =
    "id, stage_attempt_id, output_package_id, approval_id, release_digest, state, created_at, decided_at";

fn row_rr(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReleaseRequest> {
    Ok(ReleaseRequest {
        id: r.get(0)?,
        stage_attempt_id: r.get(1)?,
        output_package_id: r.get(2)?,
        approval_id: r.get(3)?,
        release_digest: r.get(4)?,
        state: r.get(5)?,
        created_at: r.get(6)?,
        decided_at: r.get(7)?,
    })
}

fn rr_by_id(store: &Store, id: &str) -> Result<Option<ReleaseRequest>, Error> {
    store.with_conn(|conn| {
        let rr = conn
            .query_row(
                &format!("SELECT {RR_COLUMNS} FROM gate_release_requests WHERE id=?1"),
                [id],
                row_rr,
            )
            .ok();
        Ok(rr)
    })
}

fn rr_by_approval(store: &Store, approval_id: &str) -> Result<Option<ReleaseRequest>, Error> {
    store.with_conn(|conn| {
        let rr = conn
            .query_row(
                &format!("SELECT {RR_COLUMNS} FROM gate_release_requests WHERE approval_id=?1"),
                [approval_id],
                row_rr,
            )
            .ok();
        Ok(rr)
    })
}

fn pending_rr_for_attempt(
    store: &Store,
    attempt_id: &str,
) -> Result<Option<ReleaseRequest>, Error> {
    store.with_conn(|conn| {
        let rr = conn
            .query_row(
                &format!(
                    "SELECT {RR_COLUMNS} FROM gate_release_requests
                     WHERE stage_attempt_id=?1 AND state='pending' ORDER BY created_at DESC LIMIT 1"
                ),
                [attempt_id],
                row_rr,
            )
            .ok();
        Ok(rr)
    })
}

/// 放行 digest = sha256(workitem|gate|attempt|entry snapshot|output package manifest|policy version)。
fn release_digest(
    workitem_id: &str,
    gate: &str,
    attempt_id: &str,
    entry_snapshot: &str,
    manifest_sha: &str,
    policy_version: &str,
) -> String {
    let mut hasher = Sha256::new();
    for part in [
        workitem_id,
        gate,
        attempt_id,
        entry_snapshot,
        manifest_sha,
        policy_version,
    ] {
        hasher.update(part.as_bytes());
        hasher.update(b"|");
    }
    ids::hex(&hasher.finalize())
}

/// 冻结输出包清单：基线修订（含内容哈希）+ 本关证据（含核验态）+ 需求覆盖摘要。
/// 序列化键由 serde_json BTreeMap 排序，保证同内容字节一致（digest 可复算）。
fn build_manifest(
    store: &Store,
    workitem_id: &str,
    gate: Gate,
    attempt: &StageAttempt,
) -> Result<(serde_json::Value, String), Error> {
    let baseline = sg_artifact::latest_baseline(store, workitem_id, gate.as_str())?;
    let mut revisions = Vec::new();
    if let Some(base) = &baseline {
        let map: serde_json::Map<String, serde_json::Value> =
            serde_json::from_value(base.revision_map.clone()).unwrap_or_default();
        for (artifact_id, rev_value) in &map {
            let rev_id = rev_value.as_str().unwrap_or_default().to_string();
            let sha: String = store
                .with_conn(|conn| {
                    conn.query_row(
                        "SELECT content_sha256 FROM revisions WHERE id=?1",
                        [&rev_id],
                        |r| r.get(0),
                    )
                    .map_err(Error::from)
                })
                .unwrap_or_default();
            revisions.push(serde_json::json!({
                "artifactId": artifact_id,
                "revisionId": rev_id,
                "contentSha256": sha,
            }));
        }
    }
    revisions.sort_by(|a, b| a["revisionId"].as_str().cmp(&b["revisionId"].as_str()));
    let evidences: Vec<serde_json::Value> =
        sg_evidence::list(store, workitem_id, Some(gate.as_str()))?
            .into_iter()
            .map(|e| {
                serde_json::json!({
                    "id": e.id,
                    "kind": e.kind,
                    "sha256": e.object_sha256,
                    "verified": e.verified,
                })
            })
            .collect();
    let coverage_value = match crate::requirements::latest_revision_id(store, workitem_id)? {
        Some(rev) => sg_provenance::coverage(store, workitem_id, &rev)?,
        None => {
            serde_json::json!({"totalItems": 0, "coveredCount": 0, "verifiedCount": 0, "items": []})
        }
    };
    let manifest = serde_json::json!({
        "workItemId": workitem_id,
        "gate": gate.as_str(),
        "attemptId": attempt.id,
        "attemptNo": attempt.attempt_no,
        "entrySnapshotId": attempt.entry_snapshot_id,
        "baselineId": baseline.as_ref().map(|b| b.id.clone()),
        "inputsSha256": baseline.as_ref().map(|b| b.inputs_sha256.clone()),
        "revisions": revisions,
        "evidences": evidences,
        "coverage": {
            "totalItems": coverage_value["totalItems"],
            "coveredCount": coverage_value["coveredCount"],
            "verifiedCount": coverage_value["verifiedCount"],
        },
    });
    let canonical = serde_json::to_string(&manifest).unwrap_or_default();
    let info = objects::put(store, canonical.as_bytes(), objects::PutOptions::default())
        .map_err(|e| Error::Message(format!("store manifest: {e}")))?;
    Ok((manifest, info.sha256))
}

/// 确保谱系节点存在（幂等注册），输出包条目落 stage_output_items。
fn attach_output_items(
    store: &Store,
    workitem_id: &str,
    package_id: &str,
    gate: Gate,
    manifest: &serde_json::Value,
) -> Result<(), Error> {
    let mut ordinal = 0i64;
    let mut attach = |store: &Store,
                      node_type_name: &str,
                      entity_id: &str,
                      digest: &str,
                      verification: &str,
                      role: &str|
     -> Result<(), Error> {
        sg_provenance::register_node(
            store,
            &NodeInput {
                project_id: "",
                workitem_id,
                node_type: node_type_name,
                entity_id,
                content_digest: digest,
                verification_state: verification,
            },
        )?;
        let node_id = sg_provenance::node_id_by_entity(store, node_type_name, entity_id)?
            .ok_or_else(|| Error::Message("trace_incomplete: 输出包节点注册失败".into()))?;
        store.with_conn(|conn| {
            conn.execute(
                "INSERT OR IGNORE INTO stage_output_items(package_id, node_id, role, ordinal)
                 VALUES (?1,?2,?3,?4)",
                rusqlite::params![package_id, node_id, role, ordinal],
            )?;
            Ok(())
        })?;
        ordinal += 1;
        Ok(())
    };
    for rev in manifest["revisions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        attach(
            store,
            node_type::ARTIFACT_REVISION,
            rev["revisionId"].as_str().unwrap_or_default(),
            rev["contentSha256"].as_str().unwrap_or_default(),
            "verified",
            "artifact_revision",
        )?;
    }
    let evidences = sg_evidence::list(store, workitem_id, Some(gate.as_str()))?;
    for ev in &evidences {
        attach(
            store,
            node_type::EVIDENCE,
            &ev.id,
            &ev.object_sha256,
            if ev.verified {
                "verified"
            } else {
                "unverified"
            },
            "evidence",
        )?;
    }
    if let Some(rev_id) = crate::requirements::latest_revision_id(store, workitem_id)? {
        for item in crate::requirements::items(store, &rev_id)? {
            if item.status != "active" {
                continue;
            }
            attach(
                store,
                node_type::REQUIREMENT_ITEM,
                &item.id,
                &item.body_sha256,
                "verified",
                "requirement_item",
            )?;
        }
    }
    let _ = gate;
    Ok(())
}

/// requestRelease：确认最新 GateEvaluation 全 Pass → 冻结输出包 → 创建 gate_release 审批 →
/// attempt awaiting_user_approval。同内容重复请求幂等；输出漂移后旧 pending 自动失效。
pub fn request_release(
    store: &Store,
    workitem_id: &str,
    gate_name: &str,
    policy_version: &str,
    ttl_secs: i64,
) -> Result<serde_json::Value, Error> {
    let gate = Gate::parse(gate_name)
        .ok_or_else(|| Error::Message("gate_release_required: unknown gate".into()))?;
    let wi = crate::get(store, workitem_id)?;
    if wi.current_gate != gate.as_str() {
        return Err(Error::Message(format!(
            "gate_release_required: 当前关为 {}，不能放行 {}",
            wi.current_gate,
            gate.as_str()
        )));
    }
    // 1. 最新 GateEvaluation 必须全 Pass，且其评估输入与当前状态逐字节一致
    //    （P0-2：评估通过后输出/证据/审批变化 → 旧评估过期，必须重新评估）。
    let (_, stored_inputs, evaluation) = gate::latest_full(store, workitem_id, gate.as_str())?
        .ok_or_else(|| {
            Error::Message("gate_release_required: 需先通过技术门禁评估（gate.evaluate）".into())
        })?;
    if !evaluation.passed {
        return Err(Error::Message(
            "gate_release_required: 需先通过技术门禁评估（gate.evaluate）".into(),
        ));
    }
    let fresh_inputs = gate::build_inputs(store, workitem_id, gate.as_str())?;
    let fresh_json = serde_json::to_string(&fresh_inputs).unwrap_or_else(|_| "{}".into());
    if fresh_json != stored_inputs {
        return Err(Error::Message(
            "gate_release_required: 门禁评估已过期（评估输入已变化），请重新评估".into(),
        ));
    }
    // P0-1：需求覆盖强制——任一 active 需求项未被任何产物 satisfies/implements
    // 即拒绝放行（蓝图完成检查表：断链 fail-closed）。
    match crate::requirements::latest_revision_id(store, workitem_id)? {
        Some(rev) => {
            let cov = sg_provenance::coverage(store, workitem_id, &rev)?;
            let uncovered: Vec<String> = cov["items"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter(|i| {
                    i["status"].as_str() == Some("active") && i["covered"].as_bool() == Some(false)
                })
                .filter_map(|i| i["requirementKey"].as_str().map(String::from))
                .collect();
            if cov["totalItems"].as_u64().unwrap_or(0) == 0 {
                return Err(Error::Message(
                    "trace_incomplete: 需求修订无可追溯条目，无法放行".into(),
                ));
            }
            if !uncovered.is_empty() {
                return Err(Error::Message(format!(
                    "trace_incomplete: 需求覆盖不足，未关联任何产物的需求项：{}",
                    uncovered.join("、")
                )));
            }
        }
        None => {
            return Err(Error::Message(
                "trace_incomplete: 工作项尚无需求修订，无法放行".into(),
            ));
        }
    }
    let attempt = attempt::advance_to_review_ready(store, workitem_id, gate)?;
    if attempt.state == "awaiting_user_approval" {
        if let Some(existing) = pending_rr_for_attempt(store, &attempt.id)? {
            return Ok(serde_json::to_value(existing).unwrap_or_default());
        }
    }

    // 2. 冻结输出包 + digest。
    let (manifest, manifest_sha) = build_manifest(store, workitem_id, gate, &attempt)?;
    let digest = release_digest(
        workitem_id,
        gate.as_str(),
        &attempt.id,
        &attempt.entry_snapshot_id,
        &manifest_sha,
        policy_version,
    );
    // 同内容重提幂等：已有同 digest 的 pending 请求原样返回（不重复建审批）。
    if let Some(existing) = pending_rr_for_attempt(store, &attempt.id)? {
        if existing.release_digest == digest {
            return Ok(serde_json::to_value(existing).unwrap_or_default());
        }
    }
    // 同 (attempt, digest) 幂等复用包；不同 digest 的旧 pending 失效（AC-SW-03 前置）。
    let package_id = match package_by_digest(store, &attempt.id, &digest)? {
        Some((existing_id, _)) => existing_id,
        None => {
            if let Some(old) = pending_rr_for_attempt(store, &attempt.id)? {
                supersede_rr(store, &old.id)?;
            }
            let package_no: i64 = store.with_conn(|conn| {
                conn.query_row(
                    "SELECT COALESCE(MAX(package_no),0) FROM stage_output_packages WHERE stage_attempt_id=?1",
                    [&attempt.id],
                    |r| r.get(0),
                )
                .map_err(Error::from)
            })?;
            let pid = ids::new_id("pkg");
            let evaluation_id = gate::latest_id(store, workitem_id, gate.as_str())?
                .ok_or_else(|| Error::Message("gate_release_required: 门禁评估记录缺失".into()))?;
            // 评估行 id 即 latest_full 校验过的那行（新鲜度已确认）。
            store.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO stage_output_packages(id, stage_attempt_id, package_no, manifest_object_sha256, digest, gate_evaluation_id, trace_coverage_json, created_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    rusqlite::params![
                        pid,
                        attempt.id,
                        package_no + 1,
                        manifest_sha,
                        digest,
                        evaluation_id,
                        serde_json::to_string(&manifest["coverage"]).unwrap_or_else(|_| "{}".into()),
                        timefmt::now()
                    ],
                )?;
                Ok(())
            })?;
            attach_output_items(store, workitem_id, &pid, gate, &manifest)?;
            pid
        }
    };

    // 3. 放行请求 + 审批（同一调用序列内回填 approval_id）。
    let rr_id = ids::new_id("grr");
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO gate_release_requests(id, stage_attempt_id, output_package_id, approval_id, release_digest, state, created_at)
             VALUES (?1,?2,?3,NULL,?4,'pending',?5)",
            rusqlite::params![rr_id, attempt.id, package_id, digest, timefmt::now()],
        )?;
        Ok(())
    })?;
    let approval = sg_policy::request_approval(
        store,
        "gate_release",
        &rr_id,
        &digest,
        Risk::High,
        "关卡放行：批准后进入下一关",
        ttl_secs,
        Some(workitem_id),
        Some(&attempt.id),
    )?;
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE gate_release_requests SET approval_id=?1 WHERE id=?2",
            rusqlite::params![approval.id, rr_id],
        )?;
        Ok(())
    })?;
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE stage_attempts SET active_output_package_id=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![package_id, timefmt::now(), attempt.id],
        )?;
        Ok(())
    })?;
    let _ = attempt::transition(store, &attempt.id, "awaiting_user_approval")?;
    outbox::emit(
        store,
        "workitem",
        workitem_id,
        "gate.release_requested",
        serde_json::json!({
            "workitemId": workitem_id,
            "gate": gate.as_str(),
            "attemptId": attempt.id,
            "releaseRequestId": rr_id,
            "approvalId": approval.id,
            "digest": digest,
        }),
    )?;
    Ok(
        serde_json::to_value(rr_by_id(store, &rr_id)?.unwrap_or_else(|| ReleaseRequest {
            id: rr_id.clone(),
            stage_attempt_id: String::new(),
            output_package_id: String::new(),
            approval_id: Some(approval.id),
            release_digest: digest,
            state: "pending".into(),
            created_at: String::new(),
            decided_at: None,
        }))
        .unwrap_or_default(),
    )
}

fn package_by_digest(
    store: &Store,
    attempt_id: &str,
    digest: &str,
) -> Result<Option<(String, String)>, Error> {
    store.with_conn(|conn| {
        let row = conn
            .query_row(
                "SELECT id, manifest_object_sha256 FROM stage_output_packages
                 WHERE stage_attempt_id=?1 AND digest=?2 LIMIT 1",
                [attempt_id, digest],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .ok();
        Ok(row)
    })
}

fn supersede_rr(store: &Store, rr_id: &str) -> Result<(), Error> {
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE gate_release_requests SET state='superseded', decided_at=?1 WHERE id=?2 AND state='pending'",
            rusqlite::params![timefmt::now(), rr_id],
        )?;
        Ok(())
    })
}

/// decideRelease：重查 digest/有效期/身份 → 记录决定 → 原子推进（蓝图 §4.2）。
/// approved：attempt approved + current_gate 前移 + 下一关 attempt 创建（幂等可恢复）；
/// rejected：只记录决定；changes_requested：attempt 回 running 继续当前尝试。
pub fn decide_release(
    store: &Store,
    approval_id: &str,
    decision: &str,
    decided_by: &str,
    reason: &str,
    policy_version: &str,
) -> Result<serde_json::Value, Error> {
    if !matches!(decision, "approved" | "rejected" | "changes_requested") {
        return Err(Error::Message(
            "decision must be approved|rejected|changes_requested".into(),
        ));
    }
    if decided_by.trim().is_empty() {
        return Err(Error::Message("approval_invalid: 缺少审批人身份".into()));
    }
    let rr = rr_by_approval(store, approval_id)?
        .ok_or_else(|| Error::Message("approval_invalid: 审批不对应任何关卡放行请求".into()))?;
    let attempt = attempt::get(store, &rr.stage_attempt_id)?;
    let gate = Gate::parse(&attempt.gate).ok_or_else(|| Error::Message("bad gate".into()))?;
    let workitem_id = attempt.workitem_id.clone();

    // 幂等重放：请求已终态 → 校验决定一致性后原样返回。
    if rr.state != "pending" {
        let expected = match decision {
            "approved" => "approved",
            "rejected" => "rejected",
            _ => "changes_requested",
        };
        if rr.state == expected {
            return release_view(store, &rr, &attempt, &workitem_id);
        }
        return Err(Error::Message(format!(
            "approval_invalid: 放行请求已处于 {} 状态，不能以 {decision} 重决",
            rr.state
        )));
    }

    // 有效期 + digest 漂移重查（AC-SW-03）。
    sg_policy::expire_stale(store).map_err(|e| Error::Message(e.to_string()))?;
    let approval = sg_policy::get(store, approval_id)?;
    if approval.status == "expired" {
        store.with_conn(|conn| {
            conn.execute(
                "UPDATE gate_release_requests SET state='expired', decided_at=?1 WHERE id=?2",
                rusqlite::params![timefmt::now(), rr.id],
            )?;
            Ok(())
        })?;
        return Err(Error::Message("approval_expired: 放行审批已过期".into()));
    }
    if approval.status == "requested" {
        let (_, current_manifest_sha) = build_manifest(store, &workitem_id, gate, &attempt)?;
        let current_digest = release_digest(
            &workitem_id,
            gate.as_str(),
            &attempt.id,
            &attempt.entry_snapshot_id,
            &current_manifest_sha,
            policy_version,
        );
        if current_digest != rr.release_digest {
            supersede_rr(store, &rr.id)?;
            sg_policy::expire(store, approval_id, "输出已变化，旧放行审批失效")?;
            return Err(Error::Message(
                "output_digest_changed: 输出已变化，旧审批失效（AC-SW-03）".into(),
            ));
        }
    }

    match decision {
        "approved" => {
            if approval.status == "requested" {
                sg_policy::decide(store, approval_id, "approved", decided_by, reason)
                    .map_err(|e| Error::Message(e.to_string()))?;
            } else if approval.status != "approved" {
                return Err(Error::Message(format!(
                    "approval_invalid: 审批状态 {} 不能批准",
                    approval.status
                )));
            }
            complete_approve(store, &rr, &attempt, &gate, decided_by)?;
        }
        "rejected" => {
            if approval.status == "requested" {
                sg_policy::decide(store, approval_id, "rejected", decided_by, reason)
                    .map_err(|e| Error::Message(e.to_string()))?;
            } else if approval.status != "rejected" {
                return Err(Error::Message(format!(
                    "approval_invalid: 审批状态 {} 不能拒绝",
                    approval.status
                )));
            }
            let _ = attempt::transition(store, &attempt.id, "rejected")?;
            store.with_conn(|conn| {
                conn.execute(
                    "UPDATE gate_release_requests SET state='rejected', decided_at=?1 WHERE id=?2",
                    rusqlite::params![timefmt::now(), rr.id],
                )?;
                Ok(())
            })?;
            outbox::emit(
                store,
                "workitem",
                &workitem_id,
                "gate.release_rejected",
                serde_json::json!({"workitemId": workitem_id, "gate": gate.as_str(), "by": decided_by}),
            )?;
        }
        _ => {
            // changes_requested：保留当前 attempt（AC-SW-04），回 running 继续修订。
            if approval.status == "requested" {
                sg_policy::decide(store, approval_id, "changes_requested", decided_by, reason)
                    .map_err(|e| Error::Message(e.to_string()))?;
            } else if approval.status != "changes_requested" {
                return Err(Error::Message(format!(
                    "approval_invalid: 审批状态 {} 不能要求修改",
                    approval.status
                )));
            }
            let awaiting = attempt::transition(store, &attempt.id, "changes_requested")?;
            let _ = attempt::transition(store, &awaiting.id, "running")?;
            store.with_conn(|conn| {
                conn.execute(
                    "UPDATE stage_attempts SET active_output_package_id=NULL, updated_at=?1 WHERE id=?2",
                    rusqlite::params![timefmt::now(), attempt.id],
                )?;
                conn.execute(
                    "UPDATE gate_release_requests SET state='changes_requested', decided_at=?1 WHERE id=?2",
                    rusqlite::params![timefmt::now(), rr.id],
                )?;
                Ok(())
            })?;
            outbox::emit(
                store,
                "workitem",
                &workitem_id,
                "gate.changes_requested",
                serde_json::json!({"workitemId": workitem_id, "gate": gate.as_str(), "by": decided_by}),
            )?;
        }
    }
    let rr =
        rr_by_id(store, &rr.id)?.ok_or_else(|| Error::Message("release request missing".into()))?;
    let attempt = attempt::get(store, &rr.stage_attempt_id)?;
    release_view(store, &rr, &attempt, &workitem_id)
}

/// 批准推进（幂等）：attempt approved → 投影 pass_gate（stage passed + current_gate 前移）
/// → 下一关 attempt（不存在才创建）→ 谱系 approval 节点/边 → 请求落 approved。
fn complete_approve(
    store: &Store,
    rr: &ReleaseRequest,
    attempt: &StageAttempt,
    gate: &Gate,
    decided_by: &str,
) -> Result<(), Error> {
    let workitem_id = attempt.workitem_id.clone();
    if attempt.state == "awaiting_user_approval" {
        attempt::transition(store, &attempt.id, "approved")?;
    } else if attempt.state != "approved" {
        return Err(Error::Message(format!(
            "approval_invalid: attempt 状态 {} 不能推进",
            attempt.state
        )));
    }
    // 投影推进（幂等：stage 已 passed 时 pass_gate 直接返回）。
    crate::pass_gate(store, &workitem_id, *gate)?;
    // 下一关 attempt（AC：放行与创建下一关在同一事务语义内；幂等：已存在则跳过）。
    if let Some(next) = gate.next() {
        if attempt::latest_for_gate(store, &workitem_id, next)?.is_none() {
            attempt::create(store, &workitem_id, next, Some(&attempt.id))?;
        }
    }
    // 谱系：approval 节点 approves → attempt 节点。
    sg_provenance::register_node(
        store,
        &NodeInput {
            project_id: "",
            workitem_id: &workitem_id,
            node_type: node_type::APPROVAL,
            entity_id: rr.approval_id.as_deref().unwrap_or(""),
            content_digest: &rr.release_digest,
            verification_state: "verified",
        },
    )?;
    sg_provenance::add_edge(
        store,
        &EdgeInput {
            workitem_id: &workitem_id,
            from_node_type: node_type::APPROVAL,
            from_entity_id: rr.approval_id.as_deref().unwrap_or(""),
            relation: relation::APPROVES,
            to_node_type: node_type::STAGE_ATTEMPT,
            to_entity_id: &attempt.id,
            stage_attempt_id: "",
            created_by_run_id: "",
        },
    )?;
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE gate_release_requests SET state='approved', decided_at=?1 WHERE id=?2 AND state='pending'",
            rusqlite::params![timefmt::now(), rr.id],
        )?;
        Ok(())
    })?;
    let _ = decided_by;
    outbox::emit(
        store,
        "workitem",
        &workitem_id,
        "gate.release_approved",
        serde_json::json!({"workitemId": workitem_id, "gate": gate.as_str(), "attemptId": attempt.id}),
    )?;
    Ok(())
}

/// 输出漂移的主动失效（createDraft/evidence.record/freezeBaseline 后调用）：
/// 重新计算当前 digest，变化即作废 pending 请求与审批（AC-SW-03）。
pub fn invalidate_pending_if_drift(
    store: &Store,
    workitem_id: &str,
    gate_name: &str,
    policy_version: &str,
) -> Result<bool, Error> {
    let Some(gate) = Gate::parse(gate_name) else {
        return Ok(false);
    };
    let Some(attempt) = attempt::active_for_gate(store, workitem_id, gate)? else {
        return Ok(false);
    };
    if attempt.state != "awaiting_user_approval" {
        return Ok(false);
    }
    let Some(rr) = pending_rr_for_attempt(store, &attempt.id)? else {
        return Ok(false);
    };
    let (_, manifest_sha) = build_manifest(store, workitem_id, gate, &attempt)?;
    let digest = release_digest(
        workitem_id,
        gate.as_str(),
        &attempt.id,
        &attempt.entry_snapshot_id,
        &manifest_sha,
        policy_version,
    );
    if digest == rr.release_digest {
        return Ok(false);
    }
    supersede_rr(store, &rr.id)?;
    if let Some(appr) = &rr.approval_id {
        sg_policy::expire(store, appr, "输出已变化，旧放行审批失效")?;
    }
    let _ = attempt::transition(store, &attempt.id, "changes_requested")
        .and_then(|a| attempt::transition(store, &a.id, "running"));
    outbox::emit(
        store,
        "workitem",
        workitem_id,
        "gate.changes_requested",
        serde_json::json!({"workitemId": workitem_id, "gate": gate.as_str(), "reason": "output_digest_changed"}),
    )?;
    Ok(true)
}

/// 崩溃恢复（启动时）：审批已决但推进未完成的放行请求补完（蓝图 §4.2 单事务语义的补偿路径）。
pub fn resume_pending(store: &Store) -> Result<usize, Error> {
    let pending: Vec<(ReleaseRequest, String, String)> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {RR_COLUMNS}, a.status, COALESCE(a.decided_by,'')
             FROM gate_release_requests r JOIN approvals a ON a.id = r.approval_id
             WHERE r.state='pending' AND a.status IN ('approved','rejected','changes_requested')"
        ))?;
        let rows = stmt.query_map([], |r| {
            Ok((
                ReleaseRequest {
                    id: r.get(0)?,
                    stage_attempt_id: r.get(1)?,
                    output_package_id: r.get(2)?,
                    approval_id: r.get(3)?,
                    release_digest: r.get(4)?,
                    state: r.get(5)?,
                    created_at: r.get(6)?,
                    decided_at: r.get(7)?,
                },
                r.get::<_, String>(8)?,
                r.get::<_, String>(9)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;
    let mut count = 0;
    for (rr, status, decided_by) in pending {
        let attempt = match attempt::get(store, &rr.stage_attempt_id) {
            Ok(a) => a,
            Err(_) => continue,
        };
        let gate = match Gate::parse(&attempt.gate) {
            Some(g) => g,
            None => continue,
        };
        let result = match status.as_str() {
            "approved" => complete_approve(store, &rr, &attempt, &gate, &decided_by),
            "rejected" => attempt::transition(store, &attempt.id, "rejected").map(|_| ()),
            _ => Ok(()),
        };
        if result.is_ok() {
            count += 1;
        }
    }
    Ok(count)
}

fn release_view(
    store: &Store,
    rr: &ReleaseRequest,
    attempt: &StageAttempt,
    workitem_id: &str,
) -> Result<serde_json::Value, Error> {
    let mut v = serde_json::to_value(rr).unwrap_or_default();
    v["attempt"] = serde_json::to_value(attempt).unwrap_or_default();
    v["workitemId"] = serde_json::json!(workitem_id);
    let _ = store;
    Ok(v)
}

/// 某关全部放行请求（stage.package 读模型用，新→旧）。
pub fn list_for_gate(
    store: &Store,
    workitem_id: &str,
    gate_name: &str,
) -> Result<Vec<ReleaseRequest>, Error> {
    let Some(gate) = Gate::parse(gate_name) else {
        return Ok(vec![]);
    };
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT r.id, r.stage_attempt_id, r.output_package_id, r.approval_id, r.release_digest, r.state, r.created_at, r.decided_at
             FROM gate_release_requests r
             JOIN stage_attempts a ON a.id = r.stage_attempt_id
             WHERE a.workitem_id=?1 AND a.gate=?2 ORDER BY r.created_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![workitem_id, gate.as_str()], row_rr)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

/// 放行详情（审批中心用）：请求 + 审批 + 输出包（含条目角色）+ 覆盖。
pub fn get_release(store: &Store, release_id: &str) -> Result<serde_json::Value, Error> {
    let rr = rr_by_id(store, release_id)?
        .ok_or_else(|| Error::Message(format!("not_found: release {release_id}")))?;
    let attempt = attempt::get(store, &rr.stage_attempt_id)?;
    let package: serde_json::Value = store.with_conn(|conn| {
        conn.query_row(
            "SELECT id, package_no, manifest_object_sha256, digest, trace_coverage_json, created_at
             FROM stage_output_packages WHERE id=?1",
            [&rr.output_package_id],
            |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, String>(0)?,
                    "packageNo": r.get::<_, i64>(1)?,
                    "manifestObjectSha256": r.get::<_, String>(2)?,
                    "digest": r.get::<_, String>(3)?,
                    "coverage": serde_json::from_str::<serde_json::Value>(&r.get::<_, String>(4)?).unwrap_or_default(),
                    "createdAt": r.get::<_, String>(5)?,
                }))
            },
        )
        .map_err(|_| Error::Message("not_found: output package".into()))
    })?;
    let items: Vec<serde_json::Value> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT i.role, i.ordinal, n.node_type, n.entity_id, n.verification_state
             FROM stage_output_items i JOIN provenance_nodes n ON n.id = i.node_id
             WHERE i.package_id=?1 ORDER BY i.ordinal",
        )?;
        let rows = stmt.query_map([&rr.output_package_id], |r| {
            Ok(serde_json::json!({
                "role": r.get::<_, String>(0)?,
                "ordinal": r.get::<_, i64>(1)?,
                "nodeType": r.get::<_, String>(2)?,
                "entityId": r.get::<_, String>(3)?,
                "verificationState": r.get::<_, String>(4)?,
            }))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;
    let approval = match &rr.approval_id {
        Some(id) => serde_json::to_value(sg_policy::get(store, id).ok()).unwrap_or_default(),
        None => serde_json::Value::Null,
    };
    Ok(serde_json::json!({
        "releaseRequest": serde_json::to_value(&rr).unwrap_or_default(),
        "attempt": serde_json::to_value(&attempt).unwrap_or_default(),
        "package": package,
        "items": items,
        "approval": approval,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EvaluateInputs, InputState, StageState};
    use sg_store::Store;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-rel-{}-{}",
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

    fn pass_inputs(workitem_id: &str, gate: &str) -> EvaluateInputs {
        EvaluateInputs {
            workitem_id: workitem_id.into(),
            gate: gate.into(),
            required_artifacts_frozen: InputState::Pass,
            required_checks_passed: InputState::Pass,
            approvals_valid: InputState::Pass,
            evidence_complete: InputState::Pass,
            no_blocking_risk: InputState::Pass,
            inputs_current: InputState::Pass,
        }
    }

    /// 需求关走到“可请求放行”的前置：评估通过（输出包需要有基线与证据）。
    /// P0-1：把工件修订链接到全部 active 需求项（对齐 dispatch requirementKeys 行为）。
    fn link_coverage(store: &Store, workitem_id: &str, artifact_revision_id: &str) {
        sg_provenance::register_node(
            store,
            &sg_provenance::NodeInput {
                project_id: "",
                workitem_id,
                node_type: sg_provenance::node_type::ARTIFACT_REVISION,
                entity_id: artifact_revision_id,
                content_digest: "test",
                verification_state: "verified",
            },
        )
        .unwrap();
        let req_rev = crate::requirements::latest_revision_id(store, workitem_id)
            .unwrap()
            .expect("需求修订存在");
        for item in crate::requirements::items(store, &req_rev).unwrap() {
            if item.status != "active" {
                continue;
            }
            sg_provenance::add_edge(
                store,
                &sg_provenance::EdgeInput {
                    workitem_id,
                    from_node_type: sg_provenance::node_type::ARTIFACT_REVISION,
                    from_entity_id: artifact_revision_id,
                    relation: sg_provenance::relation::SATISFIES,
                    to_node_type: sg_provenance::node_type::REQUIREMENT_ITEM,
                    to_entity_id: &item.id,
                    stage_attempt_id: "",
                    created_by_run_id: "",
                },
            )
            .unwrap();
        }
    }

    fn prepare_gate(store: &Store, workitem_id: &str, gate: &str) {
        crate::requirements::import_revision(
            store,
            workitem_id,
            "requirement.md",
            "# 需求\n\n- 需求项一\n",
            "inline",
            "t",
            "verified",
        )
        .unwrap();
        let art = sg_artifact::create_artifact(store, workitem_id, "prd", "PRD").unwrap();
        let rev = sg_artifact::create_draft(store, &art.id, "PRD 内容").unwrap();
        sg_artifact::add_review(store, &rev.id, "pm", "approved", "", None).unwrap();
        sg_artifact::freeze(
            store,
            workitem_id,
            gate,
            std::slice::from_ref(&rev.id),
            "",
            "",
        )
        .unwrap();
        link_coverage(store, workitem_id, &rev.id);
        let ev = sg_evidence::record(
            store,
            &sg_evidence::RecordInput {
                workitem_id,
                gate,
                kind: "review",
                title: "评审",
                content: None,
                payload: "{}",
                source: "local",
            },
        )
        .unwrap();
        sg_evidence::verify(store, &ev.id, "pm").unwrap();
        let result =
            crate::gate::evaluate_and_record(store, &pass_inputs(workitem_id, gate)).unwrap();
        assert!(result.passed);
    }

    #[test]
    fn full_release_advances_gate_and_creates_next_attempt() {
        let s = setup();
        let wi = crate::create(&s, "pj", "放行流", "", None, &[]).unwrap();
        prepare_gate(&s, &wi.id, "requirements");

        // AC-SW-02：请求放行前，current_gate 不变。
        let rr = request_release(&s, &wi.id, "requirements", "pol-1", 3600).unwrap();
        let wi_now = crate::get(&s, &wi.id).unwrap();
        assert_eq!(
            wi_now.current_gate, "requirements",
            "评估/请求均不得推进关卡"
        );
        assert_eq!(rr["state"], serde_json::json!("pending"));
        assert!(rr["approval_id"].is_string(), "审批已回填关联");

        let attempt = attempt::active_for_gate(&s, &wi.id, Gate::Requirements)
            .unwrap()
            .unwrap();
        assert_eq!(attempt.state, "awaiting_user_approval");

        // 批准 → 原子推进 + 下一关 attempt 创建。
        let view = decide_release(
            &s,
            rr["approval_id"].as_str().unwrap(),
            "approved",
            "owner",
            "E2E",
            "pol-1",
        )
        .unwrap();
        let wi_now = crate::get(&s, &wi.id).unwrap();
        assert_eq!(wi_now.current_gate, "design", "批准后进入下一关");
        assert_eq!(view["attempt"]["state"], serde_json::json!("approved"));
        let design = attempt::latest_for_gate(&s, &wi.id, Gate::Design)
            .unwrap()
            .expect("下一关 attempt 已创建");
        assert_eq!(design.state, "prepared");
        // 幂等重放：同审批重复批准不重复推进。
        decide_release(
            &s,
            rr["approval_id"].as_str().unwrap(),
            "approved",
            "owner",
            "E2E",
            "pol-1",
        )
        .unwrap();
        assert_eq!(crate::get(&s, &wi.id).unwrap().current_gate, "design");
    }

    #[test]
    fn digest_drift_invalidates_pending_approval() {
        let s = setup();
        let wi = crate::create(&s, "pj", "漂移", "", None, &[]).unwrap();
        prepare_gate(&s, &wi.id, "requirements");
        let rr = request_release(&s, &wi.id, "requirements", "pol-1", 3600).unwrap();
        // 请求后输出变化：新增证据 → digest 漂移。
        let ev2 = sg_evidence::record(
            &s,
            &sg_evidence::RecordInput {
                workitem_id: &wi.id,
                gate: "requirements",
                kind: "manual",
                title: "补充核验",
                content: None,
                payload: "{}",
                source: "local",
            },
        )
        .unwrap();
        sg_evidence::verify(&s, &ev2.id, "qa").unwrap();
        let err = decide_release(
            &s,
            rr["approval_id"].as_str().unwrap(),
            "approved",
            "owner",
            "",
            "pol-1",
        )
        .unwrap_err();
        assert!(err.to_string().contains("output_digest_changed"));
        // 请求被作废，关卡不推进。
        let pending = list_for_gate(&s, &wi.id, "requirements").unwrap();
        assert!(pending.iter().all(|r| r.state != "pending"));
        assert_eq!(crate::get(&s, &wi.id).unwrap().current_gate, "requirements");
    }

    #[test]
    fn changes_requested_keeps_attempt_and_next_gate_absent() {
        let s = setup();
        let wi = crate::create(&s, "pj", "要求修改", "", None, &[]).unwrap();
        prepare_gate(&s, &wi.id, "requirements");
        let rr = request_release(&s, &wi.id, "requirements", "pol-1", 3600).unwrap();
        decide_release(
            &s,
            rr["approval_id"].as_str().unwrap(),
            "changes_requested",
            "owner",
            "请补充边界场景",
            "pol-1",
        )
        .unwrap();
        // AC-SW-04：同一 attempt 保留并回 running；下一关未创建。
        let attempt = attempt::latest_for_gate(&s, &wi.id, Gate::Requirements)
            .unwrap()
            .unwrap();
        assert_eq!(attempt.attempt_no, 1);
        assert_eq!(attempt.state, "running");
        assert!(attempt::latest_for_gate(&s, &wi.id, Gate::Design)
            .unwrap()
            .is_none());
        assert_eq!(crate::get(&s, &wi.id).unwrap().current_gate, "requirements");
        // 修改后重新评估、重新放行 → 批准 → 下一关创建。
        let _ = crate::gate::evaluate_and_record(&s, &pass_inputs(&wi.id, "requirements")).unwrap();
        let rr2 = request_release(&s, &wi.id, "requirements", "pol-1", 3600).unwrap();
        decide_release(
            &s,
            rr2["approval_id"].as_str().unwrap(),
            "approved",
            "owner",
            "v2 通过",
            "pol-1",
        )
        .unwrap();
        assert_eq!(crate::get(&s, &wi.id).unwrap().current_gate, "design");
        let attempt = attempt::latest_for_gate(&s, &wi.id, Gate::Requirements)
            .unwrap()
            .unwrap();
        assert_eq!(attempt.attempt_no, 1, "同一 attempt 继续，未新建");
    }

    #[test]
    fn rejection_creates_no_next_gate_and_retry_gets_new_attempt() {
        let s = setup();
        let wi = crate::create(&s, "pj", "拒绝", "", None, &[]).unwrap();
        prepare_gate(&s, &wi.id, "requirements");
        let rr = request_release(&s, &wi.id, "requirements", "pol-1", 3600).unwrap();
        decide_release(
            &s,
            rr["approval_id"].as_str().unwrap(),
            "rejected",
            "owner",
            "不通过",
            "pol-1",
        )
        .unwrap();
        assert_eq!(crate::get(&s, &wi.id).unwrap().current_gate, "requirements");
        assert!(attempt::latest_for_gate(&s, &wi.id, Gate::Design)
            .unwrap()
            .is_none());
        // 重试：拒绝后的关经 ensure_active 新建 attempt（attempt_no=2）。
        let _ = crate::gate::evaluate_and_record(&s, &pass_inputs(&wi.id, "requirements")).unwrap();
        let rr2 = request_release(&s, &wi.id, "requirements", "pol-1", 3600).unwrap();
        let attempt = attempt::latest_for_gate(&s, &wi.id, Gate::Requirements)
            .unwrap()
            .unwrap();
        assert_eq!(attempt.attempt_no, 2);
        decide_release(
            &s,
            rr2["approval_id"].as_str().unwrap(),
            "approved",
            "owner",
            "第二次通过",
            "pol-1",
        )
        .unwrap();
        assert_eq!(crate::get(&s, &wi.id).unwrap().current_gate, "design");
        let _ = StageState::NotStarted;
    }

    #[test]
    fn request_requires_passing_evaluation_and_current_gate() {
        let s = setup();
        let wi = crate::create(&s, "pj", "前置校验", "", None, &[]).unwrap();
        // 未评估即请求 → gate_release_required。
        let err = request_release(&s, &wi.id, "requirements", "pol-1", 3600).unwrap_err();
        assert!(err.to_string().contains("gate_release_required"));
        // 非当前关不可请求。
        let art = sg_artifact::create_artifact(&s, &wi.id, "prd", "PRD").unwrap();
        let rev = sg_artifact::create_draft(&s, &art.id, "x").unwrap();
        sg_artifact::add_review(&s, &rev.id, "r", "approved", "", None).unwrap();
        sg_artifact::freeze(&s, &wi.id, "requirements", &[rev.id], "", "").unwrap();
        let _ = crate::gate::evaluate_and_record(&s, &pass_inputs(&wi.id, "requirements")).unwrap();
        let err = request_release(&s, &wi.id, "design", "pol-1", 3600).unwrap_err();
        assert!(err.to_string().contains("gate_release_required"));
    }
}
