//! StageAttempt（ADR-030 M2 / 蓝图 §4.1）：可查看、可审批、可回滚的最小执行单元。
//! 状态机合法迁移 + 每 WorkItem 单活跃约束（0017 partial unique index 兜底）；
//! 活动来自阶段模板（activity_key 是项目模板，不是 Agent 绑定——绑定属 M4）。
//! entry_snapshot_id 在 M2 为空串：0018 落 state_snapshots 后由应用校验非空。

use serde::Serialize;
use sg_provenance::{node_type, relation, EdgeInput, NodeInput};
use sg_store::{ids, outbox, timefmt, Error, Store};

/// 活跃状态集合（与 0017 partial unique index 保持一致）。
/// preparing 是事务内过渡态：快照失败即停留（不可执行、不占单活跃名额，SG-RBK-001）。
pub const ACTIVE_STATES: [&str; 5] = [
    "prepared",
    "running",
    "review_ready",
    "awaiting_user_approval",
    "changes_requested",
];

#[derive(Debug, Clone, Serialize)]
pub struct StageAttempt {
    pub id: String,
    pub workitem_id: String,
    pub gate: String,
    pub attempt_no: i64,
    pub branch_no: i64,
    pub state: String,
    pub entry_snapshot_id: String,
    pub input_package_sha256: String,
    pub active_output_package_id: Option<String>,
    pub predecessor_attempt_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StageActivity {
    pub id: String,
    pub stage_attempt_id: String,
    pub activity_key: String,
    pub ordinal: i64,
    pub title: String,
    pub state: String,
}

/// 阶段模板（蓝图 §8.2 示例的 activity_key；未绑定的活动在 M4 逐个回退通用 Agent）。
/// M1-04：按 gate_id 匹配内建六关模板；自定义模板关卡回退通用单活动
/// （模板级 activity 模板属 M4 Context 域扩展点）。
pub fn template_activities(gate_id: &str) -> Vec<(&'static str, &'static str)> {
    match gate_id {
        "requirements" => vec![("requirement_analysis", "需求分析")],
        "design" => vec![
            ("prototype_design", "原型设计"),
            ("technical_design", "技术设计"),
        ],
        "development" => vec![
            ("frontend", "前端开发"),
            ("backend", "后端开发"),
            ("code_analysis", "代码分析"),
        ],
        "testing" => vec![("e2e_testing", "E2E 测试")],
        "deployment" => vec![
            ("release_planning", "发布计划"),
            ("deployment_verification", "部署验证"),
        ],
        "verification" => vec![("acceptance_verification", "验收验证")],
        _ => vec![("execution", "执行")],
    }
}

/// 合法迁移表（蓝图 §4.1 状态图；changes_requested 继续当前 attempt）。
pub fn can_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("preparing", "prepared")
            // WP-8：跳关批准后取消其未开工 attempt（单活跃约束不阻塞后续关）。
            | ("preparing", "cancelled")
            | ("prepared", "cancelled")
            | ("prepared", "running")
            | ("running", "review_ready")
            | ("review_ready", "awaiting_user_approval")
            | ("awaiting_user_approval", "approved")
            | ("awaiting_user_approval", "changes_requested")
            | ("awaiting_user_approval", "rejected")
            | ("changes_requested", "running")
            | ("running", "failed")
            | ("running", "cancelled")
            | ("approved", "superseded")
            | ("prepared", "rolled_back")
            | ("running", "rolled_back")
            | ("review_ready", "rolled_back")
            | ("awaiting_user_approval", "rolled_back")
    )
}

const ATTEMPT_COLUMNS: &str = "id, workitem_id, gate, attempt_no, branch_no, state, entry_snapshot_id, input_package_sha256, active_output_package_id, predecessor_attempt_id, created_at, updated_at";

