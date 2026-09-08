//! gate skip operation v2（审计《Ratiflow_RDWS_v1.4实施审计与整改方案_v1.0》§7 P0-3；
//! 权威规格 v1.4 §WP-8「Skip 操作权威」；表 0052）。
//!
//! 与旧路径（approval.decide 直改 stage/attempt、无 operation 行）的差异：
//! - `gate_skip_requests` 是跳关唯一 operation 权威：progress 游标
//!   （prepared → stage_advanced → next_attempt_ready）承载两步执行的崩溃恢复；
//! - `decide` 批准时重算 template/attempt/current state/替代证据内容 digest，
//!   漂移即 expire（不执行）；
//! - Step A（单事务）：审批落态 + request→executing/stage_advanced + 当前
//!   attempt→superseded + stage→skipped（legacy+实例双写）+ 指针推进 +
//!   创建下一关 preparing attempt（幂等：同 request 只产生一个）+ audit/outbox 同事务；
//! - Step B（可恢复对象写）：CAS put 下一关 entry snapshot 对象（内容寻址幂等，
//!   清单含 skip waiver/替代证据 digest 联接）→ 单事务绑定 snapshot 行 +
//!   attempt.entry_snapshot_id/prepared + progress=next_attempt_ready + completed；
//!   失败 → blocked 保留 progress，`resume` 幂等恢复；
//! - `gate.decideSkip` 是唯一决定入口（approval.decide 的 gate_skip 主体路由同实现）；
//! - 存量：approved → legacy_completed 只读投影；pending → requested 投影
//!   （仅可作废，批准一律 gate_skip_state_changed——digest 无法重建不冒充 v2）。

use serde_json::{json, Value};
use sg_store::{ids, outbox, timefmt, Error, Store};

/// 替代证据行（内容摘要参与 digest——不仅绑 id）。
type EvidenceRow = (String, String, String, bool, String);

fn substitute_rows(
    store: &Store,
    workitem_id: &str,
    substitute_ids: &[String],
) -> Result<Vec<EvidenceRow>, Error> {
    let known = sg_evidence::list(store, workitem_id, None)?;
    let mut rows = Vec::new();
    for id in substitute_ids {
        let Some(e) = known.iter().find(|e| &e.id == id) else {
            return Err(Error::Message(format!(
                "substitute_evidence_missing: 证据 {id} 不存在"
            )));
        };
        rows.push((
            e.id.clone(),
            e.kind.clone(),
            e.object_sha256.clone(),
            e.verified,
            e.created_at.clone(),
        ));
    }
    rows.sort();
    Ok(rows)
}

fn sha256_hex(input: &str) -> String {
    use sha2::Digest;
    format!(
        "sha256:{}",
        ids::hex(&sha2::Sha256::digest(input.as_bytes()))
    )
}

fn evidence_digest(rows: &[EvidenceRow]) -> String {
    let lines: Vec<String> = rows
        .iter()
        .map(|(id, kind, object, verified, created)| {
            format!("E|{id}|{kind}|{object}|{verified}|{created}")
        })
        .collect();
    sha256_hex(&lines.join("\n"))
}

fn instance_gate_def(
    store: &Store,
    workitem_id: &str,
    gate: &str,
) -> Result<(sg_workflow::template::GateDefinition, String), Error> {
    let instance = sg_workflow::instance::for_workitem(store, workitem_id)?
        .ok_or_else(|| Error::Message("workflow instance missing".into()))?;
    let defs = sg_workflow::template::definitions_via_store(store, &instance.template_version_id)?;
    let def = defs
        .into_iter()
        .find(|d| d.gate_id == gate)
        .ok_or_else(|| Error::Message(format!("gate_skip_invalid: 未知关 {gate}")))?;
    Ok((def, instance.template_version_id))
}

pub struct SkipRequest {
    pub id: String,
    pub workitem_id: String,
    pub gate: String,
    pub stage_attempt_id: String,
    pub template_version_id: String,
    pub current_state_digest: String,
    pub post_step_a_digest: String,
    pub waiver: String,
    pub substitute_evidence_json: String,
    pub substitute_evidence_digest: String,
    pub approval_id: String,
    pub action_digest: String,
    pub next_attempt_id: Option<String>,
    pub progress: String,
    pub state: String,
    pub blocked_reason: String,
}

const SKIP_COLUMNS: &str = "id, workitem_id, gate, stage_attempt_id, template_version_id,
    current_state_digest, post_step_a_digest, waiver, substitute_evidence_json,
    substitute_evidence_digest, approval_id, action_digest, next_attempt_id, progress, state,
    blocked_reason";

fn row_to_request(r: &rusqlite::Row<'_>) -> rusqlite::Result<SkipRequest> {
    Ok(SkipRequest {
        id: r.get(0)?,
        workitem_id: r.get(1)?,
        gate: r.get(2)?,
        stage_attempt_id: r.get(3)?,
        template_version_id: r.get(4)?,
        current_state_digest: r.get(5)?,
        post_step_a_digest: r.get(6)?,
        waiver: r.get(7)?,
        substitute_evidence_json: r.get(8)?,
        substitute_evidence_digest: r.get(9)?,
        approval_id: r.get(10)?,
        action_digest: r.get(11)?,
        next_attempt_id: r.get(12)?,
        progress: r.get(13)?,
        state: r.get(14)?,
        blocked_reason: r.get(15)?,
    })
}

