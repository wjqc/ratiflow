//! 不可变谱系（ADR-030 / 蓝图 §6 M1 谱系底座）。
//! 节点按 (node_type, entity_id) 幂等注册；边只追加，写入时做存在性校验与环检测，
//! digest 为确定性拼接哈希（可对账防篡改）。lineage/coverage/gaps 为只读查询。
//! 本 crate 不依赖具体领域 crate（蓝图 §3.1，防循环依赖）：领域侧经
//! `node_type`/`entity_id` 字符串引用实体。

use serde::Serialize;
use sg_store::{ids, outbox, timefmt, Error, Store};
use sha2::{Digest, Sha256};

/// 稳定错误码前缀（store_err 按子串映射到 ErrorCode）。
pub const TRACE_INCOMPLETE: &str = "trace_incomplete";
pub const TRACE_CYCLE: &str = "trace_cycle";

/// 默认谱系遍历深度上限。
pub const MAX_LINEAGE_DEPTH: usize = 10;

pub mod node_type {
    pub const REQUIREMENT_REVISION: &str = "requirement_revision";
    pub const REQUIREMENT_ITEM: &str = "requirement_item";
    pub const STAGE_ATTEMPT: &str = "stage_attempt";
    pub const ACTIVITY: &str = "activity";
    pub const AGENT_RUN: &str = "agent_run";
    pub const TOOL_PROPOSAL: &str = "tool_proposal";
    pub const ARTIFACT_REVISION: &str = "artifact_revision";
    pub const COMMIT: &str = "commit";
    pub const MR: &str = "mr";
    pub const PIPELINE: &str = "pipeline";
    pub const TEST_REPORT: &str = "test_report";
    pub const EVIDENCE: &str = "evidence";
    pub const DEPLOYMENT: &str = "deployment";
    pub const APPROVAL: &str = "approval";
    pub const SNAPSHOT: &str = "snapshot";
    pub const PASSPORT: &str = "passport";

    pub const ALL: [&str; 16] = [
        REQUIREMENT_REVISION,
        REQUIREMENT_ITEM,
        STAGE_ATTEMPT,
        ACTIVITY,
        AGENT_RUN,
        TOOL_PROPOSAL,
        ARTIFACT_REVISION,
        COMMIT,
        MR,
        PIPELINE,
        TEST_REPORT,
        EVIDENCE,
        DEPLOYMENT,
        APPROVAL,
        SNAPSHOT,
        PASSPORT,
    ];
}

pub mod relation {
    pub const DERIVED_FROM: &str = "derived_from";
    pub const SATISFIES: &str = "satisfies";
    pub const IMPLEMENTS: &str = "implements";
    pub const VERIFIES: &str = "verifies";
    pub const PRODUCED_BY: &str = "produced_by";
    pub const USES: &str = "uses";
    pub const APPROVES: &str = "approves";
    pub const SUPERSEDES: &str = "supersedes";
    pub const RESTORES: &str = "restores";
    pub const WAIVES: &str = "waives";
    pub const PART_OF: &str = "part_of";
    pub const EXECUTES: &str = "executes";
    pub const VALIDATES: &str = "validates";
}

#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub id: String,
    pub project_id: String,
    pub workitem_id: String,
    pub node_type: String,
    pub entity_id: String,
    pub content_digest: String,
    pub verification_state: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct NodeInput<'a> {
    pub project_id: &'a str,
    pub workitem_id: &'a str,
    pub node_type: &'a str,
    pub entity_id: &'a str,
    pub content_digest: &'a str,
    pub verification_state: &'a str,
}

