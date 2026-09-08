//! P1-1（审计 §8 P1-1 / RDWS-004）：ToolProvider 真实 adapter——Builtin 与 MCP
//! 各一，orchestrator（tool_exec::execute_tool）只依赖 trait。
//!
//! 治理权全留 orchestrator：registry 解析、WP-4 冻结复核、PlanGuard/Autonomy/
//! Grant ledger、outcome 状态机、send_phase、audit 都不在 adapter 内（trait 契约
//! 「实现不得自行做治理判定」）。adapter 只封装执行面：
//! - `descriptor()`：身份/分类/能力声明（治理消费）；
//! - `execute(args, op_key)`：执行 + `ProviderCall.key_forwarded` 证据——
//!   `op_key` 是 Core 生成的稳定操作幂等键（proposal 级），adapter 如实回报
//!   是否已转发进 Provider 请求；**未转发 = false**（声明支持但未转发 →
//!   风险硬规则强制人工审批，见 sg-policy risk_model 单测）。
//!
//! MCP 阶段说明：stdio MCP 协议无标准幂等键字段，本阶段不向请求注入键
//! （key_forwarded 恒 false，外部写自动批准保持关死）；转发 adapter 与
//! Provider 侧确认协议落地后翻转为声明支持 + 证据回报。

use crate::tool_exec::{mcp_invoke, resolve_active_mcp};
use serde_json::Value;
use sg_agent::provider::{
    ProviderCall, ProviderCapabilities, ProviderDescriptor, ProviderOutcome, ToolId, ToolProvider,
};
use sg_agent::tools::{self, ToolCtx};
use sg_agent::Proposal;
use sg_store::Store;
use std::sync::Arc;

/// 治理前置解析结果（orchestrator 侧；adapter 构造输入）。
#[allow(clippy::large_enum_variant)] // 两 variant 均轻量字段（name/def vs active 元数据）
pub(crate) enum ResolvedProvider {
    Builtin {
        name: String,
        def: &'static tools::ToolDef,
    },
    Mcp {
        server: String,
        tool: String,
        active: sg_settings::mcp_ext::ActiveMcpTool,
    },
}

/// orchestrator 的 provider 解析步：registry/激活/冻结复核（治理，先于 trait）。
pub(crate) fn resolve_provider(
    store: &Arc<Store>,
    p: &Proposal,
) -> Result<(String, ResolvedProvider), String> {
    let tool_id = ToolId::parse(&p.tool);
    let gov_name = tool_id.short_name();
    match tool_id {
        ToolId::Mcp { server, tool } => {
            let active = resolve_active_mcp(store, &server, &tool)?;
            // WP-4：导入型 server call 前冻结复核（漂移 fail-closed）——治理步留此。
            if !active.import_id.is_empty() {
                sg_settings::mcp_import::verify_freeze(store, &store.data_dir, &active.import_id)?;
            }
            Ok((
                gov_name,
                ResolvedProvider::Mcp {
                    server,
                    tool,
                    active,
                },
            ))
        }
        ToolId::Builtin { name } => {
            let def = tools::find(&name).ok_or_else(|| format!("unknown tool: {}", p.tool))?;
            Ok((gov_name, ResolvedProvider::Builtin { name, def }))
        }
    }
}

pub(crate) fn make_provider<'a>(
    ctx: &'a ToolCtx,
    store: &'a Arc<Store>,
    project_id: &'a str,
    p: &'a Proposal,
    resolved: &'a ResolvedProvider,
) -> Box<dyn ToolProvider + 'a> {
    match resolved {
        ResolvedProvider::Builtin { name, def } => Box::new(BuiltinToolProvider {
            ctx,
            store,
            project_id,
            p,
            name,
            def,
        }),
        ResolvedProvider::Mcp {
            server,
            tool,
            active,
        } => Box::new(McpToolProvider {
            ctx,
            store,
            p,
            server,
            tool,
            active,
        }),
    }
}

/// descriptor 分类（治理消费面：plan_guard/autonomy 的 effect 词汇）。
pub(crate) fn effect_class_of(resolved: &ResolvedProvider) -> &'static str {
    match resolved {
        // builtin 以注册表声明为准（parse_effect 同口径）。
        ResolvedProvider::Builtin { def, .. } => def.effect_class,
        // MCP：readOnlyHint → read；否则保守 external_write（M6 分类步）。
        ResolvedProvider::Mcp { active, .. } => {
            if active.read_only {
                "read"
            } else {
                "external_write"
            }
        }
    }
}

/// 输出截断上限（进模型消息的截断版；M6：MCP 64KiB）。
pub(crate) fn max_result_bytes_of(resolved: &ResolvedProvider) -> usize {
    match resolved {
        ResolvedProvider::Builtin { def, .. } => def.max_result_bytes,
        ResolvedProvider::Mcp { .. } => 64 * 1024,
    }
}

