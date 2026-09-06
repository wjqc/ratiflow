//! 内建 Middleware Registry（EvoFlow 方案 M4-07 / ADR-038 §6.11）：
//! 仅允许编译进二进制并登记的中间件；配置只调顺序与参数，不能动态加载代码。
//! 统一 hook 契约（before_run/before_model/after_model/before_tool/after_tool/after_run）
//! 由 Run 固定代码在既定阶段调用；本模块负责顺序校验（security 项不可移除/重排）与
//! profile digest——安全语义不依赖配置自觉，违反即 `middleware_order_invalid`。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 默认顺序（§6.11）：FrozenContext → Workspace → PlanGuard → Clarification →
/// Budget → Summarization → ToolPolicy → Trace → MemoryCaptureEnqueue。
pub const DEFAULT_ORDER: [&str; 9] = [
    "frozen_context",
    "workspace",
    "plan_guard",
    "clarification",
    "budget",
    "summarization",
    "tool_policy",
    "trace",
    "memory_capture",
];

/// security middleware：必须全部在场且相对顺序与默认一致（不可移到执行之后）。
pub const SECURITY_MIDDLEWARE: [&str; 3] = ["plan_guard", "budget", "tool_policy"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiddlewareStep {
    pub name: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// hook 是否登记在册（内建）。
pub fn is_builtin(name: &str) -> bool {
    DEFAULT_ORDER.contains(&name)
}

/// 顺序校验（§6.11 约束）：
/// 1. 每个步骤必须是内建 hook；
/// 2. 不得重复；
/// 3. security middleware 必须全部在场且相对顺序与 DEFAULT_ORDER 一致；
/// 4. security 项之后不得出现会改写请求体的非安全项（summarization 除外——
///    其默认位在 budget/tool_policy 之前；此处以白名单后缀校验简化为：
///    tool_policy 之后只允许 trace/memory_capture）。
pub fn validate_order(steps: &[MiddlewareStep]) -> Result<(), String> {
    if steps.is_empty() {
        return Err("middleware_order_invalid: 至少一个步骤".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    for s in steps {
        if !is_builtin(&s.name) {
            return Err(format!(
                "middleware_order_invalid: 非内建中间件 {}（不允许动态加载）",
                s.name
            ));
        }
        if !seen.insert(s.name.as_str()) {
            return Err(format!("middleware_order_invalid: 重复步骤 {}", s.name));
        }
    }
    // security 全在场且相对顺序保持。
    let pos_of = |name: &str| steps.iter().position(|s| s.name == name);
    let mut last: Option<usize> = None;
    for sec in SECURITY_MIDDLEWARE {
        let pos = pos_of(sec)
            .ok_or_else(|| format!("middleware_order_invalid: security 中间件 {sec} 不可移除"))?;
        if let Some(prev) = last {
            if pos <= prev {
                return Err(format!(
                    "middleware_order_invalid: security 项 {sec} 相对顺序被改变"
                ));
            }
        }
        last = Some(pos);
    }
    // tool_policy（最后一道请求体闸）之后只允许观测类。
    let tp = pos_of("tool_policy").expect("security present");
    for (i, s) in steps.iter().enumerate() {
        if i > tp && !matches!(s.name.as_str(), "trace" | "memory_capture") {
            return Err(format!(
                "middleware_order_invalid: {} 不可位于 tool_policy 之后（执行前闸不可后移）",
                s.name
            ));
        }
    }
    Ok(())
}

/// profile digest：canonical steps（name+排序后 params）sha256。
pub fn profile_digest(steps: &[MiddlewareStep]) -> String {
    let mut entries: Vec<String> = steps
        .iter()
        .map(|s| {
            let params = match &s.params {
                serde_json::Value::Object(m) => {
                    let keys: std::collections::BTreeSet<&String> = m.keys().collect();
                    let mut body = String::new();
                    for k in keys {
                        body.push_str(&format!(
                            "{k}:{}",
                            m.get(k).map(|v| v.to_string()).unwrap_or_default()
                        ));
                    }
                    body
                }
                other => other.to_string(),
            };
            format!("{}|{}", s.name, params)
        })
        .collect();
    entries.sort();
    let mut hasher = Sha256::new();
    hasher.update(format!("mw1|{}", entries.join("\n")).as_bytes());
    sg_store::ids::hex(&hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn steps(names: &[&str]) -> Vec<MiddlewareStep> {
        names
            .iter()
            .map(|n| MiddlewareStep {
                name: (*n).to_string(),
                params: json!({}),
            })
            .collect()
    }

    #[test]
    fn default_order_is_valid_and_digest_deterministic() {
        let s = steps(&DEFAULT_ORDER);
        validate_order(&s).unwrap();
        let d1 = profile_digest(&s);
        let d2 = profile_digest(&steps(&DEFAULT_ORDER));
        assert_eq!(d1, d2);
        // 仅调 params 不改 digest 结构（params 变化会变 digest）。
        let mut tuned = steps(&DEFAULT_ORDER);
        tuned[4].params = json!({"maxTokens": 12000});
        assert_ne!(profile_digest(&tuned), d1, "参数变化 → digest 变化");
    }

    #[test]
    fn security_removed_or_reordered_rejected() {
        // 移除 budget → 拒绝。
        let s: Vec<MiddlewareStep> = steps(&DEFAULT_ORDER)
            .into_iter()
            .filter(|s| s.name != "budget")
            .collect();
        let err = validate_order(&s).unwrap_err();
        assert!(err.contains("budget"), "{err}");
        // 相对顺序改变（tool_policy 提前到 plan_guard 前）→ 拒绝。
        let reordered = steps(&[
            "frozen_context",
            "workspace",
            "tool_policy",
            "clarification",
            "budget",
            "plan_guard",
            "summarization",
            "trace",
            "memory_capture",
        ]);
        let err = validate_order(&reordered).unwrap_err();
        assert!(err.contains("middleware_order_invalid"), "{err}");
        // 非内建 → 拒绝（不允许动态加载）。
        let mut s = steps(&DEFAULT_ORDER);
        s.push(MiddlewareStep {
            name: "custom_ext".into(),
            params: json!({}),
        });
        let err = validate_order(&s).unwrap_err();
        assert!(err.contains("非内建"), "{err}");
        // 重复 → 拒绝。
        let mut s = steps(&DEFAULT_ORDER);
        s.insert(
            0,
            MiddlewareStep {
                name: "trace".into(),
                params: json!({}),
            },
        );
        assert!(validate_order(&s).is_err());
    }

    #[test]
    fn post_tool_policy_gate_restricted() {
        // summarization 不可移到 tool_policy 之后（执行前闸）。
        let reordered = steps(&[
            "frozen_context",
            "workspace",
            "plan_guard",
            "clarification",
            "budget",
            "tool_policy",
            "summarization",
            "trace",
            "memory_capture",
        ]);
        let err = validate_order(&reordered).unwrap_err();
        assert!(err.contains("summarization"), "{err}");
    }
}
