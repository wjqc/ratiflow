//! 局部重规划（EvoFlow 方案 M3-02 / ADR-037 §6.4）：
//! 失败/unknown/stale 任务为根 → 下游闭包重做；闭包外且定义未变的成功任务复用
//! （reused_from_attempt_id）；plan diff 形成新增/删除/effect 升级；
//! diff 含 external_write/irreversible 或授权扩大/预算增加时必须重新审批。
//! 不回写旧 revision——新 revision 经 supersedes 链取代（事实只追加）。

use std::collections::{BTreeMap, BTreeSet};

use sg_store::{outbox, timefmt, Error, Store};

use crate::dag;
use crate::plan::{self, AttemptInfo, PlanRevisionRecord, PlanTaskInput};

/// effect 严重度全序（升级判定；未知 effect 排最高——保守）。
pub fn effect_rank(effect_class: &str) -> u8 {
    match effect_class {
        "none" => 0,
        "read" => 1,
        "local_write" => 2,
        "external_write" => 3,
        "irreversible" => 4,
        _ => 9,
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PlanDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// 共存任务 effect 升级（授权扩大，task_key → "old->new"）。
    pub effect_upgraded: Vec<String>,
    /// 新增即含 external_write/irreversible 的任务。
    pub risk_added: Vec<String>,
    /// 预算增加（M3 占位：任务级预算字段尚未启用，恒 false）。
    pub budget_increased: bool,
}

impl PlanDiff {
    /// §6.4：diff 含 external_write/irreversible 或授权扩大时必须重新审批。
    pub fn requires_reapproval(&self) -> bool {
        self.budget_increased || !self.risk_added.is_empty() || !self.effect_upgraded.is_empty()
    }
}

/// 任务定义指纹（不含 deps——重规划允许改依赖不视为定义变化）。
fn definition_fingerprint(t: &PlanTaskInput) -> String {
    let mut machine = t.acceptance.machine.clone();
    machine.sort();
    let mut manual = t.acceptance.manual.clone();
    manual.sort();
    let mut inputs = t.inputs.clone();
    inputs.sort();
    format!(
        "{}|{}|{}|{}|{}|{}",
        t.kind,
        t.effect_class,
        inputs.join(","),
        serde_json::to_string(&t.expected_outputs).unwrap_or_default(),
        serde_json::json!({"machine": machine, "manual": manual}),
        t.team_role_key.as_deref().unwrap_or(""),
    )
}

fn to_inputs(records: &[plan::PlanTaskRecord]) -> Vec<PlanTaskInput> {
    records
        .iter()
        .map(|r| PlanTaskInput {
            task_key: r.task_key.clone(),
            kind: r.kind.clone(),
            title: r.title.clone(),
            inputs: r.inputs.clone(),
            expected_outputs: r.expected_outputs.clone(),
            acceptance: r.acceptance.clone(),
            effect_class: r.effect_class.clone(),
            deps: r.deps.clone(),
            team_role_key: r.team_role_key.clone(),
        })
        .collect()
}

/// 新旧计划 diff（§6.4 第 5 步）。
pub fn diff_plans(old: &[PlanTaskInput], new: &[PlanTaskInput]) -> PlanDiff {
    let old_map: BTreeMap<&str, &PlanTaskInput> =
        old.iter().map(|t| (t.task_key.as_str(), t)).collect();
    let new_map: BTreeMap<&str, &PlanTaskInput> =
        new.iter().map(|t| (t.task_key.as_str(), t)).collect();
    let mut diff = PlanDiff::default();
    for (k, t) in &new_map {
        if !old_map.contains_key(k) {
            diff.added.push((*k).to_string());
            if effect_rank(&t.effect_class) >= effect_rank("external_write") {
                diff.risk_added.push((*k).to_string());
            }
        }
    }
    for k in old_map.keys() {
        if !new_map.contains_key(k) {
            diff.removed.push((*k).to_string());
        }
    }
    for (k, t) in &new_map {
        if let Some(o) = old_map.get(k) {
            if effect_rank(&t.effect_class) > effect_rank(&o.effect_class) {
                diff.effect_upgraded
                    .push(format!("{}: {}->{}", k, o.effect_class, t.effect_class));
            }
        }
    }
    diff.added.sort();
    diff.removed.sort();
    diff.risk_added.sort();
    diff.effect_upgraded.sort();
    diff
}

/// 复用判定（§6.4 第 3 步）：闭包外 + 新旧定义一致 + 最新 attempt succeeded
/// → 复用该 attempt；其余重做。
#[derive(Debug, Default, serde::Serialize)]
pub struct ReuseOutcome {
    /// task_key → 复用的 succeeded attempt id。
    pub reuse: BTreeMap<String, String>,
    /// 需要重做（含闭包内与定义漂移）的 task_key。
    pub redo: BTreeSet<String>,
}

pub fn compute_reuse(
    old_tasks: &[plan::PlanTaskRecord],
    new_tasks: &[PlanTaskInput],
    attempts: &[AttemptInfo],
    closure: &BTreeSet<String>,
) -> ReuseOutcome {
    let old_map: BTreeMap<&str, &plan::PlanTaskRecord> =
        old_tasks.iter().map(|t| (t.task_key.as_str(), t)).collect();
    let new_map: BTreeMap<&str, &PlanTaskInput> =
        new_tasks.iter().map(|t| (t.task_key.as_str(), t)).collect();
    // 每任务最新 attempt（attempts 按创建序，后写覆盖）。
    let mut latest: BTreeMap<&str, &AttemptInfo> = BTreeMap::new();
    for a in attempts {
        latest.insert(a.task_key.as_str(), a);
    }
    let mut out = ReuseOutcome::default();
    for (k, old_t) in &old_map {
        if closure.contains(*k) {
            out.redo.insert((*k).to_string());
            continue;
        }
        // 闭包外但新计划删掉的任务：无复用对象。
        let Some(new_t) = new_map.get(k) else {
            continue;
        };
        // 定义漂移 → 重做。
        if definition_fingerprint(&old_t.as_input()) != definition_fingerprint(new_t) {
            out.redo.insert((*k).to_string());
            continue;
        }
        match latest.get(k) {
            Some(a) if a.state == "succeeded" => {
                out.reuse.insert((*k).to_string(), a.id.clone());
            }
            _ => {
                out.redo.insert((*k).to_string());
            }
        }
    }
    out
}

#[derive(Debug, serde::Serialize)]
pub struct ReplanOutcome {
    pub revision: PlanRevisionRecord,
    pub diff: PlanDiff,
    pub requires_reapproval: bool,
    pub reused: BTreeMap<String, String>,
    pub redo: BTreeSet<String>,
}

/// 执行局部重规划（§6.4）：
/// 1) 旧 revision 标 replan_required（executing 才允许）；
/// 2) 闭包 = roots 下游（dag::affected_closure）；
/// 3) 新任务集 = 闭包外原样携带（含 deps 修剪：上游被删则剪边）+ replacements；
/// 4) 复用判定落 reused_from_attempt_id；
/// 5) 创建新 revision（supersedes 旧）；
/// 6) diff 判定是否重新审批：需要 → draft（走 submit/decide）；不需要 → 直接 approved。
pub fn replan(
    store: &Store,
    old_revision_id: &str,
    roots: &[String],
    replacements: &[PlanTaskInput],
    created_by: &str,
) -> Result<ReplanOutcome, Error> {
    let old_rev = plan::revision_by_id(store, old_revision_id)?;
    if !matches!(old_rev.status.as_str(), "executing" | "replan_required") {
        return Err(Error::Message(format!(
            "plan_invalid_transition: {} 状态不可重规划（仅 executing/replan_required）",
            old_rev.status
        )));
    }
    let old_tasks = plan::tasks_of(store, old_revision_id)?;
    let old_inputs = to_inputs(&old_tasks);
    let attempts = plan::attempts_of(store, old_revision_id)?;
    // roots 必须是旧计划内任务。
    let known: BTreeSet<&str> = old_inputs.iter().map(|t| t.task_key.as_str()).collect();
    for r in roots {
        if !known.contains(r.as_str()) {
            return Err(Error::Message(format!(
                "plan_validation_failed: 重规划根任务 {r} 不在计划中"
            )));
        }
    }
    // 闭包（复用 dag 纯函数）。
    let dag_tasks: Vec<dag::DagTask> = old_inputs
        .iter()
        .map(|t| {
            dag::DagTask::from_strings(
                t.task_key.clone(),
                t.deps.clone(),
                effect_rank(&t.effect_class) >= effect_rank("local_write"),
            )
        })
        .collect();
    let closure = dag::affected_closure(&dag_tasks, roots);
    // 新任务集：闭包外携带（deps 剪除已删除的上游），闭包内用 replacements（缺省即删除）。
    let replacement_map: BTreeMap<&str, &PlanTaskInput> = replacements
        .iter()
        .map(|t| (t.task_key.as_str(), t))
        .collect();
    let mut new_tasks: Vec<PlanTaskInput> = Vec::new();
    let removed_keys: BTreeSet<&str> = old_inputs
        .iter()
        .filter(|t| {
            closure.contains(&t.task_key) && !replacement_map.contains_key(t.task_key.as_str())
        })
        .map(|t| t.task_key.as_str())
        .collect();
    for t in &old_inputs {
        if closure.contains(&t.task_key) {
            continue; // 闭包内由 replacements 决定
        }
        let mut carried = t.clone();
        carried.deps.retain(|d| !removed_keys.contains(d.as_str()));
        new_tasks.push(carried);
    }
    for r in replacements {
        let mut repl = r.clone();
        repl.deps.retain(|d| !removed_keys.contains(d.as_str()));
        new_tasks.push(repl);
    }
    // 复用判定。
    let reuse_out = compute_reuse(&old_tasks, &new_tasks, &attempts, &closure);
    // 孤立写豁免 = 旧计划已存在的任务键（携带/同键替换均为重执行已批准工作）；
    // 新增键（diff.added）仍受完整检查。
    let old_keys: BTreeSet<&str> = old_inputs.iter().map(|t| t.task_key.as_str()).collect();
    let exempt: std::collections::BTreeSet<String> = new_tasks
        .iter()
        .filter(|t| old_keys.contains(t.task_key.as_str()))
        .map(|t| t.task_key.clone())
        .collect();
    // 旧 revision → replan_required。
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE plan_revisions SET status='replan_required', updated_at=?1 WHERE id=?2",
            rusqlite::params![timefmt::now(), old_revision_id],
        )?;
        Ok(())
    })?;
    outbox::emit(
        store,
        "workitem",
        &old_rev.workitem_id,
        "plan.replan_required",
        serde_json::json!({"planRevisionId": old_revision_id, "roots": roots}),
    )?;
    // 新 revision。
    let new_rev = plan::create_draft_ex(
        store,
        &old_rev.workitem_id,
        &old_rev.stage_attempt_id,
        &new_tasks,
        created_by,
        Some(old_revision_id),
        &exempt,
    )?;
    // 携带标记持久化（submit/start 重校验时豁免生效）。
    for key in &exempt {
        store.with_conn(|conn| {
            conn.execute(
                "UPDATE plan_tasks SET carried_from_old=1
                 WHERE plan_revision_id=?1 AND task_key=?2",
                rusqlite::params![new_rev.id, key],
            )?;
            Ok(())
        })?;
    }
    // 落 reused_from_attempt_id。
    for (key, attempt_id) in &reuse_out.reuse {
        store.with_conn(|conn| {
            conn.execute(
                "UPDATE plan_tasks SET reused_from_attempt_id=?1
                 WHERE plan_revision_id=?2 AND task_key=?3",
                rusqlite::params![attempt_id, new_rev.id, key],
            )?;
            Ok(())
        })?;
    }
    // diff（旧全集 vs 新全集）。
    let diff = diff_plans(&old_inputs, &new_tasks);
    let requires = diff.requires_reapproval();
    let new_rev_id = new_rev.id.clone();
    let final_rev = if requires {
        new_rev // draft，等待显式 submit/decide
    } else {
        plan::approve(store, &new_rev_id, "replan_auto")?
    };
    outbox::emit(
        store,
        "workitem",
        &old_rev.workitem_id,
        "plan.replanned",
        serde_json::json!({
            "from": old_revision_id,
            "to": new_rev_id,
            "requiresReapproval": requires,
            "reused": reuse_out.reuse,
        }),
    )?;
    Ok(ReplanOutcome {
        revision: final_rev,
        diff,
        requires_reapproval: requires,
        reused: reuse_out.reuse,
        redo: reuse_out.redo,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::PlanAcceptance;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-replan-{}-{}",
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
            expected_outputs: vec![format!("file:{key}.out")],
            acceptance: PlanAcceptance {
                machine: vec!["m".into()],
                manual: vec![],
            },
            effect_class: effect.into(),
            deps: deps.iter().map(|s| s.to_string()).collect(),
            team_role_key: None,
        }
    }

    fn seed_running_plan(store: &Store) -> String {
        let tasks = vec![
            task("w1", &[], "local_write"),
            task("w2", &[], "local_write"),
            task("v", &["w1", "w2"], "read"),
        ];
        let r = plan::create_draft(store, "wi", "att1", &tasks, "agent", None).unwrap();
        plan::submit(store, &r.id).unwrap();
        plan::approve(store, &r.id, "owner").unwrap();
        plan::start(store, &r.id).unwrap();
        r.id
    }

    fn succeed(store: &Store, revision_id: &str, task_key: &str) {
        let attempts = plan::attempts_of(store, revision_id).unwrap();
        let a = attempts.iter().find(|a| a.task_key == task_key).unwrap();
        // 模拟运行推进：pending → succeeded（直接终态写入；运行细节属 plan_runtime）。
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE plan_task_attempts SET state='succeeded', input_digest='d1' WHERE id=?1",
                    [&a.id],
                )
                .map_err(Error::from)?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn diff_detects_add_remove_upgrade_and_reapproval() {
        let old = vec![task("a", &[], "read"), task("b", &["a"], "local_write")];
        let new = vec![
            task("a", &[], "read"),
            task("b", &["a"], "external_write"), // 升级
            task("c", &[], "irreversible"),      // 风险新增
        ];
        let d = diff_plans(&old, &new);
        assert_eq!(d.added, vec!["c"]);
        assert!(d.removed.is_empty(), "b 共存不删");
        assert!(d.effect_upgraded.iter().any(|s| s.starts_with("b:")));
        assert_eq!(d.risk_added, vec!["c"]);
        assert!(d.requires_reapproval());
        // 纯局部替换（local_write → local_write）无需重批。
        let new2 = [task("a", &[], "read"), task("b2", &["a"], "local_write")];
        let d2 = diff_plans(&old, &new2);
        assert_eq!(d2.removed, vec!["b"]);
        assert!(!d2.requires_reapproval(), "同档替换不扩大授权: {d2:?}");
    }

    #[test]
    fn replan_redoes_closure_reuses_independent_success() {
        let store = setup();
        let rev = seed_running_plan(&store);
        succeed(&store, &rev, "w1");
        // w2 失败为根：闭包 = {w2, v}；w1 复用。
        let out = replan(
            &store,
            &rev,
            &["w2".to_string()],
            &[task("w2", &[], "local_write")],
            "agent",
        )
        .unwrap();
        assert!(!out.requires_reapproval, "local_write 局部替换无需重批");
        assert_eq!(out.revision.status, "approved", "无需重批直接 approved");
        assert_eq!(
            out.reused.get("w1").map(String::as_str),
            plan::attempts_of(&store, &rev)
                .unwrap()
                .iter()
                .find(|a| a.task_key == "w1")
                .map(|a| a.id.as_str()),
            "w1 复用其 succeeded attempt"
        );
        assert!(out.redo.contains("w2") && out.redo.contains("v"));
        // 新 revision：w1 带 reused_from_attempt_id；w2 替换；v 保留依赖。
        let tasks = plan::tasks_of(&store, &out.revision.id).unwrap();
        let w1 = tasks.iter().find(|t| t.task_key == "w1").unwrap();
        let _w2 = tasks.iter().find(|t| t.task_key == "w2").unwrap();
        // v 无替换 → 从新计划移除（removed 路径）。
        assert!(tasks.iter().all(|t| t.task_key != "v"));
        assert!(!w1.id.is_empty());
        // 旧 revision 已 replan_required。
        let old = plan::revision_by_id(&store, &rev).unwrap();
        assert_eq!(old.status, "replan_required");
        // 新 revision start：ready = 仅 w2（w1 复用不重跑）。
        let (_, attempts) = plan::start(&store, &out.revision.id).unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].task_key, "w2");
    }

    #[test]
    fn replan_with_risk_upgrade_requires_reapproval() {
        let store = setup();
        let rev = seed_running_plan(&store);
        let out = replan(
            &store,
            &rev,
            &["w1".to_string()],
            &[task("w1", &[], "external_write")],
            "agent",
        )
        .unwrap();
        assert!(out.requires_reapproval);
        assert_eq!(out.revision.status, "draft", "授权扩大必须重新审批");
        // 走重批：submit → decide → start。
        plan::submit(&store, &out.revision.id).unwrap();
        plan::approve(&store, &out.revision.id, "owner").unwrap();
        let (r, _) = plan::start(&store, &out.revision.id).unwrap();
        assert_eq!(r.status, "executing");
    }

    #[test]
    fn replan_rejects_unknown_root_and_non_executing_revision() {
        let store = setup();
        let rev = seed_running_plan(&store);
        assert!(replan(&store, &rev, &["ghost".to_string()], &[], "a").is_err());
        // draft 状态不可重规划。
        let tasks = vec![task("x", &[], "read")];
        let d = plan::create_draft(&store, "wi", "att1", &tasks, "a", None).unwrap();
        assert!(replan(&store, &d.id, &["x".to_string()], &[], "a").is_err());
    }
}
