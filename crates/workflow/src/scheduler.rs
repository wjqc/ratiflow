//! Plan DAG 调度器（EvoFlow 方案 M3-01 / ADR-037 §6.3）：
//! ready 队列推进、容量上限（来自 policy，默认 3，不硬编码进 schema）、
//! 单写者（同 effect 串行档互斥）、取消与 backpressure。
//! 状态推进统一入口：advance（worker/runtime 回填 outcome）→ 终态 + 依赖提升。

use std::collections::BTreeSet;

use sg_store::{outbox, timefmt, Error, Store};

use crate::dag;
use crate::plan::{self, AttemptInfo};

/// 默认并行容量（§6.2：来自 Team/Workspace/Project policy，默认 3）。
pub const DEFAULT_MAX_PARALLEL: usize = 3;

fn is_active(state: &str) -> bool {
    matches!(
        state,
        "pending"
            | "ready"
            | "preparing_workspace"
            | "running"
            | "awaiting_approval"
            | "reconciliation_required"
    )
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SchedulingView {
    /// 可立即启动（依赖全部 succeeded 且自身无活跃 attempt）。
    pub ready: Vec<AttemptInfo>,
    /// 进行中（占容量）。
    pub running: Vec<AttemptInfo>,
    /// 阻塞：上游失败/unknown/取消，等 replan 或对账。
    pub blocked: Vec<String>,
    /// 剩余容量（backpressure：超容量不派发）。
    pub capacity_left: usize,
}

/// 串行档（§6.7）：external_write/irreversible 默认串行——同档任一活跃即不再派发。
pub fn serial_required(effect_class: &str) -> bool {
    matches!(effect_class, "external_write" | "irreversible")
}

fn load(
    store: &Store,
    revision_id: &str,
) -> Result<(Vec<plan::PlanTaskRecord>, Vec<AttemptInfo>), Error> {
    let tasks = plan::tasks_of(store, revision_id)?;
    let attempts = plan::attempts_of(store, revision_id)?;
    Ok((tasks, attempts))
}

/// 调度视图（只读）：ready（容量裁剪后）/running/blocked/capacity_left。
pub fn view(
    store: &Store,
    revision_id: &str,
    max_parallel: usize,
) -> Result<SchedulingView, Error> {
    let (tasks, attempts) = load(store, revision_id)?;
    let dag_tasks: Vec<dag::DagTask> = tasks
        .iter()
        .map(|t| {
            dag::DagTask::from_strings(
                t.task_key.clone(),
                t.deps.clone(),
                crate::replan::effect_rank(&t.effect_class)
                    >= crate::replan::effect_rank("local_write"),
            )
        })
        .collect();
    let mut completed: BTreeSet<String> = BTreeSet::new();
    let mut active: BTreeSet<String> = BTreeSet::new();
    let mut blocked_upstream: BTreeSet<String> = BTreeSet::new();
    // 复用任务视同已完成（执行事实在上一版本计划）。
    for t in tasks.iter().filter(|t| t.reused_from_attempt_id.is_some()) {
        completed.insert(t.task_key.clone());
    }
    let mut latest: std::collections::BTreeMap<&str, &AttemptInfo> =
        std::collections::BTreeMap::new();
    for a in &attempts {
        latest.insert(a.task_key.as_str(), a);
    }
    for (k, a) in &latest {
        match a.state.as_str() {
            "succeeded" => {
                completed.insert((*k).to_string());
            }
            s if is_active(s) => {
                active.insert((*k).to_string());
            }
            _ => {}
        }
    }
    // 阻塞：最新终态 failed/unknown/cancelled/manual → 自身及其下游不再派发。
    for (k, a) in &latest {
        if matches!(
            a.state.as_str(),
            "failed" | "unknown" | "cancelled" | "manual_action_required"
        ) {
            let closure = dag::affected_closure(&dag_tasks, &[(*k).to_string()]);
            blocked_upstream.extend(closure);
        }
    }
    // effect → 串行档占用。
    let effect_of: std::collections::BTreeMap<&str, &str> = tasks
        .iter()
        .map(|t| (t.task_key.as_str(), t.effect_class.as_str()))
        .collect();
    // 串行档只被真正运行中的任务占用（pending 是排队位，不占档）。
    let serial_occupied = latest.values().any(|a| {
        matches!(
            a.state.as_str(),
            "ready" | "preparing_workspace" | "running" | "awaiting_approval"
        ) && effect_of
            .get(a.task_key.as_str())
            .is_some_and(|e| serial_required(e))
    });
    let mut ready = Vec::new();
    let mut running = Vec::new();
    for a in latest.values() {
        if !is_active(&a.state) {
            continue;
        }
        if a.state == "pending" {
            // pending 是 start/advance 预建的排队位；deps 未满足不进 ready。
            let t = tasks.iter().find(|t| t.task_key == a.task_key);
            let deps_ok = t.is_some_and(|t| t.deps.iter().all(|d| completed.contains(d)));
            let blocked = blocked_upstream.contains(&a.task_key);
            let serial_hit = t.is_some_and(|t| serial_required(&t.effect_class)) && serial_occupied;
            if deps_ok && !blocked && !serial_hit {
                ready.push((*a).clone());
            }
        } else {
            running.push((*a).clone());
        }
    }
    ready.sort_by(|a, b| a.task_key.cmp(&b.task_key));
    let capacity_left = max_parallel.saturating_sub(running.len() + ready.len());
    let _ = capacity_left;
    ready.truncate(max_parallel.saturating_sub(running.len()));
    Ok(SchedulingView {
        ready,
        running,
        blocked: blocked_upstream.into_iter().collect(),
        capacity_left,
    })
}

/// 派发：ready（pending 且依赖满足、容量/串行闸通过）→ ready 态。
/// pending attempt 由 plan.start/advance 预建；此处只做调度闸与状态推进。
pub fn dispatch_ready(
    store: &Store,
    revision_id: &str,
    max_parallel: usize,
) -> Result<Vec<AttemptInfo>, Error> {
    let view = view(store, revision_id, max_parallel)?;
    let tasks = plan::tasks_of(store, revision_id)?;
    let effect_of: std::collections::BTreeMap<&str, &str> = tasks
        .iter()
        .map(|t| (t.task_key.as_str(), t.effect_class.as_str()))
        .collect();
    let now = timefmt::now();
    let mut out = Vec::new();
    let mut serial_flipped = false;
    for cand in &view.ready {
        if cand.id.is_empty() {
            continue;
        }
        // 单写者闸：本轮已翻转串行档任务 → 后续串行候选留在队列（backpressure）。
        let serial = effect_of
            .get(cand.task_key.as_str())
            .is_some_and(|e| serial_required(e));
        if serial && serial_flipped {
            continue;
        }
        store.with_conn(|conn| {
            conn.execute(
                "UPDATE plan_task_attempts SET state='ready', updated_at=?1 WHERE id=?2 AND state='pending'",
                rusqlite::params![now, cand.id],
            )?;
            Ok(())
        })?;
        if serial {
            serial_flipped = true;
        }
        out.push(AttemptInfo {
            state: "ready".into(),
            ..cand.clone()
        });
    }
    Ok(out)
}

/// outcome 回填（runtime/worker 统一入口）：活跃态 → 终态。
/// unknown 不自动重跑：落 unknown + reconciliation_required 由显式对账推进（EV-009）。
/// succeeded → 提升下游（dispatch_ready 由 runtime 随后调用）。
pub fn advance(
    store: &Store,
    task_attempt_id: &str,
    outcome: &str,
    output_digest: &str,
) -> Result<AttemptInfo, Error> {
    let now = timefmt::now();
    let terminal = match outcome {
        "succeeded" => "succeeded",
        "failed" => "failed",
        "unknown" => "unknown",
        "cancelled" => "cancelled",
        _ => return Err(Error::Message(format!("task_outcome_invalid: {outcome}"))),
    };
    let info = store.with_conn(|conn| {
        let (task_id, task_key, state): (String, String, String) = conn
            .query_row(
                "SELECT pt.id, pt.task_key, pa.state FROM plan_task_attempts pa
                 JOIN plan_tasks pt ON pt.id = pa.task_id WHERE pa.id=?1",
                [task_attempt_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|_| Error::Message("task_dependency_blocked: attempt 不存在".into()))?;
        if !is_active(&state) {
            return Err(Error::Message(format!(
                "task_state_invalid: {state} 不可回填 outcome"
            )));
        }
        conn.execute(
            "UPDATE plan_task_attempts SET state=?1, input_digest=CASE WHEN ?3='' THEN input_digest ELSE ?3 END,
                finished_at=?2, updated_at=?2 WHERE id=?4",
            rusqlite::params![terminal, now, output_digest, task_attempt_id],
        )?;
        Ok(AttemptInfo {
            id: task_attempt_id.to_string(),
            task_key,
            task_id,
            attempt_no: 0,
            state: terminal.into(),
        })
    })?;
    let revision_id: String = store.with_conn(|conn| {
        conn.query_row(
            "SELECT pt.plan_revision_id FROM plan_tasks pt
             JOIN plan_task_attempts pa ON pa.task_id = pt.id WHERE pa.id=?1",
            [task_attempt_id],
            |r| r.get(0),
        )
        .map_err(Error::from)
    })?;
    // 成功 → 为新满足依赖且尚无 attempt 的下游建 pending attempt（排队位）。
    if outcome == "succeeded" {
        let (tasks, attempts) = load(store, &revision_id)?;
        let dag_tasks: Vec<dag::DagTask> = tasks
            .iter()
            .map(|t| {
                dag::DagTask::from_strings(
                    t.task_key.clone(),
                    t.deps.clone(),
                    crate::replan::effect_rank(&t.effect_class)
                        >= crate::replan::effect_rank("local_write"),
                )
            })
            .collect();
        let mut completed: BTreeSet<String> = BTreeSet::new();
        // 复用任务视同已完成。
        for t in tasks.iter().filter(|t| t.reused_from_attempt_id.is_some()) {
            completed.insert(t.task_key.clone());
        }
        for a in &attempts {
            if a.state == "succeeded" {
                completed.insert(a.task_key.clone());
            }
        }
        let has_attempt: BTreeSet<String> = attempts.iter().map(|a| a.task_key.clone()).collect();
        let ready_keys = dag::ready_set(&dag_tasks, &completed, &BTreeSet::new());
        for k in ready_keys {
            if !has_attempt.contains(&k) {
                plan::ensure_attempt(store, &revision_id, &k)?;
            }
        }
    }
    let workitem_id = plan::revision_by_id(store, &revision_id)?.workitem_id;
    outbox::emit(
        store,
        "workitem",
        &workitem_id,
        &format!("task.{terminal}"),
        serde_json::json!({
            "taskAttemptId": task_attempt_id,
            "taskKey": info.task_key,
            "planRevisionId": revision_id,
        }),
    )?;
    Ok(info)
}

/// 显式对账（EV-009 / §6.4）：unknown 先查证，不允许直接重试。
/// not_executed → failed（重跑走 replan 新 attempt）；executed_ok → succeeded；
/// needs_manual → manual_action_required。
pub fn reconcile(
    store: &Store,
    task_attempt_id: &str,
    resolution: &str,
    _output_digest: &str,
) -> Result<AttemptInfo, Error> {
    let now = timefmt::now();
    let (terminal, allow_new_attempt) = match resolution {
        "not_executed" => ("failed", true),
        "executed_ok" => ("succeeded", false),
        "needs_manual" => ("manual_action_required", false),
        _ => {
            return Err(Error::Message(format!(
                "task_reconciliation_invalid: {resolution}"
            )))
        }
    };
    let info = store.with_conn(|conn| {
        let (task_id, task_key, state): (String, String, String) = conn
            .query_row(
                "SELECT pt.id, pt.task_key, pa.state FROM plan_task_attempts pa
                 JOIN plan_tasks pt ON pt.id = pa.task_id WHERE pa.id=?1",
                [task_attempt_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|_| Error::Message("task_dependency_blocked: attempt 不存在".into()))?;
        if !matches!(state.as_str(), "unknown" | "reconciliation_required") {
            return Err(Error::Message(format!(
                "task_reconciliation_invalid: {state} 无需对账"
            )));
        }
        conn.execute(
            "UPDATE plan_task_attempts SET state=?1, finished_at=?2, updated_at=?2 WHERE id=?3",
            rusqlite::params![terminal, now, task_attempt_id],
        )?;
        Ok(AttemptInfo {
            id: task_attempt_id.to_string(),
            task_key,
            task_id,
            attempt_no: 0,
            state: terminal.into(),
        })
    })?;
    if allow_new_attempt {
        // 查证未执行：显式新建 attempt（attempt_no+1，仍受单活跃约束）。
        let revision_id: String = store.with_conn(|conn| {
            conn.query_row(
                "SELECT pt.plan_revision_id FROM plan_tasks pt
                 JOIN plan_task_attempts pa ON pa.task_id = pt.id WHERE pa.id=?1",
                [task_attempt_id],
                |r| r.get(0),
            )
            .map_err(Error::from)
        })?;
        return plan::create_next_attempt(store, &revision_id, &info.task_key);
    }
    Ok(info)
}

/// 取消任务 attempt（cancelled → 下游由 blocked 语义挡住）。
pub fn cancel_task(store: &Store, task_attempt_id: &str) -> Result<AttemptInfo, Error> {
    advance(store, task_attempt_id, "cancelled", "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::PlanAcceptance;
    use crate::plan::PlanTaskInput;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-sched-{}-{}",
            std::process::id(),
            sg_store::ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        store
            .with_conn(|c| {
                c.execute_batch(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj','u','n','p','main','t');
                    INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                     VALUES ('wi','pj','t','','[]','requirements','t','t');
                    INSERT INTO context_manifests(id, workitem_id, scope, data_policy, created_at)
                     VALUES ('ctx1','wi','{}','standard','t');
                    INSERT INTO stage_attempts(id, workitem_id, gate, attempt_no, branch_no, state, entry_snapshot_id,
                        input_package_sha256, active_output_package_id, predecessor_attempt_id, created_at, updated_at)
                     VALUES ('att1','wi','requirements',1,1,'prepared','','',NULL,NULL,'t','t');",
                )
                .map_err(Error::from)?;
                Ok(())
            })
            .unwrap();
        store
    }

    fn task(key: &str, deps: &[&str], effect: &str) -> PlanTaskInput {
        PlanTaskInput {
            task_key: key.into(),
            kind: if effect == "read" {
                "analysis"
            } else {
                "local_write"
            }
            .into(),
            title: key.into(),
            inputs: vec![],
            expected_outputs: vec![],
            acceptance: PlanAcceptance::default(),
            effect_class: effect.into(),
            deps: deps.iter().map(|s| s.to_string()).collect(),
            team_role_key: None,
        }
    }

    fn seed(store: &Store, tasks: Vec<PlanTaskInput>) -> String {
        let r = plan::create_draft(store, "wi", "att1", &tasks, "agent", None).unwrap();
        plan::submit(store, &r.id).unwrap();
        plan::approve(store, &r.id, "owner").unwrap();
        plan::start(store, &r.id).unwrap();
        r.id
    }

    #[test]
    fn ready_queue_and_capacity_backpressure() {
        let store = setup();
        // 4 个独立写任务 + 汇合验证（写任务须有消费方）。
        let rev = seed(
            &store,
            vec![
                task("t1", &[], "local_write"),
                task("t2", &[], "local_write"),
                task("t3", &[], "local_write"),
                task("t4", &[], "local_write"),
                task("v", &["t1", "t2", "t3", "t4"], "read"),
            ],
        );
        let dispatched = dispatch_ready(&store, &rev, DEFAULT_MAX_PARALLEL).unwrap();
        assert_eq!(dispatched.len(), 3, "容量默认 3，第 4 个被背压");
        let v = view(&store, &rev, DEFAULT_MAX_PARALLEL).unwrap();
        assert_eq!(v.running.len(), 3);
        assert_eq!(v.capacity_left, 0);
        // 容量 8 → 全派发。
        let more = dispatch_ready(&store, &rev, 8).unwrap();
        assert_eq!(more.len(), 1, "剩余 t4 派发");
    }

    #[test]
    fn advance_promotes_dependents_only_on_success() {
        let store = setup();
        let rev = seed(
            &store,
            vec![task("a", &[], "local_write"), task("b", &["a"], "read")],
        );
        let dispatched = dispatch_ready(&store, &rev, 4).unwrap();
        assert_eq!(dispatched.len(), 1);
        let a_id = dispatched[0].id.clone();
        // 成功 → 下游可派发。
        advance(&store, &a_id, "succeeded", "od1").unwrap();
        let next = dispatch_ready(&store, &rev, 4).unwrap();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].task_key, "b");
        // 失败 → 下游不派发（blocked）。
        let b_id = next[0].id.clone();
        advance(&store, &b_id, "failed", "").unwrap();
        let v = view(&store, &rev, 4).unwrap();
        assert!(v.ready.is_empty(), "上游失败下游 blocked");
        assert!(v.blocked.contains(&"b".to_string()));
    }

    #[test]
    fn unknown_never_auto_retries_and_reconcile_gates_retry() {
        let store = setup();
        let rev = seed(
            &store,
            vec![task("a", &[], "external_write"), task("b", &["a"], "read")],
        );
        let dispatched = dispatch_ready(&store, &rev, 4).unwrap();
        let a_id = dispatched[0].id.clone();
        advance(&store, &a_id, "unknown", "").unwrap();
        // unknown 后不自动派发任何东西（含自身重试）。
        let v = view(&store, &rev, 4).unwrap();
        assert!(v.ready.is_empty(), "unknown 不自动重跑");
        // 对账 1：查证未执行 → 允许一次新 attempt。
        let fresh = reconcile(&store, &a_id, "not_executed", "").unwrap();
        assert_ne!(fresh.id, a_id, "对账后新建 attempt");
        assert_eq!(fresh.attempt_no, 2);
        // 新 attempt 仍走正常推进。
        advance(&store, &fresh.id, "succeeded", "od").unwrap();
        let next = dispatch_ready(&store, &rev, 4).unwrap();
        assert_eq!(next.len(), 1, "成功后下游提升");
        // 对账 2：needs_manual → 终态。
        let b_id = next[0].id.clone();
        advance(&store, &b_id, "unknown", "").unwrap();
        let m = reconcile(&store, &b_id, "needs_manual", "").unwrap();
        assert_eq!(m.state, "manual_action_required");
    }

    #[test]
    fn serial_effect_blocks_parallel_dispatch() {
        let store = setup();
        let rev = seed(
            &store,
            vec![
                task("e1", &[], "external_write"),
                task("e2", &[], "external_write"),
                task("r1", &[], "read"),
                task("v1", &["e1"], "read"),
                task("v2", &["e2"], "read"),
            ],
        );
        let dispatched = dispatch_ready(&store, &rev, 8).unwrap();
        let keys: Vec<&str> = dispatched.iter().map(|a| a.task_key.as_str()).collect();
        assert!(
            keys.iter().filter(|k| k.starts_with('e')).count() == 1,
            "串行档同时只派发一个: {keys:?}"
        );
        assert!(keys.contains(&"r1"), "只读不受串行档影响");
        // 串行任务完成 → 第二个外部写派发。
        let e1 = dispatched.iter().find(|a| a.task_key == "e1").unwrap();
        advance(&store, &e1.id, "succeeded", "").unwrap();
        let next = dispatch_ready(&store, &rev, 8).unwrap();
        assert!(next.iter().any(|a| a.task_key == "e2"), "串行档释放");
    }

    #[test]
    fn cancel_blocks_downstream() {
        let store = setup();
        let rev = seed(
            &store,
            vec![task("a", &[], "read"), task("b", &["a"], "local_write")],
        );
        let dispatched = dispatch_ready(&store, &rev, 4).unwrap();
        cancel_task(&store, &dispatched[0].id).unwrap();
        let v = view(&store, &rev, 4).unwrap();
        assert!(v.ready.is_empty());
        assert!(v.blocked.contains(&"b".to_string()));
    }
}