/// 幂等注册节点：同 (node_type, entity_id) 已存在则原样返回（注册不更新既有事实）。
pub fn register_node(store: &Store, input: &NodeInput<'_>) -> Result<Node, Error> {
    if !node_type::ALL.contains(&input.node_type) {
        return Err(Error::Message(format!(
            "trace_incomplete: 未知节点类型 {}",
            input.node_type
        )));
    }
    if input.entity_id.is_empty() || input.workitem_id.is_empty() {
        return Err(Error::Message(format!(
            "{TRACE_INCOMPLETE}: 节点注册缺少 entity/workitem"
        )));
    }
    if let Some(node) = find_node(store, input.node_type, input.entity_id)? {
        return Ok(node);
    }
    let id = ids::new_id("pn");
    let verification = if input.verification_state.is_empty() {
        "verified"
    } else {
        input.verification_state
    };
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO provenance_nodes(id, project_id, workitem_id, node_type, entity_id, content_digest, verification_state, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![
                id,
                input.project_id,
                input.workitem_id,
                input.node_type,
                input.entity_id,
                input.content_digest,
                verification,
                timefmt::now()
            ],
        )?;
        Ok(())
    })?;
    find_node(store, input.node_type, input.entity_id)?
        .ok_or_else(|| Error::Message("trace_incomplete: 节点注册后不可见".into()))
}

#[derive(Debug, Clone, Serialize)]
pub struct Edge {
    pub id: String,
    pub workitem_id: String,
    pub from_node_id: String,
    pub relation: String,
    pub to_node_id: String,
    pub stage_attempt_id: String,
    pub created_by_run_id: String,
    pub edge_digest: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct EdgeInput<'a> {
    pub workitem_id: &'a str,
    /// from/to 以 (node_type, entity_id) 表达；任一未注册即 trace_incomplete（fail-closed）。
    pub from_node_type: &'a str,
    pub from_entity_id: &'a str,
    pub relation: &'a str,
    pub to_node_type: &'a str,
    pub to_entity_id: &'a str,
    pub stage_attempt_id: &'a str,
    pub created_by_run_id: &'a str,
}

/// 写入不可变边：节点必须已注册，否则 trace_incomplete；成环或自环拒绝；
/// 同 (from, relation, to, attempt) 幂等返回既有边。
pub fn add_edge(store: &Store, input: &EdgeInput<'_>) -> Result<Edge, Error> {
    let from = find_node(store, input.from_node_type, input.from_entity_id)?
        .ok_or_else(|| {
            Error::Message(format!(
                "{TRACE_INCOMPLETE}: 缺少父边节点 {}:{}",
                input.from_node_type, input.from_entity_id
            ))
        })?
        .id;
    let to = find_node(store, input.to_node_type, input.to_entity_id)?
        .ok_or_else(|| {
            Error::Message(format!(
                "{TRACE_INCOMPLETE}: 缺少父边节点 {}:{}",
                input.to_node_type, input.to_entity_id
            ))
        })?
        .id;
    add_edge_by_ids(
        store,
        input.workitem_id,
        &from,
        input.relation,
        &to,
        input.stage_attempt_id,
        input.created_by_run_id,
    )
}

/// digest = sha256(from|relation|to|attempt|run)，确定性拼接。
fn edge_digest(from: &str, relation: &str, to: &str, attempt: &str, run: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(from.as_bytes());
    hasher.update(b"|");
    hasher.update(relation.as_bytes());
    hasher.update(b"|");
    hasher.update(to.as_bytes());
    hasher.update(b"|");
    hasher.update(attempt.as_bytes());
    hasher.update(b"|");
    hasher.update(run.as_bytes());
    ids::hex(&hasher.finalize())
}

