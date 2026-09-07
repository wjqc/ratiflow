//! A1 跨关返工（EvoFlow WP-9；表 0046；flag RATIFLOW_REWORK，默认 0）。
//!
//! 权威=`rework_operations`（对齐 rollback_operations 模式，补时间列）。
//! 失效范围=`rework_affected_facts` 统一登记表；读取面据此过滤——
//! 无 affected 行的库上全部查询与既往逐字等价（回退读法）。
//!
//! 两步执行（decide(approved) → executing）：
//! - Step A（单 DB 事务，幂等）：[target, from] 区间事实登记+失效
//!   （gate_results 登记；attempts 非终态+approved → superseded；pending
//!   release → superseded、approved 登记失效；pending approvals → expire；
//!   baselines superseded_by=rework id；plan 未终态 attempt → cancelled；
//!   passports 登记）+ 区间 stages 复位 not_started（set_stage_rework，绕过
//!   can_transition 仅此处可调）+ target 新 attempt（preparing，不占单活跃
//!   名额，predecessor 链）+ 指针回 target。
//! - Step B（文件写，非事务）：target attempt 关前快照装配；失败 → attempt
//!   留 preparing、op → blocked(snapshot_write_failed)，重试幂等复用，不回滚
//!   Step A（事实保留）。
//!
//! CAS=发起时全关状态+指针+活跃 attempt 的 digest；decide 时重算不符 →
//! `rework_state_changed`。活跃 Run 仍活跃 → blocked(active_run_present)
//! （不自动杀，用户走既有取消链路）。

use serde::Serialize;
use serde_json::json;
use sg_provenance::{node_type, NodeInput};
use sg_store::{ids, outbox, timefmt, Error, Store};

const REASONS: [&str; 5] = [
    "requirement_unclear",
    "design_defect",
    "implementation_defect",
    "regression",
    "other",
];

/// attempts 的非终态集合（v1.1 漏 approved 已修正：approved 一并 superseded）。
const NON_TERMINAL_ATTEMPT_STATES: &str =
    "('preparing','prepared','running','review_ready','awaiting_user_approval','changes_requested','approved')";

#[derive(Debug, Clone, Serialize)]
pub struct ReworkOperation {
    pub id: String,
    pub workitem_id: String,
    pub from_gate: String,
    pub target_gate: String,
    pub from_attempt_id: Option<String>,
    pub reason_code: String,
    pub note: String,
    pub current_state_digest: String,
    pub state: String,
    pub blocked_reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    pub action_digest: String,
    pub requested_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

const OP_COLS: &str = "id, workitem_id, from_gate, target_gate, from_attempt_id, reason_code, note,
        current_state_digest, state, blocked_reason, approval_id, action_digest,
        requested_by, decided_by, decided_at, completed_at, created_at, updated_at";

fn op_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReworkOperation> {
    Ok(ReworkOperation {
        id: r.get(0)?,
        workitem_id: r.get(1)?,
        from_gate: r.get(2)?,
        target_gate: r.get(3)?,
        from_attempt_id: r.get(4)?,
        reason_code: r.get(5)?,
        note: r.get(6)?,
        current_state_digest: r.get(7)?,
        state: r.get(8)?,
        blocked_reason: r.get(9)?,
        approval_id: r.get(10)?,
        action_digest: r.get(11)?,
        requested_by: r.get(12)?,
        decided_by: r.get(13)?,
        decided_at: r.get(14)?,
        completed_at: r.get(15)?,
        created_at: r.get(16)?,
        updated_at: r.get(17)?,
    })
}

fn op_by_id(store: &Store, id: &str) -> Result<Option<ReworkOperation>, Error> {
    store.with_conn(|conn| {
        match conn.query_row(
            &format!("SELECT {OP_COLS} FROM rework_operations WHERE id=?1"),
            [id],
            op_from,
        ) {
            Ok(op) => Ok(Some(op)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(other) => Err(other.into()),
        }
    })
}

fn op_by_approval(store: &Store, approval_id: &str) -> Result<Option<ReworkOperation>, Error> {
    store.with_conn(|conn| {
        match conn.query_row(
            &format!("SELECT {OP_COLS} FROM rework_operations WHERE approval_id=?1"),
            [approval_id],
            op_from,
        ) {
            Ok(op) => Ok(Some(op)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(other) => Err(other.into()),
        }
    })
}

fn op_view(store: &Store, op: &ReworkOperation) -> Result<serde_json::Value, Error> {
    let mut v = serde_json::to_value(op).unwrap_or_default();
    let counts: Vec<(String, i64)> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT fact_kind, COUNT(*) FROM rework_affected_facts WHERE rework_operation_id=?1 GROUP BY fact_kind",
        )?;
        let rows =
            stmt.query_map([&op.id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    })?;
    v["affectedFacts"] = serde_json::to_value(counts).unwrap_or_default();
    Ok(v)
}

/// CAS：全关状态 + 指针 + 活跃 attempt 的 digest（发起时冻结，decide 时重算）。
pub fn cas_digest(store: &Store, workitem_id: &str) -> Result<String, Error> {
    let refs = crate::gate_refs(store, workitem_id)?;
    let stages = crate::stages(store, workitem_id)?;
    let wi = crate::get(store, workitem_id)?;
    let mut lines = vec![format!("pointer|{}", wi.current_gate)];
    for g in &refs {
        let st = stages
            .iter()
            .find(|s| s.gate == g.gate_id)
            .map(|s| s.state.as_str())
            .unwrap_or("not_started");
        lines.push(format!("stage|{}|{}", g.gate_id, st));
    }
    let actives: Vec<(String, String)> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT gate, state FROM stage_attempts
             WHERE workitem_id=?1 AND state IN ('prepared','running','review_ready','awaiting_user_approval','changes_requested')
             ORDER BY gate, attempt_no",
        )?;
        let rows = stmt.query_map([workitem_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    })?;
    for (gate, st) in actives {
        lines.push(format!("attempt|{gate}|{st}"));
    }
    use sha2::{Digest, Sha256};
    Ok(format!(
        "sha256:{}",
        sg_store::ids::hex(&Sha256::digest(lines.join("\n").as_bytes()))
    ))
}

fn action_digest_of(
    workitem_id: &str,
    target: &str,
    reason: &str,
    note: &str,
    cas: &str,
) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "sha256:{}",
        sg_store::ids::hex(&Sha256::digest(
            format!("rework|{workitem_id}|{target}|{reason}|{note}|{cas}").as_bytes()
        ))
    )
}