/// Builtin 执行面：注册表工具的本机执行（write_draft/apply_patch/检索/executor）。
pub(crate) struct BuiltinToolProvider<'a> {
    pub ctx: &'a ToolCtx,
    pub store: &'a Arc<Store>,
    pub project_id: &'a str,
    pub p: &'a Proposal,
    pub name: &'a str,
    pub def: &'static tools::ToolDef,
}

impl ToolProvider for BuiltinToolProvider<'_> {
    fn descriptor(&self) -> Result<ProviderDescriptor, String> {
        Ok(ProviderDescriptor {
            tool_id: ToolId::Builtin {
                name: self.name.to_string(),
            },
            effect_class: self.def.effect_class.to_string(),
            reversibility: self.def.reversibility.to_string(),
            protected_target: self.def.protected_target,
            schema_digest: (self.def.parameters)().to_string(),
            capabilities: ProviderCapabilities {
                // builtin 本地执行结果同步确定性可知；无外部幂等面。
                supports_idempotency_key: true,
                idempotency_scope: None,
                reconcile_query: true,
            },
        })
    }

    fn probe(&self) -> Result<(), String> {
        // 注册表在册即Probe 通过（resolve_provider 已保证）。
        Ok(())
    }

    fn execute(&self, args: &Value, _op_key: &str) -> Result<ProviderCall, String> {
        // builtin 无外部 Provider——键不适用（硬规则只在 external_write 要求证据）。
        let outcome = (|| -> Result<ProviderOutcome, String> {
            let out = match self.name {
                "write_file" => tools::write_draft(self.ctx, args)?,
                "apply_patch" => crate::tool_exec::apply_patch_exec(self.ctx, self.p, args)?,
                "search_knowledge" => {
                    let query = args
                        .get("query")
                        .and_then(|v| v.as_str())
                        .ok_or("missing argument: query")?;
                    let limit = args
                        .get("limit")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(5)
                        .clamp(1, 20);
                    let hits =
                        sg_knowledge::search_v2(self.store, self.project_id, query, false, limit)
                            .map_err(|e| e.to_string())?;
                    serde_json::to_string(&hits).map_err(|e| e.to_string())?
                }
                _ => {
                    // P0-4：隔离执行域不可用时拒绝可写命令（只读工具放行）。
                    if self.ctx.read_only && self.name == "run_command" {
                        return Err(
                            "action_denied: 隔离 worktree 不可用，拒绝在用户主工作区执行命令"
                                .into(),
                        );
                    }
                    let manifest = tools::build_manifest(self.def, args, self.ctx)?;
                    let result = sg_executor::execute_with_cancel(
                        self.ctx.mode,
                        &manifest,
                        self.ctx.cancel.as_deref(),
                    )
                    .map_err(|e| e.to_string())?;
                    if result.cancelled {
                        return Err("run_cancelled: 取消请求已中止在途工具执行".into());
                    }
                    serde_json::to_string(&result).map_err(|e| e.to_string())?
                }
            };
            Ok(ProviderOutcome::Completed { output: out })
        })()?;
        match outcome {
            ProviderOutcome::Completed { output } => Ok(ProviderCall {
                outcome: ProviderOutcome::Completed { output },
                key_forwarded: false,
            }),
            other => Ok(ProviderCall {
                outcome: other,
                key_forwarded: false,
            }),
        }
    }

    fn query_result(&self, _call_ref: &str) -> Result<Option<Value>, String> {
        // builtin 同步返回确定性可知（risk-model v1 口径 reconcile_query=true）。
        Ok(Some(serde_json::json!({"source": "synchronous_return"})))
    }

    fn cancel(&self) -> Result<(), String> {
        Ok(()) // 取消经 ToolCtx.cancel token（orchestrator 持有）
    }
}

/// MCP 执行面：受控 stdio server 的 phased 调用（send_phase/outcome 状态机在
/// mcp_invoke 治理链内；adapter 只触发）。
pub(crate) struct McpToolProvider<'a> {
    pub ctx: &'a ToolCtx,
    pub store: &'a Arc<Store>,
    pub p: &'a Proposal,
    pub server: &'a str,
    pub tool: &'a str,
    pub active: &'a sg_settings::mcp_ext::ActiveMcpTool,
}