pub fn add_edge_by_ids(
    store: &Store,
    workitem_id: &str,
    from: &str,
    relation: &str,
    to: &str,
    stage_attempt_id: &str,
    created_by_run_id: &str,
) -> Result<Edge, Error> {
    if from == to {
        return Err(Error::Message(format!("{TRACE_CYCLE}: 自环 {from}")));
    }
    // 成环检测：to 已可达 from 时，再连 from→to 即闭环。
    if reaches(store, to, from, MAX_LINEAGE_DEPTH)? {
        return Err(Error::Message(format!("{TRACE_CYCLE}: {to} 已可达 {from}")));
    }
    let digest = edge_digest(from, relation, to, stage_attempt_id, created_by_run_id);
    if let Some(existing) = find_edge(store, from, relation, to, stage_attempt_id)? {
        return Ok(existing);
    }
    let id = ids::new_id("pe");
    let created_at = store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO provenance_edges(id, workitem_id, from_node_id, relation, to_node_id, stage_attempt_id, created_by_run_id, edge_digest, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![
                id,
                workitem_id,
                from,
                relation,
                to,
                stage_attempt_id,
                created_by_run_id,
                digest,
                timefmt::now()
            ],
        )?;
        let created: String =
            conn.query_row("SELECT created_at FROM provenance_edges WHERE id=?1", [&id], |r| {
                r.get(0)
            })?;
        Ok(created)
    })?;
    outbox::emit(
        store,
        "trace",
        workitem_id,
        "trace.edge_created",
        serde_json::json!({
            "workitemId": workitem_id,
            "edgeId": id,
            "fromNodeId": from,
            "relation": relation,
            "toNodeId": to,
        }),
    )?;
    Ok(Edge {
        id,
        workitem_id: workitem_id.into(),
        from_node_id: from.into(),
        relation: relation.into(),
        to_node_id: to.into(),
        stage_attempt_id: stage_attempt_id.into(),
        created_by_run_id: created_by_run_id.into(),
        edge_digest: digest,
        created_at,
    })
}

/// 按实体定位节点 id；未注册返回 None（调用方决定是否 fail-closed）。
pub fn node_id_by_entity(
    store: &Store,
    node_type: &str,
    entity_id: &str,
) -> Result<Option<String>, Error> {
    Ok(find_node(store, node_type, entity_id)?.map(|n| n.id))
}

pub fn find_node(store: &Store, node_type: &str, entity_id: &str) -> Result<Option<Node>, Error> {
    store.with_conn(|conn| {
        let node = conn
            .query_row(
                "SELECT id, project_id, workitem_id, node_type, entity_id, content_digest, verification_state, created_at
                 FROM provenance_nodes WHERE node_type=?1 AND entity_id=?2",
                [node_type, entity_id],
                |r| {
                    Ok(Node {
                        id: r.get(0)?,
                        project_id: r.get(1)?,
                        workitem_id: r.get(2)?,
                        node_type: r.get(3)?,
                        entity_id: r.get(4)?,
                        content_digest: r.get(5)?,
                        verification_state: r.get(6)?,
                        created_at: r.get(7)?,
                    })
                },
            )
            .ok();
        Ok(node)
    })
}

/// verification_state 投影更新（unverified → verified / waived；事实行不动）。
pub fn set_verification(
    store: &Store,
    node_type: &str,
    entity_id: &str,
    state: &str,
) -> Result<(), Error> {
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE provenance_nodes SET verification_state=?1 WHERE node_type=?2 AND entity_id=?3",
            rusqlite::params![state, node_type, entity_id],
        )?;
        Ok(())
    })
}

pub fn find_edge(
    store: &Store,
    from: &str,
    relation: &str,
    to: &str,
    stage_attempt_id: &str,
) -> Result<Option<Edge>, Error> {
    store.with_conn(|conn| {
        let edge = conn
            .query_row(
                "SELECT id, workitem_id, from_node_id, relation, to_node_id, stage_attempt_id, created_by_run_id, edge_digest, created_at
                 FROM provenance_edges
                 WHERE from_node_id=?1 AND relation=?2 AND to_node_id=?3 AND stage_attempt_id=?4",
                [from, relation, to, stage_attempt_id],
                |r| {
                    Ok(Edge {
                        id: r.get(0)?,
                        workitem_id: r.get(1)?,
                        from_node_id: r.get(2)?,
                        relation: r.get(3)?,
                        to_node_id: r.get(4)?,
                        stage_attempt_id: r.get(5)?,
                        created_by_run_id: r.get(6)?,
                        edge_digest: r.get(7)?,
                        created_at: r.get(8)?,
                    })
                },
            )
            .ok();
        Ok(edge)
    })
}