/// [target, from] 闭区间关卡（按实例 ordinal；target 必须早于当前关）。
fn interval_gates(
    store: &Store,
    workitem_id: &str,
    target_gate: &str,
) -> Result<Vec<String>, Error> {
    let refs = crate::gate_refs(store, workitem_id)?;
    let current = crate::get(store, workitem_id)?.current_gate;
    let pos = |g: &str| refs.iter().position(|r| r.gate_id == g);
    let (ti, ci) = (
        pos(target_gate).ok_or_else(|| Error::Message("rework_invalid: 未知目标关".into()))?,
        pos(&current).ok_or_else(|| Error::Message("rework_invalid: 未知当前关".into()))?,
    );
    if ti >= ci {
        return Err(Error::Message(format!(
            "rework_invalid: 目标关 {target_gate} 不早于当前关 {current}"
        )));
    }
    Ok(refs[ti..=ci].iter().map(|r| r.gate_id.clone()).collect())
}

fn active_runs_count(store: &Store, workitem_id: &str) -> Result<i64, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM agent_runs WHERE workitem_id=?1 AND status IN ('running','paused')",
            [workitem_id],
            |r| r.get(0),
        )
        .map_err(Error::from)
    })
}

/// rework 预览内核：校验 + CAS 冻结 + 持久化 previewed 操作（同 digest 幂等重放）。
fn preview_inner(
    store: &Store,
    workitem_id: &str,
    target_gate: &str,
    reason_code: &str,
    note: &str,
    requested_by: &str,
) -> Result<(ReworkOperation, Vec<String>, bool), Error> {
    if !REASONS.contains(&reason_code) {
        return Err(Error::Message(format!(
            "rework_invalid: reason_code 须为 {REASONS:?}（得到 {reason_code:?}）"
        )));
    }
    if !crate::gate_known(store, workitem_id, target_gate)? {
        return Err(Error::Message("rework_invalid: 未知目标关".into()));
    }
    let interval = interval_gates(store, workitem_id, target_gate)?;
    let cas = cas_digest(store, workitem_id)?;
    let action_digest = action_digest_of(workitem_id, target_gate, reason_code, note, &cas);
    let existing: Option<String> = store.with_conn(|conn| {
        match conn.query_row(
            "SELECT id FROM rework_operations WHERE action_digest=?1 AND state IN ('previewed','awaiting_approval')",
            [&action_digest],
            |r| r.get(0),
        ) {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(other) => Err(other.into()),
        }
    })?;
    let op = match existing {
        Some(id) => op_by_id(store, &id)?
            .ok_or_else(|| Error::Message("rework_invalid: 操作不可见".into()))?,
        None => {
            let id = ids::new_id("rwk");
            let now = timefmt::now();
            let current_gate = crate::get(store, workitem_id)?.current_gate;
            let from_attempt = crate::attempt::active_for_gate(store, workitem_id, &current_gate)?;
            store.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO rework_operations
                     (id, workitem_id, from_gate, target_gate, from_attempt_id, reason_code, note,
                      current_state_digest, state, blocked_reason, action_digest, requested_by, created_at, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'previewed','',?9,?10,?11,?11)",
                    rusqlite::params![
                        id,
                        workitem_id,
                        current_gate,
                        target_gate,
                        from_attempt.as_ref().map(|a| a.id.clone()),
                        reason_code,
                        note,
                        cas,
                        action_digest,
                        requested_by,
                        now
                    ],
                )?;
                Ok(())
            })?;
            outbox::emit(
                store,
                "workitem",
                workitem_id,
                "rework.previewed",
                json!({"operationId": id, "targetGate": target_gate, "intervalGates": interval}),
            )?;
            op_by_id(store, &id)?
                .ok_or_else(|| Error::Message("rework_invalid: 操作不可见".into()))?
        }
    };
    Ok((op, interval, active_runs_count(store, workitem_id)? > 0))
}

/// rework.preview：校验 + CAS 冻结 + 持久化 previewed 操作（同 digest 幂等重放）。
pub fn preview(
    store: &Store,
    workitem_id: &str,
    target_gate: &str,
    reason_code: &str,
    note: &str,
    requested_by: &str,
) -> Result<serde_json::Value, Error> {
    let (op, interval, active_run) = preview_inner(
        store,
        workitem_id,
        target_gate,
        reason_code,
        note,
        requested_by,
    )?;
    let mut v = op_view(store, &op)?;
    v["intervalGates"] = json!(interval);
    v["activeRunPresent"] = json!(active_run);
    Ok(v)
}

