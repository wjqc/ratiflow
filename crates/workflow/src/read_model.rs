//! 驾驶舱 read model（EvoFlow 方案 M5-03 / ADR-039）：
//! 只读真实事实（instance gates/plan attempts/gate results），不使用估算值；
//! checkpoint 落 read_model_checkpoints（UI 断线后重建）。

use serde::{Deserialize, Serialize};
use sg_store::{ids, timefmt, Error, Store};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateView {
    pub gate_id: String,
    pub title: String,
    pub state: String,
    /// passed/failed/unknown/等待中——真实投影，无估算百分比。
    pub progress: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskView {
    pub task_key: String,
    pub state: String,
    pub attempt_no: i64,
    /// ready/running/blocked/unknown/done——调度面真实状态。
    pub phase: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct TaskReadModel {
    pub workitem_id: String,
    pub current_gate_id: String,
    pub template_version_id: String,
    pub gates: Vec<GateView>,
    pub tasks: Vec<TaskView>,
    pub next_action: String,
    pub blocked_reason: Option<String>,
}

fn gate_progress(state: &str) -> String {
    match state {
        "passed" => "done",
        "running" | "awaiting_approval" => "active",
        "failed" | "blocked" => "blocked",
        "stale" => "unknown",
        _ => "waiting",
    }
    .to_string()
}

fn task_phase(state: &str) -> String {
    match state {
        "succeeded" => "done",
        "ready" | "preparing_workspace" => "ready",
        "running" => "running",
        "awaiting_approval" => "ready",
        "unknown" => "unknown",
        "failed" | "cancelled" | "manual_action_required" => "blocked",
        _ => "waiting",
    }
    .to_string()
}

/// 聚合 WorkItem 的真实 read model（instance gates + plan attempts + gate results）。
pub fn build(store: &Store, workitem_id: &str) -> Result<TaskReadModel, Error> {
    // 实例关卡（真实投影状态）。
    let instance = instance::for_workitem(store, workitem_id)?
        .ok_or_else(|| Error::Message("read_model: 实例不存在".into()))?;
    let gates = gates_view(store, workitem_id)?;
    let gate_views: Vec<GateView> = gates
        .iter()
        .map(|g| GateView {
            gate_id: g.gate_id.clone(),
            title: g.title.clone(),
            state: g.state.clone(),
            progress: gate_progress(&g.state),
        })
        .collect();
    // 计划任务（最新 revision 的 attempts）。
    let mut tasks = Vec::new();
    let latest_revision = latest_plan_revision(store, workitem_id)?;
    if let Some(rev) = &latest_revision {
        let rev_id = &rev.id;
        store.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT pt.task_key, pa.state, pa.attempt_no
                 FROM plan_task_attempts pa
                 JOIN plan_tasks pt ON pt.id = pa.task_id
                 WHERE pt.plan_revision_id=?1
                 ORDER BY pa.created_at, pa.id",
            )?;
            let rows = stmt.query_map([rev_id], |r| {
                Ok(TaskView {
                    task_key: r.get(0)?,
                    state: r.get(1)?,
                    attempt_no: r.get(2)?,
                    phase: String::new(),
                })
            })?;
            for row in rows {
                let mut tv = row?;
                tv.phase = task_phase(&tv.state);
                tasks.push(tv);
            }
            Ok(())
        })?;
    }
    // 下一步动作（真实状态推导，非预测）。
    let current = gates.iter().find(|g| g.gate_id == instance.current_gate_id);
    let (next_action, blocked_reason) = match current.map(|g| g.state.as_str()) {
        Some("passed") => (format!("等待进入 {}", instance.current_gate_id), None),
        Some("awaiting_approval") => (
            "等待人工放行审批".to_string(),
            Some("awaiting_approval".to_string()),
        ),
        Some("failed") => (
            "修正失败原因后重试当前关".to_string(),
            Some("failed".to_string()),
        ),
        Some("blocked") => ("处理阻塞原因".to_string(), Some("blocked".to_string())),
        Some("stale") => (
            "输入基线过期，重新绑定后重跑".to_string(),
            Some("stale".to_string()),
        ),
        _ => ("完成当前关产物并核验证据".to_string(), None),
    };
    Ok(TaskReadModel {
        workitem_id: workitem_id.to_string(),
        current_gate_id: instance.current_gate_id.clone(),
        template_version_id: instance.template_version_id.clone(),
        gates: gate_views,
        tasks,
        next_action,
        blocked_reason,
    })
}

// ---- sg_workflow 间接层（避免 crate 内循环依赖路径书写） ----
use crate::instance;
fn gates_view(store: &Store, workitem_id: &str) -> Result<Vec<instance::InstanceGate>, Error> {
    instance::gates_for_workitem(store, workitem_id)?
        .ok_or_else(|| Error::Message("read_model: 实例关卡缺失".into()))
}
fn latest_plan_revision(
    store: &Store,
    workitem_id: &str,
) -> Result<Option<crate::plan::PlanRevisionRecord>, Error> {
    // 最新 revision（含 superseded——驾驶舱按时间序展示全部任务事实）。
    // id 查询与记录读取分两段（revision_by_id 内部 with_conn，Mutex 不可重入）。
    let id: Option<String> = store
        .with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT id FROM plan_revisions WHERE workitem_id=?1
                     ORDER BY revision_no DESC LIMIT 1",
                    [workitem_id],
                    |r| r.get(0),
                )
                .ok())
        })
        .unwrap_or(None);
    match id {
        Some(id) => crate::plan::revision_by_id(store, &id).map(Some),
        None => Ok(None),
    }
}