impl ToolProvider for McpToolProvider<'_> {
    fn descriptor(&self) -> Result<ProviderDescriptor, String> {
        let effect = if self.active.read_only {
            "read"
        } else {
            "external_write"
        };
        Ok(ProviderDescriptor {
            tool_id: ToolId::Mcp {
                server: self.server.to_string(),
                tool: self.tool.to_string(),
            },
            effect_class: effect.to_string(),
            reversibility: if self.active.read_only {
                "logical_restore"
            } else {
                "manual"
            }
            .to_string(),
            protected_target: false,
            schema_digest: self.active.schema_digest.clone(),
            capabilities: ProviderCapabilities::conservative(),
        })
    }

    fn probe(&self) -> Result<(), String> {
        // 治理前置（激活/冻结复核）已在 resolve_provider 完成；执行面可用性
        // 以 mcp_invoke 的 transport/沙箱检查为准（失败即 Failed）。
        Ok(())
    }

    fn execute(&self, args: &Value, _op_key: &str) -> Result<ProviderCall, String> {
        // stdio MCP 协议无标准幂等键字段：不注入键、不声称转发（key_forwarded
        // 恒 false——外部写自动批准保持关死；转发证据协议落地后翻转）。
        match mcp_invoke(
            self.ctx,
            self.store,
            self.p,
            self.server,
            self.tool,
            self.active,
            args,
        ) {
            Ok(out) => Ok(ProviderCall {
                outcome: ProviderOutcome::Completed { output: out },
                key_forwarded: false,
            }),
            Err(e) => Ok(ProviderCall {
                outcome: ProviderOutcome::Failed { error: e },
                key_forwarded: false,
            }),
        }
    }

    fn query_result(&self, _call_ref: &str) -> Result<Option<Value>, String> {
        // P1-1 阶段：MCP 无结果查证能力（保守 None；reconcile_query=false）。
        Ok(None)
    }

    fn cancel(&self) -> Result<(), String> {
        Ok(()) // 取消经 send_phase/超时治理链
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sg_agent::provider::operation_key;

    /// P1-1 退出标准：fake external provider「声明支持但未转发」→ 评估强制人工。
    struct FakeExternalProvider {
        forward_key: bool,
    }
    impl ToolProvider for FakeExternalProvider {
        fn descriptor(&self) -> Result<ProviderDescriptor, String> {
            Ok(ProviderDescriptor {
                tool_id: ToolId::Mcp {
                    server: "fake".into(),
                    tool: "send".into(),
                },
                effect_class: "external_write".into(),
                reversibility: "compensatable".into(),
                protected_target: false,
                schema_digest: "sha256:fake".into(),
                capabilities: ProviderCapabilities {
                    supports_idempotency_key: true,
                    idempotency_scope: Some("per-server, 24h".into()),
                    reconcile_query: true,
                },
            })
        }
        fn probe(&self) -> Result<(), String> {
            Ok(())
        }
        fn execute(&self, _args: &Value, op_key: &str) -> Result<ProviderCall, String> {
            // 声明支持，但本实现未把键放进请求（转发缺失——客户端 bug/协议未接）。
            let _ = op_key;
            Ok(ProviderCall {
                outcome: ProviderOutcome::Completed {
                    output: "done".into(),
                },
                key_forwarded: self.forward_key,
            })
        }
        fn query_result(&self, _call_ref: &str) -> Result<Option<Value>, String> {
            Ok(Some(serde_json::json!({"ok": true})))
        }
        fn cancel(&self) -> Result<(), String> {
            Ok(())
        }
    }

    fn assess_after_call(
        p: &FakeExternalProvider,
    ) -> sg_policy::risk_model::ExecutionRiskAssessment {
        let desc = p.descriptor().unwrap();
        let caps = &desc.capabilities;
        let key = operation_key("run_1", "prop_1");
        let call = p.execute(&serde_json::json!({"x": 1}), &key).unwrap();
        sg_policy::risk_model::assess_provider_declared(
            "mcp:fake:send",
            &desc.effect_class,
            &desc.reversibility,
            desc.protected_target,
            Some(&key),
            call.key_forwarded,
            caps.idempotency_scope.as_deref(),
            None,
            caps.supports_idempotency_key,
            caps.reconcile_query,
        )
    }

    #[test]
    fn declared_but_not_forwarded_forces_manual_approval() {
        let missing = FakeExternalProvider { forward_key: false };
        let a = assess_after_call(&missing);
        assert!(a.external_write && a.supports_idempotency_key);
        assert!(!a.key_forwarded, "声明支持但执行未转发");
        assert!(
            !sg_policy::risk_model::auto_approvable(&a),
            "P1-1：声明支持但未转发 → 强制人工审批"
        );

        let honest = FakeExternalProvider { forward_key: true };
        let b = assess_after_call(&honest);
        assert!(b.key_forwarded);
        assert!(
            sg_policy::risk_model::auto_approvable(&b),
            "转发证据齐全 + 可查证 + 可补偿 → 硬上限放行"
        );
    }

    #[test]
    fn operation_key_is_stable_per_proposal() {
        assert_eq!(
            operation_key("run_1", "prop_1"),
            operation_key("run_1", "prop_1")
        );
        assert_ne!(
            operation_key("run_1", "prop_1"),
            operation_key("run_1", "prop_2")
        );
        assert_ne!(
            operation_key("run_1", "prop_1"),
            operation_key("run_2", "prop_1")
        );
    }
}