/// rework.request：previewed/幂等重放 → 审批挂起（awaiting_approval）。
pub fn request(
    store: &Store,
    workitem_id: &str,
    target_gate: &str,
    reason_code: &str,
    note: &str,
    requested_by: &str,
) -> Result<serde_json::Value, Error> {
    let (op, interval, active_run) = preview_inner(
        store,
        workitem_id,
        target_gate,
        reason_code,
        note,
        requested_by,
    )?;
    if op.state == "awaiting_approval" {
        // 幂等重放：已挂审批 → 原样返回（含既有 approvalId）。
        let mut v = op_view(store, &op)?;
        v["intervalGates"] = json!(interval);
        v["activeRunPresent"] = json!(active_run);
        return Ok(v);
    }
    if op.state != "previewed" {
        return Err(Error::Message(format!(
            "rework_state_changed: 操作处于 {} 状态，不能请求",
            op.state
        )));
    }
    // 发起到请求之间状态漂移 → 拒（CAS 复核）。
    let cas = cas_digest(store, workitem_id)?;
    if cas != op.current_state_digest {
        return Err(Error::Message(
            "rework_state_changed: 发起后状态已变化，请重新预览".into(),
        ));
    }
    let approval = sg_policy::request_approval(
        store,
        "rework",
        &op.id,
        &op.action_digest,
        sg_policy::Risk::High,
        &op.note,
        0,
        Some(workitem_id),
        op.from_attempt_id.as_deref(),
    )?;
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE rework_operations SET state='awaiting_approval', approval_id=?1, requested_by=?2, updated_at=?3 WHERE id=?4",
            rusqlite::params![approval.id, requested_by, timefmt::now(), op.id],
        )?;
        Ok(())
    })?;
    outbox::emit(
        store,
        "workitem",
        workitem_id,
        "rework.requested",
        json!({"operationId": op.id, "approvalId": approval.id, "targetGate": target_gate}),
    )?;
    let op = op_by_id(store, &op.id)?
        .ok_or_else(|| Error::Message("rework_invalid: 操作不可见".into()))?;
    let mut v = op_view(store, &op)?;
    v["intervalGates"] = json!(interval);
    v["approvalId"] = json!(approval.id);
    Ok(v)
}

/// rework.decide（approval.decide 的 rework 主体路由同此）：批准 → CAS 复查 →
/// 活跃 Run 阻断 → 两步执行；拒绝 → cancelled；终态重放对齐 decide_release。
pub fn decide(
    store: &Store,
    approval_id: &str,
    decision: &str,
    decided_by: &str,
    reason: &str,
) -> Result<serde_json::Value, Error> {
    if !matches!(decision, "approved" | "rejected") {
        return Err(Error::Message("decision must be approved|rejected".into()));
    }
    let op = op_by_approval(store, approval_id)?
        .ok_or_else(|| Error::Message("rework_invalid: 审批不对应返工操作".into()))?;
    let workitem_id = op.workitem_id.clone();

    // 终态幂等重放（对齐 decide_release 语义）。
    if matches!(
        op.state.as_str(),
        "completed" | "blocked" | "cancelled" | "failed"
    ) {
        if (decision == "approved" && matches!(op.state.as_str(), "completed" | "blocked"))
            || (decision == "rejected" && op.state == "cancelled")
        {
            return op_view(store, &op);
        }
        return Err(Error::Message(format!(
            "rework_state_changed: 操作已处于 {} 状态",
            op.state
        )));
    }

    sg_policy::expire_stale(store).map_err(|e| Error::Message(e.to_string()))?;
    let approval = sg_policy::get(store, approval_id)?;
    if approval.status == "expired" {
        mark_failed(store, &op.id, &workitem_id, "approval_expired")?;
        return Err(Error::Message(
            "rework_state_changed: 返工审批已过期".into(),
        ));
    }

    if decision == "rejected" {
        if approval.status == "requested" {
            sg_policy::decide(store, approval_id, "rejected", decided_by, reason)
                .map_err(|e| Error::Message(e.to_string()))?;
        }
        store.with_conn(|conn| {
            conn.execute(
                "UPDATE rework_operations SET state='cancelled', decided_by=?1, decided_at=?2, updated_at=?2 WHERE id=?3",
                rusqlite::params![decided_by, timefmt::now(), op.id],
            )?;
            Ok(())
        })?;
        outbox::emit(
            store,
            "workitem",
            &workitem_id,
            "rework.cancelled",
            json!({"workitemId": workitem_id, "operationId": op.id}),
        )?;
        let op = op_by_id(store, &op.id)?.ok_or_else(|| Error::Message("missing op".into()))?;
        return op_view(store, &op);
    }

    // 批准：CAS 重查（发起后状态漂移 → failed + 审批失效）。
    if approval.status == "requested" {
        let cas = cas_digest(store, &workitem_id)?;
        if cas != op.current_state_digest {
            mark_failed(store, &op.id, &workitem_id, "rework_state_changed")?;
            sg_policy::expire(store, approval_id, "返工发起后状态已漂移，旧审批失效")?;
            return Err(Error::Message(
                "rework_state_changed: 发起后状态已漂移，旧审批失效（请重新预览）".into(),
            ));
        }
        sg_policy::decide(store, approval_id, "approved", decided_by, reason)
            .map_err(|e| Error::Message(e.to_string()))?;
    } else if approval.status != "approved" {
        return Err(Error::Message(format!(
            "rework_state_changed: 审批状态 {} 不能批准",
            approval.status
        )));
    }

    execute(store, &op, decided_by)
}