fn by_id(store: &Store, id: &str) -> Result<Option<SkipRequest>, Error> {
    store.with_conn(|conn| {
        Ok(conn
            .query_row(
                &format!("SELECT {SKIP_COLUMNS} FROM gate_skip_requests WHERE id=?1"),
                [id],
                row_to_request,
            )
            .ok())
    })
}

fn by_approval(store: &Store, approval_id: &str) -> Result<Option<SkipRequest>, Error> {
    store.with_conn(|conn| {
        Ok(conn
            .query_row(
                &format!("SELECT {SKIP_COLUMNS} FROM gate_skip_requests WHERE approval_id=?1"),
                [approval_id],
                row_to_request,
            )
            .ok())
    })
}

fn by_action_digest(store: &Store, digest: &str) -> Result<Option<SkipRequest>, Error> {
    store.with_conn(|conn| {
        Ok(conn
            .query_row(
                &format!("SELECT {SKIP_COLUMNS} FROM gate_skip_requests WHERE action_digest=?1"),
                [digest],
                row_to_request,
            )
            .ok())
    })
}

/// 某任务全部跳关操作（新→旧）——UI 恢复状态读面（P1-5：只展示服务器状态，
/// 不拥有推进权；恢复入口经 gate.resumeSkip intent）。
pub fn list(store: &Store, workitem_id: &str) -> Result<Vec<Value>, Error> {
    let reqs: Vec<SkipRequest> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {SKIP_COLUMNS} FROM gate_skip_requests WHERE workitem_id=?1
             ORDER BY created_at DESC, rowid DESC"
        ))?;
        let rows = stmt.query_map([workitem_id], row_to_request)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    })?;
    reqs.iter().map(|r| view(store, r)).collect()
}

pub fn view(store: &Store, req: &SkipRequest) -> Result<Value, Error> {
    let (approval_state, stage_state, pointer): (String, String, String) =
        store.with_conn(|conn| {
            let approval = conn
                .query_row(
                    "SELECT status FROM approvals WHERE id=?1",
                    [&req.approval_id],
                    |r| r.get(0),
                )
                .unwrap_or_default();
            let stage = conn
                .query_row(
                    "SELECT state FROM workitem_stages WHERE workitem_id=?1 AND gate=?2",
                    rusqlite::params![req.workitem_id, req.gate],
                    |r| r.get(0),
                )
                .unwrap_or_default();
            let pointer = conn
                .query_row(
                    "SELECT current_gate FROM workitems WHERE id=?1",
                    [&req.workitem_id],
                    |r| r.get(0),
                )
                .unwrap_or_default();
            Ok((approval, stage, pointer))
        })?;
    Ok(json!({
        "skipRequestId": req.id,
        "workItemId": req.workitem_id,
        "gate": req.gate,
        "state": req.state,
        "progress": req.progress,
        "blockedReason": req.blocked_reason,
        "approvalId": req.approval_id,
        "approvalState": approval_state,
        "actionDigest": req.action_digest,
        "currentStateDigest": req.current_state_digest,
        "substituteEvidenceDigest": req.substitute_evidence_digest,
        "nextAttemptId": req.next_attempt_id,
        "stageState": stage_state,
        "currentGate": pointer,
    }))
}

