//! A4 统一风险模型（RDWS 实施计划 v1.4 WP-1）：全部执行入口共同消费的服务端派生评估。
//!
//! 词汇：effect_class 吸收 Plan 侧（plan_guard::EffectClass）；reversibility 复用
//! 0018 快照/回滚词汇（logical_restore|compensatable|manual|irreversible）。
//!
//! 硬规则（`auto_approvable`，纯函数穷举单测）——auto_approvable=false 当且仅当任一：
//! - effect_class == irreversible；
//! - reversibility ∈ {irreversible, manual}（无条件）；
//! - external_write 且（Provider 不支持幂等键 / 本次未生成稳定键 / 键未实际转发）；
//! - external_write 且无结果查证能力。
//!
//! 纪律：Provider/模型自声明只升风险不得降级；执行前 unknown 的**风险能力**与
//! 执行后 unknown 的 **outcome** 是两个事实面，后者由 tool_execution_outcomes 承载。
//!
//! Flag：`RATIFLOW_UNIFIED_RISK`（默认 0）。关闭时调用方跳过评估，不放宽既有审批链。

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const FLAG: &str = "RATIFLOW_UNIFIED_RISK";

/// 默认 0（§1.5 能力开关）；kill switch =0 只停止新评估，不改变任何已落事实。
pub fn enabled() -> bool {
    std::env::var(FLAG).ok().as_deref() == Some("1")
}

/// 模型版本（进入评估快照与审计）。
pub const POLICY_VERSION: &str = "risk-model-v1";

/// 0018 词汇的可恢复性分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reversibility {
    LogicalRestore,
    Compensatable,
    Manual,
    Irreversible,
}

impl Reversibility {
    pub fn as_str(&self) -> &'static str {
        match self {
            Reversibility::LogicalRestore => "logical_restore",
            Reversibility::Compensatable => "compensatable",
            Reversibility::Manual => "manual",
            Reversibility::Irreversible => "irreversible",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "logical_restore" => Some(Reversibility::LogicalRestore),
            "compensatable" => Some(Reversibility::Compensatable),
            "manual" => Some(Reversibility::Manual),
            "irreversible" => Some(Reversibility::Irreversible),
            _ => None,
        }
    }
}

/// 统一风险评估（服务端派生，执行前快照进 proposal 详情 + audit `risk.assessment`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRiskAssessment {
    pub tool: String,
    /// read|local_write|external_write|irreversible（plan_guard::EffectClass 词汇）。
    pub effect_class: String,
    pub reversibility: Reversibility,
    /// 补偿引用，仅人工决策展示，不进自动判定。
    pub compensation: Option<String>,
    /// Provider 能力声明：是否支持幂等键（≠ 本次已传键）。
    pub supports_idempotency_key: bool,
    /// Core 为本次操作生成的稳定键（冻结进 proposal/Run）。
    pub operation_idempotency_key: Option<String>,
    /// Provider 去重作用域/有效期（能力说明）。
    pub idempotency_scope: Option<String>,
    /// adapter 已将键传入 Provider 请求的可审计证据。
    pub key_forwarded: bool,
    /// 结果查证能力（对账查询）。
    pub reconcile_query: bool,
    pub external_write: bool,
    /// 受保护路径/关（deployment deliverable、迁移、依赖锁、CI 配置）。
    pub protected_target: bool,
    pub policy_version: String,
}

