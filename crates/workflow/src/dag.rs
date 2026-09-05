//! Plan DAG 算法（EvoFlow 方案 M2-03 / ADR-037 §6.2）：
//! 环检测、确定性拓扑序（Kahn + 同层 task_key 字典序）、ready set、下游 affected closure。
//! 纯函数域：不触库；plan.rs 激活校验与 M3 scheduler 复用。

use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagError {
    pub token: &'static str,
    pub message: String,
}

impl std::fmt::Display for DagError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.token, self.message)
    }
}

/// 节点定义：task_key 唯一；deps 引用上游 task_key。
#[derive(Debug, Clone)]
pub struct DagTask {
    pub task_key: String,
    pub deps: Vec<String>,
    /// 写任务（local_write/external_write/irreversible effect）标记，孤立检查用。
    pub is_write: bool,
}

impl DagTask {
    pub fn new(task_key: &str, deps: &[&str], is_write: bool) -> Self {
        Self {
            task_key: task_key.to_string(),
            deps: deps.iter().map(|s| s.to_string()).collect(),
            is_write,
        }
    }

    /// deps 直接来自 Vec<String>（plan_tasks 边表回读）。
    pub fn from_strings(task_key: String, deps: Vec<String>, is_write: bool) -> Self {
        Self {
            task_key,
            deps,
            is_write,
        }
    }
}

fn index(tasks: &[DagTask]) -> Result<BTreeMap<&str, &DagTask>, DagError> {
    let mut map = BTreeMap::new();
    for t in tasks {
        let inserted = map.insert(t.task_key.as_str(), t);
        if inserted.is_some() {
            return Err(DagError {
                token: "plan_validation_failed",
                message: format!("task_key 重复：{}", t.task_key),
            });
        }
    }
    Ok(map)
}

/// 激活校验（§6.2）：引用完整 + 孤立写任务 + 环检测，全部通过返回确定性拓扑序。
pub fn validate_and_order(tasks: &[DagTask]) -> Result<Vec<String>, DagError> {
    let idx = index(tasks)?;
    // 输出引用完整性：deps 必须引用已定义任务。
    for t in tasks {
        for dep in &t.deps {
            if !idx.contains_key(dep.as_str()) {
                return Err(DagError {
                    token: "plan_validation_failed",
                    message: format!("任务 {} 引用不存在的上游任务 {}", t.task_key, dep),
                });
            }
        }
    }
    // 孤立写任务：无上游依赖且无下游消费的写任务（副作用无人核对）。
    let mut has_downstream: BTreeSet<&str> = BTreeSet::new();
    for t in tasks {
        for dep in &t.deps {
            has_downstream.insert(dep.as_str());
        }
    }
    for t in tasks {
        if t.is_write && t.deps.is_empty() && !has_downstream.contains(t.task_key.as_str()) {
            return Err(DagError {
                token: "plan_validation_failed",
                message: format!("孤立写任务 {}（无上游输入且无下游消费）", t.task_key),
            });
        }
    }
    // Kahn 拓扑排序：ready 集合用 BTreeSet 保证同层按 task_key 字典序弹出（确定性）。
    let mut indegree: BTreeMap<&str, usize> = BTreeMap::new();
    let mut downstream: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for t in tasks {
        indegree.entry(t.task_key.as_str()).or_insert(0);
        for dep in &t.deps {
            *indegree.entry(t.task_key.as_str()).or_insert(0) += 1;
            downstream
                .entry(dep.as_str())
                .or_default()
                .push(t.task_key.as_str());
        }
    }
    let mut ready: BTreeSet<&str> = indegree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(k, _)| *k)
        .collect();
    let mut order = Vec::with_capacity(tasks.len());
    while let Some(key) = ready.pop_first() {
        order.push(key.to_string());
        for next in downstream.get(key).into_iter().flatten() {
            let d = indegree.get_mut(*next).expect("dep indexed");
            *d -= 1;
            if *d == 0 {
                ready.insert(next);
            }
        }
    }
    if order.len() != tasks.len() {
        let stuck: Vec<String> = tasks
            .iter()
            .map(|t| t.task_key.clone())
            .filter(|k| !order.contains(k))
            .collect();
        return Err(DagError {
            token: "plan_cycle_detected",
            message: format!("依赖环涉及 {} 个任务", stuck.len()),
        });
    }
    Ok(order)
}

