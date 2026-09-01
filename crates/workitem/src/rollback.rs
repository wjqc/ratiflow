//! 业务回滚（ADR-030 M3 / SG-RBK-003..008 / 蓝图 §7.2）。
//! preview 冻结影响清单+ActionDigest → request 创建 safety 快照+rollback 审批 →
//! decide 重查漂移/活跃 Run 后执行：恢复控制面投影、后续 attempt superseded、旧审批失效、
//! 目标关新建 attempt（带关前快照）。外部不可逆/需人工资源 → blocked，绝不伪造成功。
//! 主工作区永不触碰（SG-RBK-005）：工作区清单只读采集，恢复范围仅限控制面指针与受管状态。

use serde::Serialize;
use sg_policy::Risk;
use sg_store::{ids, objects, outbox, timefmt, Error, Store};
use sha2::{Digest, Sha256};

use crate::attempt::{self, ACTIVE_STATES};
use crate::snapshot::{self, Snapshot};
use crate::Gate;

#[derive(Debug, Clone, Serialize)]
pub struct RollbackOperation {
    pub id: String,
    pub workitem_id: String,
    pub source_attempt_id: String,
    pub target_snapshot_id: String,
    pub safety_snapshot_id: Option<String>,
    pub approval_id: Option<String>,
    pub impact_manifest_sha256: String,
    pub action_digest: String,
    pub state: String,
    pub blocked_reason: String,
    pub requested_by: String,
    pub decided_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

const OP_COLUMNS: &str = "id, workitem_id, source_attempt_id, target_snapshot_id, safety_snapshot_id, approval_id, impact_manifest_sha256, action_digest, state, blocked_reason, requested_by, decided_by, created_at, updated_at";

fn row_op(r: &rusqlite::Row<'_>) -> rusqlite::Result<RollbackOperation> {
    Ok(RollbackOperation {
        id: r.get(0)?,
        workitem_id: r.get(1)?,
        source_attempt_id: r.get(2)?,
        target_snapshot_id: r.get(3)?,
        safety_snapshot_id: r.get(4)?,
        approval_id: r.get(5)?,
        impact_manifest_sha256: r.get(6)?,
        action_digest: r.get(7)?,
        state: r.get(8)?,
        blocked_reason: r.get(9)?,
        requested_by: r.get(10)?,
        decided_by: r.get(11)?,
        created_at: r.get(12)?,
        updated_at: r.get(13)?,
    })
}

fn mark_failed(store: &Store, op_id: &str, workitem_id: &str, reason: &str) -> Result<(), Error> {
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE rollback_operations SET state='failed', blocked_reason=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![reason, timefmt::now(), op_id],
        )?;
        Ok(())
    })?;
    outbox::emit(
        store,
        "workitem",
        workitem_id,
        "rollback.failed",
        serde_json::json!({"workitemId": workitem_id, "operationId": op_id, "reason": reason}),
    )?;
    Ok(())
}

fn op_by_id(store: &Store, id: &str) -> Result<Option<RollbackOperation>, Error> {
    store.with_conn(|conn| {
        let op = conn
            .query_row(
                &format!("SELECT {OP_COLUMNS} FROM rollback_operations WHERE id=?1"),
                [id],
                row_op,
            )
            .ok();
        Ok(op)
    })
}

fn op_by_approval(store: &Store, approval_id: &str) -> Result<Option<RollbackOperation>, Error> {
    store.with_conn(|conn| {
        let op = conn
            .query_row(
                &format!("SELECT {OP_COLUMNS} FROM rollback_operations WHERE approval_id=?1"),
                [approval_id],
                row_op,
            )
            .ok();
        Ok(op)
    })
}

/// 回滚影响范围内的关（目标关及其后）。
fn affected_gates(target: Gate) -> Vec<Gate> {
    let all = Gate::ALL;
    let start = all.iter().position(|g| *g == target).unwrap_or(0);
    all[start..].to_vec()
}

