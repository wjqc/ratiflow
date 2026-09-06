//! 计划运行时（EvoFlow 方案 M3-05 / ADR-037 §6.3）：
//! TaskAttempt 生命周期编排——工作区准备（写任务）→ ready → 派发闸 →
//! outcome 回填 → 工作区 finalize → 下游排队位提升。
//!
//! 执行者边界：真正的模型执行由 Agent Run 承接（M3-04 supervisor 绑定，
//! agent_runs.plan_task_attempt_id 关联）；本模块是协议级状态机与调度闸，
//! 提供 RPC 可驱动的事实推进面（worker/对账/崩溃恢复共用）。

use sg_store::{Error, Store};

use sg_executor::workspace;
use sg_workflow::plan::AttemptInfo;
use sg_workflow::scheduler;

/// 准备阶段：ready → preparing_workspace →（写任务挂工作区）→ ready。
/// 只读任务直接就绪（无工作区）。幂等：非 ready 态拒绝重复准备。
pub fn prepare_task(
    store: &Store,
    task_attempt_id: &str,
) -> Result<Option<workspace::TaskWorkspaceRecord>, Error> {
    transition_state(store, task_attempt_id, "ready", "preparing_workspace")?;
    let rec = workspace::prepare(store, task_attempt_id)?;
    match rec {
        // 只读任务：无工作区，留在 ready（可被派发执行）。
        None => {
            transition_state(store, task_attempt_id, "preparing_workspace", "ready")?;
            Ok(None)
        }
        Some(rec) => {
            transition_state(store, task_attempt_id, "preparing_workspace", "ready")?;
            Ok(Some(rec))
        }
    }
}

/// 派发闸（容量+串行）：ready → running（开始执行，Agent Run 绑定此点）。
pub fn start_running(store: &Store, task_attempt_id: &str) -> Result<(), Error> {
    transition_state(store, task_attempt_id, "ready", "running")
}

/// outcome 回填：running/ready → 终态 + 工作区 finalize + 下游排队位提升。
pub fn complete_task(
    store: &Store,
    task_attempt_id: &str,
    outcome: &str,
    output_digest: &str,
) -> Result<AttemptInfo, Error> {
    // 工作区事实先落（retained/after digest），再回填 attempt 终态。
    if matches!(outcome, "succeeded" | "failed" | "unknown" | "cancelled") {
        let _ = workspace::finalize(store, task_attempt_id, outcome);
    }
    scheduler::advance(store, task_attempt_id, outcome, output_digest)
}

/// 对账（unknown 专用）：not_executed（新建 attempt）/executed_ok/needs_manual。
pub fn reconcile_task(
    store: &Store,
    task_attempt_id: &str,
    resolution: &str,
    output_digest: &str,
) -> Result<AttemptInfo, Error> {
    scheduler::reconcile(store, task_attempt_id, resolution, output_digest)
}

/// 调度闸推进：pending → ready（容量+串行）。
pub fn dispatch_ready(
    store: &Store,
    revision_id: &str,
    max_parallel: usize,
) -> Result<Vec<AttemptInfo>, Error> {
    scheduler::dispatch_ready(store, revision_id, max_parallel)
}