fn row_attempt(r: &rusqlite::Row<'_>) -> rusqlite::Result<StageAttempt> {
    Ok(StageAttempt {
        id: r.get(0)?,
        workitem_id: r.get(1)?,
        gate: r.get(2)?,
        attempt_no: r.get(3)?,
        branch_no: r.get(4)?,
        state: r.get(5)?,
        entry_snapshot_id: r.get(6)?,
        input_package_sha256: r.get(7)?,
        active_output_package_id: r.get(8)?,
        predecessor_attempt_id: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

pub fn get(store: &Store, attempt_id: &str) -> Result<StageAttempt, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            &format!("SELECT {ATTEMPT_COLUMNS} FROM stage_attempts WHERE id=?1"),
            [attempt_id],
            row_attempt,
        )
        .map_err(|_| Error::Message(format!("not_found: attempt {attempt_id}")))
    })
}

/// 全部 attempt（attempt_no 升序，含终态——历史不删）。
pub fn list(store: &Store, workitem_id: &str) -> Result<Vec<StageAttempt>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {ATTEMPT_COLUMNS} FROM stage_attempts WHERE workitem_id=?1
             ORDER BY gate, attempt_no"
        ))?;
        let rows = stmt.query_map([workitem_id], row_attempt)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

pub fn active_for_gate(
    store: &Store,
    workitem_id: &str,
    gate_id: &str,
) -> Result<Option<StageAttempt>, Error> {
    store.with_conn(|conn| {
        let attempt = conn
            .query_row(
                &format!(
                    "SELECT {ATTEMPT_COLUMNS} FROM stage_attempts
                     WHERE workitem_id=?1 AND gate=?2 AND state IN ({ACTIVE_PLACEHOLDERS})
                     ORDER BY attempt_no DESC LIMIT 1"
                ),
                rusqlite::params![
                    workitem_id,
                    gate_id,
                    ACTIVE_STATES[0],
                    ACTIVE_STATES[1],
                    ACTIVE_STATES[2],
                    ACTIVE_STATES[3],
                    ACTIVE_STATES[4]
                ],
                row_attempt,
            )
            .ok();
        Ok(attempt)
    })
}

const ACTIVE_PLACEHOLDERS: &str = "?,?,?,?,?";

/// 当前 WorkItem 的活跃 attempt（任意关）。
pub fn active(store: &Store, workitem_id: &str) -> Result<Option<StageAttempt>, Error> {
    store.with_conn(|conn| {
        let attempt = conn
            .query_row(
                &format!(
                    "SELECT {ATTEMPT_COLUMNS} FROM stage_attempts
                     WHERE workitem_id=?1 AND state IN ({ACTIVE_PLACEHOLDERS})
                     ORDER BY created_at DESC LIMIT 1"
                ),
                rusqlite::params![
                    workitem_id,
                    ACTIVE_STATES[0],
                    ACTIVE_STATES[1],
                    ACTIVE_STATES[2],
                    ACTIVE_STATES[3],
                    ACTIVE_STATES[4]
                ],
                row_attempt,
            )
            .ok();
        Ok(attempt)
    })
}

/// 指定关的最新 attempt（含终态；无则 None）。
pub fn latest_for_gate(
    store: &Store,
    workitem_id: &str,
    gate_id: &str,
) -> Result<Option<StageAttempt>, Error> {
    store.with_conn(|conn| {
        let attempt = conn
            .query_row(
                &format!(
                    "SELECT {ATTEMPT_COLUMNS} FROM stage_attempts
                     WHERE workitem_id=?1 AND gate=?2 ORDER BY attempt_no DESC LIMIT 1"
                ),
                [workitem_id, gate_id],
                row_attempt,
            )
            .ok();
        Ok(attempt)
    })
}