/// 发起跳关请求：策略/证据/状态校验 + digest 冻结 + operation 行 + 审批。
/// `expected_state_digest`：调用方冻结的工作项状态 CAS（None = 不校验）。
pub fn request(
    store: &Store,
    workitem_id: &str,
    gate: &str,
    waiver: &str,
    substitute_ids: &[String],
    requested_by: &str,
    expected_state_digest: Option<&str>,
) -> Result<Value, Error> {
    let (def, template_version_id) = instance_gate_def(store, workitem_id, gate)?;
    // 策略（实例冻结版本）：无声明或 forbidden → 拒；部署/迁移类恒 forbidden。
    let manual_allowed = def
        .skip_policy
        .as_ref()
        .map(|p| p.mode == sg_workflow::template::SkipMode::ManualApproval)
        .unwrap_or(false);
    let deployment_class = def
        .deliverables
        .iter()
        .any(|k| sg_workflow::template::is_deployment_class_kind(k));
    if !manual_allowed || deployment_class {
        return Err(Error::Message(format!(
            "gate_skip_forbidden: 关 {gate} 的 skip 策略为 forbidden{}",
            if deployment_class {
                "（部署/迁移类恒 forbidden）"
            } else {
                ""
            }
        )));
    }
    if substitute_ids.is_empty() {
        return Err(Error::Message(
            "substitute_evidence_missing: 替代证据必填".into(),
        ));
    }
    let evidence = substitute_rows(store, workitem_id, substitute_ids)?;
    let subst_digest = evidence_digest(&evidence);
    // 阶段须未开工（先于 ensure_active——被跳过/已开工的关直接状态错误，
    // 不让单活跃约束抢报无关错误族）。
    let stage_state = crate::stages(store, workitem_id)?
        .into_iter()
        .find(|s| s.gate == gate)
        .map(|s| s.state)
        .unwrap_or_default();
    if stage_state != "not_started" {
        return Err(Error::Message(format!(
            "gate_skip_state_changed: 关 {gate} 当前为 {stage_state}，仅未开工关可跳过"
        )));
    }
    // 再确保 attempt 存在、后取状态摘要（ensure_active 可能新建 attempt 行，
    // cas_digest 含活跃 attempt——顺序颠倒会造成自造漂移）。
    let attempt = crate::attempt::ensure_active(store, workitem_id, gate)?;
    let cas = crate::rework::cas_digest(store, workitem_id)?;
    if let Some(expected) = expected_state_digest {
        if cas != expected {
            return Err(Error::Message(
                "gate_skip_state_changed: 工作项状态与 expectedStateDigest 不一致，请刷新后重试"
                    .into(),
            ));
        }
    }
    // action_digest 绑内容：模板版本 + attempt + 状态摘要 + waiver + 证据内容摘要。
    let action_digest = sha256_hex(&format!(
        "gate_skip_v2|{workitem_id}|{template_version_id}|{gate}|{}|{cas}|{waiver}|{subst_digest}",
        attempt.id
    ));
    // 幂等重放：同 digest 既有请求 → 原样返回。
    if let Some(existing) = by_action_digest(store, &action_digest)? {
        return view(store, &existing);
    }
    let approval = match sg_policy::request_approval(
        store,
        "gate_skip",
        &attempt.id,
        &action_digest,
        sg_policy::Risk::High,
        waiver,
        0,
        Some(workitem_id),
        Some(&attempt.id),
    ) {
        Ok(a) => a,
        Err(e) if e.to_string().contains("UNIQUE constraint failed") => {
            // 异 key 同 digest 竞态撞 0050 唯一索引：收敛读既有请求。
            return match by_action_digest(store, &action_digest)? {
                Some(existing) => view(store, &existing),
                None => Err(Error::Message(
                    "gate_skip_race: 审批已存在但请求不可读".into(),
                )),
            };
        }
        Err(e) => return Err(e),
    };
    let id = ids::new_id("gsk");
    let now = timefmt::now();
    let subst_json = serde_json::to_string(
        &evidence
            .iter()
            .map(|(i, k, o, v, c)| {
                json!({"id": i, "kind": k, "objectSha256": o, "verified": v, "createdAt": c})
            })
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".into());
    let inserted = store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO gate_skip_requests
             (id, workitem_id, gate, stage_attempt_id, template_version_id, current_state_digest,
              pre_state_digest, waiver, substitute_evidence_json, substitute_evidence_digest,
              approval_id, action_digest, progress, state, requested_by, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?6,?7,?8,?9,?10,?11,'prepared','requested',?12,?13,?13)",
            rusqlite::params![
                id,
                workitem_id,
                gate,
                attempt.id,
                template_version_id,
                cas,
                waiver,
                subst_json,
                subst_digest,
                approval.id,
                action_digest,
                requested_by,
                now
            ],
        )?;
        Ok(conn.changes() == 1)
    })?;
    if !inserted {
        // 竞态：并发同 digest 已插入 → 收敛返回既有。
        let existing = by_action_digest(store, &action_digest)?
            .ok_or_else(|| Error::Message("gate_skip_race: 请求不可读".into()))?;
        return view(store, &existing);
    }
    outbox::emit(
        store,
        "workitem",
        workitem_id,
        "gate_skip.requested",
        json!({"skipRequestId": id, "gate": gate, "approvalId": approval.id}),
    )?;
    let req =
        by_id(store, &id)?.ok_or_else(|| Error::Message("gate_skip_invalid: 请求不可见".into()))?;
    view(store, &req)
}