fn mark_failed(store: &Store, op_id: &str, workitem_id: &str, reason: &str) -> Result<(), Error> {
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE rework_operations SET state='failed', blocked_reason=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![reason, timefmt::now(), op_id],
        )?;
        Ok(())
    })?;
    outbox::emit(
        store,
        "workitem",
        workitem_id,
        "rework.failed",
        json!({"workitemId": workitem_id, "operationId": op_id, "reason": reason}),
    )?;
    Ok(())
}

/// 专用复位（绕过 StageState::can_transition；仅 rework Step A 可调）。
/// legacy stages 与实例投影双写复位；审计由 rework.step_a 事件承载区间明细。
fn set_stage_rework(
    conn: &rusqlite::Connection,
    workitem_id: &str,
    gate: &str,
    now: &str,
) -> Result<(), Error> {
    conn.execute(
        "UPDATE workitem_stages SET state='not_started', input_baseline_sha='', updated_at=?1
         WHERE workitem_id=?2 AND gate=?3",
        rusqlite::params![now, workitem_id, gate],
    )?;
    conn.execute(
        "UPDATE workflow_instance_gates SET state='not_started', updated_at=?1
         WHERE instance_id=(SELECT id FROM workflow_instances WHERE workitem_id=?2)
           AND gate_definition_id IN
             (SELECT id FROM workflow_gate_definitions
              WHERE version_id=(SELECT template_version_id FROM workflow_instances WHERE workitem_id=?2)
                AND gate_id=?3)",
        rusqlite::params![now, workitem_id, gate],
    )?;
    Ok(())
}

