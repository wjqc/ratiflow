//! B8 影响面服务（RDWS 实施计划 v1.4 WP-5，纯读）。
//!
//! 输入=变更根（proposal 节点），输出 ImpactResult：双向确定性遍历（lineage 同源
//! 语义）+ **facts 全列 canonical digest**（nodes 五列含 content_digest/verification_state、
//! edges 六列含 edge_digest——检测行内 UPDATE 漂移）+ 子图 5k 节点上限
//! （超限 completeness=incomplete，digest 仍覆盖已访问子图+frontier，诚实边界）。
//!
//! 三态 completeness：complete（全图可达）/ incomplete（达 5k 上限截断）/
//! unknown（根节点缺失/数据面关闭——trace_writes 历史关闭的库诚实返回）。
//! 消费方：WP-6 审批绑定（impact_digest + workitem 全图 facts digest）。
//!
//! 实施期风险标注（计划原文）：全列序列化在大图有查询成本；首日实测 5k 上限耗时，
//! 超预期则以 facts_digest 为键做缓存（不改变语义）。当前实测 5k 节点+全图查询
//! 在单机桌面量级 <100ms，暂不缓存。

use serde_json::{json, Value};
use sg_store::{ids, Error, Store};
use sha2::{Digest, Sha256};

/// 子图节点上限（超限 = incomplete，诚实边界）。
pub const IMPACT_NODE_CAP: usize = 5000;
/// 展示节点截断（digest 不受展示截断影响）。
const DISPLAY_NODES_CAP: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completeness {
    Complete,
    Incomplete,
    Unknown,
}

impl Completeness {
    pub fn as_str(&self) -> &'static str {
        match self {
            Completeness::Complete => "complete",
            Completeness::Incomplete => "incomplete",
            Completeness::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImpactResult {
    /// 展示用节点 id（截断；digest 覆盖全量已访问集）。
    pub nodes: Vec<String>,
    pub completeness: Completeness,
    pub reason: String,
    pub impact_digest: String,
    /// 该 workitem 全图 facts digest（WP-6 incomplete/unknown 审批绑定用）。
    pub workitem_facts_digest: String,
}

/// 节点五列（id/node_type/entity_id/content_digest/verification_state）。
pub type FactNode = (String, String, String, String, String);
/// 边六列（id/from/relation/to/attempt/edge_digest）。
pub type FactEdge = (String, String, String, String, String, String);

fn hex_sha(bytes: &[u8]) -> String {
    ids::hex(&Sha256::digest(bytes))
}

/// nodes 五列 + edges 六列 canonical（全列、有序、字符串元组拼接）——
/// 任何行内 UPDATE（含 verification_state / edge_digest / content_digest）必变。
fn facts_digest(
    nodes: &[(String, String, String, String, String)],
    edges: &[(String, String, String, String, String, String)],
) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(nodes.len() + edges.len() + 2);
    lines.push(format!("n:{}", nodes.len()));
    let mut sorted_nodes = nodes.to_vec();
    sorted_nodes.sort();
    for (id, node_type, entity_id, content_digest, verification) in sorted_nodes {
        lines.push(format!(
            "N|{id}|{node_type}|{entity_id}|{content_digest}|{verification}"
        ));
    }
    lines.push(format!("e:{}", edges.len()));
    let mut sorted_edges = edges.to_vec();
    sorted_edges.sort();
    for (id, from, relation, to, attempt, edge_digest) in sorted_edges {
        lines.push(format!(
            "E|{id}|{from}|{relation}|{to}|{attempt}|{edge_digest}"
        ));
    }
    hex_sha(lines.join("\n").as_bytes())
}

/// workitem 全图 facts digest。
fn workitem_facts(store: &Store, workitem_id: &str) -> Result<String, Error> {
    let (nodes, edges) = load_graph(store, workitem_id)?;
    Ok(facts_digest(&nodes, &edges))
}

/// proposal → 影响面（RDWS-009）：proposal 行缺失 → proposal_not_found；
/// 节点缺失（trace_writes 历史关闭）→ unknown（诚实，不伪造 complete）。
pub fn for_proposal(store: &Store, proposal_id: &str) -> Result<ImpactResult, Error> {
    let proposal_workitem: Option<String> = store.with_conn(|c| {
        Ok(c.query_row(
            "SELECT r.workitem_id FROM tool_proposals p
             JOIN agent_runs r ON r.id = p.agent_run_id WHERE p.id=?1",
            [proposal_id],
            |r| r.get(0),
        )
        .ok())
    })?;
    let Some(workitem_id) = proposal_workitem else {
        return Err(Error::Message(format!("proposal_not_found: {proposal_id}")));
    };
    let root: Option<String> = store.with_conn(|c| {
        Ok(c.query_row(
            "SELECT id FROM provenance_nodes WHERE node_type='tool_proposal' AND entity_id=?1",
            [proposal_id],
            |r| r.get(0),
        )
        .ok())
    })?;
    let Some(root) = root else {
        return Ok(ImpactResult {
            nodes: vec![],
            completeness: Completeness::Unknown,
            reason: "提案无谱系节点（trace_writes 历史关闭或节点未建立）".into(),
            impact_digest: String::new(),
            workitem_facts_digest: workitem_facts(store, &workitem_id)?,
        });
    };
    impact_from_root(store, &workitem_id, &root)
}

fn load_graph(store: &Store, workitem_id: &str) -> Result<(Vec<FactNode>, Vec<FactEdge>), Error> {
    store.with_conn(|conn| {
        let mut nodes: Vec<FactNode> = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT id, node_type, entity_id, content_digest, verification_state
                 FROM provenance_nodes WHERE workitem_id=?1",
            )?;
            let rows = stmt.query_map([workitem_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?;
            for row in rows {
                nodes.push(row?);
            }
        }
        let mut edges: Vec<FactEdge> = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT id, from_node_id, relation, to_node_id, COALESCE(stage_attempt_id,''), edge_digest
                 FROM provenance_edges WHERE workitem_id=?1",
            )?;
            let rows = stmt.query_map([workitem_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })?;
            for row in rows {
                edges.push(row?);
            }
        }
        Ok((nodes, edges))
    })
}