/// Step A（单事务，幂等）：审批落态 + request 推进 + attempt superseded +
/// stage skipped（双写）+ 指针 + 下一关 preparing attempt。
/// 返回 next_attempt_id（幂等重入复用既有 preparing 行——崩溃恢复只产生一个）。
fn step_a(store: &Store, req: &SkipRequest, decided_by: &str) -> Result<String, Error> {
    let next_gate = crate::next_gate_id(store, &req.workitem_id, &req.gate)?.ok_or_else(|| {
        Error::Message(format!(
            "gate_skip_invalid: 关 {} 无下一关（末关不可跳过）",
            req.gate
        ))
    })?;
    let now = timefmt::now();
    let next_attempt_id = store.with_tx(|conn| {
        // 审批落态（同事务；幂等——已 approved 不再命中）。
        conn.execute(
            "UPDATE approvals SET status='approved', decided_by=?1, decided_at=?2
             WHERE id=?3 AND status='requested'",
            rusqlite::params![decided_by, now, req.approval_id],
        )?;
        // request → executing/stage_advanced（幂等：progress 已推进时只取 next_attempt_id）。
        conn.execute(
            "UPDATE gate_skip_requests SET state='executing', progress='stage_advanced',
                    decided_by=?1, decided_at=?2, updated_at=?2
             WHERE id=?3 AND progress='prepared'",
            rusqlite::params![decided_by, now, req.id],
        )?;
        // 当前 attempt → superseded（v2 语义；幂等——非终态才命中）。
        conn.execute(
            "UPDATE stage_attempts SET state='superseded', updated_at=?1
             WHERE id=?2 AND state IN ('prepared','running','review_ready','awaiting_user_approval','changes_requested')",
            rusqlite::params![now, req.stage_attempt_id],
        )?;
        // stage → skipped（legacy + 实例投影双写，rework step_a 同型）。
        conn.execute(
            "UPDATE workitem_stages SET state='skipped', updated_at=?1
             WHERE workitem_id=?2 AND gate=?3",
            rusqlite::params![now, req.workitem_id, req.gate],
        )?;
        conn.execute(
            "UPDATE workflow_instance_gates SET state='skipped', updated_at=?1
             WHERE instance_id=(SELECT id FROM workflow_instances WHERE workitem_id=?2)
               AND gate_definition_id IN
                 (SELECT id FROM workflow_gate_definitions
                  WHERE version_id=(SELECT template_version_id FROM workflow_instances WHERE workitem_id=?2)
                    AND gate_id=?3)",
            rusqlite::params![now, req.workitem_id, req.gate],
        )?;
        // 指针推进（Passed|Skipped 同语义；legacy workitems 与实例投影
        // current_gate_id 双写——同事务，对齐 set_stage/project_state 语义）。
        conn.execute(
            "UPDATE workitems SET current_gate=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![next_gate, now, req.workitem_id],
        )?;
        conn.execute(
            "UPDATE workflow_instances SET current_gate_id=?1, updated_at=?2
             WHERE workitem_id=?3",
            rusqlite::params![next_gate, now, req.workitem_id],
        )?;
        // 下一关 preparing attempt：幂等复用（同 request 关联的既有行）。
        let attempt_no: i64 = conn.query_row(
            "SELECT COALESCE(MAX(attempt_no),0)+1 FROM stage_attempts WHERE workitem_id=?1 AND gate=?2",
            rusqlite::params![req.workitem_id, next_gate],
            |r| r.get(0),
        )?;
        let existing_next: Option<String> = conn
            .query_row(
                "SELECT next_attempt_id FROM gate_skip_requests WHERE id=?1",
                [&req.id],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten();
        let next_attempt_id = match existing_next {
            Some(existing) => existing,
            None => {
                let fresh = ids::new_id("att");
                conn.execute(
                    "INSERT INTO stage_attempts(id, workitem_id, gate, attempt_no, branch_no, state,
                         entry_snapshot_id, input_package_sha256, active_output_package_id,
                         predecessor_attempt_id, created_at, updated_at)
                     VALUES (?1,?2,?3,?4,1,'preparing','','',NULL,NULL,?5,?5)",
                    rusqlite::params![fresh, req.workitem_id, next_gate, attempt_no, now],
                )?;
                fresh
            }
        };
        // post_step_a_digest：Step A 写入事实的摘要（Step B/恢复漂移检测用）。
        let post_digest = sha256_hex(&format!(
            "post_a|{}|{}|{}|{}|skipped|{next_gate}",
            req.workitem_id, req.gate, req.stage_attempt_id, next_attempt_id
        ));
        conn.execute(
            "UPDATE gate_skip_requests SET next_attempt_id=?1, post_step_a_digest=?2, updated_at=?3
             WHERE id=?4",
            rusqlite::params![next_attempt_id, post_digest, now, req.id],
        )?;
        sg_store::audit::append_at(
            conn,
            decided_by,
            "gate_skip.step_a",
            "workitem",
            &req.workitem_id,
            json!({
                "skipRequestId": req.id, "gate": req.gate, "supersededAttemptId": req.stage_attempt_id,
                "nextGate": next_gate, "nextAttemptId": next_attempt_id,
            }),
        )?;
        outbox::emit_at(
            conn,
            "workitem",
            &req.workitem_id,
            "stage.skipped",
            json!({"gate": req.gate, "skipRequestId": req.id, "outcome": "skipped_with_waiver"}),
        )?;
        Ok(next_attempt_id)
    })?;
    // 下一关配套事实（活动行/谱系节点/父边；幂等——崩溃窗口由 Step B 兜底补齐）。
    ensure_next_attempt_facts(store, &req.workitem_id, &next_gate, &next_attempt_id)?;
    Ok(next_attempt_id)
}