/// Step A（单事务，幂等）：区间事实登记+失效 + stages 复位 + target 新 attempt
/// （preparing，predecessor 链）+ 指针回 target。返回新 attempt id。
#[allow(clippy::vec_init_then_push)] // 区间参数逐块装配，比一次性 vec![] 可读
fn step_a(store: &Store, op: &ReworkOperation) -> Result<String, Error> {
    let workitem_id = op.workitem_id.clone();
    let interval = interval_gates(store, &workitem_id, &op.target_gate)?;
    let now = timefmt::now();
    let gates_sql = interval.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let new_attempt_id = ids::new_id("att");
    let attempt_id = store.with_tx(|conn| {
        let mut p: Vec<rusqlite::types::Value> = Vec::new();
        // ①a gate_results 登记失效（读取面 latest_valid/latest 据此排除）。
        p.push(ids::new_id("raf").into());
        p.push(op.id.clone().into());
        p.push(now.clone().into());
        p.push(workitem_id.clone().into());
        for g in &interval {
            p.push(g.clone().into());
        }
        conn.execute(
            &format!(
                "INSERT OR IGNORE INTO rework_affected_facts(id, rework_operation_id, fact_kind, fact_id, created_at)
                 SELECT ?1 || '-' || lower(hex(randomblob(8))), ?2, 'gate_result', id, ?3 FROM gate_results
                 WHERE workitem_id=?4 AND gate IN ({gates_sql})"
            ),
            rusqlite::params_from_iter(p.iter()),
        )?;
        // ①b attempts：非终态+approved → superseded（先登记后改；重入时已 superseded 不再命中）。
        p.clear();
        p.push(ids::new_id("raf").into());
        p.push(op.id.clone().into());
        p.push(now.clone().into());
        p.push(workitem_id.clone().into());
        for g in &interval {
            p.push(g.clone().into());
        }
        conn.execute(
            &format!(
                "INSERT OR IGNORE INTO rework_affected_facts(id, rework_operation_id, fact_kind, fact_id, created_at)
                 SELECT ?1 || '-' || lower(hex(randomblob(8))), ?2, 'stage_attempt', id, ?3 FROM stage_attempts
                 WHERE workitem_id=?4 AND gate IN ({gates_sql})
                   AND state IN {NON_TERMINAL_ATTEMPT_STATES}"
            ),
            rusqlite::params_from_iter(p.iter()),
        )?;
        let mut p: Vec<rusqlite::types::Value> = vec![now.clone().into(), workitem_id.clone().into()];
        for g in &interval {
            p.push(g.clone().into());
        }
        conn.execute(
            &format!(
                "UPDATE stage_attempts SET state='superseded', updated_at=?1
                 WHERE workitem_id=?2 AND gate IN ({gates_sql})
                   AND state IN {NON_TERMINAL_ATTEMPT_STATES}"
            ),
            rusqlite::params_from_iter(p.iter()),
        )?;
        // ①c output_packages 随 attempt 登记（deliverableStatus 读取面按活跃 attempt 过滤）。
        p.clear();
        p.push(ids::new_id("raf").into());
        p.push(op.id.clone().into());
        p.push(now.clone().into());
        p.push(workitem_id.clone().into());
        for g in &interval {
            p.push(g.clone().into());
        }
        conn.execute(
            &format!(
                "INSERT OR IGNORE INTO rework_affected_facts(id, rework_operation_id, fact_kind, fact_id, created_at)
                 SELECT ?1, ?2, 'output_package', p.id, ?3 FROM stage_output_packages p
                 JOIN stage_attempts a ON a.id = p.stage_attempt_id
                 WHERE a.workitem_id=?4 AND a.gate IN ({gates_sql})"
            ),
            rusqlite::params_from_iter(p.iter()),
        )?;
        // ①d release_requests：pending → superseded；approved 登记失效。
        p.clear();
        p.push(now.clone().into());
        p.push(workitem_id.clone().into());
        for g in &interval {
            p.push(g.clone().into());
        }
        conn.execute(
            &format!(
                "UPDATE gate_release_requests SET state='superseded', decided_at=?1
                 WHERE state='pending' AND stage_attempt_id IN
                   (SELECT id FROM stage_attempts WHERE workitem_id=?2 AND gate IN ({gates_sql}))"
            ),
            rusqlite::params_from_iter(p.iter()),
        )?;
        p.clear();
        p.push(ids::new_id("raf").into());
        p.push(op.id.clone().into());
        p.push(now.clone().into());
        p.push(workitem_id.clone().into());
        for g in &interval {
            p.push(g.clone().into());
        }
        conn.execute(
            &format!(
                "INSERT OR IGNORE INTO rework_affected_facts(id, rework_operation_id, fact_kind, fact_id, created_at)
                 SELECT ?1, ?2, 'release_request', r.id, ?3 FROM gate_release_requests r
                 JOIN stage_attempts a ON a.id = r.stage_attempt_id
                 WHERE r.state='approved' AND a.workitem_id=?4 AND a.gate IN ({gates_sql})"
            ),
            rusqlite::params_from_iter(p.iter()),
        )?;
        // ①e approvals：区间 pending → expire + 登记。
        p.clear();
        p.push(ids::new_id("raf").into());
        p.push(op.id.clone().into());
        p.push(now.clone().into());
        p.push(workitem_id.clone().into());
        for g in &interval {
            p.push(g.clone().into());
        }
        conn.execute(
            &format!(
                "INSERT OR IGNORE INTO rework_affected_facts(id, rework_operation_id, fact_kind, fact_id, created_at)
                 SELECT ?1 || '-' || lower(hex(randomblob(8))), ?2, 'approval', id, ?3 FROM approvals
                 WHERE workitem_id=?4 AND status='pending' AND stage_attempt_id IN
                   (SELECT id FROM stage_attempts WHERE workitem_id=?4 AND gate IN ({gates_sql}))"
            ),
            rusqlite::params_from_iter(p.iter()),
        )?;
        let mut p: Vec<rusqlite::types::Value> = vec![workitem_id.clone().into()];
        for g in &interval {
            p.push(g.clone().into());
        }
        conn.execute(
            &format!(
                "UPDATE approvals SET status='expired', reason='rework 区间失效'
                 WHERE workitem_id=?1 AND status='pending' AND stage_attempt_id IN
                   (SELECT id FROM stage_attempts WHERE workitem_id=?1 AND gate IN ({gates_sql}))"
            ),
            rusqlite::params_from_iter(p.iter()),
        )?;
        // ①f baselines：superseded_by = rework id（读取面已过滤）+ 登记。
        p.clear();
        p.push(ids::new_id("raf").into());
        p.push(op.id.clone().into());
        p.push(now.clone().into());
        p.push(workitem_id.clone().into());
        for g in &interval {
            p.push(g.clone().into());
        }
        conn.execute(
            &format!(
                "INSERT OR IGNORE INTO rework_affected_facts(id, rework_operation_id, fact_kind, fact_id, created_at)
                 SELECT ?1 || '-' || lower(hex(randomblob(8))), ?2, 'baseline', id, ?3 FROM baselines
                 WHERE workitem_id=?4 AND gate IN ({gates_sql}) AND superseded_by IS NULL"
            ),
            rusqlite::params_from_iter(p.iter()),
        )?;
        let mut p: Vec<rusqlite::types::Value> = vec![op.id.clone().into(), workitem_id.clone().into()];
        for g in &interval {
            p.push(g.clone().into());
        }
        conn.execute(
            &format!(
                "UPDATE baselines SET superseded_by=?1
                 WHERE workitem_id=?2 AND gate IN ({gates_sql}) AND superseded_by IS NULL"
            ),
            rusqlite::params_from_iter(p.iter()),
        )?;
        // ①g plan 域：该 workitem 未终态 plan_task_attempts → cancelled（plan 域无 superseded）。
        conn.execute(
            "INSERT OR IGNORE INTO rework_affected_facts(id, rework_operation_id, fact_kind, fact_id, created_at)
             SELECT ?1, ?2, 'plan_attempt', pta.id, ?3 FROM plan_task_attempts pta
             JOIN plan_tasks pt ON pt.id = pta.task_id
             JOIN plan_revisions pr ON pr.id = pt.plan_revision_id
             WHERE pr.workitem_id=?4 AND pta.state IN
               ('pending','ready','preparing_workspace','running','awaiting_approval','reconciliation_required','manual_action_required')",
            rusqlite::params![ids::new_id("raf"), op.id, now, workitem_id],
        )?;
        conn.execute(
            "UPDATE plan_task_attempts SET state='cancelled', updated_at=?1
             WHERE id IN (SELECT pta.id FROM plan_task_attempts pta
               JOIN plan_tasks pt ON pt.id = pta.task_id
               JOIN plan_revisions pr ON pr.id = pt.plan_revision_id
               WHERE pr.workitem_id=?2 AND pta.state IN
                 ('pending','ready','preparing_workspace','running','awaiting_approval','reconciliation_required','manual_action_required'))",
            rusqlite::params![now, workitem_id],
        )?;
        // ①h passports：登记（latest 过滤，要求重签）。
        conn.execute(
            "INSERT OR IGNORE INTO rework_affected_facts(id, rework_operation_id, fact_kind, fact_id, created_at)
             SELECT ?1 || '-' || lower(hex(randomblob(8))), ?2, 'passport', id, ?3 FROM passports WHERE workitem_id=?4",
            rusqlite::params![ids::new_id("raf"), op.id, now, workitem_id],
        )?;
        // ② [target,from] stages 复位 not_started（双写：legacy + 实例投影）。
        for gate in &interval {
            set_stage_rework(conn, &workitem_id, gate, &now)?;
        }
        // ④ target 新 attempt（preparing 不占单活跃名额；幂等复用既有 preparing 行）。
        let from_active: Option<String> = conn
            .query_row(
                "SELECT id FROM stage_attempts WHERE workitem_id=?1 AND gate=?2 AND state='superseded'
                 ORDER BY attempt_no DESC LIMIT 1",
                rusqlite::params![workitem_id, op.from_gate],
                |r| r.get(0),
            )
            .optional_id();
        let existing: Option<String> = match conn.query_row(
            "SELECT id FROM stage_attempts WHERE workitem_id=?1 AND gate=?2 AND state='preparing'
               AND predecessor_attempt_id=?3",
            rusqlite::params![workitem_id, op.target_gate, from_active],
            |r| r.get::<_, String>(0),
        ) {
            Ok(id) => Some(id),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(other) => return Err(other.into()),
        };
        let attempt_id = match existing {
            Some(id) => id,
            None => {
                conn.execute(
                    "INSERT INTO stage_attempts(id, workitem_id, gate, attempt_no, state, predecessor_attempt_id, created_at, updated_at)
                     VALUES (?1,?2,?3,(SELECT COALESCE(MAX(attempt_no),0)+1 FROM stage_attempts WHERE workitem_id=?2 AND gate=?3),
                             'preparing',?4,?5,?5)",
                    rusqlite::params![new_attempt_id, workitem_id, op.target_gate, from_active, now],
                )?;
                new_attempt_id.clone()
            }
        };
        // ⑤ 指针回 target（legacy + 实例投影）。
        conn.execute(
            "UPDATE workitems SET current_gate=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![op.target_gate, now, workitem_id],
        )?;
        conn.execute(
            "UPDATE workflow_instances SET current_gate_id=?1, updated_at=?2 WHERE workitem_id=?3",
            rusqlite::params![op.target_gate, now, workitem_id],
        )?;
        Ok(attempt_id)
    })?;
    // 谱系：新 attempt 节点注册（幂等；放在事务外——register_node 自带连接管理，
    // 缺失会让重做链的 trace 边写入 fail-closed）。
    sg_provenance::register_node(
        store,
        &NodeInput {
            project_id: "",
            workitem_id: &workitem_id,
            node_type: node_type::STAGE_ATTEMPT,
            entity_id: &attempt_id,
            content_digest: "",
            verification_state: "verified",
        },
    )?;
    Ok(attempt_id)
}

