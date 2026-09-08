//! ToolProvider contract（RDWS 实施计划 v1.4 WP-2）：
//! 可执行工具型插件的统一执行面——Builtin 与 MCP 各一 adapter，**治理权全留 Core
//! orchestrator**（registry/policy/审批/budget/秘密扫描/outcome 状态机/audit/reconcile，
//! 见 ratiflow-core tool_exec::execute_tool）。本 trait 只描述执行能力：
//! descriptor/probe/execute/query_result/cancel；返回 ProviderOutcome，**不决定重试**。
//!
//! ToolId 持久化契约（0042 tool_provider 列）：
//! - canonical：内置=`builtin:<name>`，MCP=`mcp:<server>:<tool>`；
//! - server/tool 名约束 `[A-Za-z0-9_-]`（与 `:`/`__` 无歧义）；
//! - 写侧一律 canonical；读侧对旧 `mcp__server__tool` 无歧义映射（首个 `__` 切分，
//!   server 不含 `_`×2 的字符集保证唯一性——废除逐段试探）；
//! - 序列化以 `toolid_tests` 测试向量固定。

use serde_json::Value;

/// Provider 类别（tool_proposals.tool_provider 列值）。
pub const PROVIDER_BUILTIN: &str = "builtin";
pub const PROVIDER_MCP: &str = "mcp";

/// 解析后的工具身份（读侧接受 canonical 与 legacy 两种形态，比较基于解析结果）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolId {
    Builtin { name: String },
    Mcp { server: String, tool: String },
}

impl ToolId {
    /// canonical 序列化（写侧唯一形态）。
    pub fn canonical(&self) -> String {
        match self {
            ToolId::Builtin { name } => format!("builtin:{name}"),
            ToolId::Mcp { server, tool } => format!("mcp:{server}:{tool}"),
        }
    }

    /// provider 类别（tool_proposals.tool_provider 列）。
    pub fn provider(&self) -> &'static str {
        match self {
            ToolId::Builtin { .. } => PROVIDER_BUILTIN,
            ToolId::Mcp { .. } => PROVIDER_MCP,
        }
    }

    /// 治理/展示用的短名（策略与注册表键；legacy 行为等价）。
    pub fn short_name(&self) -> String {
        match self {
            ToolId::Builtin { name } => name.clone(),
            ToolId::Mcp { server, tool } => format!("mcp__{server}__{tool}"),
        }
    }

    /// 读侧解析：canonical（builtin:/mcp:）优先；legacy `mcp__server__tool` 首个 `__`
    /// 切分（字符集 [A-Za-z0-9_-] 下无歧义）；不认识的形态按 builtin 短名处理
    /// （历史行/裸名），由注册表查找不到时显式失败。
    pub fn parse(tool: &str) -> ToolId {
        if let Some(rest) = tool.strip_prefix("builtin:") {
            return ToolId::Builtin {
                name: rest.to_string(),
            };
        }
        if let Some(rest) = tool.strip_prefix("mcp:") {
            // mcp:<server>:<tool>——server 无 `:`（字符集约束），首个切分无歧义。
            if let Some((server, tool)) = rest.split_once(':') {
                return ToolId::Mcp {
                    server: server.to_string(),
                    tool: tool.to_string(),
                };
            }
        }
        if let Some(rest) = tool.strip_prefix("mcp__") {
            if let Some((server, tool)) = rest.split_once("__") {
                return ToolId::Mcp {
                    server: server.to_string(),
                    tool: tool.to_string(),
                };
            }
        }
        ToolId::Builtin {
            name: tool.to_string(),
        }
    }

    /// 写侧规范化：canonical（已知 MCP/显式前缀）；裸内置名 → builtin:<name>。
    pub fn canonicalize(tool: &str) -> String {
        ToolId::parse(tool).canonical()
    }
}

/// Provider 执行结果（不决定重试；重试治理在 orchestrator）。
#[derive(Debug, Clone)]
pub enum ProviderOutcome {
    Completed {
        output: String,
    },
    Failed {
        error: String,
    },
    /// 副作用可能已发生/不可确认——orchestrator 落 unknown + reconciliation。
    Indeterminate {
        note: String,
        partial: Option<String>,
    },
}

/// 一次 Provider 调用的执行证据（P1-1：external write 的幂等键转发面）。
/// `key_forwarded` = adapter 已把 Core 生成的稳定 operation key 放进
/// Provider 请求且获得可审计证据（如响应确认）；**声明支持但未转发 = false**
/// ——风险评估据此强制人工审批（auto_approvable 硬规则）。
#[derive(Debug, Clone)]
pub struct ProviderCall {
    pub outcome: ProviderOutcome,
    pub key_forwarded: bool,
}

/// Provider 幂等/对账能力声明（P1-1；descriptor 治理消费面）。
/// 声明与执行证据分离：capabilities 说"能"，ProviderCall 证明"做了"。
#[derive(Debug, Clone)]
pub struct ProviderCapabilities {
    /// Provider 是否支持幂等键去重（≠ 本次已传键）。
    pub supports_idempotency_key: bool,
    /// 去重作用域/有效期说明（能力描述；None = 未声明）。
    pub idempotency_scope: Option<String>,
    /// 结果查证能力（对账查询可否）。
    pub reconcile_query: bool,
}

impl ProviderCapabilities {
    /// 保守缺省：不支持幂等键、不可查证——外部写自动批准被硬规则关死。
    pub fn conservative() -> Self {
        ProviderCapabilities {
            supports_idempotency_key: false,
            idempotency_scope: None,
            reconcile_query: false,
        }
    }
}