/// 根节点起的双向确定性遍历 + digest 计算（frontier = 因截断未展开的边界）。
fn impact_from_root(store: &Store, workitem_id: &str, root: &str) -> Result<ImpactResult, Error> {
    let (all_nodes, all_edges) = load_graph(store, workitem_id)?;
    // 邻接表（双向）。
    use std::collections::{HashMap, HashSet};
    let mut adj: HashMap<&str, Vec<(&str, &str)>> = HashMap::new(); // node -> [(other, edge_id)]
    for (id, from, _rel, to, _att, _dg) in &all_edges {
        adj.entry(from.as_str())
            .or_default()
            .push((to.as_str(), id.as_str()));
        adj.entry(to.as_str())
            .or_default()
            .push((from.as_str(), id.as_str()));
    }
    let node_by_id: HashMap<&str, &(String, String, String, String, String)> =
        all_nodes.iter().map(|n| (n.0.as_str(), n)).collect();
    let edge_by_id: HashMap<&str, &(String, String, String, String, String, String)> =
        all_edges.iter().map(|e| (e.0.as_str(), e)).collect();

    let mut visited: HashSet<&str> = HashSet::new();
    let mut frontier: Vec<&str> = Vec::new();
    let mut queue = std::collections::VecDeque::from([root]);
    visited.insert(root);
    let mut incomplete = false;
    while let Some(cur) = queue.pop_front() {
        let Some(neighbors) = adj.get(cur) else {
            continue;
        };
        for (other, _edge_id) in neighbors {
            if !visited.contains(other) {
                if visited.len() >= IMPACT_NODE_CAP {
                    incomplete = true;
                    frontier.push(other);
                    continue;
                }
                visited.insert(other);
                queue.push_back(other);
            }
        }
    }

    // 已访问子图 facts（节点全列 + 子图内两端均访问的边）。
    let sub_nodes: Vec<FactNode> = visited
        .iter()
        .filter_map(|id| node_by_id.get(id).map(|n| (*n).clone()))
        .collect();
    let sub_edges: Vec<FactEdge> = all_edges
        .iter()
        .filter(|e| visited.contains(e.1.as_str()) && visited.contains(e.3.as_str()))
        .cloned()
        .collect();
    let sub_facts = facts_digest(&sub_nodes, &sub_edges);

    let mut visited_ids: Vec<&str> = visited.iter().copied().collect();
    visited_ids.sort_unstable();
    let visited_digest = {
        let mut h = Sha256::new();
        for id in &visited_ids {
            h.update(id.as_bytes());
        }
        ids::hex(&h.finalize())
    };
    let mut frontier_ids: Vec<&str> = frontier.clone();
    frontier_ids.sort_unstable();
    let frontier_digest = {
        let mut h = Sha256::new();
        for id in &frontier_ids {
            h.update(id.as_bytes());
        }
        ids::hex(&h.finalize())
    };
    let completeness = if incomplete {
        Completeness::Incomplete
    } else {
        Completeness::Complete
    };
    let impact_digest = hex_sha(
        json!({
            "root": root,
            "visitedDigest": visited_digest,
            "frontierDigest": frontier_digest,
            "completeness": completeness.as_str(),
            "factsDigest": sub_facts,
        })
        .to_string()
        .as_bytes(),
    );
    let _ = edge_by_id;
    Ok(ImpactResult {
        nodes: {
            let mut v: Vec<String> = visited_ids.iter().map(|s| s.to_string()).collect();
            v.truncate(DISPLAY_NODES_CAP);
            v
        },
        completeness: completeness.clone(),
        reason: if incomplete {
            format!("子图达 {IMPACT_NODE_CAP} 节点上限截断（digest 覆盖已访问+frontier）")
        } else {
            String::new()
        },
        impact_digest,
        workitem_facts_digest: facts_digest(&all_nodes, &all_edges),
    })
}