/// 下一关 attempt 的配套事实（幂等，可重入）：模板活动行 + 谱系节点 + 最小父边。
/// Step A 事务提交后调用；resume/Step B 前兜底——任一崩溃点恢复都不缺事实。
fn ensure_next_attempt_facts(
    store: &Store,
    workitem_id: &str,
    next_gate: &str,
    next_attempt_id: &str,
) -> Result<(), Error> {
    let now = timefmt::now();
    let existing: i64 = store.with_conn(|conn| {
        Ok(conn
            .query_row(
                "SELECT COUNT(*) FROM stage_activities WHERE stage_attempt_id=?1",
                [next_attempt_id],
                |r| r.get(0),
            )
            .unwrap_or(0))
    })?;
    if existing == 0 {
        store.with_conn(|conn| {
            for (ordinal, (key, title)) in
                crate::attempt::template_activities(next_gate).into_iter().enumerate()
            {
                conn.execute(
                    "INSERT INTO stage_activities(id, stage_attempt_id, activity_key, ordinal, title, created_at, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?6)",
                    rusqlite::params![
                        ids::new_id("act"),
                        next_attempt_id,
                        key,
                        (ordinal + 1) as i64,
                        title,
                        now
                    ],
                )?;
            }
            Ok(())
        })?;
    }
    sg_provenance::register_node(
        store,
        &sg_provenance::NodeInput {
            project_id: "",
            workitem_id,
            node_type: sg_provenance::node_type::STAGE_ATTEMPT,
            entity_id: next_attempt_id,
            content_digest: "",
            verification_state: "verified",
        },
    )?;
    if let Some(revision_id) = crate::requirements::latest_revision_id(store, workitem_id)? {
        sg_provenance::add_edge(
            store,
            &sg_provenance::EdgeInput {
                workitem_id,
                from_node_type: sg_provenance::node_type::STAGE_ATTEMPT,
                from_entity_id: next_attempt_id,
                relation: sg_provenance::relation::DERIVED_FROM,
                to_node_type: sg_provenance::node_type::REQUIREMENT_REVISION,
                to_entity_id: &revision_id,
                stage_attempt_id: "",
                created_by_run_id: "",
            },
        )
        .ok();
    }
    Ok(())
}

/// Step B（可恢复）：snapshot 对象 CAS put（内容寻址幂等，清单含 skip 联接）→
/// 单事务绑定 snapshot 行 + attempt prepared + request completed。
fn step_b(store: &Store, req: &SkipRequest, next_attempt_id: &str) -> Result<(), Error> {
    if req.progress == "next_attempt_ready" {
        return Ok(());
    }
    let next_gate = crate::next_gate_id(store, &req.workitem_id, &req.gate)?
        .ok_or_else(|| Error::Message("gate_skip_invalid: 下一关不可解析".into()))?;
    // 崩溃兜底：Step A 后、事实登记前崩溃 → 此处幂等补齐。
    ensure_next_attempt_facts(store, &req.workitem_id, &next_gate, next_attempt_id)?;
    // 对象写（幂等；失败 → blocked，resume 重入）。
    let snapshot = crate::snapshot::create_for_skip(
        store,
        &req.workitem_id,
        &next_gate,
        next_attempt_id,
        &json!({
            "skipRequestId": req.id,
            "skippedGate": req.gate,
            "waiver": req.waiver,
            "substituteEvidenceDigest": req.substitute_evidence_digest,
        }),
    )
    .map_err(|e| {
        mark_blocked(store, req, &format!("skip_snapshot_write_failed: {e}"));
        Error::Message(format!("skip_snapshot_write_failed: {e}"))
    })?;
    let now = timefmt::now();
    store
        .with_tx(|conn| {
            conn.execute(
                "UPDATE stage_attempts SET entry_snapshot_id=?1, state='prepared', updated_at=?2
                 WHERE id=?3 AND state='preparing'",
                rusqlite::params![snapshot.id, now, next_attempt_id],
            )?;
            let n = conn.execute(
                "UPDATE gate_skip_requests SET progress='next_attempt_ready', state='completed',
                        completed_at=?1, blocked_reason='', updated_at=?1
                 WHERE id=?2 AND progress='stage_advanced'",
                rusqlite::params![now, req.id],
            )?;
            if n != 1 {
                return Err(Error::Message(
                    "gate_skip_state_changed: 请求进度已漂移".into(),
                ));
            }
            sg_store::audit::append_at(
                conn,
                "system",
                "gate_skip.completed",
                "workitem",
                &req.workitem_id,
                json!({"skipRequestId": req.id, "nextAttemptId": next_attempt_id, "snapshotId": snapshot.id}),
            )?;
            outbox::emit_at(
                conn,
                "workitem",
                &req.workitem_id,
                "gate_skip.completed",
                json!({"skipRequestId": req.id, "gate": req.gate, "nextAttemptId": next_attempt_id}),
            )?;
            Ok(())
        })
        .inspect_err(|e| {
            mark_blocked(store, req, &e.to_string());
        })
}

fn mark_blocked(store: &Store, req: &SkipRequest, reason: &str) {
    let _ = store.with_conn(|conn| {
        conn.execute(
            "UPDATE gate_skip_requests SET state='blocked', blocked_reason=?1, updated_at=?2
             WHERE id=?3 AND state IN ('executing','blocked')",
            rusqlite::params![reason, timefmt::now(), req.id],
        )?;
        Ok(())
    });
    let _ = outbox::emit(
        store,
        "workitem",
        &req.workitem_id,
        "gate_skip.blocked",
        json!({"skipRequestId": req.id, "reason": reason}),
    );
}