/// 工具执行能力描述（descriptor 面；治理消费）。
#[derive(Debug, Clone)]
pub struct ProviderDescriptor {
    pub tool_id: ToolId,
    pub effect_class: String,
    pub reversibility: String,
    pub protected_target: bool,
    pub schema_digest: String,
    pub capabilities: ProviderCapabilities,
}

/// ToolProvider：可执行工具型插件的执行面。Core orchestrator 是唯一调用方；
/// 实现不得自行做 registry/policy/审批/budget 判定（治理分叉即违约）。
pub trait ToolProvider {
    fn descriptor(&self) -> Result<ProviderDescriptor, String>;
    /// 可用性探测（失败 = 不可执行，orchestrator 拒绝派发）。
    fn probe(&self) -> Result<(), String>;
    /// 执行一次调用。`op_key` = Core 为本次外部写生成的稳定操作幂等键
    /// （proposal 级稳定，重放同键）；adapter 负责转发进 Provider 请求并在
    /// `ProviderCall.key_forwarded` 如实回报证据。实现不重试、不写治理事实。
    fn execute(&self, args: &Value, op_key: &str) -> Result<ProviderCall, String>;
    /// 结果查证能力（对账查询；不可查证返回 None）。
    fn query_result(&self, call_ref: &str) -> Result<Option<Value>, String>;
    /// 取消在途执行（尽力而为；无在途面返回 Ok(())）。
    fn cancel(&self) -> Result<(), String>;
}

/// 按工具身份从注册表/激活面派生能力声明（评估时消费；执行时以 ProviderCall
/// 证据为准——声明与证据分离）。保守缺省：未注册/不可解析 → 全 false。
pub fn capabilities_for(store: &sg_store::Store, tool: &str) -> ProviderCapabilities {
    match ToolId::parse(tool) {
        ToolId::Builtin { name } => crate::tools::find(&name)
            .map(|_| ProviderCapabilities {
                // builtin 本地执行结果同步确定性可知；无外部 Provider 幂等面
                //（硬规则只在 external_write 时要求键链证据）。
                supports_idempotency_key: true,
                idempotency_scope: None,
                reconcile_query: true,
            })
            .unwrap_or_else(ProviderCapabilities::conservative),
        ToolId::Mcp { server, tool: t } => {
            sg_settings::mcp_ext::active_tool_for_invocation(store, &server, &t)
                .ok()
                .map(|_| ProviderCapabilities {
                    // MCP P1-1 阶段：无键转发 adapter 证据——声明不支持
                    //（外部写自动批准关死；转发证据落地后翻转为声明支持 +
                    // ProviderCall.key_forwarded 证据）。
                    supports_idempotency_key: false,
                    idempotency_scope: None,
                    reconcile_query: false,
                })
                .unwrap_or_else(ProviderCapabilities::conservative)
        }
    }
}

/// Core 生成的稳定操作幂等键（P1-1 / RDWS-002）：sha256(op|run|proposal)——
/// 同一 proposal 的重放/对账恒同键；评估与执行两处共用（冻结进 proposal 详情
/// 与 audit risk.assessment）。
pub fn operation_key(run_id: &str, proposal_id: &str) -> String {
    use sha2::Digest;
    format!(
        "op_{}",
        sg_store::ids::hex(&sha2::Sha256::digest(
            format!("op|{run_id}|{proposal_id}").as_bytes()
        ))
    )
}

#[cfg(test)]
mod toolid_tests {
    use super::*;

    /// 序列化测试向量（WP-2：写侧 canonical、读侧 legacy 无歧义映射，向量固定）。
    #[test]
    fn canonical_vectors() {
        let cases = [
            (
                "read_file",
                "builtin:read_file",
                ToolId::Builtin {
                    name: "read_file".into(),
                },
            ),
            (
                "mcp__gitlab__create_issue",
                "mcp:gitlab:create_issue",
                ToolId::Mcp {
                    server: "gitlab".into(),
                    tool: "create_issue".into(),
                },
            ),
        ];
        for (input, canonical, parsed) in cases {
            assert_eq!(ToolId::canonicalize(input), canonical, "{input} canonical");
            assert_eq!(ToolId::parse(input), parsed, "{input} 解析");
            assert_eq!(
                ToolId::parse(canonical),
                parsed,
                "{canonical} canonical 解析"
            );
            assert_eq!(parsed.canonical(), canonical, "往返一致");
        }
    }

    /// legacy 首个 `__` 切分即唯一解：tool 名含单下划线不歧义。
    #[test]
    fn legacy_split_unambiguous() {
        let id = ToolId::parse("mcp__srv__send_thing");
        assert_eq!(
            id,
            ToolId::Mcp {
                server: "srv".into(),
                tool: "send_thing".into()
            },
            "tool 内单下划线不参与切分"
        );
        assert_eq!(id.provider(), PROVIDER_MCP);
        assert_eq!(
            ToolId::parse("builtin:run_command").provider(),
            PROVIDER_BUILTIN
        );
    }

    /// 畸形形态 fail-safe：未知前缀按裸内置名处理（注册表 miss 显式失败，不误路由 MCP）。
    #[test]
    fn malformed_falls_to_builtin_lookup() {
        assert_eq!(
            ToolId::parse("mcp:"),
            ToolId::Builtin {
                name: "mcp:".into()
            },
            "缺 server:tool 的 mcp: 不误判 MCP"
        );
        assert_eq!(
            ToolId::parse("mcp__orphan"),
            ToolId::Builtin {
                name: "mcp__orphan".into()
            },
            "缺第二段的 legacy 不误判 MCP"
        );
    }
}