/// 影响清单（preview 只读计算 + 冻结哈希）：
/// 受影响投影/将 supersede 的 attempt/将失效的审批/外部需人工资源/工作区漂移。
fn build_impact(
    store: &Store,
    workitem_id: &str,
    target: &Snapshot,
) -> Result<serde_json::Value, Error> {
    let target_attempt = attempt::get(store, &target.stage_attempt_id)?;
    let target_gate = Gate::parse(&target_attempt.gate)
        .ok_or_else(|| Error::Message("rollback_drift: 目标快照关卡非法".into()))?;
    let wi = crate::get(store, workitem_id)?;
    let gates: Vec<serde_json::Value> = affected_gates(target_gate)
        .iter()
        .map(|g| serde_json::json!({ "gate": g.as_str() }))
        .collect();
    let mut supersede = Vec::new();
    for g in affected_gates(target_gate) {
        for a in attempt::list(store, workitem_id)?
            .into_iter()
            .filter(|a| a.gate == g.as_str())
        {
            if ACTIVE_STATES.contains(&a.state.as_str()) || a.state == "approved" {
                supersede.push(serde_json::json!({
                    "attemptId": a.id, "gate": a.gate, "attemptNo": a.attempt_no, "state": a.state,
                }));
            }
        }
    }
    let mut approvals = Vec::new();
    for a in &supersede {
        let pending: Vec<String> = store.with_conn(|conn| {
            // 排除回滚操作自身的审批（否则 decide 重算影响时自指漂移）。
            let mut stmt = conn.prepare(
                "SELECT id FROM approvals
                 WHERE stage_attempt_id=?1 AND status='requested' AND subject_type<>'rollback'",
            )?;
            let rows = stmt.query_map([&a["attemptId"].as_str().unwrap_or_default()], |r| {
                r.get::<_, String>(0)
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })?;
        approvals.extend(pending);
    }
    let snap_resources = snapshot::resources(store, &target.id)?;
    let external_manual: Vec<serde_json::Value> = snap_resources
        .iter()
        .filter(|r| matches!(r.reversibility.as_str(), "manual" | "irreversible"))
        .map(|r| {
            serde_json::json!({
                "resourceType": r.resource_type,
                "resourceKey": r.resource_key,
                "versionRef": r.version_ref,
                "reversibility": r.reversibility,
            })
        })
        .collect();
    // 工作区漂移（只读提示；主工作区不由回滚恢复，SG-RBK-005）。
    let current_workspace = workspace_head(store, workitem_id)?;
    let target_head = snapshot_workspace_head(store, &target.id)?;
    let workspace_drift = match (&target_head, &current_workspace) {
        (Some(t), Some(c)) => t != c,
        _ => false,
    };
    let impact = serde_json::json!({
        "workItemId": workitem_id,
        "targetSnapshotId": target.id,
        "targetGate": target_gate.as_str(),
        "targetRootDigest": target.root_digest,
        "currentGate": wi.current_gate,
        "gatesAffected": gates,
        "attemptsToSupersede": supersede,
        "approvalsToExpire": approvals,
        "externalManualResources": external_manual,
        "workspace": {
            "targetHead": target_head,
            "currentHead": current_workspace,
            "drift": workspace_drift,
            "restoredByRollback": false,
            "managedWorktreeRestoredByRollback": true,
            "note": "主工作区只读（SG-RBK-005）；恢复范围=控制面指针+受管隔离 worktree",
        },
    });
    Ok(impact)
}

fn workspace_head(store: &Store, workitem_id: &str) -> Result<Option<String>, Error> {
    let local_root: Option<String> = store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT COALESCE(p.local_root,'') FROM projects p
                 JOIN workitems w ON w.project_id = p.id WHERE w.id=?1",
                [workitem_id],
                |r| r.get::<_, String>(0),
            )
            .map_err(Error::from)
        })
        .ok()
        .filter(|s| !s.is_empty());
    let Some(root) = local_root else {
        return Ok(None);
    };
    let out = std::process::Command::new("git")
        .args(["-C", &root, "rev-parse", "HEAD"])
        .output();
    Ok(out
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()))
}

fn snapshot_workspace_head(store: &Store, snapshot_id: &str) -> Result<Option<String>, Error> {
    let sha: Option<String> = store.with_conn(|conn| {
        conn.query_row(
            "SELECT workspace_manifest_sha256 FROM state_snapshots WHERE id=?1",
            [snapshot_id],
            |r| r.get(0),
        )
        .map_err(Error::from)
    })?;
    let Some(sha) = sha else { return Ok(None) };
    let content = sg_store::objects::open(store, &sha)?;
    let value: serde_json::Value = serde_json::from_slice(&content).unwrap_or_default();
    Ok(value["head"].as_str().map(String::from))
}

