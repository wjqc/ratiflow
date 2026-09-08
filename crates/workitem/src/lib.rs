//! WorkItem 与六关阶段状态（PRD v2 §16.1 行为等价）+ 门禁引擎（三态输入）。

pub mod acceptance_eval;
pub mod attempt;
pub mod deliverable;
pub mod docs;
pub mod fast_track;
pub mod gate;
pub mod manual_confirm;
pub mod progress;
pub mod release;
pub mod release_events;
pub mod requirements;
pub mod rework;
pub mod rollback;
pub mod search;
pub mod skip;
pub mod snapshot;
pub mod stages;
pub mod worktree;

use serde::{Deserialize, Serialize};
use sg_store::{ids, outbox, timefmt, Error, Store};

pub use gate::{EvaluateInputs, GateResult, InputState, GATE_INPUTS};

/// 六关（顺序即流程顺序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Gate {
    Requirements,
    Design,
    Development,
    Testing,
    Deployment,
    Verification,
}

impl Gate {
    pub const ALL: [Gate; 6] = [
        Gate::Requirements,
        Gate::Design,
        Gate::Development,
        Gate::Testing,
        Gate::Deployment,
        Gate::Verification,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Gate::Requirements => "requirements",
            Gate::Design => "design",
            Gate::Development => "development",
            Gate::Testing => "testing",
            Gate::Deployment => "deployment",
            Gate::Verification => "verification",
        }
    }

    pub fn parse(s: &str) -> Option<Gate> {
        Gate::ALL.iter().copied().find(|g| g.as_str() == s)
    }

    pub fn next(self) -> Option<Gate> {
        let all = Gate::ALL;
        let idx = all.iter().position(|g| *g == self)?;
        all.get(idx + 1).copied()
    }

    /// 上一关（谱系 derived_from 父边用）。
    pub fn prev(self) -> Option<Gate> {
        let all = Gate::ALL;
        let idx = all.iter().position(|g| *g == self)?;
        if idx == 0 {
            None
        } else {
            all.get(idx - 1).copied()
        }
    }
}

/// 阶段状态机：not_started -> running -> blocked -> awaiting_approval -> passed|failed|cancelled；
/// 输入基线改变进入 stale。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StageState {
    NotStarted,
    Running,
    Blocked,
    AwaitingApproval,
    Passed,
    /// WP-8：关不执行的新终态（仅 (NotStarted, Skipped)；经 gate_skip 审批落态，
    /// 指针推进条件与 Passed 同——但护照 outcome 记 skipped_with_waiver 且
    /// passed:false，旧消费者保守视为未全通过）。
    Skipped,
    Failed,
    Cancelled,
    Stale,
}

