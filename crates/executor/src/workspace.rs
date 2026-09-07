//! WorkspacePolicy 与任务工作区生命周期（EvoFlow 方案 M2-06 / ADR-037 §6.7）。
//!
//! 策略映射（风险 → workspace/沙箱/并发）+ task_workspaces 行生命周期
//! （preparing → ready → in_use → merged|merge_conflict|retained|cleaned）。
//! git 机制在 `sg_workitem::worktree`（同一受管域）；本模块负责策略解析与事实记账。
//! 只读任务（analysis/read）共享 WorkItem 快照，不建任务工作区。

use serde::Serialize;
use sg_store::{ids, timefmt, Error, Store};

/// §6.7 风险映射默认值（模板可覆盖，方向只能更严）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedPolicy {
    pub strategy: &'static str,
    pub sandbox_minimum: &'static str,
    /// external_write/irreversible 默认串行；analysis/read 只读共享。
    pub default_serial: bool,
}

/// 任务 kind/effect → 工作区策略（§6.7 默认映射表；按 effect 分档，kind 备 M4 细化）。
pub fn resolve_policy(_task_kind: &str, effect_class: &str) -> ResolvedPolicy {
    match effect_class {
        // 分析/读取：共享只读快照；kernel_restricted；并发按 readers 上限。
        "none" | "read" => ResolvedPolicy {
            strategy: "readonly_snapshot",
            sandbox_minimum: "kernel_restricted",
            default_serial: false,
        },
        // 本地写：独立 task_worktree；kernel_restricted；独立 worktree 可并发。
        "local_write" => ResolvedPolicy {
            strategy: "task_worktree",
            sandbox_minimum: "kernel_restricted",
            default_serial: false,
        },
        // 外部写：task_worktree + receipt；Docker 优先；默认串行（幂等键才可并发）。
        "external_write" => ResolvedPolicy {
            strategy: "task_worktree",
            sandbox_minimum: "docker",
            default_serial: true,
        },
        // 不可逆：专用策略；Docker + 人工审批；串行。
        "irreversible" => ResolvedPolicy {
            strategy: "task_worktree",
            sandbox_minimum: "docker",
            default_serial: true,
        },
        _ => ResolvedPolicy {
            strategy: "task_worktree",
            sandbox_minimum: "docker",
            // 未知 effect 走最严档（PlanGuard 已在上游 fail-closed，此处防御性兜底）。
            default_serial: true,
        },
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskWorkspaceRecord {
    pub id: String,
    pub task_attempt_id: String,
    pub workspace_policy_version_id: Option<String>,
    pub path: String,
    pub base_head: String,
    pub workspace_digest_before: String,
    pub workspace_digest_after: String,
    pub state: String,
}

fn record_row(conn: &rusqlite::Connection, attempt_id: &str) -> Result<TaskWorkspaceRecord, Error> {
    conn.query_row(
        "SELECT id, task_attempt_id, COALESCE(workspace_policy_version_id,''), path, base_head,
                workspace_digest_before, workspace_digest_after, state
         FROM task_workspaces WHERE task_attempt_id=?1",
        [attempt_id],
        |r| {
            Ok(TaskWorkspaceRecord {
                id: r.get(0)?,
                task_attempt_id: r.get(1)?,
                workspace_policy_version_id: {
                    let s: String = r.get(2)?;
                    if s.is_empty() {
                        None
                    } else {
                        Some(s)
                    }
                },
                path: r.get(3)?,
                base_head: r.get(4)?,
                workspace_digest_before: r.get(5)?,
                workspace_digest_after: r.get(6)?,
                state: r.get(7)?,
            })
        },
    )
    .map_err(|_| Error::Message("workspace_unavailable: 任务工作区不存在".into()))
}

/// 只读任务不建工作区（共享快照）；返回 None。
pub fn needs_workspace(effect_class: &str) -> bool {
    matches!(
        effect_class,
        "local_write" | "external_write" | "irreversible"
    )
}

/// 准备任务工作区（幂等）：解析 attempt → task → revision → workitem，
/// 挂出 base HEAD worktree，记 before digest，状态 ready。
pub fn prepare(store: &Store, task_attempt_id: &str) -> Result<Option<TaskWorkspaceRecord>, Error> {
    // attempt → task/revision/workitem/base（单连接一次取全，避免嵌套 with_conn）。
    let ctx = store.with_conn(|conn| {
        conn.query_row(
            "SELECT pr.workitem_id, pt.plan_revision_id, pt.effect_class, pt.kind,
                    COALESCE(pr.digest,''), pa.state
             FROM plan_task_attempts pa
             JOIN plan_tasks pt ON pt.id = pa.task_id
             JOIN plan_revisions pr ON pr.id = pt.plan_revision_id
             WHERE pa.id=?1",
            [task_attempt_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            },
        )
        .map_err(|_| Error::Message("task_dependency_blocked: attempt 不存在".into()))
    })?;
    let (workitem_id, plan_revision_id, effect_class, kind, plan_digest, _attempt_state) = ctx;
    if !needs_workspace(&effect_class) {
        return Ok(None);
    }
    // 已有工作区 → 幂等返回。
    let existing = store.with_conn(|conn| {
        record_row(conn, task_attempt_id)
            .map(Some)
            .or_else(|e| match e {
                Error::Message(m) if m.contains("不存在") => Ok(None),
                other => Err(other),
            })
    })?;
    if let Some(rec) = existing {
        return Ok(Some(rec));
    }
    let policy = resolve_policy(&kind, &effect_class);
    // base HEAD = 计划冻结 digest 时代的主区 HEAD；M2 语义：从 WorkItem 级受管
    // worktree 的当前 HEAD 挂出（受管域内一致基线），失败回退主工作区 HEAD。
    let base_head = sg_workitem::worktree::ensure(store, &workitem_id)
        .map(|w| w.head)
        .or_else(|_| {
            store
                .with_conn(|conn| {
                    conn.query_row(
                        "SELECT COALESCE(p.local_root,'') FROM projects p
                     JOIN workitems w ON w.project_id = p.id WHERE w.id=?1",
                        [&workitem_id],
                        |r| r.get::<_, String>(0),
                    )
                    .map_err(Error::from)
                })
                .ok()
                .and_then(|root| {
                    std::process::Command::new("git")
                        .arg("-C")
                        .arg(&root)
                        .args(["rev-parse", "HEAD"])
                        .output()
                        .ok()
                        .filter(|o| o.status.success())
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                })
                .ok_or_else(|| Error::Message("workspace_unavailable: base HEAD 不可解析".into()))
        })?;
    let info = sg_workitem::worktree::ensure_task_worktree(
        store,
        &workitem_id,
        &plan_revision_id,
        task_attempt_id,
        &base_head,
    )?;
    let before = sg_workitem::worktree::task_workspace_digest(
        store,
        &workitem_id,
        &plan_revision_id,
        task_attempt_id,
    )?;
    let id = ids::new_id("tws");
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO task_workspaces(id, task_attempt_id, path, base_head,
                workspace_digest_before, state, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,'ready',?6,?6)",
            rusqlite::params![id, task_attempt_id, info.path, base_head, before, now],
        )?;
        Ok(())
    })?;
    let _ = (policy, plan_digest);
    store.with_conn(|conn| record_row(conn, task_attempt_id).map(Some))
}

