//! WorkItem 与六关阶段状态（PRD v2 §16.1 行为等价）+ 门禁引擎（三态输入）。

pub mod docs;
pub mod gate;
pub mod progress;
pub mod stages;

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
            "failed" => StageState::Failed,
            "cancelled" => StageState::Cancelled,
            "stale" => StageState::Stale,
            _ => return None,
        })
    }

    /// 合法迁移表（与 v2 Go transitions 等价）。
    pub fn can_transition(from: StageState, to: StageState) -> bool {
        use StageState::*;
        matches!(
            (from, to),
            (NotStarted, Running) | (NotStarted, Cancelled)
                | (Running, Blocked) | (Running, AwaitingApproval) | (Running, Passed)
                | (Running, Failed) | (Running, Cancelled) | (Running, Stale)
                | (Blocked, Running) | (Blocked, Cancelled) | (Blocked, Stale)
                | (AwaitingApproval, Running) | (AwaitingApproval, Passed)
                | (AwaitingApproval, Failed) | (AwaitingApproval, Cancelled) | (AwaitingApproval, Stale)
                | (Passed, Stale)
                | (Failed, Running) | (Failed, Cancelled)
                | (Stale, Running) | (Stale, Cancelled)
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

/// 创建 WorkItem 并初始化六关阶段。
pub fn create(
    store: &Store,
    project_id: &str,
    title: &str,
    description: &str,
    issue_iid: Option<&str>,
    labels: &[String],
) -> Result<WorkItem, Error> {
    if project_id.is_empty() || title.is_empty() {
        return Err(Error::Message("project and title required".into()));
    }
    let id = ids::new_id("wi");
    let now = timefmt::now();
    let labels_json = serde_json::to_string(labels).unwrap_or_else(|_| "[]".into());
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO workitems(id, project_id, gitlab_issue_iid, title, description, labels, current_gate, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,'requirements',?7,?7)",
            rusqlite::params![id, project_id, issue_iid, title, description, labels_json, now],
        )?;
        for gate in Gate::ALL {
            conn.execute(
                "INSERT INTO workitem_stages(workitem_id, gate, state, updated_at) VALUES (?1,?2,'not_started',?3)",
                rusqlite::params![id, gate.as_str(), now],
            )?;
        }
        Ok(())
    })?;
    outbox::emit(store, "workitem", &id, "workitem.created",
        serde_json::json!({"projectId": project_id, "title": title, "issueIid": issue_iid}))?;
    get(store, &id)
}

pub fn get(store: &Store, id: &str) -> Result<WorkItem, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT id, project_id, gitlab_issue_iid, title, description, labels, current_gate, created_at, updated_at
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
                    created_at: r.get(7)?,
                    updated_at: r.get(8)?,
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

pub fn list(store: &Store, project_id: &str, cursor: &str, limit: i64) -> Result<(Vec<WorkItem>, String), Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, project_id, gitlab_issue_iid, title, labels, current_gate, created_at, (created_at || id) AS ck
             FROM workitems WHERE project_id = ?1 AND (?2 = '' OR ck < ?2)
             ORDER BY created_at DESC, id DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(rusqlite::params![project_id, cursor, limit], |r| {
            Ok(WorkItem {
                id: r.get(0)?,
                project_id: r.get(1)?,
                gitlab_issue_iid: flexible_opt_string(&r.get::<_, rusqlite::types::Value>(2)?),
                title: r.get(3)?,
                description: String::new(),
                labels: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
                current_gate: r.get(5)?,
                created_at: r.get(6)?,
                updated_at: String::new(),
            })
        })?;
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