/// Step B（文件写，非事务）：target attempt 关前快照装配 + prepared。
/// 失败 → attempt 留 preparing、op blocked(snapshot_write_failed)；重试幂等复用。
fn step_b(store: &Store, op: &ReworkOperation, attempt_id: &str) -> Result<(), Error> {
    let snapshot = crate::snapshot::create(
        store,
        &op.workitem_id,
        &op.target_gate,
        attempt_id,
        "stage_entry",
    )?;
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE stage_attempts SET entry_snapshot_id=?1, state='prepared', updated_at=?2 WHERE id=?3 AND state='preparing'",
            rusqlite::params![snapshot.id, timefmt::now(), attempt_id],
        )?;
        Ok(())
    })?;
    Ok(())
}

/// 执行（decide 批准 / executing 重入共用；Step A 幂等、Step B 可重试）。
fn execute(
    store: &Store,
    op: &ReworkOperation,
    decided_by: &str,
) -> Result<serde_json::Value, Error> {
    let workitem_id = op.workitem_id.clone();
    // 活跃 Run：仍活跃 → blocked(active_run_present)，不自动杀。
    if active_runs_count(store, &workitem_id)? > 0 {
        store.with_conn(|conn| {
            conn.execute(
                "UPDATE rework_operations SET state='blocked', blocked_reason='active_run_present', decided_by=?1, updated_at=?2 WHERE id=?3",
                rusqlite::params![decided_by, timefmt::now(), op.id],
            )?;
            Ok(())
        })?;
        outbox::emit(
            store,
            "workitem",
            &workitem_id,
            "rework.blocked",
            json!({"workitemId": workitem_id, "operationId": op.id, "reason": "active_run_present"}),
        )?;
        return Err(Error::Message(
            "rework_blocked: 存在活跃 Agent Run，先取消后再返工".into(),
        ));
    }
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE rework_operations SET state='executing', decided_by=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![decided_by, timefmt::now(), op.id],
        )?;
        Ok(())
    })?;
    outbox::emit(
        store,
        "workitem",
        &workitem_id,
        "rework.executing",
        json!({"workitemId": workitem_id, "operationId": op.id}),
    )?;
    // Step A（单事务；幂等）。
    let attempt_id = step_a(store, op)?;
    // Step B（文件写；失败 → blocked，attempt 留 preparing 供重试复用）。
    if let Err(e) = step_b(store, op, &attempt_id) {
        let reason = format!("snapshot_write_failed: {e}");
        store.with_conn(|conn| {
            conn.execute(
                "UPDATE rework_operations SET state='blocked', blocked_reason=?1, updated_at=?2 WHERE id=?3",
                rusqlite::params![reason, timefmt::now(), op.id],
            )?;
            Ok(())
        })?;
        outbox::emit(
            store,
            "workitem",
            &workitem_id,
            "rework.blocked",
            json!({"workitemId": workitem_id, "operationId": op.id, "reason": reason}),
        )?;
        return Err(Error::Message(format!("rework_blocked: {reason}")));
    }
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE rework_operations SET state='completed', completed_at=?1, updated_at=?1 WHERE id=?2",
            rusqlite::params![timefmt::now(), op.id],
        )?;
        Ok(())
    })?;
    outbox::emit(
        store,
        "workitem",
        &workitem_id,
        "rework.completed",
        json!({"workitemId": workitem_id, "operationId": op.id, "targetGate": op.target_gate}),
    )?;
    let op = op_by_id(store, &op.id)?.ok_or_else(|| Error::Message("missing op".into()))?;
    op_view(store, &op)
}