/// 记录 after digest 并按结果落终态：succeeded → retained（等 merge）；
/// failed/unknown → retained（保留现场供对账）；merge 结果态由 merge 流程落。
pub fn finalize(
    store: &Store,
    task_attempt_id: &str,
    outcome: &str,
) -> Result<TaskWorkspaceRecord, Error> {
    let ctx = store.with_conn(|conn| {
        conn.query_row(
            "SELECT pr.workitem_id, pt.plan_revision_id FROM plan_task_attempts pa
             JOIN plan_tasks pt ON pt.id = pa.task_id
             JOIN plan_revisions pr ON pr.id = pt.plan_revision_id
             WHERE pa.id=?1",
            [task_attempt_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .map_err(|_| Error::Message("task_dependency_blocked: attempt 不存在".into()))
    })?;
    let (workitem_id, plan_revision_id) = ctx;
    let after = sg_workitem::worktree::task_workspace_digest(
        store,
        &workitem_id,
        &plan_revision_id,
        task_attempt_id,
    )
    .unwrap_or_default();
    let state = match outcome {
        "succeeded" => "retained",
        "failed" | "unknown" => "retained",
        "cancelled" => "retained",
        _ => "failed",
    };
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE task_workspaces SET workspace_digest_after=?1, state=?2, updated_at=?3
             WHERE task_attempt_id=?4",
            rusqlite::params![after, state, timefmt::now(), task_attempt_id],
        )?;
        record_row(conn, task_attempt_id)
    })
}

/// 状态推进（merged/merge_conflict/cleaned/in_use）——校验合法迁移。
pub fn mark_state(
    store: &Store,
    task_attempt_id: &str,
    to: &str,
    merge_receipt: Option<&str>,
) -> Result<TaskWorkspaceRecord, Error> {
    fn can(from: &str, to: &str) -> bool {
        matches!(
            (from, to),
            ("ready", "in_use")
                | ("in_use", "retained")
                | ("in_use", "failed")
                | ("retained", "merged")
                | ("retained", "merge_conflict")
                | ("retained", "cleaned")
                | ("merge_conflict", "cleaned")
                | ("preparing", "ready")
                | ("preparing", "failed")
        )
    }
    store.with_conn(|conn| {
        let current = record_row(conn, task_attempt_id)?;
        if !can(&current.state, to) {
            return Err(Error::Message(format!(
                "workspace_state_invalid: {} -> {}",
                current.state, to
            )));
        }
        conn.execute(
            "UPDATE task_workspaces SET state=?1, merge_receipt=COALESCE(?2, merge_receipt), updated_at=?3
             WHERE task_attempt_id=?4",
            rusqlite::params![to, merge_receipt, timefmt::now(), task_attempt_id],
        )?;
        record_row(conn, task_attempt_id)
    })
}