fn transition_state(
    store: &Store,
    task_attempt_id: &str,
    from: &str,
    to: &str,
) -> Result<(), Error> {
    let changed = store.with_conn(|conn| {
        conn.execute(
            "UPDATE plan_task_attempts SET state=?1, updated_at=?2
             WHERE id=?3 AND state=?4",
            rusqlite::params![to, sg_store::timefmt::now(), task_attempt_id, from],
        )?;
        Ok(conn.changes())
    })?;
    if changed == 0 {
        // 幂等重放：已在目标态则 OK。
        let current: String = store.with_conn(|conn| {
            conn.query_row(
                "SELECT state FROM plan_task_attempts WHERE id=?1",
                [task_attempt_id],
                |r| r.get(0),
            )
            .map_err(|_| Error::Message("task_dependency_blocked: attempt 不存在".into()))
        })?;
        if current == to {
            return Ok(());
        }
        return Err(Error::Message(format!(
            "task_state_invalid: {from} -> {to}（当前 {current}）"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sg_workflow::plan::{self, PlanAcceptance, PlanTaskInput};
    use std::process::Command;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    }

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-rt-{}-{}",
            std::process::id(),
            sg_store::ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        let repo = dir.join("main-repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("README.md"), "base\n").unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "t@sixgates.local"],
            vec!["config", "user.name", "t"],
            vec!["add", "."],
            vec!["commit", "-m", "init"],
        ] {
            git(&repo, &args);
        }
        store
            .with_conn(|c| {
                let now = sg_store::timefmt::now();
                c.execute(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, local_root, created_at)
                     VALUES ('pj','u','n','p','main',?1,?2)",
                    rusqlite::params![repo.to_string_lossy(), now],
                )?;
                c.execute(
                    "INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                     VALUES ('wi','pj','t','','[]','requirements',?1,?1)",
                    [&now],
                )?;
                c.execute(
                    "INSERT INTO context_manifests(id, workitem_id, scope, data_policy, created_at)
                     VALUES ('ctx1','wi','{}','standard',?1)",
                    [&now],
                )?;
                c.execute(
                    "INSERT INTO stage_attempts(id, workitem_id, gate, attempt_no, branch_no, state, entry_snapshot_id,
                        input_package_sha256, active_output_package_id, predecessor_attempt_id, created_at, updated_at)
                     VALUES ('att1','wi','requirements',1,1,'prepared','','',NULL,NULL,?1,?1)",
                    [&now],
                )?;
                Ok(())
            })
            .unwrap();
        store
    }

    fn t(key: &str, deps: &[&str], effect: &str) -> PlanTaskInput {
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
            expected_outputs: vec![format!("file:{key}.out")],
            acceptance: PlanAcceptance::default(),
            effect_class: effect.into(),
            deps: deps.iter().map(|s| s.to_string()).collect(),
            team_role_key: None,
        }
    }

    fn seed_running(store: &Store) -> String {
        let r = plan::create_draft(
            store,
            "wi",
            "att1",
            &[
                t("w1", &[], "local_write"),
                t("w2", &[], "local_write"),
                t("v", &["w1", "w2"], "read"),
            ],
            "agent",
            None,
        )
        .unwrap();
        plan::submit(store, &r.id).unwrap();
        plan::approve(store, &r.id, "owner").unwrap();
        plan::start(store, &r.id).unwrap();
        r.id
    }

    #[test]
    fn runtime_lifecycle_prepare_run_complete_promotes_downstream() {
        let store = setup();
        let rev = seed_running(&store);
        // 派发闸：两个写任务 pending → ready。
        let ready = dispatch_ready(&store, &rev, 3).unwrap();
        assert_eq!(ready.len(), 2);
        let w1 = &ready[0];
        // 准备：挂工作区（写任务）。
        let ws = prepare_task(&store, &w1.id).unwrap();
        assert!(ws.is_some(), "写任务有工作区");
        // 派发执行。
        start_running(&store, &w1.id).unwrap();
        // 回填成功 → 工作区 retained。
        complete_task(&store, &w1.id, "succeeded", "od-w1").unwrap();
        let ws_after = workspace::get(&store, &w1.id).unwrap().unwrap();
        assert_eq!(ws_after.state, "retained");
        // 全部完成后 v 排队位出现 → 派发 → 完成。
        let w2 = &ready[1];
        prepare_task(&store, &w2.id).unwrap();
        start_running(&store, &w2.id).unwrap();
        complete_task(&store, &w2.id, "succeeded", "od-w2").unwrap();
        let v_ready = dispatch_ready(&store, &rev, 3).unwrap();
        assert_eq!(v_ready.len(), 1);
        assert_eq!(v_ready[0].task_key, "v");
        prepare_task(&store, &v_ready[0].id).unwrap();
        assert!(
            prepare_task(&store, &v_ready[0].id).unwrap().is_none(),
            "只读任务无工作区"
        );
        start_running(&store, &v_ready[0].id).unwrap();
        complete_task(&store, &v_ready[0].id, "succeeded", "od-v").unwrap();
        // 计划完成：全部 succeeded。
        let attempts = plan::attempts_of(&store, &rev).unwrap();
        assert!(attempts.iter().all(|a| a.state == "succeeded"));
    }

    #[test]
    fn unknown_outcome_blocks_and_reconcile_retries() {
        let store = setup();
        let rev = seed_running(&store);
        let ready = dispatch_ready(&store, &rev, 3).unwrap();
        let w1 = &ready[0];
        prepare_task(&store, &w1.id).unwrap();
        start_running(&store, &w1.id).unwrap();
        complete_task(&store, &w1.id, "unknown", "").unwrap();
        // unknown 后：无新派发（w1 卡住，v 未满足）。
        let v = view_after_unknown(&store, &rev);
        assert!(v.is_empty(), "unknown 不自动重跑");
        // 对账：查证未执行 → 新 attempt。
        let fresh = reconcile_task(&store, &w1.id, "not_executed", "").unwrap();
        assert_eq!(fresh.attempt_no, 2);
        // 新 attempt 走调度闸（pending→ready）后再准备。
        let disp = dispatch_ready(&store, &rev, 3).unwrap();
        assert_eq!(disp.len(), 1);
        assert_eq!(disp[0].id, fresh.id);
        prepare_task(&store, &fresh.id).unwrap();
        start_running(&store, &fresh.id).unwrap();
        complete_task(&store, &fresh.id, "succeeded", "od").unwrap();
    }

    fn view_after_unknown(store: &Store, rev: &str) -> Vec<String> {
        scheduler::dispatch_ready(store, rev, 3)
            .unwrap()
            .into_iter()
            .map(|a| a.task_key)
            .collect()
    }
}