/// 阶段状态迁移（校验合法性）；passed 时推进 current_gate。
pub fn set_stage(store: &Store, workitem_id: &str, gate: Gate, to: StageState, baseline_sha: &str) -> Result<(), Error> {
    let current = stage_state(store, workitem_id, gate)?;
    let from = StageState::parse(&current).unwrap_or(StageState::NotStarted);
    if !StageState::can_transition(from, to) {
        return Err(Error::Message(format!(
            "invalid_stage_transition: {} -> {} on {}/{}",
            from.as_str(),
            to.as_str(),
            workitem_id,
            gate.as_str()
        )));
    }
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE workitem_stages SET state=?1, input_baseline_sha=?2, updated_at=?3 WHERE workitem_id=?4 AND gate=?5",
            rusqlite::params![to.as_str(), baseline_sha, now, workitem_id, gate.as_str()],
        )?;
        if to == StageState::Passed {
            if let Some(next) = gate.next() {
                conn.execute(
                    "UPDATE workitems SET current_gate=?1, updated_at=?2 WHERE id=?3",
                    rusqlite::params![next.as_str(), now, workitem_id],
                )?;
            }
        }
        Ok(())
    })?;
    outbox::emit(store, "workitem", workitem_id, &format!("stage.{}", to.as_str()),
        serde_json::json!({"gate": gate.as_str(), "from": from.as_str(), "baselineSha": baseline_sha}))?;
    Ok(())
}

fn stage_state(store: &Store, workitem_id: &str, gate: Gate) -> Result<String, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT state FROM workitem_stages WHERE workitem_id=?1 AND gate=?2",
            [workitem_id, gate.as_str()],
            |r| r.get(0),
        )
        .map_err(|_| Error::Message("stage not found".into()))
    })
}

/// 门禁通过推进：not_started 先经 running，保持状态机合法；已 passed 幂等。
pub fn pass_gate(store: &Store, workitem_id: &str, gate: Gate) -> Result<(), Error> {
    let current = stage_state(store, workitem_id, gate)?;
    if current == "passed" {
        return Ok(());
    }
    if current == "not_started" {
        set_stage(store, workitem_id, gate, StageState::Running, "")?;
    }
    set_stage(store, workitem_id, gate, StageState::Passed, "")
}

/// 新基线下游 stale 传播：从 from_gate 起所有可进入 stale 的关卡。
pub fn mark_stale_from(store: &Store, workitem_id: &str, from_gate: Gate, new_baseline: &str) -> Result<(), Error> {
    let all = Gate::ALL;
    let start = all.iter().position(|g| *g == from_gate).ok_or_else(|| Error::Message("unknown gate".into()))?;
    for gate in &all[start..] {
        let current = stage_state(store, workitem_id, *gate)?;
        let state = StageState::parse(&current).unwrap_or(StageState::NotStarted);
        if StageState::can_transition(state, StageState::Stale) {
            set_stage(store, workitem_id, *gate, StageState::Stale, new_baseline)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!("sg-wi-{}-{}", std::process::id(), ids::new_id("t")));
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
        assert!(set_stage(&s, &wi.id, Gate::Requirements, StageState::Passed, "").is_err(), "not_started -> passed 非法");
        set_stage(&s, &wi.id, Gate::Requirements, StageState::Running, "sha-1").unwrap();
        pass_gate(&s, &wi.id, Gate::Requirements).unwrap();
        assert_eq!(get(&s, &wi.id).unwrap().current_gate, "design");
    }

    #[test]
    fn stale_propagation_and_rebind() {
        let s = setup();
        let wi = create(&s, "pj", "t", "", None, &[]).unwrap();
        set_stage(&s, &wi.id, Gate::Requirements, StageState::Running, "sha-1").unwrap();
        pass_gate(&s, &wi.id, Gate::Requirements).unwrap();
        set_stage(&s, &wi.id, Gate::Design, StageState::Running, "sha-1").unwrap();
        mark_stale_from(&s, &wi.id, Gate::Requirements, "sha-2").unwrap();
        let st = stages(&s, &wi.id).unwrap();
        assert_eq!(st[0].state, "stale");
        assert_eq!(st[1].state, "stale");
        set_stage(&s, &wi.id, Gate::Requirements, StageState::Running, "sha-2").unwrap();
    }

    #[test]
    fn gate_helpers() {
        assert_eq!(Gate::parse("deployment"), Some(Gate::Deployment));
        assert_eq!(Gate::parse("nope"), None);
        assert_eq!(Gate::Requirements.next(), Some(Gate::Design));
        assert_eq!(Gate::Verification.next(), None);
    }
}