pub fn get(store: &Store, task_attempt_id: &str) -> Result<Option<TaskWorkspaceRecord>, Error> {
    store.with_conn(|conn| {
        record_row(conn, task_attempt_id)
            .map(Some)
            .or_else(|e| match e {
                Error::Message(m) if m.contains("不存在") => Ok(None),
                other => Err(other),
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-exec-ws-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        // 主仓库（git）作为项目 local_root。
        let repo = dir.join("main-repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("README.md"), "base\n").unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "t@ratiflow.local"],
            vec!["config", "user.name", "t"],
            vec!["add", "."],
            vec!["commit", "-m", "init"],
        ] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(&args)
                .output()
                .unwrap();
            assert!(out.status.success());
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
                c.execute(
                    "INSERT INTO plan_revisions(id, workitem_id, stage_attempt_id, revision_no, status, digest, created_at, updated_at)
                     VALUES ('pr1','wi','att1',1,'approved','d',?1,?1)",
                    [&now],
                )?;
                Ok(())
            })
            .unwrap();
        store
    }

    fn seed_attempt(store: &Store, task_id: &str, kind: &str, effect: &str, attempt_id: &str) {
        store
            .with_conn(|c| {
                let now = sg_store::timefmt::now();
                c.execute(
                    "INSERT INTO plan_tasks(id, plan_revision_id, task_key, kind, effect_class, created_at)
                     VALUES (?1,'pr1',?2,?3,?4,?5)",
                    rusqlite::params![task_id, task_id, kind, effect, now],
                )?;
                c.execute(
                    "INSERT INTO plan_task_attempts(id, task_id, attempt_no, state, created_at, updated_at)
                     VALUES (?1,?2,1,'ready',?3,?3)",
                    rusqlite::params![attempt_id, task_id, now],
                )?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn policy_mapping_follows_risk_table() {
        let p = resolve_policy("analysis", "read");
        assert_eq!(
            (p.strategy, p.sandbox_minimum),
            ("readonly_snapshot", "kernel_restricted")
        );
        let p = resolve_policy("local_write", "local_write");
        assert_eq!(
            (p.strategy, p.sandbox_minimum),
            ("task_worktree", "kernel_restricted")
        );
        assert!(!p.default_serial);
        let p = resolve_policy("external_write", "external_write");
        assert_eq!(p.sandbox_minimum, "docker");
        assert!(p.default_serial, "外部写默认串行");
        let p = resolve_policy("deploy", "irreversible");
        assert_eq!(p.sandbox_minimum, "docker");
        assert!(p.default_serial);
    }

    #[test]
    fn readonly_needs_no_workspace() {
        assert!(!needs_workspace("read"));
        assert!(!needs_workspace("none"));
        assert!(needs_workspace("local_write"));
        assert!(needs_workspace("irreversible"));
    }

    #[test]
    fn prepare_finalize_lifecycle_with_digests() {
        let store = setup();
        seed_attempt(&store, "t-write", "local_write", "local_write", "pa-w");
        let rec = prepare(&store, "pa-w").unwrap().expect("写任务应有工作区");
        assert_eq!(rec.state, "ready");
        assert!(!rec.base_head.is_empty());
        assert!(!rec.workspace_digest_before.is_empty());
        assert!(rec.path.contains("wi"), "路径含 workitem 段");
        // 幂等。
        let again = prepare(&store, "pa-w").unwrap().unwrap();
        assert_eq!(again.id, rec.id);
        // ready → in_use → retained。
        let r = mark_state(&store, "pa-w", "in_use", None).unwrap();
        assert_eq!(r.state, "in_use");
        // 在工作区写入后 finalize：after digest 记录且 ≠ before。
        std::fs::write(std::path::Path::new(&rec.path).join("out.txt"), "x\n").unwrap();
        let r = finalize(&store, "pa-w", "succeeded").unwrap();
        assert_eq!(r.state, "retained");
        assert_ne!(r.workspace_digest_after, r.workspace_digest_before);
        // retained → cleaned。
        let r = mark_state(&store, "pa-w", "cleaned", None).unwrap();
        assert_eq!(r.state, "cleaned");
        // 非法迁移拒绝。
        assert!(mark_state(&store, "pa-w", "in_use", None).is_err());
    }

    #[test]
    fn readonly_attempt_returns_none() {
        let store = setup();
        seed_attempt(&store, "t-read", "analysis", "read", "pa-r");
        assert!(prepare(&store, "pa-r").unwrap().is_none());
    }
}