/// executing 崩溃恢复重入（decide(approved) 对 executing/blocked 重放即可达）。
pub fn resume(
    store: &Store,
    operation_id: &str,
    decided_by: &str,
) -> Result<serde_json::Value, Error> {
    let op = op_by_id(store, operation_id)?
        .ok_or_else(|| Error::Message("rework_invalid: 操作不存在".into()))?;
    if op.state != "executing" && op.state != "blocked" {
        return Err(Error::Message(format!(
            "rework_state_changed: 操作处于 {} 状态，无恢复面",
            op.state
        )));
    }
    if op.state == "blocked" && op.blocked_reason.contains("active_run_present") {
        // 活跃 Run 由用户取消后再恢复；直接重试。
    }
    execute(store, &op, decided_by)
}

pub fn get(store: &Store, operation_id: &str) -> Result<serde_json::Value, Error> {
    let op = op_by_id(store, operation_id)?
        .ok_or_else(|| Error::Message(format!("rework_missing: {operation_id}")))?;
    op_view(store, &op)
}

pub fn list(store: &Store, workitem_id: &str) -> Result<Vec<serde_json::Value>, Error> {
    let ops: Vec<ReworkOperation> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {OP_COLS} FROM rework_operations WHERE workitem_id=?1 ORDER BY created_at DESC, rowid DESC"
        ))?;
        let rows = stmt.query_map([workitem_id], op_from)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    })?;
    ops.iter().map(|op| op_view(store, op)).collect()
}