fn compute_digest(target_snapshot_id: &str, impact_sha: &str, policy_version: &str) -> String {
    let mut hasher = Sha256::new();
    for part in [target_snapshot_id, impact_sha, policy_version] {
        hasher.update(part.as_bytes());
        hasher.update(b"|");
    }
    ids::hex(&hasher.finalize())
}

/// rollback.preview：计算影响 + 冻结 impact manifest 与 ActionDigest（幂等更新 previewed 操作）。
pub fn preview(
    store: &Store,
    workitem_id: &str,
    target_snapshot_id: &str,
    policy_version: &str,
) -> Result<serde_json::Value, Error> {
    let (op, impact) = preview_inner(store, workitem_id, target_snapshot_id, policy_version)?;
    Ok(
        serde_json::json!({ "operation": serde_json::to_value(&op).unwrap_or_default(), "impact": impact }),
    )
}

fn preview_inner(
    store: &Store,
    workitem_id: &str,
    target_snapshot_id: &str,
    policy_version: &str,
) -> Result<(RollbackOperation, serde_json::Value), Error> {
    let target = snapshot::get(store, target_snapshot_id)?.ok_or_else(|| {
        Error::Message(format!("snapshot_failed: 快照 {target_snapshot_id} 不存在"))
    })?;
    if target.workitem_id != workitem_id {
        return Err(Error::Message("rollback_drift: 快照不属于该工作项".into()));
    }
    snapshot::verify_objects(store, target_snapshot_id)?;
    let impact = build_impact(store, workitem_id, &target)?;
    let canonical = serde_json::to_string(&impact).unwrap_or_default();
    let info = objects::put(store, canonical.as_bytes(), objects::PutOptions::default())
        .map_err(|e| Error::Message(format!("snapshot_failed: impact 存储失败 {e}")))?;
    let digest = compute_digest(target_snapshot_id, &info.sha256, policy_version);

    let source_attempt = target.stage_attempt_id.clone();
    let existing = store.with_conn(|conn| {
        let op = conn
            .query_row(
                &format!(
                    "SELECT {OP_COLUMNS} FROM rollback_operations
                     WHERE workitem_id=?1 AND target_snapshot_id=?2 AND state='previewed' LIMIT 1"
                ),
                [workitem_id, target_snapshot_id],
                row_op,
            )
            .ok();
        Ok(op)
    })?;
    let now = timefmt::now();
    let op = match existing {
        Some(mut op) => {
            store.with_conn(|conn| {
                conn.execute(
                    "UPDATE rollback_operations SET impact_manifest_sha256=?1, action_digest=?2, updated_at=?3 WHERE id=?4",
                    rusqlite::params![info.sha256, digest, now, op.id],
                )?;
                Ok(())
            })?;
            op.impact_manifest_sha256 = info.sha256.clone();
            op.action_digest = digest.clone();
            op
        }
        None => {
            let id = ids::new_id("rbk");
            store.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO rollback_operations(id, workitem_id, source_attempt_id, target_snapshot_id, safety_snapshot_id, approval_id, impact_manifest_sha256, action_digest, state, blocked_reason, requested_by, decided_by, created_at, updated_at)
                     VALUES (?1,?2,?3,?4,NULL,NULL,?5,?6,'previewed','','',NULL,?7,?7)",
                    rusqlite::params![id, workitem_id, source_attempt, target_snapshot_id, info.sha256, digest, now],
                )?;
                Ok(())
            })?;
            RollbackOperation {
                id,
                workitem_id: workitem_id.into(),
                source_attempt_id: source_attempt,
                target_snapshot_id: target_snapshot_id.into(),
                safety_snapshot_id: None,
                approval_id: None,
                impact_manifest_sha256: info.sha256,
                action_digest: digest,
                state: "previewed".into(),
                blocked_reason: String::new(),
                requested_by: String::new(),
                decided_by: None,
                created_at: now.clone(),
                updated_at: now,
            }
        }
    };
    outbox::emit(
        store,
        "workitem",
        workitem_id,
        "rollback.previewed",
        serde_json::json!({"workitemId": workitem_id, "operationId": op.id, "targetSnapshotId": target_snapshot_id}),
    )?;
    Ok((op, impact))
}