pub fn activities(store: &Store, attempt_id: &str) -> Result<Vec<StageActivity>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, stage_attempt_id, activity_key, ordinal, title, state
             FROM stage_activities WHERE stage_attempt_id=?1 ORDER BY ordinal",
        )?;
        let rows = stmt.query_map([attempt_id], |r| {
            Ok(StageActivity {
                id: r.get(0)?,
                stage_attempt_id: r.get(1)?,
                activity_key: r.get(2)?,
                ordinal: r.get(3)?,
                title: r.get(4)?,
                state: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

/// 创建新 attempt（state=prepared）：attempt_no 递增；建模板活动；
/// 谱系最小父边：derived_from → 最新需求修订；非需求关再 derived_from → 上一关已批准 attempt。
pub fn create(
    store: &Store,
    workitem_id: &str,
    gate_id: &str,
    predecessor_attempt_id: Option<&str>,
) -> Result<StageAttempt, Error> {
    crate::get(store, workitem_id)?;
    let previous_no: i64 = store.with_conn(|conn| {
        conn.query_row(
            "SELECT COALESCE(MAX(attempt_no),0) FROM stage_attempts WHERE workitem_id=?1 AND gate=?2",
            [workitem_id, gate_id],
            |r| r.get(0),
        )
        .map_err(Error::from)
    })?;
    let id = ids::new_id("att");
    let now = timefmt::now();
    let branch_no: i64 = match predecessor_attempt_id {
        Some(pred) => store
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COALESCE(branch_no,1) FROM stage_attempts WHERE id=?1",
                    [pred],
                    |r| r.get(0),
                )
                .map_err(Error::from)
            })
            .unwrap_or(1),
        None => 1,
    };
    let inserted = store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO stage_attempts(id, workitem_id, gate, attempt_no, branch_no, state, entry_snapshot_id, input_package_sha256, active_output_package_id, predecessor_attempt_id, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,'preparing','','',NULL,?6,?7,?7)",
            rusqlite::params![id, workitem_id, gate_id, previous_no + 1, branch_no, predecessor_attempt_id, now],
        )?;
        Ok(conn.changes())
    })?;
    if inserted == 0 {
        return Err(Error::Message("trace_incomplete: attempt 创建失败".into()));
    }
    // SG-RBK-001：preparing 是事务内过渡态——先建行，再建关前快照，快照成功才转 prepared。
    // 快照失败 → 错误上抛，attempt 停留 preparing（不可执行、不占单活跃名额）。
    // 受管 worktree 先于快照创建（best-effort：不可用时快照诚实记录，执行回退 local_root）。
    let _ = crate::worktree::ensure(store, workitem_id);
    let snapshot = crate::snapshot::create(store, workitem_id, gate_id, &id, "stage_entry")?;
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE stage_attempts SET state='prepared', entry_snapshot_id=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![snapshot.id, timefmt::now(), id],
        )?;
        Ok(())
    })?;
    for (ordinal, (key, title)) in template_activities(gate_id).into_iter().enumerate() {
        store.with_conn(|conn| {
            conn.execute(
                "INSERT INTO stage_activities(id, stage_attempt_id, activity_key, ordinal, title, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?6)",
                rusqlite::params![ids::new_id("act"), id, key, (ordinal + 1) as i64, title, now],
            )?;
            Ok(())
        })?;
    }
    // 谱系：节点 + 最小父边（写不进即失败——fail-closed，蓝图 §6.1）。
    sg_provenance::register_node(
        store,
        &NodeInput {
            project_id: "",
            workitem_id,
            node_type: node_type::STAGE_ATTEMPT,
            entity_id: &id,
            content_digest: "",
            verification_state: "verified",
        },
    )?;
    if let Some(revision_id) = crate::requirements::latest_revision_id(store, workitem_id)? {
        sg_provenance::add_edge(
            store,
            &EdgeInput {
                workitem_id,
                from_node_type: node_type::STAGE_ATTEMPT,
                from_entity_id: &id,
                relation: relation::DERIVED_FROM,
                to_node_type: node_type::REQUIREMENT_REVISION,
                to_entity_id: &revision_id,
                stage_attempt_id: "",
                created_by_run_id: "",
            },
        )?;
    }
    // M1-04：上一关按实例顺序解析（自定义模板同样建立 attempt 谱系父边）。
    {
        let refs = crate::gate_refs(store, workitem_id)?;
        let ordinal = refs.iter().position(|g| g.gate_id == gate_id);
        if ordinal.is_some_and(|i| i > 0) {
            if let Some(prev_gate) = ordinal.and_then(|i| refs.get(i - 1)) {
                if let Some(prev) = latest_for_gate(store, workitem_id, &prev_gate.gate_id)? {
                    if prev.state == "approved" {
                        sg_provenance::add_edge(
                            store,
                            &EdgeInput {
                                workitem_id,
                                from_node_type: node_type::STAGE_ATTEMPT,
                                from_entity_id: &id,
                                relation: relation::DERIVED_FROM,
                                to_node_type: node_type::STAGE_ATTEMPT,
                                to_entity_id: &prev.id,
                                stage_attempt_id: "",
                                created_by_run_id: "",
                            },
                        )?;
                    }
                }
            }
        }
    }
    let attempt = get(store, &id)?;
    emit_state_event(store, workitem_id, &attempt);
    Ok(attempt)
}