impl StageState {
    pub fn as_str(&self) -> &'static str {
        match self {
            StageState::NotStarted => "not_started",
            StageState::Running => "running",
            StageState::Blocked => "blocked",
            StageState::AwaitingApproval => "awaiting_approval",
            StageState::Passed => "passed",
            StageState::Skipped => "skipped",
            StageState::Failed => "failed",
            StageState::Cancelled => "cancelled",
            StageState::Stale => "stale",
        }
    }

    pub fn parse(s: &str) -> Option<StageState> {
        Some(match s {
            "not_started" => StageState::NotStarted,
            "running" => StageState::Running,
            "blocked" => StageState::Blocked,
            "awaiting_approval" => StageState::AwaitingApproval,
            "passed" => StageState::Passed,
            "skipped" => StageState::Skipped,
            "failed" => StageState::Failed,
            "cancelled" => StageState::Cancelled,
            "stale" => StageState::Stale,
            _ => return None,
        })
    }

    /// 合法迁移表（与 v2 Go transitions 等价；WP-8 增 (NotStarted, Skipped)）。
    pub fn can_transition(from: StageState, to: StageState) -> bool {
        use StageState::*;
        matches!(
            (from, to),
            (NotStarted, Running)
                | (NotStarted, Cancelled)
                | (NotStarted, Skipped)
                | (Running, Blocked)
                | (Running, AwaitingApproval)
                | (Running, Passed)
                | (Running, Failed)
                | (Running, Cancelled)
                | (Running, Stale)
                | (Blocked, Running)
                | (Blocked, Cancelled)
                | (Blocked, Stale)
                | (AwaitingApproval, Running)
                | (AwaitingApproval, Passed)
                | (AwaitingApproval, Failed)
                | (AwaitingApproval, Cancelled)
                | (AwaitingApproval, Stale)
                | (Passed, Stale)
                | (Failed, Running)
                | (Failed, Cancelled)
                | (Stale, Running)
                | (Stale, Cancelled)
        )
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid_stage_transition: {from} -> {to} on {gate}")]
pub struct InvalidTransition {
    pub from: &'static str,
    pub to: &'static str,
    pub gate: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkItem {
    pub id: String,
    pub project_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gitlab_issue_iid: Option<String>,
    pub title: String,
    pub description: String,
    pub labels: Vec<String>,
    pub current_gate: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Stage {
    pub gate: String,
    pub state: String,
    pub input_baseline_sha: String,
    pub updated_at: String,
}

/// 实例顺序解析（M1-04 / ADR-036）：WorkItem 的关卡序列来自其冻结的模板实例；
/// 无实例（迁移前残留）回退 legacy 六关。默认模板实例与 Gate::ALL 逐字同序
/// （parity 由 sg-workflow 单测断言），保证 Flag 关闭时行为等价。
#[derive(Debug, Clone, serde::Serialize)]
pub struct GateRef {
    pub gate_id: String,
    pub ordinal: usize,
    pub title: String,
}

pub fn gate_refs(store: &Store, workitem_id: &str) -> Result<Vec<GateRef>, Error> {
    if let Some(gates) = sg_workflow::instance::gates_for_workitem(store, workitem_id)? {
        return Ok(gates
            .into_iter()
            .map(|g| GateRef {
                gate_id: g.gate_id,
                ordinal: g.ordinal.max(1) as usize,
                title: g.title,
            })
            .collect());
    }
    Ok(Gate::ALL
        .iter()
        .enumerate()
        .map(|(i, g)| GateRef {
            gate_id: g.as_str().to_string(),
            ordinal: i + 1,
            title: g.as_str().to_string(),
        })
        .collect())
}

/// 实例顺序的下一关（passed 推进用）；最后一关返回 None（语义与旧 Gate::next 一致）。
pub fn next_gate_id(
    store: &Store,
    workitem_id: &str,
    gate_id: &str,
) -> Result<Option<String>, Error> {
    let refs = gate_refs(store, workitem_id)?;
    Ok(refs
        .iter()
        .position(|g| g.gate_id == gate_id)
        .and_then(|i| refs.get(i + 1))
        .map(|g| g.gate_id.clone()))
}

/// gate_id 是否属于该 WorkItem 实例（RPC 入口动态校验，取代 Gate::parse 六值假设）。
pub fn gate_known(store: &Store, workitem_id: &str, gate_id: &str) -> Result<bool, Error> {
    Ok(gate_refs(store, workitem_id)?
        .iter()
        .any(|g| g.gate_id == gate_id))
}

/// 创建 WorkItem 并按模板实例初始化阶段（默认 six-gate-default）。
pub fn create(
    store: &Store,
    project_id: &str,
    title: &str,
    description: &str,
    issue_iid: Option<&str>,
    labels: &[String],
) -> Result<WorkItem, Error> {
    create_with_template(
        store,
        project_id,
        title,
        description,
        issue_iid,
        labels,
        None,
    )
}

/// template_key：模板逻辑 key（空 = 默认模板）。WorkItem 创建时冻结 active 版本
/// （ADR-036 决策 5）：workitem_stages 按版本关卡定义初始化，实例与投影同事务落库。
pub fn create_with_template(
    store: &Store,
    project_id: &str,
    title: &str,
    description: &str,
    issue_iid: Option<&str>,
    labels: &[String],
    template_key: Option<&str>,
) -> Result<WorkItem, Error> {
    if project_id.is_empty() || title.is_empty() {
        return Err(Error::Message("project and title required".into()));
    }
    let id = ids::new_id("wi");
    let now = timefmt::now();
    let labels_json = serde_json::to_string(labels).unwrap_or_else(|_| "[]".into());
    store.with_conn(|conn| {
        // 冻结 active 版本（单连接内解析，避免嵌套 with_conn）。
        let version_id: String = conn
            .query_row(
                "SELECT v.id FROM workflow_template_versions v
                 JOIN workflow_templates t ON t.id = v.template_id
                 WHERE v.status='active' AND (?1 = '' OR t.key = ?1)",
                [template_key.unwrap_or(sg_workflow::template::DEFAULT_TEMPLATE_KEY)],
                |r| r.get(0),
            )
            .map_err(|_| {
                Error::Message(format!(
                    "workflow_version_not_active: {} 无激活版本",
                    template_key.unwrap_or(sg_workflow::template::DEFAULT_TEMPLATE_KEY)
                ))
            })?;
        let first_gate: String = conn
            .query_row(
                "SELECT gate_id FROM workflow_gate_definitions WHERE version_id=?1 ORDER BY ordinal LIMIT 1",
                [&version_id],
                |r| r.get(0),
            )
            .map_err(|_| Error::Message("workflow_template_invalid: 版本缺少关卡定义".into()))?;
        conn.execute(
            "INSERT INTO workitems(id, project_id, gitlab_issue_iid, title, description, labels, current_gate, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8)",
            rusqlite::params![id, project_id, issue_iid, title, description, labels_json, first_gate, now],
        )?;
        conn.execute(
            "INSERT INTO workitem_stages(workitem_id, gate, state, updated_at)
             SELECT ?1, gd.gate_id, 'not_started', ?2
             FROM workflow_gate_definitions gd WHERE gd.version_id=?3 ORDER BY gd.ordinal",
            rusqlite::params![id, now, version_id],
        )?;
        sg_workflow::instance::create_for_workitem(conn, &id, &version_id, &now)?;
        Ok(())
    })?;
    outbox::emit(
        store,
        "workitem",
        &id,
        "workitem.created",
        serde_json::json!({
            "projectId": project_id,
            "title": title,
            "issueIid": issue_iid,
            "templateKey": template_key.unwrap_or(sg_workflow::template::DEFAULT_TEMPLATE_KEY),
        }),
    )?;
    // 实例冻结事实事件（shadow：事件面新增不影响既有消费者）。
    outbox::emit(
        store,
        "workitem",
        &id,
        "workflow.instance_created",
        serde_json::json!({"templateKey": template_key.unwrap_or(sg_workflow::template::DEFAULT_TEMPLATE_KEY)}),
    )?;
    // WP-11：判重检索增量索引（flag 关闭 no-op）。
    search::index_workitem(store, &id, title, description)?;
    get(store, &id)
}

pub fn get(store: &Store, id: &str) -> Result<WorkItem, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT id, project_id, gitlab_issue_iid, title, description, labels, current_gate, archived_at, created_at, updated_at
             FROM workitems WHERE id = ?1",
            [id],
            |r| {
                Ok(WorkItem {
                    id: r.get(0)?,
                    project_id: r.get(1)?,
                    gitlab_issue_iid: flexible_opt_string(&r.get::<_, rusqlite::types::Value>(2)?),
                    title: r.get(3)?,
                    description: r.get(4)?,
                    labels: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
                    current_gate: r.get(6)?,
                    archived_at: flexible_opt_string(&r.get::<_, rusqlite::types::Value>(7)?),
                    created_at: r.get(8)?,
                    updated_at: r.get(9)?,
                })
            },
        )
        .map_err(|_| Error::Message(format!("workitem {id} not found")))
    })
}