/// BFS：start 能否在 max_depth 跳内沿出边到达 target。
fn reaches(store: &Store, start: &str, target: &str, max_depth: usize) -> Result<bool, Error> {
    if start == target {
        return Ok(true);
    }
    let mut visited = std::collections::HashSet::new();
    let mut frontier = vec![start.to_string()];
    for _ in 0..max_depth {
        if frontier.is_empty() {
            return Ok(false);
        }
        let mut next = Vec::new();
        for node in &frontier {
            for (out, _) in adjacent(store, node, "down")? {
                if out == target {
                    return Ok(true);
                }
                if visited.insert(out.clone()) {
                    next.push(out);
                }
            }
        }
        frontier = next;
    }
    Ok(false)
}

/// 相邻边：direction=up 取入边（node 为 to，沿 from 继续），down 取出边。
fn adjacent(store: &Store, node: &str, direction: &str) -> Result<Vec<(String, Edge)>, Error> {
    let (sql, key) = if direction == "up" {
        (
            "SELECT id, workitem_id, from_node_id, relation, to_node_id, stage_attempt_id, created_by_run_id, edge_digest, created_at
             FROM provenance_edges WHERE to_node_id=?1",
            node,
        )
    } else {
        (
            "SELECT id, workitem_id, from_node_id, relation, to_node_id, stage_attempt_id, created_by_run_id, edge_digest, created_at
             FROM provenance_edges WHERE from_node_id=?1",
            node,
        )
    };
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([key], |r| {
            Ok(Edge {
                id: r.get(0)?,
                workitem_id: r.get(1)?,
                from_node_id: r.get(2)?,
                relation: r.get(3)?,
                to_node_id: r.get(4)?,
                stage_attempt_id: r.get(5)?,
                created_by_run_id: r.get(6)?,
                edge_digest: r.get(7)?,
                created_at: r.get(8)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            let edge = row?;
            let other = if direction == "up" {
                edge.from_node_id.clone()
            } else {
                edge.to_node_id.clone()
            };
            out.push((other, edge));
        }
        Ok(out)
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct LineageResult {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub truncated: bool,
}

/// lineage：以 node 为原点的有限深度谱系遍历（up=反向来源，down=正向影响，both=双方向）。
pub fn lineage(
    store: &Store,
    node_id: &str,
    direction: &str,
    max_depth: usize,
) -> Result<LineageResult, Error> {
    let depth = max_depth.clamp(1, MAX_LINEAGE_DEPTH);
    let mut nodes: Vec<Node> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen_nodes = std::collections::HashSet::new();
    let mut seen_edges = std::collections::HashSet::new();
    let root = store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT id, project_id, workitem_id, node_type, entity_id, content_digest, verification_state, created_at
                 FROM provenance_nodes WHERE id=?1",
                [node_id],
                |r| {
                    Ok(Node {
                        id: r.get(0)?,
                        project_id: r.get(1)?,
                        workitem_id: r.get(2)?,
                        node_type: r.get(3)?,
                        entity_id: r.get(4)?,
                        content_digest: r.get(5)?,
                        verification_state: r.get(6)?,
                        created_at: r.get(7)?,
                    })
                },
            )
            .map_err(|_| Error::Message(format!("trace_incomplete: 节点 {node_id} 不存在")))
        })?;
    seen_nodes.insert(root.id.clone());
    nodes.push(root);
    let directions: [&str; 2] = if direction == "both" {
        ["up", "down"]
    } else {
        [direction, direction]
    };
    let mut truncated = false;
    for dir in directions {
        let mut frontier = vec![node_id.to_string()];
        let mut visited = seen_nodes.clone();
        for _ in 0..depth {
            let mut next = Vec::new();
            for current in &frontier {
                for (other, edge) in adjacent(store, current, dir)? {
                    if seen_edges.insert(edge.id.clone()) {
                        edges.push(edge.clone());
                    }
                    if visited.insert(other.clone()) && seen_nodes.insert(other.clone()) {
                        if let Some(n) = get_node_by_id(store, &other)? {
                            next.push(n.id.clone());
                            nodes.push(n);
                        }
                    }
                }
            }
            if nodes.len() > 500 {
                truncated = true;
                break;
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }
    }
    Ok(LineageResult {
        nodes,
        edges,
        truncated,
    })
}

fn get_node_by_id(store: &Store, id: &str) -> Result<Option<Node>, Error> {
    store.with_conn(|conn| {
        let node = conn
            .query_row(
                "SELECT id, project_id, workitem_id, node_type, entity_id, content_digest, verification_state, created_at
                 FROM provenance_nodes WHERE id=?1",
                [id],
                |r| {
                    Ok(Node {
                        id: r.get(0)?,
                        project_id: r.get(1)?,
                        workitem_id: r.get(2)?,
                        node_type: r.get(3)?,
                        entity_id: r.get(4)?,
                        content_digest: r.get(5)?,
                        verification_state: r.get(6)?,
                        created_at: r.get(7)?,
                    })
                },
            )
            .ok();
        Ok(node)
    })
}

/// coverage：给定需求修订，逐条目统计 satisfies/implements/verifies 入边。
/// covered = 至少一条 satisfies/implements；verified = 至少一条 verifies。
pub fn coverage(
    store: &Store,
    workitem_id: &str,
    revision_id: &str,
) -> Result<serde_json::Value, Error> {
    let items = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT i.id, i.requirement_key, i.title, i.status
             FROM requirement_items i
             JOIN requirement_revisions r ON r.id = i.revision_id
             JOIN requirement_documents d ON d.id = r.document_id
             WHERE r.id = ?1 AND d.workitem_id = ?2
             ORDER BY i.requirement_key",
        )?;
        let rows = stmt.query_map([revision_id, workitem_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;
    let mut items_json = Vec::new();
    let (mut covered_count, mut verified_count) = (0usize, 0usize);
    for (item_id, key, title, status) in items {
        let (mut satisfies, mut implements, mut verifies) = (0usize, 0usize, 0usize);
        let mut node_id: Option<String> = None;
        // requirement_item 节点 entity_id 即条目 id；未注册节点视为零覆盖。
        if let Some(node) = find_node(store, node_type::REQUIREMENT_ITEM, &item_id)? {
            node_id = Some(node.id.clone());
            for (_, edge) in adjacent(store, &node.id, "up")? {
                match edge.relation.as_str() {
                    "satisfies" => satisfies += 1,
                    "implements" => implements += 1,
                    "verifies" => verifies += 1,
                    _ => {}
                }
            }
        }
        let covered = satisfies + implements > 0;
        let verified = verifies > 0;
        if covered {
            covered_count += 1;
        }
        if verified {
            verified_count += 1;
        }
        items_json.push(serde_json::json!({
            "requirementKey": key,
            "title": title,
            "status": status,
            "itemId": item_id,
            "nodeId": node_id,
            "satisfies": satisfies,
            "implements": implements,
            "verifies": verifies,
        "covered": covered,
        "verified": verified,
        }));
    }
    let total = items_json.len();
    Ok(serde_json::json!({
        "workitemId": workitem_id,
        "revisionId": revision_id,
        "totalItems": total,
        "coveredCount": covered_count,
        "verifiedCount": verified_count,
        "items": items_json,
    }))
}

/// gaps：断链扫描（M1 只读）——
/// orphans：无任何边的节点；unverified：legacy 回填等未验证节点；
/// uncoveredItems：各文档最新修订中无 satisfies/implements 覆盖的条目。
pub fn gaps(store: &Store, workitem_id: &str) -> Result<serde_json::Value, Error> {
    const LIST_CAP: usize = 50;
    let orphans: Vec<serde_json::Value> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT n.id, n.node_type, n.entity_id, n.verification_state
             FROM provenance_nodes n
             WHERE n.workitem_id = ?1
               AND NOT EXISTS (SELECT 1 FROM provenance_edges e WHERE e.from_node_id = n.id)
               AND NOT EXISTS (SELECT 1 FROM provenance_edges e WHERE e.to_node_id = n.id)
             ORDER BY n.created_at",
        )?;
        let rows = stmt.query_map([workitem_id], |r| {
            Ok(serde_json::json!({
                "nodeId": r.get::<_, String>(0)?,
                "nodeType": r.get::<_, String>(1)?,
                "entityId": r.get::<_, String>(2)?,
                "verificationState": r.get::<_, String>(3)?,
            }))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;
    let unverified: Vec<serde_json::Value> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, node_type, entity_id FROM provenance_nodes
             WHERE workitem_id = ?1 AND verification_state = 'unverified'
             ORDER BY created_at",
        )?;
        let rows = stmt.query_map([workitem_id], |r| {
            Ok(serde_json::json!({
                "nodeId": r.get::<_, String>(0)?,
                "nodeType": r.get::<_, String>(1)?,
                "entityId": r.get::<_, String>(2)?,
            }))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;
    // 各文档最新修订的未覆盖条目。
    let latest_revisions: Vec<String> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT r.id FROM requirement_revisions r
             JOIN requirement_documents d ON d.id = r.document_id
             WHERE d.workitem_id = ?1
               AND r.revision_no = (SELECT MAX(r2.revision_no) FROM requirement_revisions r2
                                    WHERE r2.document_id = r.document_id)",
        )?;
        let rows = stmt.query_map([workitem_id], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;
    let mut uncovered: Vec<serde_json::Value> = Vec::new();
    for revision in &latest_revisions {
        let cov = coverage(store, workitem_id, revision)?;
        for item in cov["items"].as_array().cloned().unwrap_or_default() {
            if item["covered"].as_bool() == Some(false) && item["status"].as_str() == Some("active")
            {
                uncovered.push(item);
            }
        }
    }
    let truncated = |list: &[serde_json::Value]| list.len() > LIST_CAP;
    Ok(serde_json::json!({
        "workitemId": workitem_id,
        "orphanCount": orphans.len(),
        "orphans": &orphans[..orphans.len().min(LIST_CAP)],
        "orphansTruncated": truncated(&orphans),
        "unverifiedCount": unverified.len(),
        "unverified": &unverified[..unverified.len().min(LIST_CAP)],
        "unverifiedTruncated": truncated(&unverified),
        "uncoveredItemCount": uncovered.len(),
        "uncoveredItems": &uncovered[..uncovered.len().min(LIST_CAP)],
        "uncoveredTruncated": truncated(&uncovered),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Store {
        let dir =
            std::env::temp_dir().join(format!("sg-pn-{}-{}", std::process::id(), ids::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj','u','n','p','main',?1)",
                    [timefmt::now()],
                )?;
                c.execute(
                    "INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                     VALUES ('wi_1','pj','标题','','[]','requirements',?1,?1)",
                    [timefmt::now()],
                )?;
                Ok(())
            })
            .unwrap();
        store
    }

    fn node_in<'a>(store: &'a Store, entity: &'a str, node_type: &'a str) -> Result<Node, Error> {
        register_node(
            store,
            &NodeInput {
                project_id: "pj",
                workitem_id: "wi_1",
                node_type,
                entity_id: entity,
                content_digest: "sha-x",
                verification_state: "verified",
            },
        )
    }

    #[test]
    fn register_node_is_idempotent() {
        let s = setup();
        let a = node_in(&s, "rev-1", node_type::REQUIREMENT_REVISION).unwrap();
        let b = node_in(&s, "rev-1", node_type::REQUIREMENT_REVISION).unwrap();
        assert_eq!(a.id, b.id);
        assert!(find_node(&s, node_type::REQUIREMENT_REVISION, "missing")
            .unwrap()
            .is_none());
    }

    #[test]
    fn edge_requires_registered_nodes() {
        let s = setup();
        node_in(&s, "rev-1", node_type::REQUIREMENT_REVISION).unwrap();
        let err = add_edge(
            &s,
            &EdgeInput {
                workitem_id: "wi_1",
                from_node_type: node_type::REQUIREMENT_ITEM,
                from_entity_id: "item-404",
                relation: relation::DERIVED_FROM,
                to_node_type: node_type::REQUIREMENT_REVISION,
                to_entity_id: "rev-1",
                stage_attempt_id: "",
                created_by_run_id: "",
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains(TRACE_INCOMPLETE));
    }

    #[test]
    fn edge_is_idempotent_and_digest_deterministic() {
        let s = setup();
        node_in(&s, "rev-1", node_type::REQUIREMENT_REVISION).unwrap();
        node_in(&s, "item-1", node_type::REQUIREMENT_ITEM).unwrap();
        let input = EdgeInput {
            workitem_id: "wi_1",
            from_node_type: node_type::REQUIREMENT_ITEM,
            from_entity_id: "item-1",
            relation: relation::DERIVED_FROM,
            to_node_type: node_type::REQUIREMENT_REVISION,
            to_entity_id: "rev-1",
            stage_attempt_id: "",
            created_by_run_id: "",
        };
        let a = add_edge(&s, &input).unwrap();
        let b = add_edge(&s, &input).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(a.edge_digest, b.edge_digest);
        // 相同四元组的 digest 与手工计算一致。
        assert_eq!(
            a.edge_digest,
            edge_digest(&a.from_node_id, "derived_from", &a.to_node_id, "", "")
        );
    }

    #[test]
    fn cycles_are_rejected() {
        let s = setup();
        node_in(&s, "a", node_type::ARTIFACT_REVISION).unwrap();
        node_in(&s, "b", node_type::EVIDENCE).unwrap();
        let ab = EdgeInput {
            workitem_id: "wi_1",
            from_node_type: node_type::ARTIFACT_REVISION,
            from_entity_id: "a",
            relation: relation::DERIVED_FROM,
            to_node_type: node_type::EVIDENCE,
            to_entity_id: "b",
            stage_attempt_id: "",
            created_by_run_id: "",
        };
        let ba = EdgeInput {
            workitem_id: "wi_1",
            from_node_type: node_type::EVIDENCE,
            from_entity_id: "b",
            relation: relation::DERIVED_FROM,
            to_node_type: node_type::ARTIFACT_REVISION,
            to_entity_id: "a",
            stage_attempt_id: "",
            created_by_run_id: "",
        };
        add_edge(&s, &ab).unwrap();
        let err = add_edge(&s, &ba).unwrap_err();
        assert!(err.to_string().contains(TRACE_CYCLE));
        // 自环。
        let err = add_edge_by_ids(&s, "wi_1", "pn_x", "uses", "pn_x", "", "").unwrap_err();
        assert!(err.to_string().contains(TRACE_CYCLE));
    }

    #[test]
    fn lineage_traverses_both_directions() {
        let s = setup();
        node_in(&s, "rev-1", node_type::REQUIREMENT_REVISION).unwrap();
        node_in(&s, "item-1", node_type::REQUIREMENT_ITEM).unwrap();
        node_in(&s, "art-1", node_type::ARTIFACT_REVISION).unwrap();
        add_edge(
            &s,
            &EdgeInput {
                workitem_id: "wi_1",
                from_node_type: node_type::REQUIREMENT_ITEM,
                from_entity_id: "item-1",
                relation: relation::DERIVED_FROM,
                to_node_type: node_type::REQUIREMENT_REVISION,
                to_entity_id: "rev-1",
                stage_attempt_id: "",
                created_by_run_id: "",
            },
        )
        .unwrap();
        add_edge(
            &s,
            &EdgeInput {
                workitem_id: "wi_1",
                from_node_type: node_type::ARTIFACT_REVISION,
                from_entity_id: "art-1",
                relation: relation::SATISFIES,
                to_node_type: node_type::REQUIREMENT_ITEM,
                to_entity_id: "item-1",
                stage_attempt_id: "",
                created_by_run_id: "",
            },
        )
        .unwrap();
        let item = find_node(&s, node_type::REQUIREMENT_ITEM, "item-1")
            .unwrap()
            .unwrap();
        let up = lineage(&s, &item.id, "up", 5).unwrap();
        assert_eq!(up.edges.len(), 1, "item 只有一条入边（来自 artifact）");
        let down = lineage(&s, &item.id, "down", 5).unwrap();
        assert_eq!(down.edges.len(), 1, "item 只有一条出边（指向 revision）");
        let both = lineage(&s, &item.id, "both", 5).unwrap();
        assert_eq!(both.edges.len(), 2);
        assert_eq!(both.nodes.len(), 3);
    }

    #[test]
    fn coverage_and_gaps_reflect_edges() {
        let s = setup();
        // 需求修订 + 两个条目。
        s.with_conn(|c| {
            c.execute(
                "INSERT INTO requirement_documents(id, workitem_id, source_kind, source_ref, title, created_at)
                 VALUES ('rd_1','wi_1','inline','requirement.md','需求',?1)",
                [timefmt::now()],
            )?;
            c.execute(
                "INSERT INTO requirement_revisions(id, document_id, revision_no, object_sha256, content_sha256, created_by, created_at)
                 VALUES ('rr_1','rd_1',1,'obj','cnt','local-user',?1)",
                [timefmt::now()],
            )?;
            c.execute(
                "INSERT INTO requirement_items(id, revision_id, requirement_key, title, created_at)
                 VALUES ('ri_1','rr_1','REQ-001','登录',?1),
                        ('ri_2','rr_1','REQ-002','登出',?1)",
                [timefmt::now()],
            )?;
            Ok(())
        })
        .unwrap();
        node_in(&s, "rr_1", node_type::REQUIREMENT_REVISION).unwrap();
        node_in(&s, "ri_1", node_type::REQUIREMENT_ITEM).unwrap();
        node_in(&s, "ri_2", node_type::REQUIREMENT_ITEM).unwrap();
        node_in(&s, "art-1", node_type::ARTIFACT_REVISION).unwrap();
        add_edge(
            &s,
            &EdgeInput {
                workitem_id: "wi_1",
                from_node_type: node_type::ARTIFACT_REVISION,
                from_entity_id: "art-1",
                relation: relation::SATISFIES,
                to_node_type: node_type::REQUIREMENT_ITEM,
                to_entity_id: "ri_1",
                stage_attempt_id: "",
                created_by_run_id: "",
            },
        )
        .unwrap();
        let cov = coverage(&s, "wi_1", "rr_1").unwrap();
        assert_eq!(cov["totalItems"], serde_json::json!(2));
        assert_eq!(cov["coveredCount"], serde_json::json!(1));
        let gaps = gaps(&s, "wi_1").unwrap();
        assert_eq!(gaps["uncoveredItemCount"], serde_json::json!(1));
        // ri_1/ri_2 有入边不再是孤儿；art-1 与 rr_1 是孤儿。
        assert_eq!(gaps["orphanCount"], serde_json::json!(2));
    }

    #[test]
    fn verification_state_updates() {
        let s = setup();
        node_in(&s, "ev-1", node_type::EVIDENCE).unwrap();
        set_verification(&s, node_type::EVIDENCE, "ev-1", "verified").unwrap();
        let n = find_node(&s, node_type::EVIDENCE, "ev-1").unwrap().unwrap();
        assert_eq!(n.verification_state, "verified");
        set_verification(&s, node_type::EVIDENCE, "ev-1", "unverified").unwrap();
        let n = find_node(&s, node_type::EVIDENCE, "ev-1").unwrap().unwrap();
        assert_eq!(n.verification_state, "unverified");
    }
}