/// 取指定关活跃 attempt；无活跃且最新 attempt 已终态/不存在时新建（legacy 懒补建）。
pub fn ensure_active(
    store: &Store,
    workitem_id: &str,
    gate_id: &str,
) -> Result<StageAttempt, Error> {
    if let Some(attempt) = active_for_gate(store, workitem_id, gate_id)? {
        // M3 升级路径：此前（M2 时代）创建的活跃 attempt 无关前快照 → 懒补（SG-RBK-001）。
        return if attempt.entry_snapshot_id.is_empty() {
            let snapshot =
                crate::snapshot::create(store, workitem_id, gate_id, &attempt.id, "stage_entry")?;
            store.with_conn(|conn| {
                conn.execute(
                    "UPDATE stage_attempts SET entry_snapshot_id=?1, updated_at=?2 WHERE id=?3",
                    rusqlite::params![snapshot.id, timefmt::now(), attempt.id],
                )?;
                Ok(())
            })?;
            get(store, &attempt.id)
        } else {
            Ok(attempt)
        };
    }
    // 单活跃约束：其他关有活跃 attempt 时不得跨关新建（投影一致性由调用方保证）。
    if let Some(other) = active(store, workitem_id)? {
        return Err(Error::Message(format!(
            "attempt_active_exists: 工作项已有 {} 关活跃 attempt {}",
            other.gate, other.id
        )));
    }
    let predecessor = latest_for_gate(store, workitem_id, gate_id)?.map(|a| a.id);
    create(store, workitem_id, gate_id, predecessor.as_deref())
}

/// 评估通过后的 attempt 投影推进：prepared/changes_requested → running → review_ready；
/// 已在 review_ready/awaiting 幂等返回。要求 evaluate 已通过（由 release::request_release 前置校验）。
pub fn advance_to_review_ready(
    store: &Store,
    workitem_id: &str,
    gate_id: &str,
) -> Result<StageAttempt, Error> {
    let mut attempt = ensure_active(store, workitem_id, gate_id)?;
    match attempt.state.as_str() {
        "prepared" => attempt = transition(store, &attempt.id, "running")?,
        "changes_requested" => attempt = transition(store, &attempt.id, "running")?,
        _ => {}
    }
    if attempt.state == "running" {
        attempt = transition(store, &attempt.id, "review_ready")?;
    }
    Ok(attempt)
}