/// rollback.request：创建 safety 快照 + rollback 审批（SG-RBK-003：批准后才可执行）。
pub fn request(
    store: &Store,
    workitem_id: &str,
    target_snapshot_id: &str,
    requested_by: &str,
    policy_version: &str,
    ttl_secs: i64,
) -> Result<serde_json::Value, Error> {
    let (op, _) = preview_inner(store, workitem_id, target_snapshot_id, policy_version)?;
    if op.state != "previewed" {
        return Err(Error::Message(format!(
            "rollback_drift: 操作处于 {} 状态，不能请求",
            op.state
        )));
    }
    let attempt = attempt::get(store, &op.source_attempt_id)?;
    let gate = Gate::parse(&attempt.gate)
        .ok_or_else(|| Error::Message("rollback_drift: 关卡非法".into()))?;
    let safety = snapshot::create(store, workitem_id, gate, &attempt.id, "safety")?;
    let approval = sg_policy::request_approval(
        store,
        "rollback",
        &op.id,
        &op.action_digest,
        Risk::High,
        "阶段回滚：批准后恢复控制面并保留历史",
        ttl_secs,
        Some(workitem_id),
        Some(&attempt.id),
    )?;
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE rollback_operations SET safety_snapshot_id=?1, approval_id=?2, requested_by=?3, state='awaiting_approval', updated_at=?4 WHERE id=?5",
            rusqlite::params![safety.id, approval.id, requested_by, timefmt::now(), op.id],
        )?;
        Ok(())
    })?;
    outbox::emit(
        store,
        "workitem",
        workitem_id,
        "rollback.requested",
        serde_json::json!({
            "workitemId": workitem_id,
            "operationId": op.id,
            "approvalId": approval.id,
            "safetySnapshotId": safety.id,
        }),
    )?;
    let op = op_by_id(store, &op.id)?
        .ok_or_else(|| Error::Message("rollback_drift: 操作不可见".into()))?;
    Ok(
        serde_json::json!({ "operation": op, "safetySnapshotId": safety.id, "approvalId": approval.id }),
    )
}

/// rollback.decide：批准 → 漂移/活跃 Run 重查 → 执行恢复（幂等）→ completed；
/// 外部需人工资源 → blocked/manual_action_required（AC-SW-07）；拒绝 → 取消。
pub fn decide(
    store: &Store,
    approval_id: &str,
    decision: &str,
    decided_by: &str,
    reason: &str,
    policy_version: &str,
) -> Result<serde_json::Value, Error> {
    if !matches!(decision, "approved" | "rejected") {
        return Err(Error::Message("decision must be approved|rejected".into()));
    }
    if decided_by.trim().is_empty() {
        return Err(Error::Message("rollback_drift: 缺少审批人身份".into()));
    }
    let op = op_by_approval(store, approval_id)?
        .ok_or_else(|| Error::Message("rollback_drift: 审批不对应回滚操作".into()))?;
    let workitem_id = op.workitem_id.clone();

    // 幂等：已终态按决定一致性返回。
    if matches!(
        op.state.as_str(),
        "completed" | "cancelled" | "blocked" | "failed"
    ) {
        if (decision == "approved" && matches!(op.state.as_str(), "completed" | "blocked"))
            || (decision == "rejected" && op.state == "cancelled")
        {
            return op_view(store, &op);
        }
        return Err(Error::Message(format!(
            "rollback_drift: 操作已处于 {} 状态",
            op.state
        )));
    }

    sg_policy::expire_stale(store).map_err(|e| Error::Message(e.to_string()))?;
    let approval = sg_policy::get(store, approval_id)?;
    if approval.status == "expired" {
        mark_failed(store, &op.id, &workitem_id, "approval_expired")?;
        return Err(Error::Message("rollback_drift: 回滚审批已过期".into()));
    }

    if decision == "rejected" {
        if approval.status == "requested" {
            sg_policy::decide(store, approval_id, "rejected", decided_by, reason)
                .map_err(|e| Error::Message(e.to_string()))?;
        }
        store.with_conn(|conn| {
            conn.execute(
                "UPDATE rollback_operations SET state='cancelled', decided_by=?1, updated_at=?2 WHERE id=?3",
                rusqlite::params![decided_by, timefmt::now(), op.id],
            )?;
            Ok(())
        })?;
        outbox::emit(
            store,
            "workitem",
            &workitem_id,
            "rollback.cancelled",
            serde_json::json!({"workitemId": workitem_id, "operationId": op.id, "by": decided_by}),
        )?;
        let op = op_by_id(store, &op.id)?.ok_or_else(|| Error::Message("missing op".into()))?;
        return op_view(store, &op);
    }

    // 批准：digest 漂移重查（重算当前影响 vs 冻结 digest）。
    if approval.status == "requested" {
        let impact = build_impact(
            store,
            &workitem_id,
            &snapshot::get(store, &op.target_snapshot_id)?
                .ok_or_else(|| Error::Message("snapshot_failed: 目标快照缺失".into()))?,
        )?;
        let canonical = serde_json::to_string(&impact).unwrap_or_default();
        let info = objects::put(store, canonical.as_bytes(), objects::PutOptions::default())
            .map_err(|e| Error::Message(format!("snapshot_failed: {e}")))?;
        let current = compute_digest(&op.target_snapshot_id, &info.sha256, policy_version);
        if current != op.action_digest {
            mark_failed(store, &op.id, &workitem_id, "rollback_drift")?;
            sg_policy::expire(store, approval_id, "回滚影响已漂移，旧审批失效")?;
            return Err(Error::Message(
                "rollback_drift: 影响已漂移，旧审批失效（请重新预览）".into(),
            ));
        }
        sg_policy::decide(store, approval_id, "approved", decided_by, reason)
            .map_err(|e| Error::Message(e.to_string()))?;
    } else if approval.status != "approved" {
        return Err(Error::Message(format!(
            "rollback_drift: 审批状态 {} 不能批准",
            approval.status
        )));
    }

    execute(store, &op, decided_by)
}

