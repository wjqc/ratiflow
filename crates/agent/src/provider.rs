//! ToolProvider contract（RDWS 实施计划 v1.4 WP-2）：
//! 可执行工具型插件的统一执行面——Builtin 与 MCP 各一 adapter，**治理权全留 Core
//! orchestrator**（registry/policy/审批/budget/秘密扫描/outcome 状态机/audit/reconcile，
//! 见 sixgates-core tool_exec::execute_tool）。本 trait 只描述执行能力：
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

/// 工具执行能力描述（descriptor 面；治理消费）。
#[derive(Debug, Clone)]
pub struct ProviderDescriptor {
    pub tool_id: ToolId,
    pub effect_class: String,
    pub reversibility: String,
    pub protected_target: bool,
    pub schema_digest: String,
}

/// ToolProvider：可执行工具型插件的执行面。Core orchestrator 是唯一调用方；
/// 实现不得自行做 registry/policy/审批/budget 判定（治理分叉即违约）。
pub trait ToolProvider {
    fn descriptor(&self) -> Result<ProviderDescriptor, String>;
    /// 可用性探测（失败 = 不可执行，orchestrator 拒绝派发）。
    fn probe(&self) -> Result<(), String>;
    /// 执行一次调用。返回 ProviderOutcome；实现不重试、不写治理事实。
    fn execute(&self, args: &Value) -> Result<ProviderOutcome, String>;
    /// 结果查证能力（对账查询；不可查证返回 None）。
    fn query_result(&self, call_ref: &str) -> Result<Option<Value>, String>;
    /// 取消在途执行（尽力而为；无在途面返回 Ok(())）。
    fn cancel(&self) -> Result<(), String>;
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