/// iid 列在 v2 schema 中是 INTEGER（动态类型可能存 Text/Integer/Null）。
pub fn flexible_opt_string(value: &rusqlite::types::Value) -> Option<String> {
    use rusqlite::types::Value;
    match value {
        Value::Null => None,
        Value::Text(s) => Some(s.clone()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Real(f) => Some(f.to_string()),
        Value::Blob(b) => Some(String::from_utf8_lossy(b).to_string()),
    }
}

pub fn list(
    store: &Store,
    project_id: &str,
    cursor: &str,
    limit: i64,
    include_archived: bool,
) -> Result<(Vec<WorkItem>, String), Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, project_id, gitlab_issue_iid, title, labels, current_gate, archived_at, created_at, updated_at, (created_at || id) AS ck
             FROM workitems WHERE project_id = ?1 AND (?4 OR archived_at IS NULL) AND (?2 = '' OR ck < ?2)
             ORDER BY created_at DESC, id DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![project_id, cursor, limit, include_archived],
            |r| {
                Ok(WorkItem {
                    id: r.get(0)?,
                    project_id: r.get(1)?,
                    gitlab_issue_iid: flexible_opt_string(&r.get::<_, rusqlite::types::Value>(2)?),
                    title: r.get(3)?,
                    description: String::new(),
                    labels: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
                    current_gate: r.get(5)?,
                    archived_at: flexible_opt_string(&r.get::<_, rusqlite::types::Value>(6)?),
                    created_at: r.get(7)?,
                    updated_at: r.get(8)?,
                })
            },
        )?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        let next = if items.len() as i64 == limit {
            let last = items.last().unwrap();
            format!("{}{}", last.created_at, last.id)
        } else {
            String::new()
        };
        Ok((items, next))
    })
}