/// 执行恢复（幂等，可由 resume 调用）：控制面投影恢复 + attempt superseded + 旧审批失效 +
/// 目标关新建 attempt（含关前快照）。外部 manual/irreversible 资源 → blocked（AC-SW-07）。
fn execute(
    store: &Store,
    op: &RollbackOperation,
    decided_by: &str,
) -> Result<serde_json::Value, Error> {
    let workitem_id = op.workitem_id.clone();
    let target = snapshot::get(store, &op.target_snapshot_id)?
        .ok_or_else(|| Error::Message("snapshot_failed: 目标快照缺失".into()))?;
    snapshot::verify_objects(store, &target.id)?;
    let target_attempt = attempt::get(store, &target.stage_attempt_id)?;
    let target_gate = Gate::parse(&target_attempt.gate)
        .ok_or_else(|| Error::Message("rollback_drift: 关卡非法".into()))?;

    store.with_conn(|conn| {
        conn.execute(
            "UPDATE rollback_operations SET state='executing', decided_by=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![decided_by, timefmt::now(), op.id],
        )?;
        Ok(())
    })?;
    outbox::emit(
        store,
        "workitem",
        &workitem_id,
        "rollback.started",
        serde_json::json!({"workitemId": workitem_id, "operationId": op.id}),
    )?;

    // 活跃 Run 检查（蓝图 §7.2：锁 WorkItem 重查活跃 Run）。
    let active_runs: i64 = store.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM agent_runs WHERE workitem_id=?1 AND status IN ('running','paused')",
            [&workitem_id],
            |r| r.get(0),
        )
        .map_err(Error::from)
    })?;
    if active_runs > 0 {
        mark_failed(store, &op.id, &workitem_id, "active_runs")?;
        return Err(Error::Message(
            "rollback_drift: 存在活跃 Agent Run，先取消后再回滚".into(),
        ));
    }

    // 外部需人工/不可逆资源 → blocked（AC-SW-07；不显示成功，SG-RBK-006/008）。
    let blocking: Vec<_> = snapshot::resources(store, &target.id)?
        .into_iter()
        .filter(|r| matches!(r.reversibility.as_str(), "manual" | "irreversible"))
        .collect();
    if !blocking.is_empty() {
        let reason = format!(
            "manual_action_required: {}",
            blocking
                .iter()
                .map(|r| format!("{}/{}", r.resource_type, r.resource_key))
                .collect::<Vec<_>>()
                .join(",")
        );
        store.with_conn(|conn| {
            conn.execute(
                "UPDATE rollback_operations SET state='blocked', blocked_reason=?1, updated_at=?2 WHERE id=?3",
                rusqlite::params![reason, timefmt::now(), op.id],
            )?;
            Ok(())
        })?;
        outbox::emit(
            store,
            "workitem",
            &workitem_id,
            "rollback.blocked",
            serde_json::json!({"workitemId": workitem_id, "operationId": op.id, "reason": reason}),
        )?;
        return Err(Error::Message(format!(
            "rollback_manual_action_required: {reason}"
        )));
    }

    // 控制面恢复（回滚应用服务特权写，ADR-030 决策 2）。
    let now = timefmt::now();
    for g in affected_gates(target_gate) {
        store.with_conn(|conn| {
            conn.execute(
                "UPDATE workitem_stages SET state='not_started', input_baseline_sha='', updated_at=?1 WHERE workitem_id=?2 AND gate=?3",
                rusqlite::params![now, workitem_id, g.as_str()],
            )?;
            Ok(())
        })?;
        // 后续 attempt/approval：active + approved → superseded；pending gate_release 失效。
        for a in attempt::list(store, &workitem_id)?
            .into_iter()
            .filter(|a| a.gate == g.as_str())
        {
            if ACTIVE_STATES.contains(&a.state.as_str()) || a.state == "approved" {
                store.with_conn(|conn| {
                    conn.execute(
                        "UPDATE stage_attempts SET state='superseded', updated_at=?1 WHERE id=?2",
                        rusqlite::params![now, a.id],
                    )?;
                    Ok(())
                })?;
                outbox::emit(
                    store,
                    "workitem",
                    &workitem_id,
                    "stage.attempt_superseded",
                    serde_json::json!({"workitemId": workitem_id, "attemptId": a.id, "gate": a.gate, "attemptNo": a.attempt_no, "reason": "rollback"}),
                )?;
                let pending: Vec<String> = store.with_conn(|conn| {
                    let mut stmt = conn.prepare(
                        "SELECT id FROM approvals WHERE stage_attempt_id=?1 AND status='requested'",
                    )?;
                    let rows = stmt.query_map([&a.id], |r| r.get::<_, String>(0))?;
                    let mut out = Vec::new();
                    for row in rows {
                        out.push(row?);
                    }
                    Ok(out)
                })?;
                for approval_id in pending {
                    let _ = sg_policy::expire(store, &approval_id, "回滚：旧放行审批失效");
                }
            }
        }
    }
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE workitems SET current_gate=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![target_gate.as_str(), now, workitem_id],
        )?;
        Ok(())
    })?;
    // 受管 worktree 恢复（SG-RBK-005：仅 SixGates 隔离区；主工作区不参与）。
    if let Some(head) = snapshot::resources(store, &target.id)?
        .into_iter()
        .find(|r| {
            r.resource_type == "worktree_head" && r.metadata_json.contains("\"managed\":true")
        })
        .map(|r| r.version_ref)
        .filter(|h| !h.is_empty())
    {
        if let Err(e) = crate::worktree::restore(store, &workitem_id, &head) {
            mark_failed(store, &op.id, &workitem_id, "worktree_restore")?;
            return Err(Error::Message(format!(
                "snapshot_failed: 受管 worktree 恢复失败 {e}"
            )));
        }
    }
    // 目标关新建 attempt（含关前快照，SG-RBK-007：回滚后重算门禁输入）。
    let predecessor = attempt::latest_for_gate(store, &workitem_id, target_gate)?.map(|a| a.id);
    let fresh = attempt::create(store, &workitem_id, target_gate, predecessor.as_deref())?;
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE rollback_operations SET state='completed', updated_at=?1 WHERE id=?2",
            rusqlite::params![timefmt::now(), op.id],
        )?;
        Ok(())
    })?;
    outbox::emit(
        store,
        "workitem",
        &workitem_id,
        "rollback.completed",
        serde_json::json!({
            "workitemId": workitem_id,
            "operationId": op.id,
            "targetGate": target_gate.as_str(),
            "newAttemptId": fresh.id,
        }),
    )?;
    let op = op_by_id(store, &op.id)?.ok_or_else(|| Error::Message("missing op".into()))?;
    op_view(store, &op)
}