/// 唯一决定入口（approval.decide 的 gate_skip 主体路由同实现）。
/// 决定时重算 template/attempt/state/evidence digest，漂移即 expire。
pub fn decide(
    store: &Store,
    approval_id: &str,
    decision: &str,
    decided_by: &str,
    reason: &str,
) -> Result<Value, Error> {
    if !matches!(decision, "approved" | "rejected") {
        return Err(Error::Message(
            "decision must be approved|rejected for gate_skip".into(),
        ));
    }
    let req = by_approval(store, approval_id)?
        .ok_or_else(|| Error::Message("gate_skip_invalid: 审批不对应跳关请求".into()))?;
    // 终态幂等重放。
    match req.state.as_str() {
        "completed" | "blocked" if decision == "approved" => return view(store, &req),
        "rejected" if decision == "rejected" => return view(store, &req),
        "legacy_completed" => {
            return Err(Error::Message(
                "gate_skip_state_changed: legacy 跳关已完结（只读投影，不可再决）".into(),
            ));
        }
        _ => {}
    }
    if req.state == "requested" && req.current_state_digest.is_empty() {
        // legacy pending 投影：只允许作废（安全收紧通道）。
        if decision == "rejected" {
            return decide_reject(store, &req, decided_by, reason);
        }
        return Err(Error::Message(
            "gate_skip_state_changed: legacy 请求无法重建摘要，不可批准（请作废后重新发起）".into(),
        ));
    }
    if decision == "rejected" {
        return decide_reject(store, &req, decided_by, reason);
    }

    // 批准：CAS 重查四要素（模板版本 / attempt / 状态摘要 / 替代证据内容）。
    let (_, template_version_id) = instance_gate_def(store, &req.workitem_id, &req.gate)?;
    let attempt_ok = crate::attempt::active_for_gate(store, &req.workitem_id, &req.gate)?
        .map(|a| a.id == req.stage_attempt_id)
        .unwrap_or(false);
    let cas = crate::rework::cas_digest(store, &req.workitem_id)?;
    let subst_ids: Vec<String> = serde_json::from_str::<Vec<Value>>(&req.substitute_evidence_json)
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.get("id").and_then(|i| i.as_str()).map(String::from))
        .collect();
    let current_subst = substitute_rows(store, &req.workitem_id, &subst_ids)
        .map(|rows| evidence_digest(&rows))
        .unwrap_or_default();
    let drifted = template_version_id != req.template_version_id
        || !attempt_ok
        || cas != req.current_state_digest
        || current_subst != req.substitute_evidence_digest;
    if drifted {
        expire_request(store, &req, decided_by)?;
        return Err(Error::Message(
            "gate_skip_state_changed: 审批等待期间状态已漂移，请求已过期（请重新发起）".into(),
        ));
    }
    // approval 状态校验（requested → 批准；approved + 已推进 = Step A 后崩溃重放，续跑）。
    let approval_state = store.with_conn(|conn| {
        Ok(conn
            .query_row(
                "SELECT status FROM approvals WHERE id=?1",
                [&req.approval_id],
                |r| r.get::<_, String>(0),
            )
            .unwrap_or_default())
    })?;
    match approval_state.as_str() {
        "requested" => {}
        "approved" if req.progress != "prepared" => {}
        other => {
            return Err(Error::Message(format!(
                "gate_skip_state_changed: 审批状态 {other} 不能批准"
            )));
        }
    }
    let next_attempt_id = step_a(store, &req, decided_by)?;
    let fresh = by_id(store, &req.id)?
        .ok_or_else(|| Error::Message("gate_skip_invalid: 请求不可见".into()))?;
    step_b(store, &fresh, &next_attempt_id)?;
    let done = by_id(store, &req.id)?
        .ok_or_else(|| Error::Message("gate_skip_invalid: 请求不可见".into()))?;
    view(store, &done)
}

fn decide_reject(
    store: &Store,
    req: &SkipRequest,
    decided_by: &str,
    reason: &str,
) -> Result<Value, Error> {
    let now = timefmt::now();
    store.with_tx(|conn| {
        conn.execute(
            "UPDATE approvals SET status='rejected', decided_by=?1, decided_at=?2
             WHERE id=?3 AND status='requested'",
            rusqlite::params![decided_by, now, req.approval_id],
        )?;
        conn.execute(
            "UPDATE gate_skip_requests SET state='rejected', decided_by=?1, decided_at=?2,
                    updated_at=?2 WHERE id=?3 AND state='requested'",
            rusqlite::params![decided_by, now, req.id],
        )?;
        sg_store::audit::append_at(
            conn,
            decided_by,
            "gate_skip.rejected",
            "workitem",
            &req.workitem_id,
            json!({"skipRequestId": req.id, "reason": reason}),
        )?;
        outbox::emit_at(
            conn,
            "workitem",
            &req.workitem_id,
            "gate_skip.rejected",
            json!({"skipRequestId": req.id, "gate": req.gate}),
        )?;
        Ok(())
    })?;
    let fresh = by_id(store, &req.id)?
        .ok_or_else(|| Error::Message("gate_skip_invalid: 请求不可见".into()))?;
    view(store, &fresh)
}

fn expire_request(store: &Store, req: &SkipRequest, actor: &str) -> Result<(), Error> {
    let now = timefmt::now();
    store.with_tx(|conn| {
        conn.execute(
            "UPDATE gate_skip_requests SET state='expired', updated_at=?1
             WHERE id=?2 AND state='requested'",
            rusqlite::params![now, req.id],
        )?;
        conn.execute(
            "UPDATE approvals SET status='expired', reason='跳关等待期间状态漂移，请求失效'
             WHERE id=?1 AND status='requested'",
            rusqlite::params![req.approval_id],
        )?;
        sg_store::audit::append_at(
            conn,
            actor,
            "gate_skip.expired",
            "workitem",
            &req.workitem_id,
            json!({"skipRequestId": req.id}),
        )?;
        Ok(())
    })
}