/// 活动状态推进（M4 stage.startActivity 启动活动）。
pub fn set_activity_state(
    store: &Store,
    attempt_id: &str,
    activity_key: &str,
    state: &str,
) -> Result<(), Error> {
    let updated = store.with_conn(|conn| {
        conn.execute(
            "UPDATE stage_activities SET state=?1, updated_at=?2 WHERE stage_attempt_id=?3 AND activity_key=?4",
            rusqlite::params![state, timefmt::now(), attempt_id, activity_key],
        )?;
        Ok(conn.changes())
    })?;
    if updated == 0 {
        return Err(Error::Message(format!(
            "not_found: activity {activity_key} on attempt {attempt_id}"
        )));
    }
    Ok(())
}

/// 状态迁移（校验合法性 + outbox 事件）；同状态幂等。
pub fn transition(store: &Store, attempt_id: &str, to: &str) -> Result<StageAttempt, Error> {
    let attempt = get(store, attempt_id)?;
    if attempt.state == to {
        return Ok(attempt);
    }
    if !can_transition(&attempt.state, to) {
        return Err(Error::Message(format!(
            "invalid_stage_transition: attempt {} {} -> {to}",
            attempt.state, attempt_id
        )));
    }
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE stage_attempts SET state=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![to, timefmt::now(), attempt_id],
        )?;
        Ok(())
    })?;
    let updated = get(store, attempt_id)?;
    emit_state_event(store, &attempt.workitem_id, &updated);
    Ok(updated)
}

fn emit_state_event(store: &Store, workitem_id: &str, attempt: &StageAttempt) {
    let _ = outbox::emit(
        store,
        "workitem",
        workitem_id,
        &format!("stage.attempt_{}", attempt.state),
        serde_json::json!({
            "workitemId": workitem_id,
            "gate": attempt.gate,
            "attemptId": attempt.id,
            "attemptNo": attempt.attempt_no,
            "state": attempt.state,
        }),
    );
}

/// 上游输入变化：from_gate 起已批准的 attempt 标记 superseded（蓝图 §4.1 approved→superseded）。
/// 活跃 attempt 不动：其继续有效性由 evaluate 的 inputs_current（基线新鲜度）把关。
pub fn supersede_from(
    store: &Store,
    workitem_id: &str,
    from_gate_id: &str,
) -> Result<usize, Error> {
    let refs = crate::gate_refs(store, workitem_id)?;
    let start = refs
        .iter()
        .position(|g| g.gate_id == from_gate_id)
        .ok_or_else(|| Error::Message("unknown gate".into()))?;
    let mut count = 0;
    for gate in &refs[start..] {
        for attempt in list_for_gate(store, workitem_id, &gate.gate_id)? {
            if attempt.state != "approved" {
                continue;
            }
            transition(store, &attempt.id, "superseded")?;
            count += 1;
        }
    }
    Ok(count)
}

/// 指定关全部 attempt（attempt_no 升序）。
fn list_for_gate(
    store: &Store,
    workitem_id: &str,
    gate_id: &str,
) -> Result<Vec<StageAttempt>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {ATTEMPT_COLUMNS} FROM stage_attempts
             WHERE workitem_id=?1 AND gate=?2 ORDER BY attempt_no"
        ))?;
        let rows = stmt.query_map(rusqlite::params![workitem_id, gate_id], row_attempt)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