/// 崩溃恢复（AC-SW-12）：启动时补完 executing 中断的回滚；digest 不变（快照不可变）。
pub fn resume(store: &Store) -> Result<usize, Error> {
    let ops: Vec<RollbackOperation> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {OP_COLUMNS} FROM rollback_operations WHERE state='executing'"
        ))?;
        let rows = stmt.query_map([], row_op)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;
    let mut count = 0;
    for op in ops {
        if execute(store, &op, op.decided_by.as_deref().unwrap_or("system")).is_ok() {
            count += 1;
        }
    }
    Ok(count)
}

fn op_view(store: &Store, op: &RollbackOperation) -> Result<serde_json::Value, Error> {
    Ok(serde_json::json!({
        "operation": serde_json::to_value(op).unwrap_or_default(),
        "targetSnapshot": serde_json::to_value(snapshot::get(store, &op.target_snapshot_id)?).unwrap_or_default(),
        "resources": serde_json::to_value(snapshot::resources(store, &op.target_snapshot_id)?).unwrap_or_default(),
    }))
}

pub fn get(store: &Store, operation_id: &str) -> Result<serde_json::Value, Error> {
    let op = op_by_id(store, operation_id)?
        .ok_or_else(|| Error::Message(format!("not_found: rollback operation {operation_id}")))?;
    op_view(store, &op)
}