/// ready set：未完成任务中，所有上游依赖均已完成的（供 start/推进计算）。
pub fn ready_set(
    tasks: &[DagTask],
    completed_keys: &BTreeSet<String>,
    running_or_pending_keys: &BTreeSet<String>,
) -> Vec<String> {
    let idx: BTreeMap<&str, &DagTask> = tasks.iter().map(|t| (t.task_key.as_str(), t)).collect();
    let mut ready = Vec::new();
    for t in tasks {
        if completed_keys.contains(&t.task_key) || running_or_pending_keys.contains(&t.task_key) {
            continue;
        }
        let deps_ok = t
            .deps
            .iter()
            .all(|d| completed_keys.contains(d) && idx.contains_key(d.as_str()));
        if deps_ok {
            ready.push(t.task_key.clone());
        }
    }
    ready.sort();
    ready
}

/// 下游受影响闭包（局部重规划 §6.4）：roots 及其全部传递下游（含自身）。
pub fn affected_closure(tasks: &[DagTask], roots: &[String]) -> BTreeSet<String> {
    let mut downstream: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for t in tasks {
        for dep in &t.deps {
            downstream
                .entry(dep.as_str())
                .or_default()
                .push(t.task_key.as_str());
        }
    }
    let mut closure = BTreeSet::new();
    let mut stack: Vec<String> = roots.to_vec();
    while let Some(k) = stack.pop() {
        if closure.insert(k.clone()) {
            if let Some(nexts) = downstream.get(k.as_str()) {
                for n in nexts {
                    stack.push((*n).to_string());
                }
            }
        }
    }
    closure
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(key: &str, deps: &[&str], write: bool) -> DagTask {
        DagTask::new(key, deps, write)
    }

    #[test]
    fn toposort_is_deterministic_lexicographic_within_layer() {
        // 无边：纯字典序。
        let tasks = vec![t("c", &[], false), t("a", &[], false), t("b", &[], false)];
        assert_eq!(validate_and_order(&tasks).unwrap(), vec!["a", "b", "c"]);
        // 分层：层内字典序、跨层依赖优先。
        let tasks = vec![
            t("b2", &["a"], false),
            t("a1", &["a"], false),
            t("a", &[], true),
        ];
        assert_eq!(validate_and_order(&tasks).unwrap(), vec!["a", "a1", "b2"]);
    }

    #[test]
    fn cycle_detected() {
        let tasks = vec![
            t("a", &["b"], false),
            t("b", &["a"], false),
            t("c", &[], false),
        ];
        let err = validate_and_order(&tasks).unwrap_err();
        assert_eq!(err.token, "plan_cycle_detected");
    }

    #[test]
    fn missing_dependency_rejected() {
        let tasks = vec![t("a", &["ghost"], false)];
        let err = validate_and_order(&tasks).unwrap_err();
        assert_eq!(err.token, "plan_validation_failed");
        assert!(err.message.contains("ghost"));
    }

    #[test]
    fn orphan_write_task_rejected_but_read_orphan_allowed() {
        let tasks = vec![t("w", &[], true)];
        let err = validate_and_order(&tasks).unwrap_err();
        assert!(err.message.contains("孤立写任务"));
        // 纯分析任务允许无依赖无边。
        let tasks = vec![t("analysis_only", &[], false)];
        assert!(validate_and_order(&tasks).is_ok());
        // 有下游消费的写任务合法。
        let tasks = vec![t("w", &[], true), t("v", &["w"], false)];
        assert!(validate_and_order(&tasks).is_ok());
    }

    #[test]
    fn duplicate_task_key_rejected() {
        let tasks = vec![t("a", &[], false), t("a", &[], false)];
        let err = validate_and_order(&tasks).unwrap_err();
        assert!(err.message.contains("重复"));
    }

    #[test]
    fn ready_set_respects_dependencies_and_excludes_active() {
        let tasks = vec![
            t("a", &[], false),
            t("b", &["a"], false),
            t("c", &["a"], false),
        ];
        let mut completed = BTreeSet::new();
        let mut active = BTreeSet::new();
        assert_eq!(ready_set(&tasks, &completed, &active), vec!["a"]);
        completed.insert("a".to_string());
        assert_eq!(ready_set(&tasks, &completed, &active), vec!["b", "c"]);
        active.insert("b".to_string());
        assert_eq!(ready_set(&tasks, &completed, &active), vec!["c"]);
        completed.insert("b".to_string());
        completed.insert("c".to_string());
        assert!(ready_set(&tasks, &completed, &active).is_empty());
    }

    #[test]
    fn affected_closure_includes_transitive_downstream() {
        let tasks = vec![
            t("a", &[], true),
            t("b", &["a"], false),
            t("c", &["b"], true),
            t("d", &[], false), // 无关分支
        ];
        let closure = affected_closure(&tasks, &["a".to_string()]);
        assert_eq!(
            closure,
            BTreeSet::from(["a".to_string(), "b".to_string(), "c".to_string()])
        );
        // 根=失败任务本身。
        let closure = affected_closure(&tasks, &["b".to_string()]);
        assert_eq!(closure, BTreeSet::from(["b".to_string(), "c".to_string()]));
    }
}
