//! 放行决策 → 事件镜像（ADR-031 C2 写路径）：
//! SQLite 放行成功后，把 ReleaseDecided 事件写入项目仓库的 `.ratiflow/` 事件域。
//!
//! 模式约定（C5 / AC-1）：
//! - 工作项无 `local_root`（纯本地无仓库）→ 返回 `Ok(None)` 跳过，行为与现状等价；
//! - 有 `local_root` 而写事件失败 → 报错（禁止只写投影不写事件）。
//!
//! v1 信任级别取 `Local`（本机低信任放行）；repository-backed 的协作者/门禁级
//! 证明链属后续里程碑（见 ADR-031 实施状态）。

use sg_eventlog::store::EventStore;
use sg_eventlog::{release_digest, Decision, Envelope, Payload, TrustLevel};

use crate::Store;

fn local_root_of(store: &Store, workitem_id: &str) -> Result<Option<String>, crate::Error> {
    Ok(store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT COALESCE(p.local_root,'') FROM projects p
                 JOIN workitems w ON w.project_id = p.id WHERE w.id=?1",
                [workitem_id],
                |r| r.get::<_, String>(0),
            )
            .map_err(sg_store::Error::from)
        })
        .ok()
        .filter(|s| !s.is_empty()))
}

/// 事件链头：无孩子（叶子）的 eventId 中取字典序最大者；空链 → ROOT_PARENT。
/// 父链已由 DagView 校验（fail-closed），此处只做确定性挑选。
fn head_event_id(es: &EventStore, workitem_id: &str) -> Result<String, crate::Error> {
    let view = sg_eventlog::store::load_dag(es, workitem_id)
        .map_err(|e| crate::Error::Message(e.to_string()))?;
    let is_parent: std::collections::BTreeSet<&str> =
        view.children.keys().map(String::as_str).collect();
    Ok(view
        .events
        .keys()
        .rfind(|id| !is_parent.contains(id.as_str()))
        .cloned()
        .unwrap_or_else(|| sg_eventlog::ROOT_PARENT.to_string()))
}

/// 镜像一次放行决定。返回事件 id；无 local_root 返回 `None`（AC-1 等价模式）。
/// `decision` 取值与 `release::decide_release` 一致：approved|rejected|changes_requested。
pub fn mirror_release_decided(
    store: &Store,
    workitem_id: &str,
    gate: &str,
    attempt_id: &str,
    digest: &str,
    decision: &str,
    decided_by: &str,
) -> Result<Option<String>, crate::Error> {
    // gate 已编码进 digest 六分量；参数保留以约束调用方显式声明关卡。
    let _ = gate;
    let Some(local_root) = local_root_of(store, workitem_id)? else {
        return Ok(None);
    };
    let mapped = match decision {
        "approved" => Decision::Approved,
        _ => Decision::Rejected,
    };
    let es = EventStore::open(std::path::Path::new(&local_root).join(".ratiflow"));
    let head = head_event_id(&es, workitem_id)?;
    let env = Envelope::new(
        workitem_id,
        attempt_id,
        &head,
        Payload::ReleaseDecided {
            digest: digest.to_string(),
            decision: mapped,
            reviewer: decided_by.to_string(),
            trust: TrustLevel::Local,
            proof: None,
        },
        concat!(env!("CARGO_PKG_NAME"), "@", env!("CARGO_PKG_VERSION")),
    );
    es.write_event(&env)
        .map_err(|e| crate::Error::Message(format!("eventlog_write_failed: {e}")))?;
    Ok(Some(env.event_id))
}