pub fn list(store: &Store, workitem_id: &str) -> Result<Vec<RollbackOperation>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {OP_COLUMNS} FROM rollback_operations WHERE workitem_id=?1 ORDER BY created_at DESC"
        ))?;
        let rows = stmt.query_map([workitem_id], row_op)?;
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
    use crate::{EvaluateInputs, InputState};
    use sg_store::Store;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-rbk-{}-{}",
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

    /// 放行一关（评估 → 请求 → 批准）。
    fn release_gate(store: &Store, workitem_id: &str, gate: &str) {
        let art = sg_artifact::create_artifact(store, workitem_id, "doc", gate).unwrap();
        let rev = sg_artifact::create_draft(store, &art.id, "内容").unwrap();
        sg_artifact::add_review(store, &rev.id, "r", "approved", "", None).unwrap();
        sg_artifact::freeze(
            store,
            workitem_id,
            gate,
            std::slice::from_ref(&rev.id),
            "",
            "",
        )
        .unwrap();
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
        sg_evidence::verify(store, &ev.id, "r").unwrap();
        crate::gate::evaluate_and_record(store, &pass_inputs(workitem_id, gate)).unwrap();
        let rr = crate::release::request_release(store, workitem_id, gate, "pol", 3600).unwrap();
        crate::release::decide_release(
            store,
            rr["approval_id"].as_str().unwrap(),
            "approved",
            "owner",
            "",
            "pol",
        )
        .unwrap();
    }

    fn entry_snapshot_id(store: &Store, workitem_id: &str, gate: Gate) -> Option<String> {
        snapshot::latest_entry(store, workitem_id, gate)
            .map(|s| s.map(|x| x.id))
            .ok()
            .flatten()
    }

    #[test]
    fn rollback_restores_projection_and_preserves_history() {
        let s = setup();
        let wi = crate::create(&s, "pj", "回滚", "", None, &[]).unwrap();
        release_gate(&s, &wi.id, "requirements");
        assert_eq!(crate::get(&s, &wi.id).unwrap().current_gate, "design");
        // 挂一个未决定的 design 放行请求（回滚后其审批必须失效）。
        crate::gate::evaluate_and_record(&s, &pass_inputs(&wi.id, "design")).unwrap();
        let _pending_rr =
            crate::release::request_release(&s, &wi.id, "design", "pol", 3600).unwrap();

        // 目标：回到方案关前的快照（requirements attempt 的关前快照是回到需求关；
        // 这里选择 design attempt 的 entry snapshot → 回滚到方案关执行前）。
        let target = entry_snapshot_id(&s, &wi.id, Gate::Design).unwrap();
        let view = preview(&s, &wi.id, &target, "pol").unwrap();
        assert_eq!(view["impact"]["currentGate"], serde_json::json!("design"));
        assert!(
            view["impact"]["approvalsToExpire"]
                .as_array()
                .map(|a| !a.is_empty())
                .unwrap_or(false),
            "影响清单包含将失效的 pending 放行审批"
        );
        let req = request(&s, &wi.id, &target, "owner", "pol", 3600).unwrap();
        let approval_id = req["approvalId"].as_str().unwrap().to_string();
        decide(&s, &approval_id, "approved", "owner", "", "pol").unwrap();

        // 投影恢复：current_gate 回到 design；design 及之后投影 not_started。
        let wi_now = crate::get(&s, &wi.id).unwrap();
        assert_eq!(wi_now.current_gate, "design", "AC-SW-06：控制面指针恢复");
        // 历史保留：development/design attempt 仍在列表（superseded）。
        let attempts = attempt::list(&s, &wi.id).unwrap();
        assert!(
            attempts
                .iter()
                .any(|a| a.gate == "design" && a.state == "superseded"),
            "历史保留且 design attempt 已 superseded"
        );
        assert!(
            attempts.iter().all(|a| a.gate != "development"),
            "development 未开始无 attempt"
        );
        // 目标关新建 attempt（带新关前快照）。
        let fresh = attempt::latest_for_gate(&s, &wi.id, Gate::Design)
            .unwrap()
            .unwrap();
        assert_eq!(fresh.state, "prepared");
        assert!(
            !fresh.entry_snapshot_id.is_empty(),
            "SG-RBK-001：新 attempt 有关前快照"
        );
        // 旧放行审批已失效。
        let expired: i64 = s
            .with_conn(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM approvals WHERE status='expired'",
                    [],
                    |r| r.get(0),
                )
                .map_err(Error::from)
            })
            .unwrap();
        assert!(expired >= 1, "AC-SW-06：旧审批失效");
        // 操作已完成。
        let ops = list(&s, &wi.id).unwrap();
        assert!(ops.iter().any(|o| o.state == "completed"));
    }

    #[test]
    fn manual_external_resource_blocks_rollback() {
        let s = setup();
        let wi = crate::create(&s, "pj", "不可逆", "", None, &[]).unwrap();
        // 部署副作用：阶段回滚不得冒充部署补偿（SG-RBK-008）。
        s.with_conn(|c| {
            c.execute(
                "INSERT INTO deployments(id, workitem_id, target, image_digest, plan, state, created_at, updated_at)
                 VALUES ('dp_1',?1,'host','sha256:x','{}','verified',?2,?2)",
                rusqlite::params![wi.id, timefmt::now()],
            )?;
            Ok(())
        })
        .unwrap();
        release_gate(&s, &wi.id, "requirements");
        let target = entry_snapshot_id(&s, &wi.id, Gate::Requirements).unwrap();
        let req = request(&s, &wi.id, &target, "owner", "pol", 3600).unwrap();
        let approval_id = req["approvalId"].as_str().unwrap().to_string();
        let err = decide(&s, &approval_id, "approved", "owner", "", "pol").unwrap_err();
        assert!(
            err.to_string().contains("rollback_manual_action_required"),
            "{err}"
        );
        // 控制面不得显示成功：操作 blocked，current_gate 不变。
        let ops = list(&s, &wi.id).unwrap();
        assert!(ops.iter().any(|o| o.state == "blocked"));
        assert_eq!(crate::get(&s, &wi.id).unwrap().current_gate, "design");
    }

    #[test]
    fn resume_completes_interrupted_rollback() {
        let s = setup();
        let wi = crate::create(&s, "pj", "崩溃恢复", "", None, &[]).unwrap();
        release_gate(&s, &wi.id, "requirements");
        let target = entry_snapshot_id(&s, &wi.id, Gate::Requirements).unwrap();
        let req = request(&s, &wi.id, &target, "owner", "pol", 3600).unwrap();
        let approval_id = req["approvalId"].as_str().unwrap().to_string();
        // 模拟崩溃：审批已批准但操作仍 awaiting/executing。
        sg_policy::decide(&s, &approval_id, "approved", "owner", "").unwrap();
        let op = op_by_approval(&s, &approval_id).unwrap().unwrap();
        s.with_conn(|c| {
            c.execute(
                "UPDATE rollback_operations SET state='executing' WHERE id=?1",
                [&op.id],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
        let digest_before = snapshot::get(&s, &target).unwrap().unwrap().root_digest;
        assert_eq!(resume(&s).unwrap(), 1, "AC-SW-12：崩溃后恢复完成");
        assert_eq!(
            snapshot::get(&s, &target).unwrap().unwrap().root_digest,
            digest_before,
            "快照 digest 不变"
        );
        assert_eq!(crate::get(&s, &wi.id).unwrap().current_gate, "requirements");
    }
}