/// checkpoint 落库（UI 断线后从 durable facts 重建）。
pub fn save_checkpoint(store: &Store, workitem_id: &str) -> Result<String, Error> {
    let model = build(store, workitem_id)?;
    let body = serde_json::to_string(&model).unwrap_or_default();
    let facts_sha = sg_store::ids::hex(&Sha256::digest(body.as_bytes()));
    let id = ids::new_id("rmc");
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO read_model_checkpoints(id, workitem_id, checkpoint_json, facts_sha256, built_at)
             VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(workitem_id) DO UPDATE SET
                checkpoint_json=excluded.checkpoint_json,
                facts_sha256=excluded.facts_sha256,
                built_at=excluded.built_at",
            rusqlite::params![id, workitem_id, body, facts_sha, timefmt::now()],
        )?;
        Ok(())
    })?;
    Ok(facts_sha)
}

/// 从 checkpoint 重建（UI 断线恢复面）。
pub fn load_checkpoint(store: &Store, workitem_id: &str) -> Result<Option<TaskReadModel>, Error> {
    store.with_conn(|conn| {
        let row: Option<String> = conn
            .query_row(
                "SELECT checkpoint_json FROM read_model_checkpoints WHERE workitem_id=?1",
                [workitem_id],
                |r| r.get(0),
            )
            .ok();
        match row {
            Some(body) => serde_json::from_str(&body)
                .map(Some)
                .map_err(|e| Error::Message(format!("read_model_checkpoint_corrupt: {e}"))),
            None => Ok(None),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-rm-{}-{}",
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
        // 实例 + stages（M1 语义：WorkItem 需配实例）。
        let version_id = crate::template::default_active_version_id(&store).unwrap();
        store
            .with_conn(|conn| {
                let now = timefmt::now();
                conn.execute(
                    "INSERT INTO workitem_stages(workitem_id, gate, state, updated_at)
                     SELECT 'wi', gd.gate_id, 'not_started', ?1
                     FROM workflow_gate_definitions gd WHERE gd.version_id=?2 ORDER BY gd.ordinal",
                    rusqlite::params![now, version_id],
                )?;
                crate::instance::create_for_workitem(conn, "wi", &version_id, &now)?;
                Ok(())
            })
            .unwrap();
        store
    }

    #[test]
    fn read_model_reflects_real_states_and_checkpoints() {
        let store = setup();
        // 计划：w1 + v（依赖 w1）。
        let tasks = vec![
            crate::plan::PlanTaskInput {
                task_key: "w1".into(),
                kind: "local_write".into(),
                title: "w1".into(),
                inputs: vec![],
                expected_outputs: vec![],
                acceptance: Default::default(),
                effect_class: "local_write".into(),
                deps: vec![],
                team_role_key: None,
            },
            crate::plan::PlanTaskInput {
                task_key: "v".into(),
                kind: "analysis".into(),
                title: "v".into(),
                inputs: vec![],
                expected_outputs: vec![],
                acceptance: Default::default(),
                effect_class: "read".into(),
                deps: vec!["w1".into()],
                team_role_key: None,
            },
        ];
        let rev = crate::plan::create_draft(&store, "wi", "att1", &tasks, "agent", None).unwrap();
        crate::plan::submit(&store, &rev.id).unwrap();
        crate::plan::approve(&store, &rev.id, "owner").unwrap();
        crate::plan::start(&store, &rev.id).unwrap();
        // read model：真实状态。
        let model = build(&store, "wi").unwrap();
        assert_eq!(
            model.template_version_id,
            crate::template::default_active_version_id(&store).unwrap()
        );
        assert_eq!(model.gates.len(), 6);
        assert!(model
            .gates
            .iter()
            .all(|g| g.progress != "done" || g.state == "passed"));
        // w1 pending（排队位）→ waiting；v 未建 attempt（deps 未满足）→ 不在 tasks。
        assert!(
            model.tasks.iter().all(|t| t.task_key != "v"),
            "依赖未满足的下游不出现（无估算假值）"
        );
        // checkpoint 保存 → 断线重建。
        let sha1 = save_checkpoint(&store, "wi").unwrap();
        let restored = load_checkpoint(&store, "wi").unwrap().unwrap();
        assert_eq!(restored.workitem_id, "wi");
        assert_eq!(restored.tasks.len(), model.tasks.len());
        // 事实变化 → checkpoint 可刷新（sha 变化）。
        crate::scheduler::advance(
            &store,
            &crate::plan::attempts_of(&store, &rev.id).unwrap()[0].id,
            "succeeded",
            "od",
        )
        .unwrap();
        let sha2 = save_checkpoint(&store, "wi").unwrap();
        assert_ne!(sha1, sha2, "事实变化后 checkpoint 刷新");
    }
}