/// 放行 digest 统一走 eventlog 的六分量定义（与 release.rs 等价，C1：不另造事实）。
pub fn digest_of(
    workitem_id: &str,
    gate: &str,
    attempt_id: &str,
    entry_snapshot: &str,
    manifest_sha: &str,
    policy_version: &str,
) -> String {
    release_digest(
        workitem_id,
        gate,
        attempt_id,
        entry_snapshot,
        manifest_sha,
        policy_version,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sg_eventlog::store::load_dag;
    use sg_store::{ids, Store};

    struct TempRoot {
        path: std::path::PathBuf,
    }

    impl TempRoot {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "sg-rel-ev-{}-{}",
                std::process::id(),
                ids::new_id("t")
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn setup_with_local_root(local_root: Option<&TempRoot>) -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-rel-ev-db-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        let lr = local_root
            .map(|t| t.path.to_string_lossy().to_string())
            .unwrap_or_default();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, local_root, created_at)
                     VALUES ('pj','u','n','p','main',?1,?2)",
                    rusqlite::params![lr, sg_store::timefmt::now()],
                )
                .map_err(sg_store::Error::from)?;
                Ok(())
            })
            .unwrap();
        crate::create(&store, "pj", "t", "", None, &[]).unwrap();
        store
    }

    fn any_workitem_id(store: &Store) -> String {
        store
            .with_conn(|c| {
                c.query_row("SELECT id FROM workitems LIMIT 1", [], |r| {
                    r.get::<_, String>(0)
                })
                .map_err(sg_store::Error::from)
            })
            .unwrap()
    }

    #[test]
    fn skipped_when_no_local_root() {
        let store = setup_with_local_root(None);
        let wi = any_workitem_id(&store);
        let r = mirror_release_decided(&store, &wi, "requirements", "at1", "d", "approved", "a@x")
            .unwrap();
        assert!(r.is_none(), "AC-1：无仓库工作项行为与现状等价");
    }

    #[test]
    fn mirrored_event_lands_in_repo_and_chain_advances() {
        let root = TempRoot::new();
        let store = setup_with_local_root(Some(&root));
        let wi = any_workitem_id(&store);
        let e1 =
            mirror_release_decided(&store, &wi, "requirements", "at1", "d1", "approved", "a@x")
                .unwrap()
                .expect("有 local_root 必须落事件");
        let e2 =
            mirror_release_decided(&store, &wi, "requirements", "at1", "d1", "rejected", "a@x")
                .unwrap()
                .expect("第二次决定");
        assert_ne!(e1, e2);
        let es = EventStore::open(root.path.join(".ratiflow"));
        let view = load_dag(&es, &wi).unwrap();
        assert_eq!(view.causal_order().len(), 2, "两次决定构成链");
        let facts = sg_eventlog::reducer::fact_projection(&view);
        let sg_eventlog::reducer::Projection::Facts(f) = facts else {
            panic!()
        };
        assert_eq!(f.release_claims.len(), 2);
        assert!(f
            .release_claims
            .iter()
            .all(|c| c.status == "release_decided_claimed"));
        assert!(!f.attempts.contains_key("at1"), "决定事件不虚构 attempt");
    }

    #[test]
    fn corrupt_dag_fails_closed() {
        let root = TempRoot::new();
        let store = setup_with_local_root(Some(&root));
        let wi = any_workitem_id(&store);
        // 手工写入坏事件（未知 parent）→ 镜像必须报错而不是静默续链。
        let bad = format!(
            "{{\"event_id\":\"ZZZZZZZZZZZZZZZZZZZZZZZZZZ\",\"workitem_id\":\"{wi}\",\"attempt_id\":\"atX\",\"kind\":\"attempt_started\",\"schema_version\":1,\"parent_head\":\"GHOST\",\"idempotency_key\":\"k\",\"created_at\":\"t\",\"producer\":\"t\",\"payload\":{{\"type\":\"attempt_started\",\"gate\":\"requirements\"}}}}"
        );
        let dir = root.path.join(".ratiflow/events").join(&wi);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ZZZZZZZZZZZZZZZZZZZZZZZZZZ.json"), bad).unwrap();
        let r = mirror_release_decided(&store, &wi, "requirements", "at1", "d", "approved", "a@x");
        assert!(r.is_err(), "DAG 损坏 → fail-closed（C2）");
    }
}
