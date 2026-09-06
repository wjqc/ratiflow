//! 结构化计划权威（EvoFlow 方案 M2-02 / ADR-036 §6.2 / ADR-037 §6.4）：
//! PlanRevision 生命周期 + digest（plan_digest_drift 防篡改）+ 激活校验
//! （环/缺引用/孤立写任务，复用 dag.rs）+ 确定性 Markdown 投影。
//! 事实只追加：重规划 = 新 revision（supersedes_id / replan_links），不回写旧版本。

use rusqlite::OptionalExtension;
use sg_store::{ids, outbox, timefmt, Error, Store};
use sha2::{Digest, Sha256};

use crate::dag;

/// §6.2 PlanRevision 状态机合法迁移。
fn can_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("draft", "awaiting_approval")
            | ("draft", "cancelled")
            // replan 无需重批的快路径（§6.4：diff 无授权扩大时免重审）；
            // RPC plan.decide 仍要求命中 pending 审批行，正常计划不可绕过评审。
            | ("draft", "approved")
            | ("awaiting_approval", "approved")
            | ("awaiting_approval", "rejected")
            | ("awaiting_approval", "cancelled")
            | ("approved", "executing")
            | ("approved", "cancelled")
            | ("executing", "completed")
            | ("executing", "replan_required")
            | ("replan_required", "superseded")
    )
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct PlanAcceptance {
    #[serde(default)]
    pub machine: Vec<String>,
    #[serde(default)]
    pub manual: Vec<String>,
}