/// 归档/恢复（移除语义，非删除）：archived_at 非空即从默认列表隐藏，子表不受影响。
pub fn archive(store: &Store, id: &str, archived: bool) -> Result<(), Error> {
    let now = timefmt::now();
    let changed = store.with_conn(|conn| {
        conn.execute(
            "UPDATE workitems SET archived_at = CASE WHEN ?1 THEN ?2 ELSE NULL END, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![archived, now, id],
        )?;
        Ok(conn.changes())
    })?;
    if changed == 0 {
        return Err(Error::Message(format!("workitem {id} not found")));
    }
    Ok(())
}

pub fn stages(store: &Store, workitem_id: &str) -> Result<Vec<Stage>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT gate, state, input_baseline_sha, updated_at FROM workitem_stages
             WHERE workitem_id = ?1 ORDER BY rowid",
        )?;
        let rows = stmt.query_map([workitem_id], |r| {
            Ok(Stage {
                gate: r.get(0)?,
                state: r.get(1)?,
                input_baseline_sha: r.get(2)?,
                updated_at: r.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

/// 阶段状态迁移（校验合法性）；passed 时按实例顺序推进 current_gate。
/// 兼容期入口：六关调用方传 `gate.as_str()`；新代码直接使用 gate_id。
pub fn set_stage(
    store: &Store,
    workitem_id: &str,
    gate_id: &str,
    to: StageState,
    baseline_sha: &str,
) -> Result<(), Error> {
    let current = stage_state(store, workitem_id, gate_id)?;
    let from = StageState::parse(&current).unwrap_or(StageState::NotStarted);
    if !StageState::can_transition(from, to) {
        return Err(Error::Message(format!(
            "invalid_stage_transition: {} -> {} on {}/{}",
            from.as_str(),
            to.as_str(),
            workitem_id,
            gate_id
        )));
    }
    let next = next_gate_id(store, workitem_id, gate_id)?;
    let now = timefmt::now();
    // 单事务（缺陷审计）：stage 状态与 current_gate 指针原子推进。
    store.with_tx(|conn| {
        conn.execute(
            "UPDATE workitem_stages SET state=?1, input_baseline_sha=?2, updated_at=?3 WHERE workitem_id=?4 AND gate=?5",
            rusqlite::params![to.as_str(), baseline_sha, now, workitem_id, gate_id],
        )?;
        // WP-8：指针推进条件 Passed | Skipped（skip 关同样进入下一关）。
        if to == StageState::Passed || to == StageState::Skipped {
            if let Some(next) = &next {
                conn.execute(
                    "UPDATE workitems SET current_gate=?1, updated_at=?2 WHERE id=?3",
                    rusqlite::params![next, now, workitem_id],
                )?;
            }
        }
        Ok(())
    })?;
    // 实例投影双写（shadow；失败不阻断 legacy 路径，留痕审计）。
    if let Err(e) = sg_workflow::instance::project_state(
        store,
        workitem_id,
        gate_id,
        to.as_str(),
        &get(store, workitem_id)?.current_gate,
    ) {
        let _ = sg_store::audit::append(
            store,
            "system",
            "workflow.instance_projection_failed",
            "workitem",
            workitem_id,
            serde_json::json!({"gate": gate_id, "state": to.as_str(), "error": e.to_string()}),
        );
    }
    outbox::emit(
        store,
        "workitem",
        workitem_id,
        &format!("stage.{}", to.as_str()),
        serde_json::json!({"gate": gate_id, "from": from.as_str(), "baselineSha": baseline_sha}),
    )?;
    Ok(())
}

fn stage_state(store: &Store, workitem_id: &str, gate_id: &str) -> Result<String, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT state FROM workitem_stages WHERE workitem_id=?1 AND gate=?2",
            [workitem_id, gate_id],
            |r| r.get(0),
        )
        .map_err(|_| Error::Message("stage not found".into()))
    })
}

