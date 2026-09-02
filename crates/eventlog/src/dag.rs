//! 事件 DAG：完整性校验与因果序（ADR-031 C2 r3）。
//!
//! 校验 fail-closed（C2）：未知 parent、重复 eventId、成环、文件名与 eventId 不一致
//! → 整个工作项投影失败，不产半投影。排序 = parentHead 拓扑序（因果序），
//! 同层并列按 eventId 字典序平局（ULID 仅平局键，不作因果序）。

use std::collections::{BTreeMap, BTreeSet};

use crate::{Envelope, Error, ROOT_PARENT, is_ulid};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DagError {
    #[error("duplicate_event_id: {0}")]
    DuplicateEventId(String),
    #[error("unknown_parent: {0} 的 parent {1} 不存在")]
    UnknownParent(String, String),
    #[error("cycle_detected: 涉及 {0}")]
    Cycle(String),
    #[error("filename_mismatch: 文件名 {0} != eventId {1}")]
    FilenameMismatch(String, String),
    #[error("bad_event_id: {0}")]
    BadEventId(String),
    #[error("schema_version_unsupported: {0}")]
    SchemaVersionUnsupported(u32),
}

/// 已装载的事件视图（装载即校验，fail-closed）。
#[derive(Debug, Clone, Default)]
pub struct DagView {
    pub events: BTreeMap<String, Envelope>,
    /// parent head -> 子事件 id（字典序）。
    pub children: BTreeMap<String, Vec<String>>,
}

impl DagView {
    /// `loaded` 为 (文件名词干, 信封)——文件名词干必须与 eventId 一致。
    pub fn build(loaded: Vec<(String, Envelope)>) -> Result<Self, Error> {
        let mut view = Self::default();
        for (stem, env) in loaded {
            if env.schema_version != crate::SCHEMA_VERSION {
                return Err(DagError::SchemaVersionUnsupported(env.schema_version).into());
            }
            if !is_ulid(&env.event_id) && env.event_id != crate::ROOT_PARENT {
                return Err(DagError::BadEventId(env.event_id.clone()).into());
            }
            if stem != env.event_id {
                return Err(DagError::FilenameMismatch(stem, env.event_id.clone()).into());
            }
            if view.events.insert(env.event_id.clone(), env).is_some() {
                return Err(DagError::DuplicateEventId(stem).into());
            }
        }
        let ids: BTreeSet<String> = view.events.keys().cloned().collect();
        for env in view.events.values() {
            if env.parent_head == ROOT_PARENT {
                continue;
            }
            if !ids.contains(&env.parent_head) {
                return Err(DagError::UnknownParent(
                    env.event_id.clone(),
                    env.parent_head.clone(),
                )
                .into());
            }
            view.children
                .entry(env.parent_head.clone())
                .or_default()
                .push(env.event_id.clone());
        }
        for v in view.children.values_mut() {
            v.sort(); // 字典序平局键
            v.dedup();
        }
        // 环检测：parent 链必须收敛到 ROOT（DAG 无环的充要判定，单亲结构下）。
        for id in ids {
            let mut seen = BTreeSet::new();
            let mut cur = Some(id.clone());
            while let Some(x) = cur {
                if x == ROOT_PARENT {
                    break;
                }
                if !seen.insert(x.clone()) {
                    return Err(DagError::Cycle(id).into());
                }
                cur = view.events.get(&x).map(|e| e.parent_head.clone());
            }
        }
        Ok(view)
    }

    /// 因果序：Kahn 拓扑 + 就绪集按 eventId 字典序（确定性）。
    /// 单亲结构：事件在其 parent 发出后即就绪，每个事件恰入队一次。
    pub fn causal_order(&self) -> Vec<&Envelope> {
        let mut emitted: BTreeSet<&str> = BTreeSet::new();
        let mut ready: BTreeSet<&str> = BTreeSet::new();
        for (id, env) in &self.events {
            if env.parent_head == ROOT_PARENT {
                ready.insert(id);
            }
        }
        let mut order = Vec::with_capacity(self.events.len());
        while let Some(id) = ready.pop_first() {
            order.push(&self.events[id]);
            emitted.insert(id);
            if let Some(children) = self.children.get(id) {
                for c in children {
                    if !emitted.contains(c.as_str()) {
                        ready.insert(c.as_str());
                    }
                }
            }
        }
        debug_assert_eq!(order.len(), self.events.len(), "无环已由 build 保证");
        order
    }