/// 任务定义输入（ordinal 无意义：依赖边决定顺序）。
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct PlanTaskInput {
    pub task_key: String,
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub expected_outputs: Vec<String>,
    #[serde(default)]
    pub acceptance: PlanAcceptance,
    pub effect_class: String,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub team_role_key: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PlanRevisionRecord {
    pub id: String,
    pub workitem_id: String,
    pub stage_attempt_id: String,
    pub revision_no: i64,
    pub status: String,
    pub digest: String,
    pub supersedes_id: Option<String>,
    pub approved_by: Option<String>,
    pub approved_at: Option<String>,
    pub markdown_digest: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PlanTaskRecord {
    pub id: String,
    pub plan_revision_id: String,
    pub task_key: String,
    /// 复用自上一版本计划的 attempt（非空 = 不重跑，视同已完成）。
    pub reused_from_attempt_id: Option<String>,
    pub kind: String,
    pub title: String,
    pub inputs: Vec<String>,
    pub expected_outputs: Vec<String>,
    pub acceptance: PlanAcceptance,
    pub effect_class: String,
    pub deps: Vec<String>,
    pub team_role_key: Option<String>,
}

impl PlanTaskRecord {
    /// 回读转定义输入（replan 携带/diff 用）。
    pub fn as_input(&self) -> PlanTaskInput {
        PlanTaskInput {
            task_key: self.task_key.clone(),
            kind: self.kind.clone(),
            title: self.title.clone(),
            inputs: self.inputs.clone(),
            expected_outputs: self.expected_outputs.clone(),
            acceptance: self.acceptance.clone(),
            effect_class: self.effect_class.clone(),
            deps: self.deps.clone(),
            team_role_key: self.team_role_key.clone(),
        }
    }
}

fn is_write_effect(effect_class: &str) -> bool {
    matches!(
        effect_class,
        "local_write" | "external_write" | "irreversible"
    )
}

/// 内容 digest：task_key 排序的 canonical 结构（deps 同步排序），前缀 pd1。
pub fn content_digest(tasks: &[PlanTaskInput]) -> String {
    let mut entries: Vec<(String, serde_json::Value)> = tasks
        .iter()
        .map(|t| {
            let mut deps = t.deps.clone();
            deps.sort();
            let mut machine = t.acceptance.machine.clone();
            machine.sort();
            let mut manual = t.acceptance.manual.clone();
            manual.sort();
            (
                t.task_key.clone(),
                serde_json::json!({
                    "kind": t.kind,
                    "title": t.title,
                    "inputs": t.inputs,
                    "expectedOutputs": t.expected_outputs,
                    "acceptance": {"machine": machine, "manual": manual},
                    "effectClass": t.effect_class,
                    "deps": deps,
                    "teamRoleKey": t.team_role_key,
                }),
            )
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let canonical = serde_json::to_string(&entries).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(format!("pd1|{canonical}").as_bytes());
    sg_store::ids::hex(&hasher.finalize())
}

const KINDS: [&str; 6] = [
    "analysis",
    "read",
    "local_write",
    "external_write",
    "verification",
    "merge",
];
const EFFECTS: [&str; 5] = [
    "none",
    "read",
    "local_write",
    "external_write",
    "irreversible",
];

fn validate_inputs_ex(
    tasks: &[PlanTaskInput],
    exempt_orphan: &std::collections::BTreeSet<String>,
) -> Result<Vec<String>, Error> {
    // 结构校验（kind/effect 枚举）在入库前；图校验（环/引用/孤立写）复用 dag.rs。
    for t in tasks {
        if !KINDS.contains(&t.kind.as_str()) {
            return Err(Error::Message(format!(
                "plan_validation_failed: 非法任务 kind {}（任务 {}）",
                t.kind, t.task_key
            )));
        }
        if !EFFECTS.contains(&t.effect_class.as_str()) {
            return Err(Error::Message(format!(
                "plan_validation_failed: 非法 effect_class {}（任务 {}）",
                t.effect_class, t.task_key
            )));
        }
        if t.task_key.is_empty() {
            return Err(Error::Message(
                "plan_validation_failed: task_key 必填".into(),
            ));
        }
    }
    let dag_tasks: Vec<dag::DagTask> = tasks
        .iter()
        .map(|t| {
            let mut d = dag::DagTask::from_strings(
                t.task_key.clone(),
                t.deps.clone(),
                is_write_effect(&t.effect_class),
            );
            d.is_reused = exempt_orphan.contains(&t.task_key);
            d
        })
        .collect();
    dag::validate_and_order(&dag_tasks)
        .map_err(|e| Error::Message(format!("{}: {}", e.token, e.message)))
}

fn validate_inputs(tasks: &[PlanTaskInput]) -> Result<Vec<String>, Error> {
    validate_inputs_ex(tasks, &std::collections::BTreeSet::new())
}

/// 确定性 Markdown 投影（用户可读；权威在结构化表）。按拓扑序渲染。
pub fn render_markdown(tasks: &[PlanTaskInput], order: &[String], revision_no: i64) -> String {
    let by_key: std::collections::BTreeMap<&str, &PlanTaskInput> =
        tasks.iter().map(|t| (t.task_key.as_str(), t)).collect();
    let mut out = format!("# 计划 v{revision_no}\n\n");
    out.push_str("> 本投影由结构化计划生成；调度与验证以 plan_tasks/edges 为权威。\n\n");
    for (i, key) in order.iter().enumerate() {
        let Some(t) = by_key.get(key.as_str()) else {
            continue;
        };
        out.push_str(&format!(
            "{}. **{}**（{} / {}）{}\n",
            i + 1,
            t.task_key,
            t.kind,
            t.effect_class,
            if t.title.is_empty() {
                ""
            } else {
                t.title.as_str()
            },
        ));
        if !t.deps.is_empty() {
            out.push_str(&format!("   - 依赖：{}\n", t.deps.join(", ")));
        }
        for o in &t.expected_outputs {
            out.push_str(&format!("   - 产出：{o}\n"));
        }
        for m in &t.acceptance.machine {
            out.push_str(&format!("   - 机器判据：{m}\n"));
        }
        for m in &t.acceptance.manual {
            out.push_str(&format!("   - 人工判据：{m}\n"));
        }
    }
    out
}

fn insert_tasks(
    conn: &rusqlite::Connection,
    revision_id: &str,
    tasks: &[PlanTaskInput],
    now: &str,
) -> Result<(), Error> {
    // 先全量落任务、再落边：边引用两侧任务行，插一半时上游可能尚不存在。
    for t in tasks {
        conn.execute(
            "INSERT INTO plan_tasks(id, plan_revision_id, task_key, kind, title, inputs_json,
                expected_outputs_json, acceptance_json, effect_class, team_role_key, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            rusqlite::params![
                ids::new_id("ptask"),
                revision_id,
                t.task_key,
                t.kind,
                t.title,
                serde_json::to_string(&t.inputs).unwrap_or_default(),
                serde_json::to_string(&t.expected_outputs).unwrap_or_default(),
                serde_json::to_string(&serde_json::json!({
                    "machine": t.acceptance.machine, "manual": t.acceptance.manual,
                }))
                .unwrap_or_default(),
                t.effect_class,
                t.team_role_key,
                now
            ],
        )?;
    }
    for t in tasks {
        for dep in &t.deps {
            let inserted = conn.execute(
                "INSERT INTO plan_task_edges(id, plan_revision_id, from_task_id, to_task_id, created_at)
                 SELECT ?1, ?2, up.id, cur.id, ?3
                 FROM plan_tasks up, plan_tasks cur
                 WHERE up.plan_revision_id=?2 AND up.task_key=?4
                   AND cur.plan_revision_id=?2 AND cur.task_key=?5",
                rusqlite::params![ids::new_id("pedge"), revision_id, now, dep, t.task_key],
            )?;
            if inserted == 0 {
                return Err(Error::Message(format!(
                    "plan_validation_failed: 依赖边落库失败（{} -> {}）",
                    dep, t.task_key
                )));
            }
        }
    }
    Ok(())
}

fn revision_row(conn: &rusqlite::Connection, id: &str) -> Result<PlanRevisionRecord, Error> {
    conn.query_row(
        "SELECT id, workitem_id, stage_attempt_id, revision_no, status, digest, supersedes_id,
                approved_by, approved_at, markdown_digest, created_at, updated_at
         FROM plan_revisions WHERE id=?1",
        [id],
        |r| {
            Ok(PlanRevisionRecord {
                id: r.get(0)?,
                workitem_id: r.get(1)?,
                stage_attempt_id: r.get(2)?,
                revision_no: r.get(3)?,
                status: r.get(4)?,
                digest: r.get(5)?,
                supersedes_id: r.get(6)?,
                approved_by: r.get(7)?,
                approved_at: r.get(8)?,
                markdown_digest: r.get(9)?,
                created_at: r.get(10)?,
                updated_at: r.get(11)?,
            })
        },
    )
    .map_err(|_| Error::Message(format!("plan_not_found: 计划版本 {id} 不存在")))
}

fn task_inputs(
    conn: &rusqlite::Connection,
    revision_id: &str,
) -> Result<Vec<PlanTaskInput>, Error> {
    let mut stmt = conn.prepare(
        "SELECT task_key, kind, title, inputs_json, expected_outputs_json, acceptance_json,
                effect_class, team_role_key
         FROM plan_tasks WHERE plan_revision_id=?1",
    )?;
    let mut tasks: Vec<PlanTaskInput> = stmt
        .query_map([revision_id], |r| {
            Ok(PlanTaskInput {
                task_key: r.get(0)?,
                kind: r.get(1)?,
                title: r.get(2)?,
                inputs: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
                expected_outputs: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
                acceptance: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or(
                    PlanAcceptance {
                        machine: vec![],
                        manual: vec![],
                    },
                ),
                effect_class: r.get(6)?,
                deps: vec![],
                team_role_key: r.get(7)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    // deps 由边表回读。
    let mut edge_stmt = conn.prepare(
        "SELECT to_t.task_key, from_t.task_key
         FROM plan_task_edges e
         JOIN plan_tasks to_t ON to_t.id = e.to_task_id
         JOIN plan_tasks from_t ON from_t.id = e.from_task_id
         WHERE e.plan_revision_id=?1",
    )?;
    let edges: Vec<(String, String)> = edge_stmt
        .query_map([revision_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for t in &mut tasks {
        t.deps = edges
            .iter()
            .filter(|(to, _)| *to == t.task_key)
            .map(|(_, from)| from.clone())
            .collect();
        t.deps.sort();
    }
    Ok(tasks)
}

/// 创建 draft 计划（激活校验通过才落库）。supersede：替代旧 revision 时传其 id。
pub fn create_draft(
    store: &Store,
    workitem_id: &str,
    stage_attempt_id: &str,
    tasks: &[PlanTaskInput],
    created_by: &str,
    supersedes_id: Option<&str>,
) -> Result<PlanRevisionRecord, Error> {
    create_draft_ex(
        store,
        workitem_id,
        stage_attempt_id,
        tasks,
        created_by,
        supersedes_id,
        &std::collections::BTreeSet::new(),
    )
}

/// replan 变体：豁免集合内的复用任务跳过孤立写检查（产物已物化）。
pub fn create_draft_ex(
    store: &Store,
    workitem_id: &str,
    stage_attempt_id: &str,
    tasks: &[PlanTaskInput],
    created_by: &str,
    supersedes_id: Option<&str>,
    exempt_orphan: &std::collections::BTreeSet<String>,
) -> Result<PlanRevisionRecord, Error> {
    validate_inputs_ex(tasks, exempt_orphan)?;
    let id = ids::new_id("prev");
    let now = timefmt::now();
    store.with_conn(|conn| {
        let revision_no: i64 = conn.query_row(
            "SELECT COALESCE(MAX(revision_no),0)+1 FROM plan_revisions WHERE stage_attempt_id=?1",
            [stage_attempt_id],
            |r| r.get(0),
        )?;
        conn.execute(
            "INSERT INTO plan_revisions(id, workitem_id, stage_attempt_id, revision_no, status, digest,
                supersedes_id, created_by, created_at, updated_at)
             VALUES (?1,?2,?3,?4,'draft',?5,?6,?7,?8,?8)",
            rusqlite::params![
                id,
                workitem_id,
                stage_attempt_id,
                revision_no,
                content_digest(tasks),
                supersedes_id,
                created_by,
                now
            ],
        )?;
        insert_tasks(conn, &id, tasks, &now)?;
        revision_row(conn, &id)
    })?;
    outbox::emit(
        store,
        "workitem",
        workitem_id,
        "plan.draft_created",
        serde_json::json!({"planRevisionId": id, "stageAttemptId": stage_attempt_id}),
    )?;
    revision_by_id(store, &id)
}

/// 更新 draft 任务集（仅 draft；重算 digest；事实只追加——approved 后不可改）。
pub fn update_draft(
    store: &Store,
    revision_id: &str,
    tasks: &[PlanTaskInput],
) -> Result<PlanRevisionRecord, Error> {
    validate_inputs(tasks)?;
    store.with_conn(|conn| {
        let rev = revision_row(conn, revision_id)?;
        if rev.status != "draft" {
            return Err(Error::Message(
                "plan_invalid_transition: 仅 draft 版本可编辑".into(),
            ));
        }
        conn.execute(
            "DELETE FROM plan_task_edges WHERE plan_revision_id=?1",
            [revision_id],
        )?;
        conn.execute(
            "DELETE FROM plan_tasks WHERE plan_revision_id=?1",
            [revision_id],
        )?;
        let now = timefmt::now();
        insert_tasks(conn, revision_id, tasks, &now)?;
        conn.execute(
            "UPDATE plan_revisions SET digest=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![content_digest(tasks), now, revision_id],
        )?;
        revision_row(conn, revision_id)
    })
}

pub fn revision_by_id(store: &Store, id: &str) -> Result<PlanRevisionRecord, Error> {
    store.with_conn(|conn| revision_row(conn, id))
}

pub fn tasks_of(store: &Store, revision_id: &str) -> Result<Vec<PlanTaskRecord>, Error> {
    store.with_conn(|conn| {
        let inputs = task_inputs(conn, revision_id)?;
        // task_key → (id, reused_from_attempt_id)。
        let mut meta: std::collections::BTreeMap<String, (String, Option<String>)> =
            std::collections::BTreeMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT task_key, id, reused_from_attempt_id FROM plan_tasks WHERE plan_revision_id=?1",
            )?;
            let rows = stmt.query_map([revision_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })?;
            for row in rows {
                let (k, id, reused) = row?;
                meta.insert(k, (id, reused));
            }
        }
        Ok(inputs
            .into_iter()
            .map(|t| PlanTaskRecord {
                id: meta.get(&t.task_key).map(|(id, _)| id.clone()).unwrap_or_default(),
                reused_from_attempt_id: meta.get(&t.task_key).and_then(|(_, r)| r.clone()),
                plan_revision_id: revision_id.to_string(),
                task_key: t.task_key,
                kind: t.kind,
                title: t.title,
                inputs: t.inputs,
                expected_outputs: t.expected_outputs,
                acceptance: t.acceptance,
                effect_class: t.effect_class,
                deps: t.deps,
                team_role_key: t.team_role_key,
            })
            .collect())
    })
}

/// 状态迁移（统一入口；digest 漂移在 submit/approve 处强制重查）。
fn transition(store: &Store, id: &str, to: &str) -> Result<PlanRevisionRecord, Error> {
    let current = revision_by_id(store, id)?;
    if !can_transition(&current.status, to) {
        return Err(Error::Message(format!(
            "plan_invalid_transition: {} -> {}",
            current.status, to
        )));
    }
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE plan_revisions SET status=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![to, now, id],
        )?;
        revision_row(conn, id)
    })
}

fn current_status(store: &Store, id: &str) -> Result<String, Error> {
    Ok(revision_by_id(store, id)?.status)
}

/// 携带/复用任务键（孤立写检查豁免集）：replan 落 carried_from_old / reused_from_attempt_id。
fn exemptions_of(conn: &rusqlite::Connection, revision_id: &str) -> Result<Vec<String>, Error> {
    let mut stmt = conn.prepare(
        "SELECT task_key FROM plan_tasks
         WHERE plan_revision_id=?1 AND (carried_from_old=1 OR reused_from_attempt_id IS NOT NULL)",
    )?;
    let rows = stmt.query_map([revision_id], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn check_digest(store: &Store, id: &str) -> Result<(), Error> {
    let current = revision_by_id(store, id)?;
    let tasks = store.with_conn(|conn| task_inputs(conn, id))?;
    if content_digest(&tasks) != current.digest {
        return Err(Error::Message(
            "plan_digest_drift: 计划内容与 digest 不一致".into(),
        ));
    }
    Ok(())
}

/// 提交审批：draft → awaiting_approval（再次激活校验，防 draft 期间内容被改出环）。
pub fn submit(store: &Store, id: &str) -> Result<PlanRevisionRecord, Error> {
    check_digest(store, id)?;
    let (tasks, exempt) = store.with_conn(|conn| {
        let tasks = task_inputs(conn, id)?;
        let ex = exemptions_of(conn, id)?;
        Ok((tasks, ex))
    })?;
    validate_inputs_ex(&tasks, &exempt.into_iter().collect())?;
    let r = transition(store, id, "awaiting_approval")?;
    outbox::emit(
        store,
        "workitem",
        &r.workitem_id,
        "plan.approval_requested",
        serde_json::json!({"planRevisionId": id, "digest": r.digest}),
    )?;
    Ok(r)
}

/// 批准：awaiting_approval → approved（digest 漂移拒绝），记录审批身份。
pub fn approve(store: &Store, id: &str, decided_by: &str) -> Result<PlanRevisionRecord, Error> {
    check_digest(store, id)?;
    let current = revision_by_id(store, id)?;
    if !can_transition(&current.status, "approved") {
        return Err(Error::Message(format!(
            "plan_invalid_transition: {} -> approved",
            current.status
        )));
    }
    let now = timefmt::now();
    let r = store.with_conn(|conn| {
        conn.execute(
            "UPDATE plan_revisions SET status='approved', approved_by=?3, approved_at=?2, updated_at=?2 WHERE id=?1",
            rusqlite::params![id, now, decided_by],
        )?;
        revision_row(conn, id)
    })?;
    outbox::emit(
        store,
        "workitem",
        &r.workitem_id,
        "plan.approved",
        serde_json::json!({"planRevisionId": id, "approvedBy": decided_by}),
    )?;
    Ok(r)
}

pub fn reject(store: &Store, id: &str, _decided_by: &str) -> Result<PlanRevisionRecord, Error> {
    transition(store, id, "rejected")
}

pub fn cancel(store: &Store, id: &str) -> Result<PlanRevisionRecord, Error> {
    transition(store, id, "cancelled")
}

/// 开始执行：approved → executing；被本版本取代的旧 revision 标 superseded。
/// 返回 ready set（首批准入任务，M3 scheduler 据此创建 TaskAttempt）。
/// 开始执行：approved → executing；被本版本取代的旧 revision 标 superseded；
/// 为 ready set（无依赖任务）创建 pending TaskAttempt（§6.2 第 7 步；
/// M3 scheduler 据此推进，attempt 幂等：已存在的跳过）。
pub fn start(store: &Store, id: &str) -> Result<(PlanRevisionRecord, Vec<AttemptInfo>), Error> {
    check_digest(store, id)?;
    let current = revision_by_id(store, id)?;
    if let Some(old) = &current.supersedes_id {
        let old_rev = revision_by_id(store, old)?;
        if old_rev.status == "executing" || old_rev.status == "replan_required" {
            store.with_conn(|conn| {
                conn.execute(
                    "UPDATE plan_revisions SET status='superseded', updated_at=?1 WHERE id=?2",
                    rusqlite::params![timefmt::now(), old],
                )?;
                Ok(())
            })?;
            outbox::emit(
                store,
                "workitem",
                &current.workitem_id,
                "plan.superseded",
                serde_json::json!({"planRevisionId": old, "by": id}),
            )?;
        }
    }
    // 幂等：已 executing 时跳过状态迁移（崩溃恢复重放安全）。
    let r = if current_status(store, id)?.as_str() == "executing" {
        revision_by_id(store, id)?
    } else {
        transition(store, id, "executing")?
    };
    let (tasks, exempt) = store.with_conn(|conn| {
        let tasks = task_inputs(conn, id)?;
        let ex = exemptions_of(conn, id)?;
        Ok((tasks, ex))
    })?;
    let order = validate_inputs_ex(&tasks, &exempt.into_iter().collect())?;
    // 复用任务（reused_from_attempt_id）不重跑：排除出 ready attempt 创建。
    let reused: std::collections::BTreeSet<String> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT task_key FROM plan_tasks
             WHERE plan_revision_id=?1 AND reused_from_attempt_id IS NOT NULL",
        )?;
        let rows = stmt.query_map([id], |r| r.get::<_, String>(0))?;
        let mut out = std::collections::BTreeSet::new();
        for row in rows {
            out.insert(row?);
        }
        Ok(out)
    })?;
    // ready set = 无依赖任务（拓扑序内保持确定性顺序）。
    let ready: Vec<String> = order
        .into_iter()
        .filter(|k| {
            tasks
                .iter()
                .find(|t| &t.task_key == k)
                .is_some_and(|t| t.deps.is_empty())
        })
        .collect();
    let mut attempts = Vec::new();
    for key in &ready {
        if reused.contains(key) {
            continue; // 复用：执行事实已在上一版本，不建新 attempt
        }
        attempts.push(ensure_attempt(store, id, key)?);
    }
    outbox::emit(
        store,
        "workitem",
        &r.workitem_id,
        "plan.started",
        serde_json::json!({"planRevisionId": id, "readySet": ready}),
    )?;
    Ok((r, attempts))
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AttemptInfo {
    pub id: String,
    pub task_key: String,
    pub task_id: String,
    pub attempt_no: i64,
    pub state: String,
}

/// 任务 attempt（幂等）：已存在任何 attempt → 返回最新；否则建 pending。
pub fn ensure_attempt(
    store: &Store,
    revision_id: &str,
    task_key: &str,
) -> Result<AttemptInfo, Error> {
    store.with_conn(|conn| {
        let task_id: String = conn
            .query_row(
                "SELECT id FROM plan_tasks WHERE plan_revision_id=?1 AND task_key=?2",
                rusqlite::params![revision_id, task_key],
                |r| r.get(0),
            )
            .map_err(|_| {
                Error::Message(format!("plan_validation_failed: 任务 {task_key} 不存在"))
            })?;
        if let Some(a) = conn
            .query_row(
                "SELECT id, attempt_no, state FROM plan_task_attempts
                 WHERE task_id=?1 ORDER BY attempt_no DESC LIMIT 1",
                [&task_id],
                |r| {
                    Ok(AttemptInfo {
                        id: r.get(0)?,
                        task_key: task_key.to_string(),
                        task_id: task_id.clone(),
                        attempt_no: r.get(1)?,
                        state: r.get(2)?,
                    })
                },
            )
            .optional()?
        {
            return Ok(a);
        }
        let id = ids::new_id("ptatt");
        let now = timefmt::now();
        conn.execute(
            "INSERT INTO plan_task_attempts(id, task_id, attempt_no, state, created_at, updated_at)
             VALUES (?1,?2,1,'pending',?3,?3)",
            rusqlite::params![id, task_id, now],
        )?;
        Ok(AttemptInfo {
            id,
            task_key: task_key.to_string(),
            task_id,
            attempt_no: 1,
            state: "pending".into(),
        })
    })
}

/// 显式新建下一 attempt（仅 reconcile not_executed 重试路径；绕过幂等返回）。
pub fn create_next_attempt(
    store: &Store,
    revision_id: &str,
    task_key: &str,
) -> Result<AttemptInfo, Error> {
    store.with_conn(|conn| {
        let task_id: String = conn
            .query_row(
                "SELECT id FROM plan_tasks WHERE plan_revision_id=?1 AND task_key=?2",
                rusqlite::params![revision_id, task_key],
                |r| r.get(0),
            )
            .map_err(|_| {
                Error::Message(format!("plan_validation_failed: 任务 {task_key} 不存在"))
            })?;
        let next_no: i64 = conn.query_row(
            "SELECT COALESCE(MAX(attempt_no),0)+1 FROM plan_task_attempts WHERE task_id=?1",
            [&task_id],
            |r| r.get(0),
        )?;
        let id = ids::new_id("ptatt");
        let now = timefmt::now();
        conn.execute(
            "INSERT INTO plan_task_attempts(id, task_id, attempt_no, state, created_at, updated_at)
             VALUES (?1,?2,?3,'pending',?4,?4)",
            rusqlite::params![id, task_id, next_no, now],
        )?;
        Ok(AttemptInfo {
            id,
            task_key: task_key.to_string(),
            task_id,
            attempt_no: next_no,
            state: "pending".into(),
        })
    })
}

/// 全部 attempt（按创建序）——read model / workspace prepare 用。
pub fn attempts_of(store: &Store, revision_id: &str) -> Result<Vec<AttemptInfo>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT pa.id, pt.task_key, pt.id, pa.attempt_no, pa.state
             FROM plan_task_attempts pa
             JOIN plan_tasks pt ON pt.id = pa.task_id
             WHERE pt.plan_revision_id=?1
             ORDER BY pa.created_at, pa.id",
        )?;
        let rows = stmt.query_map([revision_id], |r| {
            Ok(AttemptInfo {
                id: r.get(0)?,
                task_key: r.get(1)?,
                task_id: r.get(2)?,
                attempt_no: r.get(3)?,
                state: r.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

/// 某关当前有效计划（最新非 superseded/rejected/cancelled）。
pub fn latest_for_stage(
    store: &Store,
    stage_attempt_id: &str,
) -> Result<Option<PlanRevisionRecord>, Error> {
    store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT id FROM plan_revisions WHERE stage_attempt_id=?1
             AND status NOT IN ('superseded','rejected','cancelled')
             ORDER BY revision_no DESC LIMIT 1",
                [stage_attempt_id],
                |r| r.get::<_, String>(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other.into()),
            })
        })
        .and_then(|opt| match opt {
            Some(id) => revision_by_id(store, &id).map(Some),
            None => Ok(None),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-plan-{}-{}",
            std::process::id(),
            ids::new_id("t")
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

    fn tasks_ok() -> Vec<PlanTaskInput> {
        vec![
            PlanTaskInput {
                task_key: "implement".into(),
                kind: "local_write".into(),
                title: "实现".into(),
                inputs: vec!["spec:spec@1".into()],
                expected_outputs: vec!["file:src/lib.rs".into()],
                acceptance: PlanAcceptance {
                    machine: vec!["tests_pass".into()],
                    manual: vec![],
                },
                effect_class: "local_write".into(),
                deps: vec!["spec".into()],
                team_role_key: None,
            },
            PlanTaskInput {
                task_key: "spec".into(),
                kind: "analysis".into(),
                title: "分析".into(),
                inputs: vec![],
                expected_outputs: vec!["doc:spec@1".into()],
                acceptance: PlanAcceptance {
                    machine: vec![],
                    manual: vec!["评审通过".into()],
                },
                effect_class: "read".into(),
                deps: vec![],
                team_role_key: None,
            },
            PlanTaskInput {
                task_key: "verify".into(),
                kind: "verification".into(),
                title: "验证".into(),
                inputs: vec![],
                expected_outputs: vec![],
                acceptance: PlanAcceptance {
                    machine: vec![],
                    manual: vec![],
                },
                effect_class: "read".into(),
                deps: vec!["implement".into()],
                team_role_key: None,
            },
        ]
    }

    #[test]
    fn lifecycle_draft_to_executing_and_markdown_deterministic() {
        let store = setup();
        let r = create_draft(&store, "wi", "att1", &tasks_ok(), "agent", None).unwrap();
        assert_eq!(r.revision_no, 1);
        assert_eq!(r.status, "draft");
        let md1 = render_markdown(
            &tasks_ok(),
            &["spec".into(), "implement".into(), "verify".into()],
            1,
        );
        let md2 = render_markdown(
            &tasks_ok(),
            &["spec".into(), "implement".into(), "verify".into()],
            1,
        );
        assert_eq!(md1, md2, "Markdown 投影确定性");
        assert!(md1.contains("spec"));
        // 提交 → 批准 → 开始。
        let r = submit(&store, &r.id).unwrap();
        assert_eq!(r.status, "awaiting_approval");
        let r = approve(&store, &r.id, "owner").unwrap();
        assert_eq!(r.status, "approved");
        assert!(r.approved_by.as_deref() == Some("owner"), "审批身份落库");
        let (r, attempts) = start(&store, &r.id).unwrap();
        assert_eq!(r.status, "executing");
        // ready set = 无依赖任务（spec）；start 已为其创建 pending attempt。
        assert_eq!(attempts.len(), 1, "仅 spec 无依赖");
        assert_eq!(attempts[0].task_key, "spec");
        assert_eq!(attempts[0].state, "pending");
        // 幂等：再次 start 不新建 attempt（executing 状态迁移跳过）。
        let (_, again) = start(&store, &r.id).unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].id, attempts[0].id, "attempt 幂等");
        // executing 不可迁移回 awaiting_approval。
        assert!(transition(&store, &r.id, "awaiting_approval").is_err());
    }

    /// EV-005 前置：内容被改后提交/批准均被 digest 漂移拒绝（plan_digest_drift）。
    #[test]
    fn digest_drift_rejected_on_submit_and_approve() {
        let store = setup();
        let r = create_draft(&store, "wi", "att1", &tasks_ok(), "agent", None).unwrap();
        // 直接改库模拟内容被篡改（绕过服务层）。
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE plan_tasks SET title='篡改' WHERE task_key='spec'",
                    [],
                )
                .unwrap();
                Ok(())
            })
            .unwrap();
        let err = submit(&store, &r.id).unwrap_err();
        assert!(err.to_string().contains("plan_digest_drift"), "{err}");
        // 直接 approve 同样拒绝（不依赖 submit 前置）。
        let err = approve(&store, &r.id, "owner").unwrap_err();
        assert!(err.to_string().contains("plan_digest_drift"), "{err}");
    }

    #[test]
    fn invalid_structures_rejected() {
        let store = setup();
        let mut tasks = tasks_ok();
        tasks[0].deps.push("ghost".into());
        let err = create_draft(&store, "wi", "att1", &tasks, "agent", None).unwrap_err();
        assert!(err.to_string().contains("ghost"), "{err}");
        // 环。
        let cycle = vec![
            PlanTaskInput {
                task_key: "a".into(),
                kind: "read".into(),
                title: String::new(),
                inputs: vec![],
                expected_outputs: vec![],
                acceptance: PlanAcceptance {
                    machine: vec![],
                    manual: vec![],
                },
                effect_class: "read".into(),
                deps: vec!["b".into()],
                team_role_key: None,
            },
            PlanTaskInput {
                task_key: "b".into(),
                kind: "read".into(),
                title: String::new(),
                inputs: vec![],
                expected_outputs: vec![],
                acceptance: PlanAcceptance {
                    machine: vec![],
                    manual: vec![],
                },
                effect_class: "read".into(),
                deps: vec!["a".into()],
                team_role_key: None,
            },
        ];
        let err = create_draft(&store, "wi", "att1", &cycle, "agent", None).unwrap_err();
        assert!(err.to_string().contains("plan_cycle_detected"), "{err}");
        // 孤立写任务。
        let mut orphan = tasks_ok();
        orphan.push(PlanTaskInput {
            task_key: "stray_write".into(),
            kind: "local_write".into(),
            title: String::new(),
            inputs: vec![],
            expected_outputs: vec![],
            acceptance: PlanAcceptance {
                machine: vec![],
                manual: vec![],
            },
            effect_class: "irreversible".into(),
            deps: vec![],
            team_role_key: None,
        });
        let err = create_draft(&store, "wi", "att1", &orphan, "agent", None).unwrap_err();
        assert!(err.to_string().contains("孤立写任务"), "{err}");
    }

    #[test]
    fn supersedes_marks_old_revision_on_start() {
        let store = setup();
        let v1 = create_draft(&store, "wi", "att1", &tasks_ok(), "agent", None).unwrap();
        submit(&store, &v1.id).unwrap();
        approve(&store, &v1.id, "owner").unwrap();
        start(&store, &v1.id).unwrap();
        // replan_required 后创建 v2（替代 v1）。
        transition(&store, &v1.id, "replan_required").unwrap();
        let v2 = create_draft(&store, "wi", "att1", &tasks_ok(), "agent", Some(&v1.id)).unwrap();
        assert_eq!(v2.revision_no, 2);
        submit(&store, &v2.id).unwrap();
        approve(&store, &v2.id, "owner").unwrap();
        start(&store, &v2.id).unwrap();
        let v1 = revision_by_id(&store, &v1.id).unwrap();
        assert_eq!(v1.status, "superseded", "旧 revision 在 v2 start 时被取代");
        // latest_for_stage 返回 v2。
        let latest = latest_for_stage(&store, "att1").unwrap().unwrap();
        assert_eq!(latest.id, v2.id);
    }
}