/// 门禁通过推进：not_started 先经 running，保持状态机合法；已 passed 幂等。
pub fn pass_gate(store: &Store, workitem_id: &str, gate_id: &str) -> Result<(), Error> {
    let current = stage_state(store, workitem_id, gate_id)?;
    if current == "passed" {
        return Ok(());
    }
    if current == "not_started" {
        set_stage(store, workitem_id, gate_id, StageState::Running, "")?;
    }
    set_stage(store, workitem_id, gate_id, StageState::Passed, "")
}

/// 新基线下游 stale 传播：从 from_gate 起按实例顺序所有可进入 stale 的关卡。
pub fn mark_stale_from(
    store: &Store,
    workitem_id: &str,
    from_gate_id: &str,
    new_baseline: &str,
) -> Result<(), Error> {
    let refs = gate_refs(store, workitem_id)?;
    let start = refs
        .iter()
        .position(|g| g.gate_id == from_gate_id)
        .ok_or_else(|| Error::Message("unknown gate".into()))?;
    for gate in &refs[start..] {
        let current = stage_state(store, workitem_id, &gate.gate_id)?;
        let state = StageState::parse(&current).unwrap_or(StageState::NotStarted);
        if StageState::can_transition(state, StageState::Stale) {
            set_stage(
                store,
                workitem_id,
                &gate.gate_id,
                StageState::Stale,
                new_baseline,
            )?;
        }
    }
    // M2：上游变化的已批准 attempt 标记 superseded（蓝图 §4.1 approved→superseded）。
    attempt::supersede_from(store, workitem_id, from_gate_id)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Store {
        let dir =
            std::env::temp_dir().join(format!("sg-wi-{}-{}", std::process::id(), ids::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        store.with_conn(|c| {
            c.execute("INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at) VALUES ('pj','u','n','p','main',?1)", [timefmt::now()])?;
            Ok(())
        }).unwrap();
        store
    }

    #[test]
    fn create_initializes_six_stages() {
        let s = setup();
        let wi = create(&s, "pj", "标题", "描述", Some("42"), &["feat".into()]).unwrap();
        assert_eq!(wi.current_gate, "requirements");
        assert_eq!(stages(&s, &wi.id).unwrap().len(), 6);
    }

    #[test]
    fn transitions_and_gate_advance() {
        let s = setup();
        let wi = create(&s, "pj", "t", "", None, &[]).unwrap();
        assert!(
            set_stage(&s, &wi.id, "requirements", StageState::Passed, "").is_err(),
            "not_started -> passed 非法"
        );
        set_stage(&s, &wi.id, "requirements", StageState::Running, "sha-1").unwrap();
        pass_gate(&s, &wi.id, "requirements").unwrap();
        assert_eq!(get(&s, &wi.id).unwrap().current_gate, "design");
    }

    #[test]
    fn stale_propagation_and_rebind() {
        let s = setup();
        let wi = create(&s, "pj", "t", "", None, &[]).unwrap();
        set_stage(&s, &wi.id, "requirements", StageState::Running, "sha-1").unwrap();
        pass_gate(&s, &wi.id, "requirements").unwrap();
        set_stage(&s, &wi.id, "design", StageState::Running, "sha-1").unwrap();
        mark_stale_from(&s, &wi.id, "requirements", "sha-2").unwrap();
        let st = stages(&s, &wi.id).unwrap();
        assert_eq!(st[0].state, "stale");
        assert_eq!(st[1].state, "stale");
        set_stage(&s, &wi.id, "requirements", StageState::Running, "sha-2").unwrap();
    }

    #[test]
    fn progress_aggregate_works() {
        let s = setup();
        let wi = create(&s, "pj", "进度任务", "", None, &[]).unwrap();
        let p = progress::progress(&s, &wi.id).unwrap();
        assert_eq!(p["workItemId"], serde_json::json!(wi.id));
        assert_eq!(p["currentGate"], "requirements");
        assert_eq!(p["evidenceCount"], serde_json::json!(0));
    }

    #[test]
    fn gate_helpers() {
        assert_eq!(Gate::parse("deployment"), Some(Gate::Deployment));
        assert_eq!(Gate::parse("nope"), None);
        assert_eq!(Gate::Requirements.next(), Some(Gate::Design));
        assert_eq!(Gate::Verification.next(), None);
    }

    /// M1 退出标准根基：默认模板实例顺序与 legacy 枚举逐字同序，
    /// gate_refs/next/gate_known 在 Flag 关闭下与旧行为等价（ADR-036 parity）。
    #[test]
    fn gate_refs_parity_with_legacy_enum() {
        let s = setup();
        let wi = create(&s, "pj", "t", "", None, &[]).unwrap();
        let refs = gate_refs(&s, &wi.id).unwrap();
        assert_eq!(refs.len(), Gate::ALL.len());
        for (r, g) in refs.iter().zip(Gate::ALL) {
            assert_eq!(r.gate_id, g.as_str());
        }
        assert_eq!(
            next_gate_id(&s, &wi.id, "requirements").unwrap().as_deref(),
            Some("design")
        );
        assert_eq!(next_gate_id(&s, &wi.id, "verification").unwrap(), None);
        assert!(gate_known(&s, &wi.id, "testing").unwrap());
        assert!(!gate_known(&s, &wi.id, "nonexistent").unwrap());
        // 实例投影与 stages 双写一致。
        set_stage(&s, &wi.id, "requirements", StageState::Running, "sha-1").unwrap();
        pass_gate(&s, &wi.id, "requirements").unwrap();
        let instance = sg_workflow::instance::for_workitem(&s, &wi.id)
            .unwrap()
            .unwrap();
        assert_eq!(instance.current_gate_id, "design");
        let projection = sg_workflow::instance::gates_for_workitem(&s, &wi.id)
            .unwrap()
            .unwrap();
        assert_eq!(projection[0].state, "passed");
    }
}