impl ExecutionRiskAssessment {
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// 自动批准硬上限（纯函数）：false = 必须人工审批。
/// 注意这是**上限**而非放行：auto_approvable=true 仍受 ToolRule 风险分级约束；
/// false 则无条件强制审批（调用方以 needs_approval ||= !auto_approvable 组合）。
pub fn auto_approvable(a: &ExecutionRiskAssessment) -> bool {
    if a.effect_class == "irreversible" {
        return false;
    }
    if matches!(
        a.reversibility,
        Reversibility::Irreversible | Reversibility::Manual
    ) {
        return false;
    }
    if a.external_write
        && (!a.supports_idempotency_key
            || a.operation_idempotency_key.is_none()
            || !a.key_forwarded)
    {
        return false;
    }
    if a.external_write && !a.reconcile_query {
        return false;
    }
    true
}

/// 从注册表声明派生保守评估（builtin 工具与未接 WP-2/3 adapter 的 MCP）。
///
/// 保守面（只升不降）：无 Provider 能力证据时 external_write 一律按
/// 不支持幂等键/键未转发/不可查证处理——外部写自动批准被硬规则关死；
/// builtin 本地写的结果由同步返回确定性可知（reconcile_query=true）。
#[allow(clippy::too_many_arguments)]
pub fn assess_registry_tool(
    tool: &str,
    effect_class: &str,
    reversibility: &str,
    protected_target: bool,
    operation_idempotency_key: Option<&str>,
    key_forwarded: bool,
    idempotency_scope: Option<&str>,
    compensation: Option<&str>,
) -> ExecutionRiskAssessment {
    let external_write = matches!(effect_class, "external_write" | "irreversible");
    let mcp = tool.starts_with("mcp__") || tool.starts_with("mcp:");
    // MCP 在 WP-3 adapter 证据落地前：无幂等键能力、无结果查证（只升不降）。
    let (supports_key, reconcile) = if mcp {
        (false, false)
    } else {
        // builtin：本地执行结果同步确定；无外部 Provider 幂等面（键能力不适用即视为
        // 已满足——硬规则只在 external_write 时要求键链证据）。
        (true, true)
    };
    ExecutionRiskAssessment {
        tool: tool.to_string(),
        effect_class: effect_class.to_string(),
        reversibility: Reversibility::parse(reversibility).unwrap_or(Reversibility::Manual), // 未知词汇按 manual（fail-closed）
        compensation: compensation.map(String::from),
        supports_idempotency_key: supports_key,
        operation_idempotency_key: operation_idempotency_key.map(String::from),
        idempotency_scope: idempotency_scope.map(String::from),
        key_forwarded,
        reconcile_query: reconcile,
        external_write,
        protected_target,
        policy_version: POLICY_VERSION.to_string(),
    }
}

/// P1-1：能力声明驱动的评估（Provider capabilities 面）。
/// 与 `assess_registry_tool` 的差异：supports/reconcile 来自 Provider 声明
/// （descriptor.capabilities），而非注册表启发；`key_forwarded` 必须是
/// **执行证据**（ProviderCall.key_forwarded），评估时（未执行）一律 false——
/// 「声明支持但未转发」因此不可能自动批准（硬规则）。
#[allow(clippy::too_many_arguments)]
pub fn assess_provider_declared(
    tool: &str,
    effect_class: &str,
    reversibility: &str,
    protected_target: bool,
    operation_idempotency_key: Option<&str>,
    key_forwarded: bool,
    idempotency_scope: Option<&str>,
    compensation: Option<&str>,
    supports_idempotency_key: bool,
    reconcile_query: bool,
) -> ExecutionRiskAssessment {
    let external_write = matches!(effect_class, "external_write" | "irreversible");
    ExecutionRiskAssessment {
        tool: tool.to_string(),
        effect_class: effect_class.to_string(),
        reversibility: Reversibility::parse(reversibility).unwrap_or(Reversibility::Manual),
        compensation: compensation.map(String::from),
        supports_idempotency_key,
        operation_idempotency_key: operation_idempotency_key.map(String::from),
        idempotency_scope: idempotency_scope.map(String::from),
        key_forwarded,
        reconcile_query,
        external_write,
        protected_target,
        policy_version: POLICY_VERSION.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P1-1 退出标准：「声明支持但未转发」必须人工审批——
    /// external_write + supports=true + op key 在场 + key_forwarded=false
    /// → auto_approvable=false（硬规则不因声明放宽）。
    #[test]
    fn declared_but_not_forwarded_requires_manual_approval() {
        let a = assess_provider_declared(
            "mcp:fake:send",
            "external_write",
            "manual",
            false,
            Some("op_stable_1"),
            false, // 未转发（执行证据缺失）
            None,
            None,
            true, // 声明支持幂等键
            true, // 声明可查证
        );
        assert!(a.external_write);
        assert!(a.supports_idempotency_key);
        assert!(!a.key_forwarded, "声明≠转发：证据缺失");
        assert!(!auto_approvable(&a), "声明支持但未转发 → 强制人工审批");
        // 对照：转发证据齐全 + 可查证 → 硬规则放行（仍受 ToolRule 风险分级约束）。
        let b = assess_provider_declared(
            "mcp:fake:send",
            "external_write",
            "compensatable",
            false,
            Some("op_stable_1"),
            true,
            Some("per-server, 24h"),
            Some("snapshot-1"),
            true,
            true,
        );
        assert!(auto_approvable(&b), "证据齐全的外部写通过硬上限");
    }

    fn base() -> ExecutionRiskAssessment {
        ExecutionRiskAssessment {
            tool: "apply_patch".into(),
            effect_class: "local_write".into(),
            reversibility: Reversibility::LogicalRestore,
            compensation: None,
            supports_idempotency_key: true,
            operation_idempotency_key: Some("op-1".into()),
            idempotency_scope: None,
            key_forwarded: true,
            reconcile_query: true,
            external_write: false,
            protected_target: false,
            policy_version: POLICY_VERSION.into(),
        }
    }

    /// 穷举（RDWS-002）：硬规则每条独立触发即 false；基线其余组合为 true。
    #[test]
    fn hard_rules_exhaustive() {
        // 基线：本地写 + 可恢复 + 键链完备 → 可自动批准上限成立。
        assert!(
            auto_approvable(&base()),
            "基线 local_write 应达自动批准上限"
        );
        // 只读。
        let mut a = base();
        a.effect_class = "read".into();
        assert!(auto_approvable(&a));
        // 规则 1：irreversible effect。
        let mut a = base();
        a.effect_class = "irreversible".into();
        a.external_write = true;
        assert!(!auto_approvable(&a), "irreversible effect 恒不自动批");
        // 规则 2：manual / irreversible 可恢复性（无条件，即使只读）。
        for rev in [Reversibility::Manual, Reversibility::Irreversible] {
            let mut a = base();
            a.reversibility = rev;
            a.effect_class = "read".into();
            assert!(!auto_approvable(&a), "{rev:?} 恒不自动批（无条件）");
        }
        for rev in [Reversibility::LogicalRestore, Reversibility::Compensatable] {
            let mut a = base();
            a.reversibility = rev;
            assert!(auto_approvable(&a), "{rev:?} 不触发规则 2");
        }
        // 规则 3：external_write 的幂等键链三要件——任一缺失即 false。
        let ext = || ExecutionRiskAssessment {
            effect_class: "external_write".into(),
            external_write: true,
            reconcile_query: true,
            ..base()
        };
        assert!(auto_approvable(&ext()), "键链完备的外部写可达上限");
        let mut a = ext();
        a.supports_idempotency_key = false;
        assert!(!auto_approvable(&a), "Provider 不支持幂等键");
        let mut a = ext();
        a.operation_idempotency_key = None;
        assert!(!auto_approvable(&a), "本次未生成稳定键");
        let mut a = ext();
        a.key_forwarded = false;
        assert!(!auto_approvable(&a), "键未实际转发（无证据）");
        // 规则 4：external_write 且不可查证。
        let mut a = ext();
        a.reconcile_query = false;
        assert!(!auto_approvable(&a), "外部写无结果查证");
        // 非 external_write 时键链缺失不阻断（本地写无外部幂等面）。
        let mut a = base();
        a.supports_idempotency_key = false;
        a.operation_idempotency_key = None;
        a.key_forwarded = false;
        assert!(auto_approvable(&a), "本地写不受键链约束");
    }

    /// 保守评估：注册表声明 → MCP 外部写不可自动批；builtin 本地写可；未知词汇 fail-closed。
    #[test]
    fn registry_assessment_conservative() {
        let mcp_write = assess_registry_tool(
            "mcp__srv__send",
            "external_write",
            "compensatable",
            false,
            None,
            false,
            None,
            None,
        );
        assert!(
            !auto_approvable(&mcp_write),
            "MCP 外部写（无键证据）不可自动批"
        );
        assert!(!mcp_write.reconcile_query, "MCP 查证能力保守为 false");
        let builtin = assess_registry_tool(
            "apply_patch",
            "local_write",
            "logical_restore",
            false,
            None,
            false,
            None,
            None,
        );
        assert!(
            auto_approvable(&builtin),
            "builtin 本地写可达上限（仍受 ToolRule 约束）"
        );
        let unknown_rev = assess_registry_tool(
            "x",
            "local_write",
            "who-knows",
            false,
            None,
            false,
            None,
            None,
        );
        assert_eq!(
            unknown_rev.reversibility,
            Reversibility::Manual,
            "未知词汇按 manual"
        );
        assert!(!auto_approvable(&unknown_rev));
        let protected = assess_registry_tool(
            "deploy",
            "external_write",
            "compensatable",
            true,
            Some("op-9"),
            true,
            None,
            None,
        );
        assert!(protected.protected_target, "受保护目标声明进入评估");
        assert_eq!(protected.policy_version, POLICY_VERSION);
    }

    #[test]
    fn flag_defaults_off() {
        // 不读环境直接测默认语义：enabled 仅在显式 =1 时开（默认 0）。
        // （CI 环境可能注入变量，这里只验证函数契约的显式开启路径。）
        std::env::set_var(FLAG, "1");
        assert!(enabled());
        std::env::set_var(FLAG, "0");
        assert!(!enabled(), "=0 是 kill switch");
        std::env::remove_var(FLAG);
        // 默认关（若环境未注入）。
        assert_eq!(enabled(), std::env::var(FLAG).is_ok());
    }
}