/// legacy 回填（幂等）：无任何 attempt 的 WorkItem，为已 passed 的关生成 synthetic approved attempt。
/// 谱系节点 unverified、不伪造父边（蓝图 §11.3-4 诚实性要求）。
pub fn backfill_legacy(store: &Store) -> Result<usize, Error> {
    let workitems: Vec<String> = store.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT id FROM workitems ORDER BY created_at")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;
    let mut count = 0usize;
    for workitem_id in workitems {
        let existing: i64 = store.with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM stage_attempts WHERE workitem_id=?1",
                [&workitem_id],
                |r| r.get(0),
            )
            .map_err(Error::from)
        })?;
        if existing > 0 {
            continue;
        }
        let stages = crate::stages(store, &workitem_id)?;
        for (idx, stage) in stages.iter().enumerate() {
            if stage.state != "passed" {
                continue;
            }
            let gate = stage.gate.clone();
            let id = ids::new_id("att");
            let created_at = stage.updated_at.clone();
            store.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO stage_attempts(id, workitem_id, gate, attempt_no, branch_no, state, entry_snapshot_id, input_package_sha256, active_output_package_id, predecessor_attempt_id, created_at, updated_at)
                     VALUES (?1,?2,?3,1,1,'approved','','',NULL,NULL,?4,?4)",
                    rusqlite::params![id, workitem_id, gate.as_str(), created_at],
                )?;
                Ok(())
            })?;
            sg_provenance::register_node(
                store,
                &NodeInput {
                    project_id: "",
                    workitem_id: &workitem_id,
                    node_type: node_type::STAGE_ATTEMPT,
                    entity_id: &id,
                    content_digest: "",
                    verification_state: "unverified",
                },
            )?;
            let _ = idx;
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sg_store::Store;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-att-{}-{}",
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
    fn create_builds_activities_and_prepared_state() {
        let s = setup();
        let wi = crate::create(&s, "pj", "任务", "", None, &[]).unwrap();
        let att = create(&s, &wi.id, "requirements", None).unwrap();
        assert_eq!(att.state, "prepared");
        assert_eq!(att.attempt_no, 1);
        let acts = activities(&s, &att.id).unwrap();
        assert_eq!(acts.len(), 1);
        assert_eq!(acts[0].activity_key, "requirement_analysis");
        // 设计关模板有两个活动。
        let design = create(&s, &wi.id, "design", None);
        assert!(
            design.is_err(),
            "同 WorkItem 双活跃 attempt 应被单活跃约束拒绝"
        );
    }

    #[test]
    fn transitions_are_guarded() {
        let s = setup();
        let wi = crate::create(&s, "pj", "任务", "", None, &[]).unwrap();
        let att = create(&s, &wi.id, "requirements", None).unwrap();
        assert!(transition(&s, &att.id, "awaiting_user_approval").is_err());
        let running = transition(&s, &att.id, "running").unwrap();
        assert_eq!(running.state, "running");
        let rr = transition(&s, &att.id, "review_ready").unwrap();
        let awaiting = transition(&s, &rr.id, "awaiting_user_approval").unwrap();
        let cr = transition(&s, &awaiting.id, "changes_requested").unwrap();
        let back = transition(&s, &cr.id, "running").unwrap();
        assert_eq!(back.state, "running");
        // 同状态幂等。
        assert_eq!(
            transition(&s, &back.id, "running").unwrap().state,
            "running"
        );
    }

    #[test]
    fn attempt_no_increments_after_terminal() {
        let s = setup();
        let wi = crate::create(&s, "pj", "任务", "", None, &[]).unwrap();
        let first = create(&s, &wi.id, "requirements", None).unwrap();
        transition(&s, &first.id, "running").unwrap();
        transition(&s, &first.id, "failed").unwrap();
        let second = ensure_active(&s, &wi.id, "requirements").unwrap();
        assert_eq!(second.attempt_no, 2);
        assert_eq!(
            second.predecessor_attempt_id.as_deref(),
            Some(first.id.as_str())
        );
    }

    #[test]
    fn legacy_backfill_creates_approved_attempts_only_for_passed() {
        let s = setup();
        let wi = crate::create(&s, "pj", "历史", "", None, &[]).unwrap();
        crate::set_stage(&s, &wi.id, "requirements", crate::StageState::Running, "").unwrap();
        crate::pass_gate(&s, &wi.id, "requirements").unwrap();
        assert_eq!(backfill_legacy(&s).unwrap(), 1);
        assert_eq!(backfill_legacy(&s).unwrap(), 0, "幂等");
        let attempts = list(&s, &wi.id).unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].state, "approved");
        assert_eq!(attempts[0].gate, "requirements");
    }
}