    /// 分叉头：children ≥ 2 的 parent head（ADR-031 C6 fork 判定）。
    pub fn fork_heads(&self) -> Vec<&str> {
        self.children
            .iter()
            .filter(|(_, ch)| ch.len() >= 2)
            .map(|(h, _)| h.as_str())
            .collect()
    }

    /// 从若干 head 出发的后代闭包（含 head 自身），children 边传递。
    pub fn descendants(&self, heads: &[String]) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut stack: Vec<String> = heads.to_vec();
        while let Some(x) = stack.pop() {
            if !out.insert(x.clone()) {
                continue;
            }
            if let Some(children) = self.children.get(&x) {
                stack.extend(children.iter().cloned());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Payload;

    fn env(id_num: u128, parent: &str, attempt: &str) -> Envelope {
        let mut e = Envelope::new("wi", attempt, parent, Payload::AttemptStarted { gate: "dev".into() }, "t");
        e.event_id = crate::encode_ulid(id_num);
        e
    }

    fn loaded(list: Vec<Envelope>) -> Vec<(String, Envelope)> {
        list.into_iter().map(|e| (e.event_id.clone(), e)).collect()
    }

    #[test]
    fn causal_order_respects_parent_chain_not_lexicographic() {
        // 字典序 a<b，但 b 是 a 的孩子 → 因果序必须 a 先。
        let a = env(1, ROOT_PARENT, "at1");
        let b = env(2, &a.event_id, "at1");
        let view = DagView::build(loaded(vec![b.clone(), a.clone()])).unwrap();
        let order: Vec<&str> = view.causal_order().iter().map(|e| e.event_id.as_str()).collect();
        assert_eq!(order, vec![a.event_id.as_str(), b.event_id.as_str()]);
    }

    #[test]
    fn tie_break_by_event_id_when_parallel() {
        let p = env(1, ROOT_PARENT, "at1");
        let c2 = env(20, &p.event_id, "at2"); // 字典序更大
        let c1 = env(10, &p.event_id, "at3");
        let view = DagView::build(loaded(vec![c2, p.clone(), c1.clone()])).unwrap();
        let order: Vec<&str> = view.causal_order().iter().map(|e| e.event_id.as_str()).collect();
        assert_eq!(order.len(), 3);
        assert_eq!(order[0], p.event_id);
        assert_eq!(order[1], c1.event_id, "同层并列按 eventId 字典序");
        assert_eq!(order[2], c2.event_id);
    }

    #[test]
    fn duplicate_event_id_fails_closed() {
        let a = env(1, ROOT_PARENT, "at1");
        let dup = env(1, ROOT_PARENT, "at2");
        let err = DagView::build(loaded(vec![a, dup])).unwrap_err();
        assert!(matches!(err, Error::Dag(DagError::DuplicateEventId(_))));
    }

    #[test]
    fn unknown_parent_fails_closed() {
        let a = env(1, "NOPE", "at1");
        let err = DagView::build(loaded(vec![a])).unwrap_err();
        assert!(matches!(err, Error::Dag(DagError::UnknownParent(_, _))));
    }

    #[test]
    fn cycle_fails_closed() {
        // 单亲结构下的环：a 的 parent 是 b，b 的 parent 是 a（篡改场景）。
        let mut a = env(1, ROOT_PARENT, "at1");
        let mut b = env(2, ROOT_PARENT, "at1");
        a.parent_head = b.event_id.clone();
        b.parent_head = a.event_id.clone();
        let err = DagView::build(loaded(vec![a, b])).unwrap_err();
        assert!(matches!(err, Error::Dag(DagError::Cycle(_))));
    }

    #[test]
    fn filename_mismatch_fails_closed() {
        let a = env(1, ROOT_PARENT, "at1");
        let err = DagView::build(vec![("OTHER".into(), a)]).unwrap_err();
        assert!(matches!(err, Error::Dag(DagError::FilenameMismatch(_, _))));
    }

    #[test]
    fn fork_heads_and_descendants() {
        let p = env(1, ROOT_PARENT, "at1");
        let c1 = env(10, &p.event_id, "at2");
        let c2 = env(20, &p.event_id, "at3");
        let g1 = env(100, &c1.event_id, "at2");
        let view = DagView::build(loaded(vec![p, c1.clone(), c2.clone(), g1.clone()])).unwrap();
        assert_eq!(view.fork_heads().len(), 1);
        let desc = view.descendants(&[c2.event_id.clone(), c1.event_id.clone()]);
        assert_eq!(desc.len(), 3); // c1, g1, c2
        assert!(desc.contains(&g1.event_id));
    }
}
