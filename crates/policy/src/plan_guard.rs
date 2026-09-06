//! PlanGuard（EvoFlow 方案 M2-04 / ADR-037 §6.6）：执行前硬门禁，不是 prompt 约定。
//!
//! 决策顺序固定：Registry/Schema → phase effect classification → **PlanGuard** →
//! ToolRule → approval validation → workspace/sandbox validation → execution。
//! 本模块只做 phase × effect 的判定；ToolRule/审批/沙箱校验由调用链后续环节承接。
//!
//! 原则：
//! - planning 阶段仅允许读/澄清/计划草稿写入；write_file、apply_patch、run_command、
//!   外部写 MCP 与未知工具一律拒绝。
//! - effect 无法确认（Schema 漂移等）= `plan_guard_unknown_effect`，fail-closed。
//! - reconciliation 阶段与 planning 同等严格：对账只允许查询类操作。

use serde::{Deserialize, Serialize};

/// 执行阶段（§6.6 GuardInput.phase）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardPhase {
    Planning,
    Execution,
    Reconciliation,
}

impl GuardPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            GuardPhase::Planning => "planning",
            GuardPhase::Execution => "execution",
            GuardPhase::Reconciliation => "reconciliation",
        }
    }
}

/// effect 分类结果；None = 无法确认（Schema 漂移/未注册），fail-closed。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectClass {
    None,
    Read,
    LocalWrite,
    ExternalWrite,
    Irreversible,
}

impl EffectClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            EffectClass::None => "none",
            EffectClass::Read => "read",
            EffectClass::LocalWrite => "local_write",
            EffectClass::ExternalWrite => "external_write",
            EffectClass::Irreversible => "irreversible",
        }
    }

    pub fn is_write(&self) -> bool {
        matches!(
            self,
            EffectClass::LocalWrite | EffectClass::ExternalWrite | EffectClass::Irreversible
        )
    }
}

/// §6.6 GuardInput（结构化输入；digest 字段由调用方填入，M2 阶段允许空串占位）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardInput {
    pub phase: GuardPhase,
    pub autonomy_mode: String,
    pub plan_revision_id: String,
    pub plan_task_attempt_id: String,
    pub tool_name: String,
    pub tool_schema_digest: String,
    /// Registry/Schema 分类结果；None = 无法确认。
    pub effect_class: Option<EffectClass>,
    pub workspace_policy_digest: String,
    pub autonomy_grant_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardDecision {
    Allow,
    Deny { token: &'static str, reason: String },
}

fn deny(token: &'static str, reason: impl Into<String>) -> GuardDecision {
    GuardDecision::Deny {
        token,
        reason: reason.into(),
    }
}

/// planning/reconciliation 阶段允许的工具白名单（读 + 澄清 + 计划草稿）。
fn phase_allowlisted(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "read_file" | "search_knowledge" | "ask_clarification" | "plan.writeDraft"
    )
}

/// PlanGuard 判定（纯函数；确定性、无 IO）。
pub fn evaluate(input: &GuardInput) -> GuardDecision {
    // 1) effect 分类必须可确认（§6.6：Schema 漂移无法确认 → 失败关闭）。
    let Some(effect) = input.effect_class else {
        return deny(
            "plan_guard_unknown_effect",
            format!(
                "工具 {} 的 effect 无法确认（Schema 漂移/未注册），失败关闭",
                input.tool_name
            ),
        );
    };
    // 2) 阶段判定。
    match input.phase {
        GuardPhase::Planning | GuardPhase::Reconciliation => {
            if phase_allowlisted(&input.tool_name) {
                return GuardDecision::Allow;
            }
            // 明确只读 MCP：mcp__ 前缀且 effect 为 none/read。
            if input.tool_name.starts_with("mcp__") && matches!(effect, EffectClass::None | EffectClass::Read)
            {
                return GuardDecision::Allow;
            }
            let _ = effect.is_write();
            deny(
                "plan_guard_denied",
                format!(
                    "phase={} 仅允许读/澄清/计划草稿与明确只读 MCP；{}（effect={}）被拒绝",
                    input.phase.as_str(),
                    input.tool_name,
                    effect.as_str()
                ),
            )
        }
        GuardPhase::Execution => GuardDecision::Allow,
    }
}

/// 便捷判定：给工具名+effect 快速给出 Allow/Deny（供 M2-08 tools 接线复用）。
pub fn check(phase: GuardPhase, tool_name: &str, effect: Option<EffectClass>) -> GuardDecision {
    evaluate(&GuardInput {
        phase,
        autonomy_mode: String::new(),
        plan_revision_id: String::new(),
        plan_task_attempt_id: String::new(),
        tool_name: tool_name.to_string(),
        tool_schema_digest: String::new(),
        effect_class: effect,
        workspace_policy_digest: String::new(),
        autonomy_grant_digest: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planning_rejects_side_effect_tools() {
        // EV-006：planning 阶段调 write/apply/command 一律拒绝。
        for tool in ["write_file", "apply_patch", "run_command"] {
            let d = check(GuardPhase::Planning, tool, Some(EffectClass::LocalWrite));
            assert!(
                matches!(d, GuardDecision::Deny { token: "plan_guard_denied", .. }),
                "{tool} 应被拒绝: {d:?}"
            );
        }
    }

    #[test]
    fn planning_allows_reads_clarification_and_plan_draft() {
        for tool in ["read_file", "search_knowledge", "ask_clarification", "plan.writeDraft"] {
            let d = check(GuardPhase::Planning, tool, Some(EffectClass::Read));
            assert!(matches!(d, GuardDecision::Allow), "{tool} 应放行: {d:?}");
        }
        // plan.writeDraft effect 是 none 也放行。
        let d = check(GuardPhase::Planning, "plan.writeDraft", Some(EffectClass::None));
        assert!(matches!(d, GuardDecision::Allow));
    }

    #[test]
    fn planning_allows_only_readonly_mcp() {
        let d = check(GuardPhase::Planning, "mcp__srv__read_thing", Some(EffectClass::Read));
        assert!(matches!(d, GuardDecision::Allow));
        let d = check(GuardPhase::Planning, "mcp__srv__send_thing", Some(EffectClass::ExternalWrite));
        assert!(matches!(d, GuardDecision::Deny { token: "plan_guard_denied", .. }));
    }

    #[test]
    fn unknown_effect_fails_closed_in_every_phase() {
        for phase in [GuardPhase::Planning, GuardPhase::Execution, GuardPhase::Reconciliation] {
            let d = check(phase, "run_command", None);
            assert!(
                matches!(d, GuardDecision::Deny { token: "plan_guard_unknown_effect", .. }),
                "{phase:?} unknown effect 必须 fail-closed: {d:?}"
            );
        }
    }

    #[test]
    fn execution_allows_known_effects_and_reconciliation_stays_readonly() {
        let d = check(GuardPhase::Execution, "run_command", Some(EffectClass::LocalWrite));
        assert!(matches!(d, GuardDecision::Allow));
        // reconciliation：写工具拒绝（对账只允许查询类）。
        let d = check(GuardPhase::Reconciliation, "apply_patch", Some(EffectClass::LocalWrite));
        assert!(matches!(d, GuardDecision::Deny { token: "plan_guard_denied", .. }));
        let d = check(GuardPhase::Reconciliation, "read_file", Some(EffectClass::Read));
        assert!(matches!(d, GuardDecision::Allow));
    }
}