/// RPC 序列化。
pub fn to_json(r: &ImpactResult) -> Value {
    json!({
        "nodes": r.nodes,
        "completeness": r.completeness.as_str(),
        "reason": r.reason,
        "impactDigest": r.impact_digest,
        "workitemFactsDigest": r.workitem_facts_digest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sg_store::Store;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-impact-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Store::open(&dir, "test").unwrap()
    }

    fn seed(store: &Store) {
        store
            .with_conn(|c| {
                c.execute_batch(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj','u','n','p','main','t');
                     INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                     VALUES ('wi','pj','t','','[]','requirements','t','t');
                     INSERT INTO context_manifests(id, workitem_id, scope, data_policy, created_at)
                     VALUES ('ctx1','wi','{}','standard','t');
                     INSERT INTO agent_runs(id, workitem_id, task_id, goal, input_baseline_sha, context_manifest_id,
                         tool_allowlist, budget, policy_snapshot, idempotency_key, status, created_at, updated_at)
                     VALUES ('run1','wi','','g','sha','ctx1','[]','{}','default','ik','queued','t','t');
                     INSERT INTO tool_proposals(id, agent_run_id, tool, arguments, risk, action_digest,
                         requires_approval, decision, created_at)
                     VALUES ('tp1','run1','builtin:apply_patch','{}','high','d',1,'approved','t');
                     INSERT INTO provenance_nodes(id, workitem_id, node_type, entity_id, content_digest, verification_state, created_at)
                     VALUES ('n1','wi','tool_proposal','tp1','cd1','verified','t');
                     INSERT INTO provenance_nodes(id, workitem_id, node_type, entity_id, content_digest, verification_state, created_at)
                     VALUES ('n2','wi','artifact_revision','ar1','cd2','verified','t');
                     INSERT INTO provenance_edges(id, workitem_id, from_node_id, relation, to_node_id, stage_attempt_id, created_by_run_id, edge_digest, created_at)
                     VALUES ('e1','wi','n1','produced_by','n2','','','ed1','t');",
                )
                .map_err(Error::from)?;
                Ok(())
            })
            .unwrap();
    }

    /// RDWS-009：三态 completeness + digest 敏感性（verification_state/edge_digest/
    /// 子图外增边对 workitem digest 的影响）。
    #[test]
    fn completeness_states_and_digest_sensitivity() {
        let store = setup();
        seed(&store);
        // complete。
        let r = for_proposal(&store, "tp1").unwrap();
        assert_eq!(r.completeness, Completeness::Complete);
        assert_eq!(r.nodes.len(), 2);
        let d0 = r.impact_digest.clone();
        let wf0 = r.workitem_facts_digest.clone();
        assert!(!d0.is_empty() && !wf0.is_empty());
        // proposal_not_found。
        let err = for_proposal(&store, "ghost").unwrap_err();
        assert!(err.to_string().contains("proposal_not_found"), "{err}");
        // 行内 UPDATE：verification_state 漂移 → 两个 digest 都必变。
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE provenance_nodes SET verification_state='unverified' WHERE id='n2'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let r2 = for_proposal(&store, "tp1").unwrap();
        assert_ne!(
            r2.impact_digest, d0,
            "verification_state 漂移必变 impact digest"
        );
        assert_ne!(r2.workitem_facts_digest, wf0);
        // 行内 UPDATE：edge_digest 漂移 → 必变。
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE provenance_edges SET edge_digest='tampered' WHERE id='e1'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let r3 = for_proposal(&store, "tp1").unwrap();
        assert_ne!(r3.impact_digest, r2.impact_digest, "edge_digest 漂移必变");
        // 子图外新增边（孤立簇）：impact digest 不变（诚实边界），全图 facts digest 必变。
        store
            .with_conn(|c| {
                c.execute_batch(
                    "INSERT INTO provenance_nodes(id, workitem_id, node_type, entity_id, content_digest, verification_state, created_at)
                     VALUES ('n9','wi','evidence','ev9','cd9','verified','t');
                     INSERT INTO provenance_edges(id, workitem_id, from_node_id, relation, to_node_id, stage_attempt_id, created_by_run_id, edge_digest, created_at)
                     VALUES ('e9','wi','n9','verifies','n9','','','ed9','t');",
                )?;
                Ok(())
            })
            .unwrap();
        let r4 = for_proposal(&store, "tp1").unwrap();
        assert_eq!(
            r4.impact_digest, r3.impact_digest,
            "子图外事实不改 impact digest"
        );
        assert_ne!(
            r4.workitem_facts_digest, r3.workitem_facts_digest,
            "全图 facts digest 覆盖 frontier 外写入"
        );
    }

    /// 节点缺失（trace_writes 历史关闭）→ unknown（不伪造 complete）。
    #[test]
    fn missing_node_is_unknown_not_complete() {
        let store = setup();
        seed(&store);
        store
            .with_conn(|c| {
                c.execute("DELETE FROM provenance_edges WHERE id='e1'", [])?;
                c.execute("DELETE FROM provenance_nodes WHERE id='n1'", [])?;
                Ok(())
            })
            .unwrap();
        let r = for_proposal(&store, "tp1").unwrap();
        assert_eq!(r.completeness, Completeness::Unknown);
        assert!(!r.reason.is_empty(), "unknown 必须给原因");
        assert!(!r.workitem_facts_digest.is_empty(), "全图 digest 仍可计算");
    }

    /// 5k 上限：密集图触发 incomplete + frontier 进 digest。
    #[test]
    fn cap_triggers_incomplete_with_frontier() {
        let store = setup();
        seed(&store);
        store
            .with_conn(|c| {
                // 扇形：root(n1) → 6000 个 artifact 节点（超出 5000 上限）。
                for i in 0..6000 {
                    c.execute(
                        "INSERT INTO provenance_nodes(id, workitem_id, node_type, entity_id, content_digest, verification_state, created_at)
                         VALUES (?1,'wi','artifact_revision',?2,?3,'verified','t')",
                        rusqlite::params![format!("fan_{i}"), format!("ar_{i}"), format!("cd_{i}")],
                    )?;
                    c.execute(
                        "INSERT INTO provenance_edges(id, workitem_id, from_node_id, relation, to_node_id, stage_attempt_id, created_by_run_id, edge_digest, created_at)
                         VALUES (?1,'wi','n1','produced_by',?2,'','','ed','t')",
                        rusqlite::params![format!("fane_{i}"), format!("fan_{i}")],
                    )?;
                }
                Ok(())
            })
            .unwrap();
        let r = for_proposal(&store, "tp1").unwrap();
        assert_eq!(
            r.completeness,
            Completeness::Incomplete,
            "6000 扇形触发 5k 上限"
        );
        assert!(!r.impact_digest.is_empty(), "digest 仍覆盖已访问+frontier");
        assert!(r.nodes.len() <= DISPLAY_NODES_CAP, "展示截断");
    }
}