/// 查询小助手：Option<String> 的 optional 行（ rusqlite optional 扩展的本地形态）。
trait OptionalId {
    fn optional_id(self) -> Option<String>;
}
impl OptionalId for rusqlite::Result<String> {
    fn optional_id(self) -> Option<String> {
        self.ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sg_store::timefmt;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-rwk-{}-{}",
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

    /// 推进 workitem：requirements passed → current=design；design passed → current=development。
    fn advance(store: &Store, workitem_id: &str, gate: &str) {
        crate::set_stage(store, workitem_id, gate, crate::StageState::Running, "sha").unwrap();
        crate::pass_gate(store, workitem_id, gate).unwrap();
    }

    /// 在指定关落一条 gate_result + 一条基线（直接 SQL 造事实）。
    fn seed_facts(store: &Store, workitem_id: &str, gate: &str) {
        let _ = store.with_conn(|c| {
            c.execute(
                "INSERT INTO gate_results(id, workitem_id, gate, inputs, result, computed_at)
                 VALUES (?1,?2,?3,'{}','{\"gate\":\"\",\"passed\":true,\"failed_inputs\":[],\"computed_at\":\"t\"}',?4)",
                rusqlite::params![ids::new_id("gr"), workitem_id, gate, timefmt::now()],
            )
            .unwrap();
            c.execute(
                "INSERT INTO baselines(id, workitem_id, gate, revision_map, inputs_sha256, frozen_at)
                 VALUES (?1,?2,?3,'{}','sha',?4)",
                rusqlite::params![ids::new_id("base"), workitem_id, gate, timefmt::now()],
            )
            .unwrap();
            Ok(())
        });
    }

    fn seed_passport(store: &Store, workitem_id: &str) {
        let _ = store.with_conn(|c| {
            c.execute(
                "INSERT INTO passports(id, workitem_id, object_sha256, inputs_sha256, created_at)
                 VALUES (?1,?2,'sha','sha',?3)",
                rusqlite::params![ids::new_id("psp"), workitem_id, timefmt::now()],
            )
            .unwrap();
            Ok(())
        });
    }

    #[test]
    fn cas_changes_with_stage_and_attempt_state() {
        let store = setup();
        let wi = crate::create(&store, "pj", "CAS 任务", "", None, &[]).unwrap();
        let d0 = cas_digest(&store, &wi.id).unwrap();
        advance(&store, &wi.id, "requirements");
        let d1 = cas_digest(&store, &wi.id).unwrap();
        assert_ne!(d0, d1, "阶段/指针变化必须改变 CAS");
        assert_eq!(cas_digest(&store, &wi.id).unwrap(), d1, "CAS 确定");
    }

    #[test]
    fn full_chain_invalidates_interval_facts_and_read_side() {
        let store = setup();
        let wi = crate::create(&store, "pj", "返工任务", "", None, &[]).unwrap();
        advance(&store, &wi.id, "requirements");
        advance(&store, &wi.id, "design");
        // 当前关 development：造 design/development 两关事实（gate_result+baseline+passport）。
        seed_facts(&store, &wi.id, "design");
        seed_facts(&store, &wi.id, "development");
        seed_passport(&store, &wi.id);
        // 读取面基准：design 的 gate_result 可见、护照可见。
        assert!(crate::gate::latest(&store, &wi.id, "design")
            .unwrap()
            .is_some());
        assert!(sg_evidence::latest_passport(&store, &wi.id)
            .unwrap()
            .is_some());

        // 打回 design：preview → request → decide(approved)。
        let pv = preview(&store, &wi.id, "design", "regression", "回归打回", "agent").unwrap();
        assert_eq!(pv["state"], json!("previewed"));
        assert_eq!(pv["intervalGates"], json!(["design", "development"]));
        let req = request(&store, &wi.id, "design", "regression", "回归打回", "agent").unwrap();
        let approval_id = req["approvalId"].as_str().unwrap().to_string();
        let out = decide(&store, &approval_id, "approved", "owner", "批准").unwrap();
        assert_eq!(out["state"], json!("completed"));
        assert!(out["completed_at"].is_string());

        // 失效读取面：区间 gate_result 排除、护照过滤（要求重签）。
        assert!(
            crate::gate::latest(&store, &wi.id, "design")
                .unwrap()
                .is_none(),
            "design 旧 gate_result 已失效"
        );
        assert!(
            crate::gate::latest(&store, &wi.id, "development")
                .unwrap()
                .is_none(),
            "development 旧 gate_result 已失效"
        );
        // 未打回关（requirements）的事实不受影响——无 gate_result，但指针应回到 design。
        let wi_after = crate::get(&store, &wi.id).unwrap();
        assert_eq!(wi_after.current_gate, "design");
        // stages 复位：design/development not_started；requirements 保持 passed。
        let stages = crate::stages(&store, &wi.id).unwrap();
        let st = |g: &str| stages.iter().find(|s| s.gate == g).unwrap().state.clone();
        assert_eq!(st("requirements"), "passed");
        assert_eq!(st("design"), "not_started");
        assert_eq!(st("development"), "not_started");
        // target 新 attempt：prepared + 关前快照。
        let att = crate::attempt::active_for_gate(&store, &wi.id, "design")
            .unwrap()
            .expect("target 新 attempt");
        assert_eq!(att.state, "prepared");
        assert!(!att.entry_snapshot_id.is_empty(), "Step B 关前快照已装配");
        // 护照失效（要求重签）。
        assert!(sg_evidence::latest_passport(&store, &wi.id)
            .unwrap()
            .is_none());
        // 基线失效。
        let sup: i64 = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM baselines WHERE workitem_id=?1 AND superseded_by IS NOT NULL",
                    [&wi.id],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(sup, 2, "design/development 基线已 superseded_by=rework id"); // 失效登记概要：gate_result×2（design/development）在册。
        let counts = out["affectedFacts"].clone();
        let gr = counts
            .as_array()
            .and_then(|a| {
                a.iter()
                    .find(|e| e[0] == json!("gate_result"))
                    .map(|e| e[1].as_i64().unwrap_or(0))
            })
            .unwrap_or(0);
        assert_eq!(gr, 2, "两条区间 gate_result 都在册：{counts}");
        // decide 终态重放：同决定幂等返回。
        let replay = decide(&store, &approval_id, "approved", "owner", "").unwrap();
        assert_eq!(replay["state"], json!("completed"));
    }

    #[test]
    fn cas_drift_between_request_and_decide_fails_operation() {
        let store = setup();
        let wi = crate::create(&store, "pj", "漂移任务", "", None, &[]).unwrap();
        advance(&store, &wi.id, "requirements");
        advance(&store, &wi.id, "design");
        let req = request(&store, &wi.id, "requirements", "other", "n", "agent").unwrap();
        let approval_id = req["approvalId"].as_str().unwrap().to_string();
        // 发起后状态漂移（design 关阶段状态变化）。
        crate::set_stage(
            &store,
            &wi.id,
            "development",
            crate::StageState::Running,
            "sha-x",
        )
        .unwrap();
        let err = decide(&store, &approval_id, "approved", "owner", "").unwrap_err();
        assert!(err.to_string().contains("rework_state_changed"), "{err}");
        let list = list(&store, &wi.id).unwrap();
        assert_eq!(list[0]["state"], json!("failed"));
        assert_eq!(list[0]["blocked_reason"], json!("rework_state_changed"));
    }

    #[test]
    fn invalid_reason_and_forward_target_rejected() {
        let store = setup();
        let wi = crate::create(&store, "pj", "非法参数", "", None, &[]).unwrap();
        assert!(preview(&store, &wi.id, "design", "magic", "", "a").is_err());
        // 当前关 requirements：打回 requirements（不早于当前）拒绝。
        assert!(preview(&store, &wi.id, "requirements", "other", "", "a").is_err());
    }
}