/// 幂等恢复：blocked/executing（Step A 后崩溃）→ 续跑 Step B；其余状态原样返回。
pub fn resume(store: &Store, request_id: &str) -> Result<Value, Error> {
    let req = by_id(store, request_id)?
        .ok_or_else(|| Error::Message(format!("gate_skip_invalid: 请求 {request_id} 不存在")))?;
    match (req.state.as_str(), req.progress.as_str()) {
        ("blocked", "stage_advanced") | ("executing", "stage_advanced") => {
            let next = req
                .next_attempt_id
                .clone()
                .ok_or_else(|| Error::Message("gate_skip_invalid: 缺 next_attempt_id".into()))?;
            step_b(store, &req, &next)?;
            let done = by_id(store, &req.id)?
                .ok_or_else(|| Error::Message("gate_skip_invalid: 请求不可见".into()))?;
            view(store, &done)
        }
        ("blocked", "next_attempt_ready") => {
            // Step B DB 收尾已提交、响应前崩溃 → 只补终态字段。
            let now = timefmt::now();
            store.with_conn(|conn| {
                conn.execute(
                    "UPDATE gate_skip_requests SET state='completed', blocked_reason='', updated_at=?1
                     WHERE id=?2 AND progress='next_attempt_ready' AND state='blocked'",
                    rusqlite::params![now, req.id],
                )?;
                Ok(())
            })?;
            let done = by_id(store, &req.id)?
                .ok_or_else(|| Error::Message("gate_skip_invalid: 请求不可见".into()))?;
            view(store, &done)
        }
        _ => view(store, &req),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-skip-{}-{}",
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

    fn workitem_skippable(store: &Store) -> String {
        let t = sg_workflow::template::create_template(store, "skip-tpl", "可跳模板").unwrap();
        let v = sg_workflow::template::create_version(
            store,
            &t.id,
            &[
                sg_workflow::template::GateDefInput {
                    gate_id: "build".into(),
                    title: "构建关".into(),
                    purpose: String::new(),
                    deliverables: vec!["code".into()],
                    acceptance: vec![],
                    context_policy_ref: None,
                    team_policy_ref: None,
                    workspace_policy_ref: None,
                    skip_policy: Some(sg_workflow::template::SkipPolicy {
                        mode: sg_workflow::template::SkipMode::ManualApproval,
                    }),
                    fast_track_policy: None,
                },
                sg_workflow::template::GateDefInput {
                    gate_id: "verify".into(),
                    title: "验证关".into(),
                    purpose: String::new(),
                    deliverables: vec!["verification".into()],
                    acceptance: vec![],
                    context_policy_ref: None,
                    team_policy_ref: None,
                    workspace_policy_ref: None,
                    skip_policy: None,
                    fast_track_policy: None,
                },
            ],
            "tester",
        )
        .unwrap();
        sg_workflow::template::activate(store, &v.id).unwrap();
        crate::create_with_template(store, "pj", "跳关任务", "", None, &[], Some("skip-tpl"))
            .unwrap()
            .id
    }

    fn seed_evidence(store: &Store, wi: &str, gate: &str) -> String {
        let now = timefmt::now();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO evidences(id, workitem_id, gate, kind, title, object_sha256, payload, source, verified, verified_at, verified_by, created_at)
                     VALUES ('ev_s1', ?1, ?2, 'manual', '替代核验', 'obj', '{}', 'local', 1, ?3, 'qa', ?3)",
                    rusqlite::params![wi, gate, now],
                )?;
                Ok(())
            })
            .unwrap();
        "ev_s1".into()
    }

    #[test]
    fn request_decide_reject_flow() {
        let store = setup();
        let wi = workitem_skippable(&store);
        let ev = seed_evidence(&store, &wi, "build");
        // 拒绝路径：request → decide(rejected) → 状态 rejected、stage 不变。
        let req = request(&store, &wi, "build", "线下评审", &[ev], "owner", None).unwrap();
        assert_eq!(req["state"], json!("requested"));
        assert_eq!(req["approvalState"], json!("requested"));
        let out = decide(
            &store,
            req["approvalId"].as_str().unwrap(),
            "rejected",
            "owner",
            "不批",
        )
        .unwrap();
        assert_eq!(out["state"], json!("rejected"));
        let stage: String = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT state FROM workitem_stages WHERE workitem_id=?1 AND gate='build'",
                    [&wi],
                    |r| r.get(0),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(stage, "not_started", "拒绝不改阶段");
    }

    #[test]
    fn approve_drift_expires_request() {
        let store = setup();
        let wi = workitem_skippable(&store);
        let ev = seed_evidence(&store, &wi, "build");
        let req = request(&store, &wi, "build", "线下评审", &[ev], "owner", None).unwrap();
        // 等待期漂移：替代证据内容变化（verified 翻转 → 证据内容摘要变）→
        // 四要素 CAS 重查不过 → 批准拒绝 + 请求过期。
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE evidences SET verified=0, verified_at=NULL, verified_by=NULL WHERE id='ev_s1'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let err = decide(
            &store,
            req["approvalId"].as_str().unwrap(),
            "approved",
            "owner",
            "",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("gate_skip_state_changed"), "{err}");
        let fresh = by_id(&store, req["skipRequestId"].as_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(fresh.state, "expired", "漂移使请求过期");
        let approval: String = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT status FROM approvals WHERE id=?1",
                    [&fresh.approval_id],
                    |r| r.get(0),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(approval, "expired", "审批同步失效");
    }

    #[test]
    fn approve_completes_two_step_and_resume_idempotent() {
        let store = setup();
        let wi = workitem_skippable(&store);
        let ev = seed_evidence(&store, &wi, "build");
        let req = request(&store, &wi, "build", "线下评审", &[ev], "owner", None).unwrap();
        let out = decide(
            &store,
            req["approvalId"].as_str().unwrap(),
            "approved",
            "owner",
            "批",
        )
        .unwrap();
        assert_eq!(out["state"], json!("completed"), "两步执行完成：{out}");
        assert_eq!(out["progress"], json!("next_attempt_ready"));
        let next_attempt = out["nextAttemptId"].as_str().unwrap().to_string();
        assert!(!next_attempt.is_empty(), "产生下一关 attempt");
        // 阶段 skipped + 指针推进 + 单一 prepared attempt + entry snapshot 绑定。
        let (stage, pointer, states, snap): (String, String, Vec<(String, String)>, String) = store
            .with_conn(|c| {
                let stage = c
                    .query_row(
                        "SELECT state FROM workitem_stages WHERE workitem_id=?1 AND gate='build'",
                        [&wi],
                        |r| r.get(0),
                    )
                    .unwrap();
                let pointer: String = c
                    .query_row("SELECT current_gate FROM workitems WHERE id=?1", [&wi], |r| {
                        r.get(0)
                    })
                    .unwrap();
                let mut stmt = c
                    .prepare("SELECT id, state FROM stage_attempts WHERE workitem_id=?1 AND gate='verify'")
                    .unwrap();
                let states = stmt
                    .query_map([&wi], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                    })
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                let snap: String = c
                    .query_row(
                        "SELECT COALESCE(entry_snapshot_id,'') FROM stage_attempts WHERE id=?1",
                        [&next_attempt],
                        |r| r.get(0),
                    )
                    .unwrap();
                Ok((stage, pointer, states, snap))
            })
            .unwrap();
        assert_eq!(stage, "skipped");
        assert_eq!(pointer, "verify", "指针推进到下一关");
        assert_eq!(states.len(), 1, "只产生一个下一关 attempt");
        assert_eq!(states[0].0, next_attempt);
        assert_eq!(states[0].1, "prepared", "attempt 已 prepared");
        assert!(!snap.is_empty(), "entry snapshot 已绑定");
        // resume 幂等：completed 原样返回，不新增 attempt。
        let resumed = resume(&store, req["skipRequestId"].as_str().unwrap()).unwrap();
        assert_eq!(resumed["state"], json!("completed"));
        let count: i64 = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM stage_attempts WHERE workitem_id=?1 AND gate='verify'",
                    [&wi],
                    |r| r.get(0),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(count, 1, "resume 不重复创建 attempt");
    }

    #[test]
    fn crash_after_step_a_resumes_single_attempt() {
        // 崩溃注入（Step A 提交后、Step B 前）：手工构造 executing/stage_advanced
        // 中间态 → resume 只补 Step B，且仍只有一个下一关 attempt。
        let store = setup();
        let wi = workitem_skippable(&store);
        let ev = seed_evidence(&store, &wi, "build");
        let req = request(&store, &wi, "build", "线下评审", &[ev], "owner", None).unwrap();
        let req_id = req["skipRequestId"].as_str().unwrap().to_string();
        let approval_id = req["approvalId"].as_str().unwrap().to_string();
        // 模拟 Step A 已提交（直接调用内部链路的一半：借 decide 到 Step A 后打断
        // 不可行——改为手工构造等价中间态：执行 step_a 后立刻置 blocked）。
        let model = by_approval(&store, &approval_id).unwrap().unwrap();
        let next = step_a(&store, &model, "owner").unwrap();
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE gate_skip_requests SET state='blocked', blocked_reason='crashInjected'
                     WHERE id=?1",
                    [&req_id],
                )?;
                Ok(())
            })
            .unwrap();
        let out = resume(&store, &req_id).unwrap();
        assert_eq!(
            out["state"],
            json!("completed"),
            "Step A 后崩溃 → resume 收尾：{out}"
        );
        assert_eq!(out["nextAttemptId"].as_str().unwrap(), next);
        let count: i64 = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM stage_attempts WHERE workitem_id=?1 AND gate='verify'",
                    [&wi],
                    |r| r.get(0),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(count, 1, "恢复只产生一个下一关 attempt");
    }

    #[test]
    fn forbidden_gate_rejected() {
        let store = setup();
        let wi = workitem_skippable(&store);
        let ev = seed_evidence(&store, &wi, "verify");
        let err = request(&store, &wi, "verify", "w", &[ev], "owner", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("gate_skip_forbidden"), "{err}");
    }
}
