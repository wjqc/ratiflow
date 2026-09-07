//! Agent Run + 模型网关（v2 ADR-022 行为等价）：
//! 模型只能产出 ToolCallProposal；出网内容统一裁剪/脱敏/预算；completed_execution ≠ 过关。
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sg_integrations::model::{ChatMessage, CompletionRequest};
use sg_policy::{self, PolicyError, Snapshot};
use sg_store::{ids, outbox, timefmt, Error, Store};

pub mod instructions;
pub mod middleware;
pub mod model_protocol;
pub mod modelgw;
pub mod patch;
pub mod profile;
pub mod prompt;
pub mod provider;
pub mod reasoning_state;
pub mod rollout;
pub mod router;
pub mod schema;
pub mod team;
pub mod tools;
pub use modelgw::{Budget, Gateway, Usage};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunBudget {
    pub max_tool_calls: i64,
    pub max_duration_sec: i64,
    pub model: Budget,
}

impl Default for RunBudget {
    fn default() -> Self {
        Self {
            max_tool_calls: 40,
            max_duration_sec: 1800,
            model: Budget::default(),
        }
    }
}

/// 自动压缩策略（F09/M3）：输入估算超阈值触发；keep_turns 保留最近 K 轮工具往返。
#[derive(Clone, Debug)]
pub struct CompactPolicy {
    pub threshold_tokens: usize,
    pub keep_turns: usize,
}

impl Default for CompactPolicy {
    fn default() -> Self {
        Self {
            threshold_tokens: 24000,
            keep_turns: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentRun {
    pub id: String,
    pub workitem_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub task_id: String,
    pub goal: String,
    pub status: String,
    pub result: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Proposal {
    pub id: String,
    pub run_id: String,
    pub tool: String,
    pub arguments: String,
    pub risk: String,
    pub action_digest: String,
    pub decision: String,
    pub result: String,
    pub created_at: String,
}

/// 工具执行回调（装配层注入：executor manifest 映射）。
pub type ToolExecutor = dyn Fn(&Proposal) -> Result<String, String> + Send + Sync;

pub struct RunOutput {
    pub run: AgentRun,
    /// 输出文本（供 UI/工件草稿使用）。
    pub output: String,
}

/// 启动一次 Run（同步循环，受预算/迭代/取消约束）。
/// 取消旗标：迭代边界与工具执行前检查；置位后以 cancelled 终态收尾。
#[derive(Debug, Clone)]
pub struct RunConfig<'a> {
    pub workitem_id: &'a str,
    pub task_id: &'a str,
    pub goal: &'a str,
    pub manifest_id: &'a str,
    pub tool_allowlist: &'a [String],
    pub idempotency_key: &'a str,
    pub budget: &'a RunBudget,
    pub max_iterations: usize,
}

pub fn start(
    store: &Store,
    gateway: &Gateway,
    policy: &Snapshot,
    executor: Option<&ToolExecutor>,
    config: &RunConfig<'_>,
) -> Result<RunOutput, Error> {
    let (run, created) = create_run(store, config)?;
    if !created {
        let output = run.result.clone();
        return Ok(RunOutput { run, output });
    }
    // 默认装配（无边界/知识——测试与旧调用方语义；装配层用 prompt::assemble 注入完整段）。
    let initial = prompt::assemble(
        &prompt::PromptEnv::default(),
        config.tool_allowlist,
        &prompt::knowledge_text("", ""),
        config.goal,
    );
    execute_run(
        store,
        gateway,
        policy,
        executor,
        config,
        &run.id,
        None,
        None,
        &initial,
        &CompactPolicy::default(),
        None,
    )
}

/// 建行（幂等）：重复 idempotency_key 返回既有 Run，created=false。
/// status→running 时发 run.started 事件（M0-② 契约事件）。
pub fn create_run(store: &Store, config: &RunConfig<'_>) -> Result<(AgentRun, bool), Error> {
    let RunConfig {
        workitem_id,
        task_id,
        goal,
        manifest_id,
        tool_allowlist,
        idempotency_key,
        budget,
        ..
    } = *config;
    if workitem_id.is_empty() || goal.is_empty() || manifest_id.is_empty() {
        return Err(Error::Message("workitem/goal/manifest required".into()));
    }
    // 幂等。
    let existing: Option<String> = store.with_conn(|conn| {
        let result: rusqlite::Result<String> = conn.query_row(
            "SELECT id FROM agent_runs WHERE idempotency_key=?1",
            [idempotency_key],
            |r| r.get(0),
        );
        Ok(result.ok())
    })?;
    if let Some(id) = existing {
        let run = get_run(store, &id)?;
        return Ok((run, false));
    }

    let id = ids::new_id("run");
    let now = timefmt::now();
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO agent_runs(id, workitem_id, task_id, goal, input_baseline_sha, context_manifest_id,
                tool_allowlist, budget, policy_snapshot, idempotency_key, status, created_at, updated_at)
             VALUES (?1,?2,?3,?4,'',?5,?6,?7,'default',?8,'queued',?9,?9)",
            rusqlite::params![id, workitem_id, task_id, goal, manifest_id,
                serde_json::to_string(tool_allowlist).unwrap_or_default(),
                serde_json::to_string(budget).unwrap_or_default(),
                idempotency_key, now],
        )?;
        Ok(())
    })?;
    let mut run = get_run(store, &id)?;
    set_status(store, &mut run, "running", "run.started")?;
    Ok((run, true))
}

/// 单步结果（M1/F03）：NeedsApproval 表示挂起等待审批（Run 置 paused 并写 checkpoint）。
pub enum StepOutcome {
    /// 继续：返回给模型的 tool 消息文本。
    Continue(String),
    /// 挂起：审批已请求、提案保持 proposed。
    NeedsApproval { proposal_id: String, tool: String },
}

/// 执行既有 Run 的循环（M0-② 起由装配层在独立任务/连接上驱动；M1 支持从 waiting_approval 恢复）。
/// 恢复语义：messages/迭代号/挂起提案取自 checkpoint；预算台账不入 checkpoint——
/// 模型调用数/工具数从库重算（单一事实源，不重复计费）。
/// M2（ADR-033）：cancel 为取消令牌——模型 HTTP/SSE 可即时中止；forwarder 转发
/// 易失 UI delta（不落库）。
#[allow(clippy::too_many_arguments)]
pub fn execute_run(
    store: &Store,
    gateway: &Gateway,
    policy: &Snapshot,
    executor: Option<&ToolExecutor>,
    config: &RunConfig<'_>,
    run_id: &str,
    cancel: Option<&sg_integrations::CancelToken>,
    mut rollout: Option<crate::rollout::Rollout>,
    initial: &prompt::InitialTurn,
    compact: &CompactPolicy,
    forwarder: Option<std::sync::Arc<dyn modelgw::TurnDeltaForwarder>>,
) -> Result<RunOutput, Error> {
    let RunConfig {
        goal: _goal,
        tool_allowlist,
        budget,
        max_iterations,
        ..
    } = *config;
    let system_prompt = initial.system_prompt.clone();
    let mut run = get_run(store, run_id)?;
    let noop_cancel = sg_integrations::CancelToken::new();
    let cancel = cancel.unwrap_or(&noop_cancel);
    let cancelled = || cancel.is_cancelled();

    // M4：Run 启动冻结——Provider 指纹（checkpoint 回放绑定）、缓存域键、压缩策略。
    let provider_fingerprint = gateway.fingerprint();
    let compaction_strategy =
        modelgw::select_compaction(gateway.capability(store), gateway.data_policy());
    let strategy_digest = {
        use sha2::{Digest, Sha256};
        sg_store::ids::hex(Sha256::digest(compaction_strategy.as_str().as_bytes()).as_slice())
    };
    log_rollout(
        &mut rollout,
        "compaction_strategy",
        json!({
            "strategy": compaction_strategy.as_str(),
            "digest": strategy_digest,
        }),
    );
    let cache_key = {
        use sha2::{Digest, Sha256};
        let tool_digest = crate::tools::schema_digest(tool_allowlist);
        let instr_digest = sg_store::ids::hex(Sha256::digest(system_prompt.as_bytes()).as_slice());
        let raw = format!(
            "tenant:local|workitem:{}|tools:{}|instructions:{}",
            run.workitem_id, tool_digest, instr_digest
        );
        sg_store::ids::hex(Sha256::digest(raw.as_bytes()).as_slice())
    };

    let mut reasoning_items: Vec<crate::model_protocol::ModelInputItem> = Vec::new();
    let (mut messages, start_iteration, mut pending_proposal) = {
        match load_checkpoint(store, run_id, &provider_fingerprint, &mut rollout) {
            Some(loaded) => (loaded.messages, loaded.iteration, loaded.pending),
            None => (initial.prefix.clone(), 0, None),
        }
    };
    log_rollout(
        &mut rollout,
        "instructions_assembled",
        json!({"segments": prompt::segment_bytes(initial), "messages": messages.len()}),
    );
    if run.status == "paused" {
        log_rollout(
            &mut rollout,
            "resumed",
            json!({"pendingProposal": pending_proposal.is_some()}),
        );
        set_status(store, &mut run, "running", "run.resumed")?;
    } else {
        log_rollout(
            &mut rollout,
            "run_started",
            json!({"workitemId": run.workitem_id}),
        );
    }
    let mut tool_calls = count_tool_calls(store, run_id)?;
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(budget.max_duration_sec.max(1) as u64);

    // WP-1（RDWS v1.4）：Grant 计量归属——Run 快照中的 autonomyGrantId。
    // 空 = 无 grant（默认人工路径）或 SIXGATES_UNIFIED_RISK=0（回退 = WP-1 前行为，
    // 不做限额校验；已落 ledger 行仍由 settle/启动对账照实收尾）。
    let grant_ledger_id = {
        let snap: String = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COALESCE(policy_snapshot,'') FROM agent_runs WHERE id=?1",
                    [run_id],
                    |r| r.get::<_, String>(0),
                )
                .unwrap_or_default())
            })
            .unwrap_or_default();
        if sg_policy::risk_model::enabled() {
            sg_policy::autonomy::grant_id_of_snapshot(&snap)
        } else {
            String::new()
        }
    };

    // 恢复：先执行挂起提案（digest 绑定再验一次，fail-closed；通过则追加 developer 通知而非改写历史）。
    if let Some(proposal_id) = pending_proposal.take() {
        let proposal = get_proposal(store, &proposal_id)?;
        sg_policy::validate_for(
            store,
            "tool_proposal",
            &proposal.id,
            &proposal.action_digest,
        )
        .map_err(|e| Error::Message(format!("approval_invalid: {e}")))?;
        let result_text = run_executor(store, executor, &proposal)?;
        tool_calls += 1;
        messages.push(ChatMessage {
            role: "developer".into(),
            content: format!("审批已通过，执行工具提案：{}", proposal.tool),
            ..Default::default()
        });
        messages.push(ChatMessage {
            role: "tool".into(),
            content: result_text.clone(),
            ..Default::default()
        });
        log_rollout(
            &mut rollout,
            "tool_result",
            json!({"tool": proposal.tool, "preview": tools::truncate_output(&result_text, 400)}),
        );
    }

    #[allow(unused_assignments)]
    for iteration in start_iteration..max_iterations {
        if cancelled() {
            run.result = "已取消（用户请求）".into();
            set_status(store, &mut run, "cancelled", "run.cancelled")?;
            finish(store, &run)?;
            log_rollout(&mut rollout, "run_finished", json!({"status": "cancelled"}));
            return Ok(RunOutput {
                run,
                output: String::new(),
            });
        }
        if std::time::Instant::now() > deadline {
            run.result = "budget: duration exceeded".into();
            set_status(store, &mut run, "failed", "run.failed")?;
            finish(store, &run)?;
            return Err(Error::Message("budget_exhausted: duration".into()));
        }
        // F09/M3：输入估算超阈值 → 压缩（回滚点→专用调用→冻结头+摘要+保留 K 轮）。
        let est = estimate_input_tokens(&system_prompt, &messages);
        if compact.threshold_tokens > 0 && est > compact.threshold_tokens {
            match compact_history(
                store,
                gateway,
                &budget.model,
                &run,
                &system_prompt,
                &mut messages,
                compact.keep_turns,
                iteration,
                est,
                &grant_ledger_id,
            ) {
                Ok(after) => {
                    log_rollout(
                        &mut rollout,
                        "compacted",
                        json!({"beforeEst": est, "afterEst": after, "keptTurns": compact.keep_turns}),
                    );
                }
                Err(e) => {
                    run.result = e.to_string();
                    set_status(store, &mut run, "failed", "run.failed")?;
                    finish(store, &run)?;
                    return Err(e);
                }
            }
        }
        // ADR-033 M1：Run 冻结 codec（能力快照协商；native → 原生 tools + call_id transcript）。
        // ADR-033 M1：Run 启动时冻结 codec（能力快照协商；暂停/恢复不换协议）。
        let codec = gateway.codec(store);
        let mut system_prompt = system_prompt.clone();
        if codec == crate::model_protocol::Codec::NativeTools {
            system_prompt.push_str(
                "\n\n工具通过原生 function calling 调用；无需输出 JSON 决策对象，\
                 完成任务时直接以正文作为最终答复。",
            );
        }
        let request = CompletionRequest {
            model: String::new(),
            system_prompt: system_prompt.clone(),
            messages: messages.clone(),
            // 推理型模型（如 deepseek-v4 系列）的推理也计入 max_tokens，
            // 上限太小会 finish_reason=length 且正文为空——给足余量。
            max_tokens: 16384,
            response_schema: None,
            tools_json: if codec == crate::model_protocol::Codec::NativeTools {
                // Run 冻结的 allowlist（agent_runs.tool_allowlist）。
                Some(crate::tools::provider_tools_json(tool_allowlist))
            } else {
                None
            },
        };
        log_rollout(
            &mut rollout,
            "model_request",
            json!({"messages": messages.len(), "estTokens": estimate_tokens(&request)}),
        );
        // WP-1（RDWS v1.4）：模型每 model_call 独立消费行（每次重试/压缩后新 key）。
        // 保留量用 utf8_bytes_upper_v1 上界（bytes/4 对中文是低估、非上界，禁用）；
        // tokens_out 按请求 max_tokens 保留。超限 → autonomy_budget_exhausted（零消耗）。
        // P1-1（评审修复）：model_call id 在调用方生成并经 TurnOpts 透传——
        // model_calls 权威行 id == ledger consumption_key，崩溃对账才能命中权威用量。
        let mc_id = ids::new_id("mc");
        let ledger_key = if grant_ledger_id.is_empty() {
            String::new()
        } else {
            mc_id.clone()
        };
        if !ledger_key.is_empty() {
            let est_in = tokens_upper_v1(&request);
            let evidence = json!({
                "estimator": "utf8_bytes_upper_v1",
                "tokensInUpper": est_in,
                "maxTokens": request.max_tokens,
                "iteration": iteration,
            });
            if let Err(e) = sg_policy::autonomy::reserve(
                store,
                &grant_ledger_id,
                run_id,
                &ledger_key,
                &[
                    ("model_calls", 1),
                    ("tokens_in", est_in),
                    ("tokens_out", request.max_tokens.max(0)),
                ],
                &evidence,
            ) {
                run.result = e.to_string();
                set_status(store, &mut run, "failed", "run.failed")?;
                finish(store, &run)?;
                return Err(e);
            }
        }
        let response = match gateway.call_turn(
            store,
            run_id,
            &budget.model,
            &request,
            &modelgw::TurnOpts {
                cancel,
                forwarder: forwarder.clone(),
                prompt_cache_key: cache_key.clone(),
                model_call_id: mc_id,
            },
        ) {
            Ok(resp) => {
                // 实际用量 settle：仅 Provider 真实返回过 usage 才作为 actual（P1-2：
                // usage 缺失时数值是估算/缺省——settle None 保留 reserve 进
                // reconciliation，不写 0 冒充）；actual 超 reserve 由 ledger 判
                // reconciliation_required。
                if !ledger_key.is_empty() {
                    let (tin, tout) = if resp.usage_present {
                        (Some(resp.tokens_in), Some(resp.tokens_out))
                    } else {
                        (None, None)
                    };
                    let _ = sg_policy::autonomy::settle(
                        store,
                        &grant_ledger_id,
                        run_id,
                        &ledger_key,
                        &[
                            ("model_calls", Some(1)),
                            ("tokens_in", tin),
                            ("tokens_out", tout),
                        ],
                        &json!({"source": "call_turn", "finishReason": resp.finish_reason})
                            .to_string(),
                    );
                }
                resp
            }
            Err(e) => {
                // usage 缺失面：预算预检失败或打开期失败（请求未送达/无任何产出，
                // 与网关「禁止双重消费」同界）→ 全维 settle 0；已发起（取消/协议/
                // 已见输出后中断）→ model_calls 按实消费 1，tokens 保留 reserve 进
                // reconciliation（Provider 是否计费未知，不估 0）。
                if !ledger_key.is_empty() {
                    let preflight =
                        e.contains("budget_exceeded") || modelgw::open_phase_failure(&e);
                    let _ = sg_policy::autonomy::settle(
                        store,
                        &grant_ledger_id,
                        run_id,
                        &ledger_key,
                        if preflight {
                            &[
                                ("model_calls", Some(0)),
                                ("tokens_in", Some(0)),
                                ("tokens_out", Some(0)),
                            ]
                        } else {
                            &[("model_calls", Some(1))]
                        },
                        &json!({"source": "call_turn_failed", "error": e, "preflight": preflight})
                            .to_string(),
                    );
                }
                // M2 §4.3：本地 abort → cancelled 终态（恰好一条 run.cancelled），
                // 已显示 delta 作废，不重放（Provider 是否继续计费未知）。
                if e.starts_with("model_cancelled") {
                    run.result = "已取消（用户请求）".into();
                    set_status(store, &mut run, "cancelled", "run.cancelled")?;
                    finish(store, &run)?;
                    log_rollout(
                        &mut rollout,
                        "run_finished",
                        json!({"status": "cancelled", "phase": "model_call"}),
                    );
                    return Ok(RunOutput {
                        run,
                        output: String::new(),
                    });
                }
                if e.contains("budget_exceeded") {
                    run.result = e.clone();
                    set_status(store, &mut run, "failed", "run.failed")?;
                    finish(store, &run)?;
                    return Err(Error::Message(e));
                }
                let lower = e.to_lowercase();
                run.result = if lower.starts_with("model_stream_interrupted")
                    || lower.starts_with("model_protocol_violation")
                    || lower.starts_with("model_capability_missing")
                {
                    // §4.3 独立错误码原样落库：流中断/协议违规不得伪装成其他失败，
                    // 已显示的 delta 也不是最终答案。
                    e.clone()
                } else if lower.contains("model_empty_output") {
                    // 输出上限耗尽（finish_reason=length）≠ 上下文过大：
                    // 按独立前缀优先归类，避免误导排查方向。
                    format!("model_output_empty: {e}")
                } else if lower.contains("context")
                    || lower.contains("too large")
                    || lower.contains("length")
                {
                    format!("context_too_large: {e}")
                } else {
                    format!("model: {e}")
                };
                set_status(store, &mut run, "failed", "run.failed")?;
                finish(store, &run)?;
                return Err(Error::Message(e));
            }
        };

        // M4：加密 reasoning 状态入 checkpoint 待写集（明文已止于网关）。
        if let Some(blob) = &response.reasoning_state_encrypted {
            reasoning_items.push(crate::model_protocol::ModelInputItem::ReasoningOpaque {
                provider: provider_fingerprint.clone(),
                payload: blob.clone(),
            });
        }

        // M2：耐久 turn commit——只有完成帧经聚合器验证后才发。
        // delta 是易失体验事件；最终 assistant/tool-call/usage 以本事件与
        // checkpoint 为准（断线后 agent.get/trace 可重建）。
        outbox::emit(
            store,
            "agent_run",
            run_id,
            "run.turn_committed",
            json!({
                "workitemId": run.workitem_id,
                "finishReason": response.finish_reason,
                "tokensIn": response.tokens_in,
                "tokensOut": response.tokens_out,
                "hasToolCalls": !response.tool_calls.is_empty(),
                "tool": response.tool_calls.first().map(|t| t.name.clone()),
                "textPreview": tools::truncate_output(&response.content, 200),
            }),
        )?;

        // M1：native 分支优先消费原生 tool_calls（正文非空 = 最终答复）；
        // legacy 分支维持 parse_decision + 修复轮。
        let (decision, native_call) = if codec == crate::model_protocol::Codec::NativeTools {
            match response.tool_calls.len() {
                1 => {
                    let tc = &response.tool_calls[0];
                    (
                        Some(Decision {
                            action: tc.name.clone(),
                            arguments: tc.arguments.clone(),
                            summary: String::new(),
                        }),
                        Some(tc.clone()),
                    )
                }
                0 if !response.content.trim().is_empty() => (
                    Some(Decision {
                        action: "final".into(),
                        arguments: String::new(),
                        summary: response.content.clone(),
                    }),
                    None,
                ),
                0 => (None, None),
                n => {
                    run.result =
                        format!("model_protocol_violation: 并行工具调用 {n} 个（首期拒绝）");
                    set_status(store, &mut run, "failed", "run.failed")?;
                    finish(store, &run)?;
                    return Err(Error::Message(run.result.clone()));
                }
            }
        } else {
            match parse_decision(&response.content) {
                Some(d) => (Some(d), None),
                None => (None, None),
            }
        };
        let decision = match decision {
            Some(d) => d,
            None => {
                messages.push(ChatMessage {
                    role: "assistant".into(),
                    content: response.content,
                    ..Default::default()
                });
                messages.push(ChatMessage {
                    role: "user".into(),
                    content: "输出必须是 JSON（schema 见系统提示）；请重新输出决策对象。".into(),
                    ..Default::default()
                });
                continue;
            }
        };
        log_rollout(
            &mut rollout,
            "model_response",
            json!({"action": decision.action, "summary": tools::truncate_output(&decision.summary, 200)}),
        );

        if decision.action == "final" {
            run.result = decision.summary.clone();
            set_status(
                store,
                &mut run,
                "completed_execution",
                "run.completed_execution",
            )?;
            checkpoint(
                store,
                run_id,
                iteration,
                &messages,
                None,
                &reasoning_items,
                &provider_fingerprint,
            );
            finish(store, &run)?;
            log_rollout(
                &mut rollout,
                "run_finished",
                json!({"status": "completed_execution"}),
            );
            return Ok(RunOutput {
                run,
                output: decision.summary,
            });
        }

        if tool_calls >= budget.max_tool_calls {
            run.result = "budget: tool calls exhausted".into();
            set_status(store, &mut run, "failed", "run.failed")?;
            finish(store, &run)?;
            return Err(Error::Message("budget_exhausted: tool calls".into()));
        }

        if cancelled() {
            run.result = "已取消（用户请求）".into();
            set_status(store, &mut run, "cancelled", "run.cancelled")?;
            finish(store, &run)?;
            return Ok(RunOutput {
                run,
                output: String::new(),
            });
        }

        let outcome = propose_and_execute(store, policy, executor, &mut rollout, &run, &decision)?;
        match outcome {
            StepOutcome::NeedsApproval { proposal_id, tool } => {
                messages.push(native_assistant_msg(
                    &response,
                    native_call.as_ref(),
                    &decision,
                ));
                if let Some(tc) = &native_call {
                    messages.push(ChatMessage {
                        role: "tool".into(),
                        content: format!("等待审批：{tool}"),
                        tool_call_id: Some(tc.id.clone()),
                        tool_calls_json: None,
                    });
                }
                checkpoint(
                    store,
                    run_id,
                    iteration,
                    &messages,
                    Some(&proposal_id),
                    &reasoning_items,
                    &provider_fingerprint,
                );
                log_rollout(
                    &mut rollout,
                    "checkpoint_saved",
                    json!({"iteration": iteration, "phase": "waiting_approval"}),
                );
                run.result = format!("等待审批：{tool}");
                set_status(store, &mut run, "paused", "run.waiting_approval")?;
                finish(store, &run)?;
                log_rollout(&mut rollout, "run_finished", json!({"status": "paused"}));
                return Ok(RunOutput {
                    run,
                    output: String::new(),
                });
            }
            StepOutcome::Continue(output_text) => {
                tool_calls += 1;
                messages.push(native_assistant_msg(
                    &response,
                    native_call.as_ref(),
                    &decision,
                ));
                if let Some(tc) = &native_call {
                    messages.push(ChatMessage {
                        role: "tool".into(),
                        content: output_text,
                        tool_call_id: Some(tc.id.clone()),
                        tool_calls_json: None,
                    });
                } else {
                    messages.push(ChatMessage {
                        role: "tool".into(),
                        content: output_text,
                        ..Default::default()
                    });
                }
                checkpoint(
                    store,
                    run_id,
                    iteration,
                    &messages,
                    None,
                    &reasoning_items,
                    &provider_fingerprint,
                );
                log_rollout(
                    &mut rollout,
                    "checkpoint_saved",
                    json!({"iteration": iteration, "phase": "running"}),
                );
                log_rollout(
                    &mut rollout,
                    "turn_completed",
                    json!({"iteration": iteration, "toolCalls": tool_calls}),
                );
            }
        }
    }

    run.result = "max iterations reached without final answer".into();
    set_status(store, &mut run, "failed", "run.failed")?;
    finish(store, &run)?;
    Err(Error::Message("agent loop exhausted".into()))
}

/// 输入 token 估算（启发式 chars/4+512；不引 tokenizer——诚实声明精度，阈值语义足够）。
fn estimate_input_tokens(system: &str, messages: &[ChatMessage]) -> usize {
    let chars = system.len() + messages.iter().map(|m| m.content.len()).sum::<usize>();
    chars / 4 + 512
}

/// F09/M3 压缩：回滚 checkpoint → 专用压缩调用（计入 model_calls/预算）→
/// 新历史 = 冻结头（developer/knowledge/goal，前缀纪律）+ 摘要 + 最近 K 轮工具往返。
/// 失败重试一次，仍失败返回 Err（execute_run 置 failed）。
#[allow(clippy::too_many_arguments)]
fn compact_history(
    store: &Store,
    gateway: &Gateway,
    budget: &Budget,
    run: &AgentRun,
    system_prompt: &str,
    messages: &mut Vec<ChatMessage>,
    keep_turns: usize,
    iteration: usize,
    est_before: usize,
    grant_ledger_id: &str,
) -> Result<usize, Error> {
    // 回滚点（phase=running；压缩失败可从此恢复重试）。
    checkpoint(store, &run.id, iteration, messages, None, &[], "compaction");
    let compress_system = "你是会话压缩器。把对话历史压缩为结构化摘要 JSON：         {\"summary\":\"...\",\"facts\":[\"...\"],\"pendingApprovals\":[],\
         \"executedTools\":[{\"tool\":\"..\",\"result\":\"..\"}],\"keyFiles\":[\"..\"]}\u{3002}         必须保留：任务目标、当前状态、未决审批、已执行工具与结论、关键文件路径。只输出 JSON。";
    let request = CompletionRequest {
        model: String::new(),
        system_prompt: compress_system.into(),
        messages: messages.clone(),
        max_tokens: 8192,
        response_schema: None,
        tools_json: None,
    };
    // P1-3（评审修复）：压缩调用是真实计费消费——每次调用（含重试）以独立
    // model_call id 走同一记账管线（RDWS v1.4「每次 retry/压缩后重试都使用新
    // model_call id 建新 reservation」）；ledger key 与 model_calls 行 id 共用。
    let compact_call = |attempt: u32| -> Result<sg_integrations::model::CompletionResponse, Error> {
        let mc_id = ids::new_id("mc");
        if !grant_ledger_id.is_empty() {
            let est_in = tokens_upper_v1(&request);
            sg_policy::autonomy::reserve(
                store,
                grant_ledger_id,
                &run.id,
                &mc_id,
                &[
                    ("model_calls", 1),
                    ("tokens_in", est_in),
                    ("tokens_out", request.max_tokens.max(0)),
                ],
                &json!({"estimator": "utf8_bytes_upper_v1", "kind": "compaction", "attempt": attempt}),
            )?;
        }
        let result = gateway
            .call_with_id(store, &run.id, budget, &request, &mc_id)
            .map_err(|e| Error::Message(format!("context_too_large: 压缩调用失败 {e}")));
        if !grant_ledger_id.is_empty() {
            match &result {
                Ok(resp) => {
                    let (tin, tout) = if resp.usage_present {
                        (Some(resp.tokens_in), Some(resp.tokens_out))
                    } else {
                        (None, None)
                    };
                    let _ = sg_policy::autonomy::settle(
                        store,
                        grant_ledger_id,
                        &run.id,
                        &mc_id,
                        &[
                            ("model_calls", Some(1)),
                            ("tokens_in", tin),
                            ("tokens_out", tout),
                        ],
                        &json!({"source": "compaction", "attempt": attempt}).to_string(),
                    );
                }
                Err(e) => {
                    let preflight = e.to_string().contains("budget_exceeded")
                        || modelgw::open_phase_failure(&e.to_string());
                    let _ = sg_policy::autonomy::settle(
                        store, grant_ledger_id, &run.id, &mc_id,
                        if preflight {
                            &[("model_calls", Some(0)), ("tokens_in", Some(0)), ("tokens_out", Some(0))]
                        } else {
                            &[("model_calls", Some(1))]
                        },
                        &json!({"source": "compaction_failed", "attempt": attempt, "error": e.to_string()}).to_string(),
                    );
                }
            }
        }
        result
    };
    // M4：摘要 JSON 校验（字段/大小）——失败重试一次，仍失败保留原 checkpoint。
    let mut resp = compact_call(0)?;
    if validate_summary_json(&resp.content).is_err() {
        resp = compact_call(1)?;
        if validate_summary_json(&resp.content).is_err() {
            return Err(Error::Message(
                "context_too_large: 压缩摘要校验失败（字段缺失或超限）；原 checkpoint 保留".into(),
            ));
        }
    }
    let history_start = messages
        .iter()
        .position(|m| m.role == "assistant")
        .unwrap_or(messages.len());
    let frozen: Vec<ChatMessage> = messages[..history_start].to_vec();
    let history = &messages[history_start..];
    let keep_msgs = keep_turns.saturating_mul(2).min(history.len());
    let tail: Vec<ChatMessage> = history[history.len() - keep_msgs..].to_vec();
    let summary_text = tools::truncate_output(&resp.content, 8192);
    let mut new_messages = frozen;
    new_messages.push(ChatMessage {
        role: "user".into(),
        content: format!("【会话已压缩】此前对话（约 {est_before} tokens）摘要：\n{summary_text}"),
        ..Default::default()
    });
    new_messages.extend(tail);
    let est_after = estimate_input_tokens(system_prompt, &new_messages);
    outbox::emit(
        store,
        "agent_run",
        &run.id,
        "run.compacted",
        json!({"workitemId": run.workitem_id, "beforeEst": est_before, "afterEst": est_after, "keptTurns": keep_turns}),
    )?;
    *messages = new_messages;
    Ok(est_after)
}

/// M4：F09 压缩摘要 JSON 校验（字段完整性 + 大小上限）。
fn validate_summary_json(text: &str) -> Result<Value, String> {
    let v: Value =
        serde_json::from_str(text.trim()).map_err(|e| format!("摘要非合法 JSON: {e}"))?;
    let summary = v["summary"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "摘要缺 summary 字段".to_string())?;
    if summary.len() > 8192 {
        return Err("summary 超 8192 字节上限".into());
    }
    for field in ["facts", "pendingApprovals", "executedTools", "keyFiles"] {
        let arr = v[field]
            .as_array()
            .ok_or_else(|| format!("摘要缺 {field} 数组"))?;
        if arr.len() > 128 {
            return Err(format!("{field} 超 128 项上限"));
        }
        for entry in arr {
            let ok = match field {
                "executedTools" => {
                    entry.is_object()
                        && entry["tool"].is_string()
                        && (entry["result"].is_string() || entry["result"].is_null())
                }
                _ => entry.is_string(),
            };
            if !ok {
                return Err(format!("{field} 项类型非法"));
            }
        }
    }
    if text.len() > 64 * 1024 {
        return Err("摘要超 64KiB 上限".into());
    }
    Ok(v)
}

fn estimate_tokens(req: &CompletionRequest) -> usize {
    let chars =
        req.system_prompt.len() + req.messages.iter().map(|m| m.content.len()).sum::<usize>();
    chars / 4 + 64
}

/// WP-1（RDWS v1.4 Grant 计量）token 上界估算 `utf8_bytes_upper_v1`：
/// UTF-8 字节数 + 每消息边界固定 overhead。任何 token 至少 1 字节 → 真上界；
/// bytes/4 对 CJK（3 字节 ≈ 1-2 token）是低估，禁止用于保留量。
/// estimator kind 随 reserve evidence 冻结进 Run policy snapshot 语义。
fn tokens_upper_v1(req: &CompletionRequest) -> i64 {
    let bytes = req.system_prompt.len()
        + req.messages.iter().map(|m| m.content.len()).sum::<usize>()
        + req.tools_json.as_ref().map(|t| t.len()).unwrap_or(0);
    bytes as i64 + 16 * (req.messages.len() as i64 + 1)
}

fn log_rollout(rollout: &mut Option<crate::rollout::Rollout>, kind: &str, data: Value) {
    if let Some(r) = rollout {
        // rollout 是观测数据：写失败不中断 Run（记录 stderr）。
        if let Err(e) = r.append(kind, data) {
            eprintln!("{{\"level\":\"warn\",\"msg\":\"rollout append failed: {e}\"}}");
        }
    }
}

struct Decision {
    action: String,
    arguments: String,
    summary: String,
}

fn parse_decision(content: &str) -> Option<Decision> {
    let start = content.find('{')?;
    let end = content.rfind('}')?;
    let value: Value = serde_json::from_str(&content[start..=end]).ok()?;
    let action = value["action"].as_str()?;
    let arguments = value.get("arguments").cloned().unwrap_or(Value::Null);
    let summary = value["summary"].as_str().unwrap_or_default();
    // F11/M4：契约 schema 运行时校验（违例视同解析失败 → retry-nudge）。
    schema::validate_decision(action, &arguments, summary).ok()?;
    Some(Decision {
        action: action.into(),
        arguments: arguments.to_string(),
        summary: summary.into(),
    })
}

fn propose_and_execute(
    store: &Store,
    policy: &Snapshot,
    executor: Option<&ToolExecutor>,
    rollout: &mut Option<crate::rollout::Rollout>,
    run: &AgentRun,
    decision: &Decision,
) -> Result<StepOutcome, Error> {
    let action = json!({"tool": decision.action, "arguments": serde_json::from_str::<Value>(&decision.arguments).unwrap_or(Value::Null)});
    let digest = sg_policy::action_digest(&action);
    let id = ids::new_id("tp");
    let proposal = Proposal {
        id: id.clone(),
        run_id: run.id.clone(),
        tool: decision.action.clone(),
        arguments: decision.arguments.clone(),
        risk: "medium".into(),
        action_digest: digest.clone(),
        decision: "proposed".into(),
        result: String::new(),
        created_at: timefmt::now(),
    };
    // WP-2（RDWS v1.4）：tool 列写侧一律 canonical（builtin:/mcp: 前缀）+ tool_provider
    // 区分列；执行/治理链经 ToolId::parse 双形态等价消费（legacy 行为不变）。
    let tool_id = crate::provider::ToolId::parse(&decision.action);
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO tool_proposals(id, agent_run_id, tool, arguments, risk, action_digest,
                 requires_approval, decision, created_at, tool_provider)
             VALUES (?1,?2,?3,?4,?5,?6,0,'proposed',?7,?8)",
            rusqlite::params![
                proposal.id,
                proposal.run_id,
                tool_id.canonical(),
                proposal.arguments,
                proposal.risk,
                proposal.action_digest,
                proposal.created_at,
                tool_id.provider()
            ],
        )?;
        Ok(())
    })?;
    outbox::emit(
        store,
        "agent_run",
        &run.id,
        "tool.proposed",
        json!({"workitemId": run.workitem_id, "tool": proposal.tool, "digest": digest}),
    )?;
    log_rollout(
        rollout,
        "tool_proposed",
        json!({"tool": proposal.tool, "digest": digest}),
    );

    let needs_approval = match sg_policy::evaluate(policy, &decision.action, &digest) {
        Ok(_) => false,
        Err(PolicyError::ApprovalRequired) => true,
        Err(e) => {
            mark_proposal(store, &proposal, "rejected", &e.to_string())?;
            return Ok(StepOutcome::Continue(format!("工具被策略拒绝：{e}")));
        }
    };

    // WP-1（RDWS v1.4 A4）：统一风险评估——SIXGATES_UNIFIED_RISK 开启时服务端派生、
    // 审计 risk.assessment，且 auto_approvable=false 强制人工审批（只收紧：硬规则命中的
    // irreversible/manual/无键链外部写不得因 ToolRule 低档被自动放行）。
    // MCP 在 WP-2 ToolProvider adapter 证据落地前按外部写保守评估（只升不降）。
    let needs_approval = if needs_approval || !sg_policy::risk_model::enabled() {
        needs_approval
    } else {
        let assessment = match crate::tools::find(&decision.action) {
            Some(def) => sg_policy::risk_model::assess_registry_tool(
                &decision.action,
                def.effect_class,
                def.reversibility,
                def.protected_target,
                None,
                false,
                None,
                None,
            ),
            None => sg_policy::risk_model::assess_registry_tool(
                &decision.action,
                "external_write",
                "manual",
                false,
                None,
                false,
                None,
                None,
            ),
        };
        let auto = sg_policy::risk_model::auto_approvable(&assessment);
        let _ = sg_store::audit::append(
            store,
            "system",
            "risk.assessment",
            "tool_proposal",
            &proposal.id,
            json!({"autoApprovable": auto, "digest": digest, "assessment": assessment.to_json()}),
        );
        needs_approval || !auto
    };

    if needs_approval
        && sg_policy::validate_for(store, "tool_proposal", &proposal.id, &digest).is_err()
    {
        sg_policy::request_approval(
            store,
            "tool_proposal",
            &proposal.id,
            &digest,
            sg_policy::Risk::High,
            &format!("run {} tool {}", run.id, decision.action),
            3600,
            Some(&run.workitem_id),
            None,
        )?;
        // M1/F03：提案保持 proposed（不再记 rejected/approval_required），Run 挂起等待审批。
        log_rollout(
            rollout,
            "approval_requested",
            json!({"tool": proposal.tool, "digest": digest}),
        );
        return Ok(StepOutcome::NeedsApproval {
            proposal_id: proposal.id,
            tool: proposal.tool,
        });
    }

    let text = run_executor(store, executor, &proposal)?;
    log_rollout(
        rollout,
        "tool_result",
        json!({"tool": proposal.tool, "preview": tools::truncate_output(&text, 400)}),
    );
    Ok(StepOutcome::Continue(text))
}

/// M0-07（EvoFlow 方案 §3 不变量 7）：executor 以 Ok 返回但副作用未知的约定前缀
/// （MCP 写超时等）。命中即落一等 unknown 终态，绝不标 executed、不透明重试。
pub const TOOL_OUTCOME_UNKNOWN_PREFIX: &str = "tool_outcome_unknown:";

/// 一等执行结果落库（0031 `tool_execution_outcomes`）：每提案至多一条（幂等重放安全）。
/// unknown 的对账状态初始为 pending，查证后由 reconcile 推进。
fn record_outcome(
    store: &Store,
    proposal_id: &str,
    outcome: &str,
    reason: &str,
) -> Result<(), Error> {
    let reconciliation = if outcome == "unknown" || outcome == "indeterminate" {
        "pending"
    } else {
        "none"
    };
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO tool_execution_outcomes(id, proposal_id, outcome, reason, reconciliation, send_phase, created_at)
             SELECT ?1, ?2, ?3, ?4, ?5, COALESCE(p.send_phase,'not_sent'), ?6
             FROM tool_proposals p WHERE p.id=?2
             ON CONFLICT(proposal_id) DO NOTHING",
            rusqlite::params![
                ids::new_id("tout"),
                proposal_id,
                outcome,
                reason,
                reconciliation,
                timefmt::now()
            ],
        )?;
        Ok(())
    })
}

/// 执行提案并落 executed/exec_failed/unknown 标记，返回给模型的文本。
/// M0-07：Ok(text) 不再一律视为 executed —— `tool_outcome_unknown:` 前缀
/// （MCP 写超时等副作用未验证场景）落一等 unknown，Proposal 与 outcome 表均诚实。
fn run_executor(
    store: &Store,
    executor: Option<&ToolExecutor>,
    proposal: &Proposal,
) -> Result<String, Error> {
    match executor {
        Some(exec) => match exec(proposal) {
            Ok(result) => {
                if result.starts_with(TOOL_OUTCOME_UNKNOWN_PREFIX) {
                    mark_proposal(store, proposal, "unknown", &result)?;
                    record_outcome(store, &proposal.id, "unknown", "side_effect_unverified")?;
                } else {
                    mark_proposal(store, proposal, "executed", &result)?;
                    record_outcome(store, &proposal.id, "ok", "")?;
                }
                Ok(result)
            }
            Err(e) => {
                mark_proposal(store, proposal, "exec_failed", &e)?;
                record_outcome(store, &proposal.id, "failed", &e)?;
                Ok(format!("执行失败：{e}"))
            }
        },
        None => {
            mark_proposal(store, proposal, "rejected", "no executor")?;
            Ok("执行器不可用。".into())
        }
    }
}

fn mark_proposal(
    store: &Store,
    proposal: &Proposal,
    decision: &str,
    result: &str,
) -> Result<(), Error> {
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE tool_proposals SET decision=?1, result=?2 WHERE id=?3",
            rusqlite::params![decision, result, proposal.id],
        )?;
        Ok(())
    })
}

/// 终态集合：completed_execution（成功）/ failed / cancelled。
/// "completed" 不是本实现的合法 run 终态，列入以防状态漂移。
fn is_run_terminal(status: &str) -> bool {
    matches!(
        status,
        "completed_execution" | "failed" | "cancelled" | "completed"
    )
}

fn set_status(store: &Store, run: &mut AgentRun, to: &str, event: &str) -> Result<(), Error> {
    // 终态 CAS（缺陷审计 P1-8）：取消/完成/失败竞争时，后写不得翻转已有终态、
    // 不得对同一终态重复发事件。条件更新：行处于非终态、或已与目标同态（幂等重放）才写。
    // status 与 result 同语句原子写：终态轮询方不可见"failed 而 result 未落"的中间窗口。
    let changed = store.with_conn(|conn| {
        conn.execute(
            "UPDATE agent_runs SET status=?1, result=?2, updated_at=?3
             WHERE id=?4 AND (status NOT IN ('completed_execution','failed','cancelled','completed') OR status=?1)",
            rusqlite::params![to, run.result, timefmt::now(), run.id],
        )?;
        Ok(conn.changes())
    })?;
    if changed == 0 {
        // 行已被并发方终结为不同终态：以库内现状为准，本地静默收敛（不发事件）。
        let current: String = store.with_conn(|conn| {
            conn.query_row(
                "SELECT status FROM agent_runs WHERE id=?1",
                [&run.id],
                |r| r.get(0),
            )
            .map_err(|_| Error::Message(format!("run {} 不存在", run.id)))
        })?;
        run.status = current;
        return Ok(());
    }
    run.status = to.into();
    outbox::emit(
        store,
        "agent_run",
        &run.id,
        event,
        json!({"workitemId": run.workitem_id, "status": to}),
    )?;
    Ok(())
}

fn finish(store: &Store, run: &AgentRun) -> Result<(), Error> {
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE agent_runs SET result=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![run.result, timefmt::now(), run.id],
        )?;
        Ok(())
    })
}

/// M4 checkpoint v2：versioned ModelInputItem[] + Provider 指纹绑定。
/// reasoning opaque 项（加密 blob）随 items 持久化，跨 Provider 拒绝回放。
fn checkpoint(
    store: &Store,
    run_id: &str,
    seq: usize,
    messages: &[ChatMessage],
    pending: Option<&str>,
    reasoning_items: &[crate::model_protocol::ModelInputItem],
    provider_fingerprint: &str,
) {
    let mut items = messages_to_items(messages);
    items.extend(reasoning_items.iter().cloned());
    let body = serde_json::json!({
        "schemaVersion": 2,
        "items": items,
        "iteration": seq,
        "pending_proposal_id": pending,
        "phase": if pending.is_some() { "waiting_approval" } else { "running" },
        "providerFingerprint": provider_fingerprint,
    })
    .to_string();
    let _ = store.with_conn(|conn| {
        let _ = conn.execute(
            "INSERT INTO agent_checkpoints(id, agent_run_id, seq, state, created_at)
             VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(agent_run_id, seq) DO UPDATE SET state=excluded.state",
            rusqlite::params![
                ids::new_id("ckpt"),
                run_id,
                seq as i64,
                body,
                timefmt::now()
            ],
        );
        Ok(())
    });
}

pub fn get_run(store: &Store, id: &str) -> Result<AgentRun, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT id, workitem_id, COALESCE(task_id,''), goal, status, COALESCE(result,''), created_at, updated_at
             FROM agent_runs WHERE id=?1",
            [id],
            |r| {
                Ok(AgentRun {
                    id: r.get(0)?, workitem_id: r.get(1)?, task_id: r.get(2)?,
                    goal: r.get(3)?, status: r.get(4)?, result: r.get(5)?,
                    created_at: r.get(6)?, updated_at: r.get(7)?,
                })
            },
        )
        .map_err(|_| Error::Message("run_not_found".into()))
    })
}

/// 某工作项的近期 Run（工作台 Agent 执行详情横条用），新→旧。
pub fn list_recent(store: &Store, workitem_id: &str, limit: i64) -> Result<Vec<AgentRun>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, workitem_id, COALESCE(task_id,''), goal, status, COALESCE(result,''), created_at, updated_at
             FROM agent_runs WHERE workitem_id=?1 ORDER BY created_at DESC, id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![workitem_id, limit], |r| {
            Ok(AgentRun {
                id: r.get(0)?, workitem_id: r.get(1)?, task_id: r.get(2)?,
                goal: r.get(3)?, status: r.get(4)?, result: r.get(5)?,
                created_at: r.get(6)?, updated_at: r.get(7)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

/// 执行轨迹（工作台「执行过程」页签）：从 rollout JSONL 提取推理摘要与工具调用。
/// tool 步骤带 proposeTs，时长由前端按时间戳差值计算（避免引入 RFC3339 解析依赖）。
pub fn trace(store: &Store, run_id: &str) -> Result<serde_json::Value, Error> {
    let path = rollout::Rollout::path_for(&store.data_dir, run_id);
    let body = std::fs::read_to_string(&path).unwrap_or_default();
    let mut steps: Vec<serde_json::Value> = Vec::new();
    // 待配对的 tool_proposed（tool 名, ts）：tool_result 按序配对计算耗时。
    let mut pending_tools: std::collections::VecDeque<(String, String)> = Default::default();
    for line in body.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let kind = v["kind"].as_str().unwrap_or("");
        let ts = v["ts"].as_str().unwrap_or("").to_string();
        let data = &v["data"];
        match kind {
            "model_response" => steps.push(serde_json::json!({
                "kind": "reasoning",
                "seq": v["seq"],
                "ts": ts,
                "name": data["action"].as_str().unwrap_or("推理"),
                "summary": data["summary"].as_str().unwrap_or(""),
            })),
            "tool_proposed" => {
                if let Some(t) = data["tool"].as_str() {
                    pending_tools.push_back((t.to_string(), ts));
                }
            }
            "tool_result" => {
                let (tool, propose_ts) = pending_tools.pop_front().unwrap_or_else(|| {
                    (
                        data["tool"].as_str().unwrap_or("tool").to_string(),
                        ts.clone(),
                    )
                });
                steps.push(serde_json::json!({
                    "kind": "tool",
                    "seq": v["seq"],
                    "ts": ts,
                    "name": tool,
                    "proposeTs": propose_ts,
                    "preview": data["preview"].as_str().map(|p| p.chars().take(240).collect::<String>()),
                }));
            }
            _ => {}
        }
    }
    let checkpoints = store
        .with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT seq, created_at FROM agent_checkpoints WHERE agent_run_id=?1 ORDER BY seq")?;
            let rows = stmt.query_map([run_id], |r| {
                Ok(serde_json::json!({"seq": r.get::<_, i64>(0)?, "createdAt": r.get::<_, String>(1)?}))
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
        .map_err(|e| Error::Message(e.to_string()))?;
    Ok(serde_json::json!({"steps": steps, "checkpoints": checkpoints}))
}

pub fn cancel(store: &Store, id: &str) -> Result<(), Error> {
    let mut run = get_run(store, id)?;
    if is_run_terminal(&run.status) {
        return Ok(());
    }
    set_status(store, &mut run, "cancelled", "run.cancelled")?;
    Ok(())
}

/// 恢复的 checkpoint 内容。
struct LoadedCheckpoint {
    messages: Vec<ChatMessage>,
    iteration: usize,
    pending: Option<String>,
}

/// M4：读取 checkpoint——v2（ModelInputItem[] + Provider 指纹绑定）为主；
/// v1（messages 数组）兼容读取（无 reasoning 项）；指纹不匹配 = 明确回退走全新
/// 循环（rollout 记录，禁止跨 Provider 回放）；opaque payload 损坏项丢弃并记录。
fn load_checkpoint(
    store: &Store,
    run_id: &str,
    current_fingerprint: &str,
    rollout: &mut Option<crate::rollout::Rollout>,
) -> Option<LoadedCheckpoint> {
    let raw: Option<String> = store
        .with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT state FROM agent_checkpoints WHERE agent_run_id=?1
                     ORDER BY seq DESC LIMIT 1",
                    [run_id],
                    |r| r.get(0),
                )
                .ok())
        })
        .ok()
        .flatten();
    let v: Value = serde_json::from_str(&raw?).ok()?;
    let iteration = v.get("iteration").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
    let pending = v
        .get("pending_proposal_id")
        .and_then(|x| x.as_str())
        .map(String::from);

    if v.get("schemaVersion").and_then(|x| x.as_u64()) == Some(2) {
        // 指纹绑定：Provider/base URL/模型族变化 → 拒绝回放（明确回退）。
        let stored_fp = v
            .get("providerFingerprint")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if stored_fp != current_fingerprint {
            log_rollout(
                rollout,
                "checkpoint_provider_mismatch",
                json!({"stored": stored_fp, "current": current_fingerprint, "action": "fresh_loop"}),
            );
            return None;
        }
        let arr = v.get("items")?.as_array()?;
        let mut items: Vec<crate::model_protocol::ModelInputItem> = Vec::with_capacity(arr.len());
        let mut dropped_opaque = 0usize;
        for it in arr {
            match serde_json::from_value::<crate::model_protocol::ModelInputItem>(it.clone()) {
                Ok(item) => {
                    // opaque payload 完整性：base64 可解码；损坏项丢弃（明确回退）。
                    if let crate::model_protocol::ModelInputItem::ReasoningOpaque {
                        payload, ..
                    } = &item
                    {
                        use base64::Engine;
                        if base64::engine::general_purpose::STANDARD
                            .decode(payload)
                            .is_err()
                        {
                            dropped_opaque += 1;
                            continue;
                        }
                    }
                    items.push(item);
                }
                Err(_) => return None, // 无法解析的项 = checkpoint 损坏 → 全新循环
            }
        }
        if dropped_opaque > 0 {
            log_rollout(
                rollout,
                "checkpoint_opaque_dropped",
                json!({"count": dropped_opaque}),
            );
        }
        let messages = items_to_messages(&items);
        return Some(LoadedCheckpoint {
            messages,
            iteration,
            pending,
        });
    }

    // v1 兼容：messages 数组。
    let arr = v.get("messages")?.as_array()?;
    let mut messages = Vec::with_capacity(arr.len());
    for m in arr {
        messages.push(ChatMessage {
            role: m.get("role")?.as_str()?.into(),
            content: m.get("content")?.as_str()?.into(),
            ..Default::default()
        });
    }
    Some(LoadedCheckpoint {
        messages,
        iteration,
        pending,
    })
}

/// ChatMessage transcript → ModelInputItem[]（checkpoint 持久化形状）。
/// assistant+tool_calls：ToolCall 项在前、Message(assistant, content) 在后——
/// 重建时 pending 调用挂到紧随的 assistant Message。
fn messages_to_items(messages: &[ChatMessage]) -> Vec<crate::model_protocol::ModelInputItem> {
    use crate::model_protocol::ModelInputItem;
    let mut items = Vec::with_capacity(messages.len());
    for m in messages {
        if m.role == "assistant" {
            if let Some(calls_json) = m.tool_calls_json.as_deref() {
                if let Ok(calls) = serde_json::from_str::<Value>(calls_json) {
                    if let Some(list) = calls.as_array() {
                        for c in list {
                            items.push(ModelInputItem::ToolCall {
                                call_id: c["id"].as_str().unwrap_or_default().into(),
                                name: c["function"]["name"].as_str().unwrap_or_default().into(),
                                arguments_json: c["function"]["arguments"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .into(),
                            });
                        }
                    }
                }
            }
            items.push(ModelInputItem::Message {
                role: m.role.clone(),
                content: m.content.clone(),
            });
        } else if m.role == "tool" {
            if let Some(call_id) = m.tool_call_id.as_deref() {
                items.push(ModelInputItem::ToolResult {
                    call_id: call_id.into(),
                    output: m.content.clone(),
                });
            } else {
                items.push(ModelInputItem::Message {
                    role: m.role.clone(),
                    content: m.content.clone(),
                });
            }
        } else {
            items.push(ModelInputItem::Message {
                role: m.role.clone(),
                content: m.content.clone(),
            });
        }
    }
    items
}

/// ModelInputItem[] → ChatMessage transcript（请求形状重建）。
fn items_to_messages(items: &[crate::model_protocol::ModelInputItem]) -> Vec<ChatMessage> {
    use crate::model_protocol::ModelInputItem;
    let mut out: Vec<ChatMessage> = Vec::with_capacity(items.len());
    let mut pending_calls: Vec<(String, String, String)> = Vec::new();
    let flush_calls =
        |out: &mut Vec<ChatMessage>, pending: &mut Vec<(String, String, String)>, content: &str| {
            if pending.is_empty() {
                return;
            }
            let calls: Value = Value::Array(
                pending
                    .drain(..)
                    .map(|(id, name, arguments)| {
                        json!({"id": id, "type": "function",
                           "function": {"name": name, "arguments": arguments}})
                    })
                    .collect(),
            );
            out.push(ChatMessage {
                role: "assistant".into(),
                content: content.to_string(),
                tool_calls_json: Some(calls.to_string()),
                ..Default::default()
            });
        };
    for item in items {
        match item {
            ModelInputItem::ToolCall {
                call_id,
                name,
                arguments_json,
            } => {
                pending_calls.push((call_id.clone(), name.clone(), arguments_json.clone()));
            }
            ModelInputItem::Message { role, content } => {
                if role == "assistant" {
                    // assistant Message：pending 调用并入同一条 assistant 消息。
                    let content_c = content.clone();
                    let mut calls_json = None;
                    if !pending_calls.is_empty() {
                        let calls: Value = Value::Array(
                            pending_calls
                                .drain(..)
                                .map(|(id, name, arguments)| {
                                    json!({"id": id, "type": "function",
                                           "function": {"name": name, "arguments": arguments}})
                                })
                                .collect(),
                        );
                        calls_json = Some(calls.to_string());
                    }
                    out.push(ChatMessage {
                        role: "assistant".into(),
                        content: content_c,
                        tool_calls_json: calls_json,
                        ..Default::default()
                    });
                } else {
                    flush_calls(&mut out, &mut pending_calls, "");
                    out.push(ChatMessage {
                        role: role.clone(),
                        content: content.clone(),
                        ..Default::default()
                    });
                }
            }
            ModelInputItem::ToolResult { call_id, output } => {
                flush_calls(&mut out, &mut pending_calls, "");
                out.push(ChatMessage {
                    role: "tool".into(),
                    content: output.clone(),
                    tool_call_id: Some(call_id.clone()),
                    tool_calls_json: None,
                });
            }
            // reasoning/compaction opaque 项不回放（chat_completions 协议）。
            ModelInputItem::ReasoningOpaque { .. } | ModelInputItem::CompactionOpaque { .. } => {}
        }
    }
    flush_calls(&mut out, &mut pending_calls, "");
    out
}

pub fn get_proposal(store: &Store, id: &str) -> Result<Proposal, Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT id, agent_run_id, tool, arguments, risk, action_digest, decision, COALESCE(result,''), created_at
             FROM tool_proposals WHERE id=?1",
            [id],
            |r| {
                Ok(Proposal {
                    id: r.get(0)?,
                    run_id: r.get(1)?,
                    tool: crate::provider::ToolId::parse(&r.get::<_, String>(2)?).short_name(),
                    arguments: r.get(3)?,
                    risk: r.get(4)?,
                    action_digest: r.get(5)?,
                    decision: r.get(6)?,
                    result: r.get(7)?,
                    created_at: r.get(8)?,
                })
            },
        )
        .map_err(|_| Error::Message("proposal_not_found".into()))
    })
}

/// 已执行工具数（恢复时从库重算，不依赖 checkpoint）。
/// M0-07：unknown/indeterminate 消耗了一次执行尝试，计入诚实预算口径。
pub fn count_tool_calls(store: &Store, run_id: &str) -> Result<i64, Error> {
    store.with_conn(|conn| {
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM tool_proposals WHERE agent_run_id=?1 AND decision IN ('executed','exec_failed','unknown','indeterminate')",
            [run_id],
            |r| r.get(0),
        )?)
    })
}

/// 模型调用数（不重复计费的对账口径）。
pub fn count_model_calls(store: &Store, run_id: &str) -> Result<i64, Error> {
    store.with_conn(|conn| {
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM model_calls WHERE agent_run_id=?1",
            [run_id],
            |r| r.get(0),
        )?)
    })
}

/// 从 run 行重建任务侧配置（审批通过后拉起恢复任务用）：
/// (workitem_id, goal, manifest_id, tool_allowlist, budget)
pub fn row_config(
    store: &Store,
    run_id: &str,
) -> Result<(String, String, String, Vec<String>, RunBudget), Error> {
    store.with_conn(|conn| {
        conn.query_row(
            "SELECT workitem_id, goal, context_manifest_id, tool_allowlist, budget FROM agent_runs WHERE id=?1",
            [run_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            },
        )
        .map_err(|_| Error::Message("run_not_found".into()))
    })
    .map(|(wi, goal, manifest, allow, budget)| {
        let allowlist: Vec<String> =
            serde_json::from_str(&allow).unwrap_or_else(|_| vec!["read_file".into()]);
        let budget: RunBudget =
            serde_json::from_str(&budget).unwrap_or_default();
        (wi, goal, manifest, allowlist, budget)
    })
}

/// 审批拒绝后的收尾：paused → failed（F03）。
pub fn fail_paused_run(store: &Store, run_id: &str, message: &str) -> Result<AgentRun, Error> {
    let mut run = get_run(store, run_id)?;
    if run.status != "paused" {
        return Ok(run);
    }
    run.result = message.into();
    set_status(store, &mut run, "failed", "run.failed")?;
    finish(store, &run)?;
    Ok(run)
}

/// 启动对账（ADR-028 崩溃恢复）：进程每次启动都是全新执行循环，
/// 上一进程遗留的 queued/running Run 已无人在推进，统一标 failed，
/// 前端轮询立即见到终态而不是干等超时。paused 是合法的等待审批态，不动。
/// M0-07：被打断 Run 遗留的 proposed/approved 提案落一等 unknown —— 崩溃可能
/// 发生在 executor 调用中途，副作用是否已发生不可证明；已 failed/cancelled Run
/// 的同态孤儿提案一并收敛（EV-009）。
pub fn reconcile_interrupted(store: &Store) -> Result<usize, Error> {
    let stale: Vec<String> = store.with_conn(|conn| {
        let mut stmt =
            conn.prepare("SELECT id FROM agent_runs WHERE status IN ('queued','running')")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;
    let n = if stale.is_empty() {
        0
    } else {
        store.with_conn(|conn| {
            Ok(conn.execute(
                "UPDATE agent_runs SET status='failed',
                        result='运行中断：应用重启时未完成（请重新发起）',
                        updated_at=?1
                 WHERE status IN ('queued','running')",
                rusqlite::params![timefmt::now()],
            )? as usize)
        })?
    };
    for id in &stale {
        let _ = outbox::emit(
            store,
            "agent_run",
            id,
            "run.failed",
            json!({"status": "failed", "reason": "interrupted"}),
        );
    }
    let orphaned = reconcile_orphan_proposals(store)?;
    Ok(n + orphaned)
}

/// 启动对账第二段：终态 Run（failed/cancelled）上仍处 proposed/approved 的提案
/// 不可能再被执行，也无法证明 executor 未被调用过 —— 诚实终态是 unknown +
/// 对账 pending（M0-07）。
fn reconcile_orphan_proposals(store: &Store) -> Result<usize, Error> {
    let orphans: Vec<String> = store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT p.id FROM tool_proposals p
             JOIN agent_runs r ON r.id = p.agent_run_id
             WHERE p.decision IN ('proposed','approved')
               AND r.status IN ('failed','cancelled')",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;
    for id in &orphans {
        store.with_conn(|conn| {
            conn.execute(
                "UPDATE tool_proposals SET decision='unknown',
                        result=COALESCE(NULLIF(result,''), '运行中断：执行结果未确认（待对账）')
                 WHERE id=?1 AND decision IN ('proposed','approved')",
                rusqlite::params![id],
            )?;
            Ok(())
        })?;
        // WP-3：send_phase 携带进对账原因——phase≥send_intent_persisted 即
        // 「intent 已落库、flush 是否发生不可知」窗口（非只读按 unknown 收敛）；
        // not_sent 侧写明安全面（未送出，可新 proposal 重试）。
        let phase: String = store
            .with_conn(|conn| {
                Ok(conn
                    .query_row(
                        "SELECT COALESCE(send_phase,'not_sent') FROM tool_proposals WHERE id=?1",
                        [id],
                        |r| r.get(0),
                    )
                    .unwrap_or_else(|_| "not_sent".into()))
            })
            .unwrap_or_else(|_| "not_sent".into());
        record_outcome(
            store,
            id,
            "unknown",
            &format!("run_interrupted_side_effect_unverified(send_phase={phase})"),
        )?;
    }
    Ok(orphans.len())
}

pub fn proposals(store: &Store, run_id: &str) -> Result<Vec<Proposal>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, agent_run_id, tool, arguments, risk, action_digest, decision, COALESCE(result,''), created_at
             FROM tool_proposals WHERE agent_run_id=?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map([run_id], |r| {
            Ok(Proposal {
                id: r.get(0)?, run_id: r.get(1)?,
                tool: crate::provider::ToolId::parse(&r.get::<_, String>(2)?).short_name(),
                arguments: r.get(3)?,
                risk: r.get(4)?, action_digest: r.get(5)?, decision: r.get(6)?,
                result: r.get(7)?, created_at: r.get(8)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

/// 结构化决策 Schema 校验（简易：action/summary 必填）。
pub fn validate_decision_shape(content: &str) -> bool {
    parse_decision(content).is_some()
}

/// 原生/legacy assistant 消息构造：native 携带 tool_calls JSON（call_id 回传链），
/// legacy 维持"tool 名(参数)"扁平文本。
fn native_assistant_msg(
    response: &sg_integrations::model::CompletionResponse,
    native_call: Option<&sg_integrations::model::ProviderToolCall>,
    decision: &Decision,
) -> ChatMessage {
    if let Some(tc) = native_call {
        let calls = serde_json::json!([
            {"id": tc.id, "type": "function",
             "function": {"name": tc.name, "arguments": tc.arguments}}
        ]);
        ChatMessage {
            role: "assistant".into(),
            content: response.content.clone(),
            tool_calls_json: Some(calls.to_string()),
            ..Default::default()
        }
    } else if decision.action == "final" {
        ChatMessage {
            role: "assistant".into(),
            content: decision.summary.clone(),
            ..Default::default()
        }
    } else {
        ChatMessage {
            role: "assistant".into(),
            content: format!("tool {}({})", decision.action, decision.arguments),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sg_integrations::model::FakeModel;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-agent-{}-{}",
            std::process::id(),
            ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        store.with_conn(|c| {
            c.execute("INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at) VALUES ('pj','u','n','p','main',?1)", [timefmt::now()])?;
            c.execute("INSERT INTO workitems(id, project_id, title, created_at, updated_at) VALUES ('wi','pj','t',?1,?1)", [timefmt::now()])?;
            c.execute("INSERT INTO context_manifests(id, workitem_id, scope, data_policy, created_at) VALUES ('ctx1','wi','{}','standard',?1)", [timefmt::now()])?;
            Ok(())
        }).unwrap();
        store
    }

    fn policy_snapshot() -> Snapshot {
        Snapshot {
            tool_rules: vec![
                sg_policy::ToolRule {
                    tool: "read_file".into(),
                    risk: sg_policy::Risk::Low,
                    requires_approval: false,
                    data_level: "internal".into(),
                    max_result_bytes: 65536,
                    timeout_sec: 60,
                },
                sg_policy::ToolRule {
                    tool: "run_command".into(),
                    risk: sg_policy::Risk::High,
                    requires_approval: true,
                    data_level: "internal".into(),
                    max_result_bytes: 65536,
                    timeout_sec: 60,
                },
            ],
            approval_ttl_secs: 3600,
        }
    }

    #[test]
    fn run_completes_with_final() {
        let store = setup();
        let fake = FakeModel::default();
        fake.push_response(
            r#"{"action":"read_file","arguments":{"path":"README.md"},"summary":"读取"}"#,
            10,
            5,
        );
        fake.push_response(
            r#"{"action":"final","summary":"分析完成：3 个模块"}"#,
            20,
            8,
        );
        let gateway = Gateway::new(Box::new(fake));
        let executed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = executed.clone();
        let executor = move |p: &Proposal| -> Result<String, String> {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(format!("content of {}", p.tool))
        };
        let out = start(
            &store,
            &gateway,
            &policy_snapshot(),
            Some(&executor),
            &RunConfig {
                workitem_id: "wi",
                task_id: "",
                goal: "分析仓库",
                manifest_id: "ctx1",
                tool_allowlist: &["read_file".into()],
                idempotency_key: "key-1",
                budget: &RunBudget::default(),
                max_iterations: 10,
            },
        )
        .unwrap();
        assert_eq!(out.run.status, "completed_execution");
        assert_eq!(out.output, "分析完成：3 个模块");
        assert_eq!(executed.load(std::sync::atomic::Ordering::SeqCst), 1);
        let props = proposals(&store, &out.run.id).unwrap();
        assert_eq!(props.len(), 1);
        assert_eq!(props[0].decision, "executed");
    }

    /// ADR-033 M1：原生 tool_calls 全链——call_id transcript + 提案执行 + 正文 final。
    #[test]
    fn native_tool_call_run_completes_with_call_id_transcript() {
        let store = setup();
        let fake = FakeModel::default();
        fake.enable_native();
        fake.push_response(
            r#"{"action":"read_file","arguments":{"path":"README.md"},"summary":"读取"}"#,
            10,
            5,
        );
        fake.push_response(r#"{"action":"final","summary":"native done"}"#, 20, 8);
        let gateway = Gateway::new(Box::new(fake));
        let executor =
            move |p: &Proposal| -> Result<String, String> { Ok(format!("content of {}", p.tool)) };
        let out = start(
            &store,
            &gateway,
            &policy_snapshot(),
            Some(&executor),
            &RunConfig {
                workitem_id: "wi",
                task_id: "",
                goal: "native 场景",
                manifest_id: "ctx1",
                tool_allowlist: &["read_file".into()],
                idempotency_key: "key-native",
                budget: &RunBudget::default(),
                max_iterations: 10,
            },
        )
        .unwrap();
        assert_eq!(out.run.status, "completed_execution");
        assert_eq!(out.output, "native done");
        let props = proposals(&store, &out.run.id).unwrap();
        assert_eq!(props.len(), 1);
        // WP-2：tool 列写侧 canonical，读侧（RPC/执行链）映射回短名 = legacy 等价。
        assert_eq!(props[0].tool, "read_file");
        let raw_tool: String = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT tool FROM tool_proposals WHERE id=?1",
                    [&props[0].id],
                    |r| r.get(0),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(raw_tool, "builtin:read_file", "持久化列为 canonical 形态");
        assert_eq!(props[0].decision, "executed");

        // transcript：M4 checkpoint v2 —— schemaVersion=2 + ModelInputItem[]，
        // tool 结果以 ToolResult 项携带同一 call_id（不再降级 user）。
        let state: String = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT state FROM agent_checkpoints WHERE agent_run_id=?1 ORDER BY seq DESC LIMIT 1",
                    [&out.run.id],
                    |r| r.get(0),
                )
                .map_err(sg_store::Error::from)
            })
            .unwrap();
        assert!(
            state.contains(r#""schemaVersion":2"#),
            "checkpoint 应为 v2：{state}"
        );
        let has_call_id = state.contains(r#""call_id":"call_0""#);
        assert!(has_call_id, "checkpoint 应携带 call_id transcript：{state}");
        let has_tool_calls = state.contains(r#""name":"read_file""#) || state.contains("read_file");
        assert!(has_tool_calls);
    }

    /// 原生并行 tool_calls 首期拒绝（ADR-033 决策 6）。
    #[test]
    fn native_parallel_tool_calls_fail_run() {
        let store = setup();
        let fake = FakeModel::default();
        fake.enable_native();
        // 数组脚本 → 单响应并行 tool_calls（桥接层）。
        fake.push_response(
            r#"[{"action":"read_file","arguments":{"path":"a.md"}},{"action":"read_file","arguments":{"path":"b.md"}}]"#,
            5,
            5,
        );
        let gateway = Gateway::new(Box::new(fake));
        let executor = move |_p: &Proposal| -> Result<String, String> { Ok("ok".into()) };
        let outcome = start(
            &store,
            &gateway,
            &policy_snapshot(),
            Some(&executor),
            &RunConfig {
                workitem_id: "wi",
                task_id: "",
                goal: "parallel",
                manifest_id: "ctx1",
                tool_allowlist: &["read_file".into()],
                idempotency_key: "key-par",
                budget: &RunBudget::default(),
                max_iterations: 10,
            },
        );
        let err = match outcome {
            Ok(_) => panic!("并行 tool_calls 应被拒绝"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("model_protocol_violation"),
            "应拒绝并行工具调用：{err}"
        );
    }

    #[test]
    fn invalid_json_retried_then_final() {
        let store = setup();
        let fake = FakeModel::default();
        fake.push_response("我觉得应该直接完成", 5, 3);
        fake.push_response(r#"{"action":"final","summary":"done"}"#, 5, 3);
        let gateway = Gateway::new(Box::new(fake));
        let executor = |_p: &Proposal| -> Result<String, String> { Ok(String::new()) };
        let out = start(
            &store,
            &gateway,
            &policy_snapshot(),
            Some(&executor),
            &RunConfig {
                workitem_id: "wi",
                task_id: "",
                goal: "g",
                manifest_id: "ctx1",
                tool_allowlist: &["read_file".into()],
                idempotency_key: "key-2",
                budget: &RunBudget::default(),
                max_iterations: 5,
            },
        )
        .unwrap();
        assert_eq!(out.run.status, "completed_execution");
    }

    #[test]
    fn high_risk_pauses_run_waiting_approval() {
        let store = setup();
        let fake = FakeModel::default();
        fake.push_response(
            r#"{"action":"run_command","arguments":{"argv":["ls"]},"summary":"执行"}"#,
            5,
            3,
        );
        fake.push_response(r#"{"action":"final","summary":"完成"}"#, 5, 3);
        let gateway = Gateway::new(Box::new(fake));
        let executed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = executed.clone();
        let executor = move |_p: &Proposal| -> Result<String, String> {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(String::new())
        };
        let out = start(
            &store,
            &gateway,
            &policy_snapshot(),
            Some(&executor),
            &RunConfig {
                workitem_id: "wi",
                task_id: "",
                goal: "g",
                manifest_id: "ctx1",
                tool_allowlist: &["read_file".into(), "run_command".into()],
                idempotency_key: "key-3",
                budget: &RunBudget::default(),
                max_iterations: 5,
            },
        )
        .unwrap();
        // M1/F03：Run 挂起而非作废；提案保持 proposed；审批待办出现。
        assert_eq!(out.run.status, "paused");
        assert_eq!(
            executed.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "高风险工具未获批准不得执行"
        );
        let props = proposals(&store, &out.run.id).unwrap();
        assert_eq!(props[0].decision, "proposed");
        let pending = sg_policy::pending(&store, 10).unwrap();
        assert_eq!(pending.len(), 1);

        let count = |ty: &str| -> i64 {
            store
                .with_conn(|c| {
                    let n: i64 = c.query_row(
                        "SELECT COUNT(*) FROM events_outbox WHERE type=?1 AND aggregate_id=?2",
                        rusqlite::params![ty, out.run.id],
                        |r| r.get(0),
                    )?;
                    Ok(n)
                })
                .unwrap()
        };
        assert_eq!(
            count("run.waiting_approval"),
            1,
            "run.waiting_approval 恰好一条"
        );
        assert_eq!(count("run.failed"), 0, "暂停不是失败");

        // 审批通过 → 恢复 → 执行挂起提案 → final 完成；模型调用共 2 次（不重复计费）。
        let approval_id = pending[0].id.clone();
        sg_policy::decide(&store, &approval_id, "approved", "tester", "测试批准").unwrap();
        let config2 = RunConfig {
            workitem_id: "wi",
            task_id: "",
            goal: "g",
            manifest_id: "ctx1",
            tool_allowlist: &["read_file".into(), "run_command".into()],
            idempotency_key: "key-3-resume",
            budget: &RunBudget::default(),
            max_iterations: 5,
        };
        let initial = crate::prompt::assemble(
            &crate::prompt::PromptEnv::default(),
            &["read_file".to_string(), "run_command".to_string()],
            &crate::prompt::knowledge_text("", ""),
            "g",
        );
        let resumed = execute_run(
            &store,
            &gateway,
            &policy_snapshot(),
            Some(&executor),
            &config2,
            &out.run.id,
            None,
            None,
            &initial,
            &CompactPolicy::default(),
            None,
        )
        .unwrap();
        assert_eq!(resumed.run.status, "completed_execution");
        assert_eq!(
            executed.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "恢复后执行挂起提案一次"
        );
        let props = proposals(&store, &out.run.id).unwrap();
        assert_eq!(props[0].decision, "executed");
        assert_eq!(
            count_model_calls(&store, &out.run.id).unwrap(),
            2,
            "模型调用恰好 2 次（不重复计费）"
        );
        assert_eq!(count("run.resumed"), 1, "run.resumed 恰好一条");
        assert_eq!(count("run.completed_execution"), 1);
    }

    #[test]
    fn approval_rejected_fails_paused_run() {
        let store = setup();
        let fake = FakeModel::default();
        fake.push_response(
            r#"{"action":"run_command","arguments":{"argv":["ls"]},"summary":"执行"}"#,
            5,
            3,
        );
        let gateway = Gateway::new(Box::new(fake));
        let executor = |_p: &Proposal| -> Result<String, String> { Ok(String::new()) };
        let out = start(
            &store,
            &gateway,
            &policy_snapshot(),
            Some(&executor),
            &RunConfig {
                workitem_id: "wi",
                task_id: "",
                goal: "g",
                manifest_id: "ctx1",
                tool_allowlist: &["run_command".into()],
                idempotency_key: "key-reject",
                budget: &RunBudget::default(),
                max_iterations: 5,
            },
        )
        .unwrap();
        assert_eq!(out.run.status, "paused");
        let pending = sg_policy::pending(&store, 10).unwrap();
        sg_policy::decide(&store, &pending[0].id, "rejected", "tester", "风险过大").unwrap();
        let run = fail_paused_run(&store, &out.run.id, "审批拒绝：风险过大（tester）").unwrap();
        assert_eq!(run.status, "failed");
        assert!(run.result.contains("审批拒绝"));
    }

    #[test]
    fn idempotency_reuses_run() {
        let store = setup();
        let fake = FakeModel::default();
        fake.push_response(r#"{"action":"final","summary":"s"}"#, 5, 3);
        let gateway = Gateway::new(Box::new(fake));
        let executor = |_p: &Proposal| -> Result<String, String> { Ok(String::new()) };
        let first = start(
            &store,
            &gateway,
            &policy_snapshot(),
            Some(&executor),
            &RunConfig {
                workitem_id: "wi",
                task_id: "",
                goal: "g",
                manifest_id: "ctx1",
                tool_allowlist: &["read_file".into()],
                idempotency_key: "same-key",
                budget: &RunBudget::default(),
                max_iterations: 5,
            },
        )
        .unwrap();
        let second = start(
            &store,
            &gateway,
            &policy_snapshot(),
            Some(&executor),
            &RunConfig {
                workitem_id: "wi",
                task_id: "",
                goal: "g",
                manifest_id: "ctx1",
                tool_allowlist: &["read_file".into()],
                idempotency_key: "same-key",
                budget: &RunBudget::default(),
                max_iterations: 5,
            },
        )
        .unwrap();
        assert_eq!(first.run.id, second.run.id);
    }

    #[test]
    fn cancel_flag_stops_loop_with_single_event() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let store = setup();
        let fake = FakeModel::default();
        for _ in 0..5 {
            fake.push_response(
                r#"{"action":"read_file","arguments":{"path":"a"},"summary":"r"}"#,
                5,
                3,
            );
        }
        let gateway = Gateway::new(Box::new(fake));
        // M2：取消令牌（取代 AtomicBool）；工具执行后请求取消，下一迭代边界收尾。
        let token = std::sync::Arc::new(sg_integrations::CancelToken::new());
        let t2 = token.clone();
        let executed = Arc::new(AtomicUsize::new(0));
        let counter = executed.clone();
        let executor = move |_p: &Proposal| -> Result<String, String> {
            counter.fetch_add(1, Ordering::SeqCst);
            t2.cancel();
            Ok("ok".into())
        };
        let config = RunConfig {
            workitem_id: "wi",
            task_id: "",
            goal: "g",
            manifest_id: "ctx1",
            tool_allowlist: &["read_file".into()],
            idempotency_key: "key-cancel",
            budget: &RunBudget::default(),
            max_iterations: 10,
        };
        let (run, created) = create_run(&store, &config).unwrap();
        assert!(created);
        assert_eq!(run.status, "running");
        let initial = crate::prompt::assemble(
            &crate::prompt::PromptEnv::default(),
            &["read_file".to_string()],
            &crate::prompt::knowledge_text("", ""),
            "g",
        );
        let out = execute_run(
            &store,
            &gateway,
            &policy_snapshot(),
            Some(&executor),
            &config,
            &run.id,
            Some(&token),
            None,
            &initial,
            &CompactPolicy::default(),
            None,
        )
        .unwrap();
        assert_eq!(out.run.status, "cancelled");
        assert_eq!(executed.load(Ordering::SeqCst), 1);

        let count = |ty: &str| -> i64 {
            store
                .with_conn(|c| {
                    let n: i64 = c.query_row(
                        "SELECT COUNT(*) FROM events_outbox WHERE type=?1 AND aggregate_id=?2",
                        rusqlite::params![ty, run.id],
                        |r| r.get(0),
                    )?;
                    Ok(n)
                })
                .unwrap()
        };
        assert_eq!(count("run.started"), 1, "run.started 恰好一条");
        assert_eq!(count("run.cancelled"), 1, "run.cancelled 恰好一条");
    }

    #[test]
    fn empty_output_error_classified_as_output_not_context() {
        // 回归：finish_reason=length 的空正文是输出上限耗尽，
        // 不得因错误串含 "length" 被误归类为 context_too_large。
        let store = setup();
        let fake = FakeModel::default();
        fake.push_error(
            "model_empty_output: 模型未返回正文（finish_reason=length）；输出 token 预算耗尽",
        );
        let gateway = Gateway::new(Box::new(fake));
        let err = match start(
            &store,
            &gateway,
            &policy_snapshot(),
            None,
            &RunConfig {
                workitem_id: "wi",
                task_id: "",
                goal: "g",
                manifest_id: "ctx1",
                tool_allowlist: &["read_file".into()],
                idempotency_key: "key-empty-out",
                budget: &RunBudget::default(),
                max_iterations: 5,
            },
        ) {
            Err(e) => e,
            Ok(_) => panic!("空输出错误必须使 Run failed"),
        };
        let run = get_run(&store, &err_run_id(&store, "key-empty-out")).unwrap();
        assert_eq!(run.status, "failed");
        assert!(
            run.result.starts_with("model_output_empty"),
            "实际归类：{}（err={err}）",
            run.result
        );
        assert!(!run.result.contains("context_too_large"));
    }

    /// M2：流式全链——fake 流式桥 → 聚合器 → run.turn_committed 恰好每轮一条；
    /// delta 不写 outbox（万级 delta 也只产生 1 条耐久 turn 事件）。
    #[test]
    fn streaming_run_emits_single_turn_commit_and_no_delta_rows() {
        let store = setup();
        let fake = std::sync::Arc::new(FakeModel::default());
        fake.enable_native();
        fake.enable_streaming();
        fake.push_response(
            r#"{"action":"read_file","arguments":{"path":"a.md"},"summary":"读取"}"#,
            10,
            5,
        );
        fake.push_response(r#"{"action":"final","summary":"流式任务完成"}"#, 20, 8);
        let gateway = Gateway::with_shared(fake.clone(), Some(fake.clone()));
        let rt = tokio::runtime::Runtime::new().unwrap();
        gateway.set_runtime_handle(rt.handle().clone());
        let executor =
            move |p: &Proposal| -> Result<String, String> { Ok(format!("content of {}", p.tool)) };
        let out = start(
            &store,
            &gateway,
            &policy_snapshot(),
            Some(&executor),
            &RunConfig {
                workitem_id: "wi",
                task_id: "",
                goal: "流式场景",
                manifest_id: "ctx1",
                tool_allowlist: &["read_file".into()],
                idempotency_key: "key-stream-e2e",
                budget: &RunBudget::default(),
                max_iterations: 10,
            },
        )
        .unwrap();
        assert_eq!(out.run.status, "completed_execution");
        assert_eq!(out.output, "流式任务完成");
        let count = |ty: &str| -> i64 {
            store
                .with_conn(|c| {
                    Ok(c.query_row(
                        "SELECT COUNT(*) FROM events_outbox WHERE type=?1 AND aggregate_id=?2",
                        rusqlite::params![ty, out.run.id],
                        |r| r.get::<_, i64>(0),
                    )?)
                })
                .unwrap()
        };
        // 两轮模型调用 → 恰好两条耐久 turn commit。
        assert_eq!(count("run.turn_committed"), 2, "每轮恰好一条 turn commit");
        // delta 是易失事件：永不落 outbox。
        assert_eq!(count("run.output_delta"), 0);
        assert_eq!(count("run.tool_arguments_delta"), 0);
        // 流式 turn 的协议观测与首 token 延迟落 model_turns。
        let rows: Vec<(String, Option<i64>)> = store
            .with_conn(|c| {
                let mut stmt =
                    c.prepare("SELECT protocol, ttft_ms FROM model_turns WHERE agent_run_id=?1")?;
                let rows = stmt.query_map([&out.run.id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?))
                })?;
                rows.collect::<std::result::Result<Vec<_>, rusqlite::Error>>()
                    .map_err(sg_store::Error::from)
            })
            .unwrap();
        assert_eq!(rows.len(), 2);
        for (protocol, ttft) in &rows {
            assert_eq!(protocol, "native_tools_sse");
            assert!(ttft.is_some(), "流式轮次应有首 token 延迟");
        }
    }

    /// M2：提案指纹顺序——run.turn_committed 先于 tool.proposed（完成帧验证后才
    /// 创建 Proposal）。
    #[test]
    fn turn_commit_precedes_proposal_in_outbox_order() {
        let store = setup();
        let fake = std::sync::Arc::new(FakeModel::default());
        fake.enable_native();
        fake.enable_streaming();
        fake.push_response(
            r#"{"action":"read_file","arguments":{"path":"a.md"},"summary":"读取"}"#,
            5,
            3,
        );
        fake.push_response(r#"{"action":"final","summary":"ok"}"#, 5, 3);
        let gateway = Gateway::with_shared(fake.clone(), Some(fake.clone()));
        let rt = tokio::runtime::Runtime::new().unwrap();
        gateway.set_runtime_handle(rt.handle().clone());
        let executor = |p: &Proposal| -> Result<String, String> { Ok(format!("c of {}", p.tool)) };
        let out = start(
            &store,
            &gateway,
            &policy_snapshot(),
            Some(&executor),
            &RunConfig {
                workitem_id: "wi",
                task_id: "",
                goal: "g",
                manifest_id: "ctx1",
                tool_allowlist: &["read_file".into()],
                idempotency_key: "key-commit-order",
                budget: &RunBudget::default(),
                max_iterations: 10,
            },
        )
        .unwrap();
        let seqs: Vec<(String, i64)> = store
            .with_conn(|c| {
                let mut stmt = c
                    .prepare("SELECT type, sequence FROM events_outbox WHERE aggregate_id=?1 ORDER BY sequence")?;
                let rows = stmt.query_map([&out.run.id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                })?;
                rows.collect::<std::result::Result<Vec<_>, rusqlite::Error>>()
                    .map_err(sg_store::Error::from)
            })
            .unwrap();
        let commit = seqs
            .iter()
            .find(|(t, _)| t == "run.turn_committed")
            .expect("应有 turn commit");
        let propose = seqs
            .iter()
            .find(|(t, _)| t == "tool.proposed")
            .expect("应有提案");
        assert!(
            commit.1 < propose.1,
            "turn commit (seq {}) 必须先于提案 (seq {})",
            commit.1,
            propose.1
        );
    }

    /// M2：流中段取消——挂起的流被令牌中止，Run 进入 cancelled，
    /// 无 completed/turn_committed，run.cancelled 恰好一条。
    #[test]
    fn cancel_mid_stream_finalizes_cancelled_within_budget() {
        let store = setup();
        let fake = std::sync::Arc::new(FakeModel::default());
        fake.enable_native();
        fake.enable_streaming();
        fake.hold_stream_until_cancel();
        fake.push_response(r#"{"action":"final","summary":"不该到达"}"#, 5, 3);
        let gateway = Gateway::with_shared(fake.clone(), Some(fake.clone()));
        let rt = tokio::runtime::Runtime::new().unwrap();
        gateway.set_runtime_handle(rt.handle().clone());
        let token = std::sync::Arc::new(sg_integrations::CancelToken::new());
        let t2 = token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            t2.cancel();
        });
        let config = RunConfig {
            workitem_id: "wi",
            task_id: "",
            goal: "g",
            manifest_id: "ctx1",
            tool_allowlist: &["read_file".into()],
            idempotency_key: "key-cancel-stream",
            budget: &RunBudget::default(),
            max_iterations: 10,
        };
        let (run, created) = create_run(&store, &config).unwrap();
        assert!(created);
        let started = std::time::Instant::now();
        let initial = crate::prompt::assemble(
            &crate::prompt::PromptEnv::default(),
            &["read_file".to_string()],
            &crate::prompt::knowledge_text("", ""),
            "g",
        );
        let out = execute_run(
            &store,
            &gateway,
            &policy_snapshot(),
            None,
            &config,
            &run.id,
            Some(&token),
            None,
            &initial,
            &CompactPolicy::default(),
            None,
        )
        .unwrap();
        let elapsed = started.elapsed();
        assert_eq!(out.run.status, "cancelled");
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "取消收尾应在秒级：{elapsed:?}"
        );
        let count = |ty: &str| -> i64 {
            store
                .with_conn(|c| {
                    Ok(c.query_row(
                        "SELECT COUNT(*) FROM events_outbox WHERE type=?1 AND aggregate_id=?2",
                        rusqlite::params![ty, out.run.id],
                        |r| r.get::<_, i64>(0),
                    )?)
                })
                .unwrap()
        };
        assert_eq!(count("run.cancelled"), 1, "cancelled 恰好一条");
        assert_eq!(count("run.completed_execution"), 0, "无 completed 事件");
        assert_eq!(count("run.turn_committed"), 0, "中断轮次无 turn commit");
    }

    /// M2：call_turn 转发 output/tool-arguments delta（半个参数分片可见），
    /// 且工具参数完成后才聚合出完整 arguments（提案原材料不残缺）。
    #[test]
    fn call_turn_forwards_deltas_and_assembles_full_arguments() {
        use crate::modelgw::{DeltaKind, TurnDeltaForwarder};
        struct Recorder(std::sync::Mutex<Vec<(&'static str, String)>>);
        impl TurnDeltaForwarder for Recorder {
            fn forward(&self, _run_id: &str, kind: DeltaKind, text: &str) {
                self.0
                    .lock()
                    .unwrap()
                    .push((kind.event_type(), text.to_string()));
            }
        }
        let store = setup();
        let fake = std::sync::Arc::new(FakeModel::default());
        fake.enable_native();
        fake.enable_streaming();
        fake.push_response(
            r#"{"action":"read_file","arguments":{"path":"a.md"},"summary":"读取"}"#,
            5,
            3,
        );
        fake.push_response(r#"{"action":"final","summary":"流式正文"}"#, 5, 3);
        let gateway = Gateway::with_shared(fake.clone(), Some(fake.clone()));
        let rt = tokio::runtime::Runtime::new().unwrap();
        gateway.set_runtime_handle(rt.handle().clone());
        let recorder = std::sync::Arc::new(Recorder(std::sync::Mutex::new(Vec::new())));
        let token = sg_integrations::CancelToken::new();
        let resp = gateway
            .call_turn(
                &store,
                "run-fw",
                &Budget::default(),
                &CompletionRequest {
                    model: String::new(),
                    system_prompt: "s".into(),
                    messages: vec![ChatMessage {
                        role: "user".into(),
                        content: "hi".into(),
                        ..Default::default()
                    }],
                    max_tokens: 256,
                    response_schema: None,
                    tools_json: Some(crate::tools::provider_tools_json(&["read_file".into()])),
                },
                &modelgw::TurnOpts {
                    cancel: &token,
                    forwarder: Some(recorder.clone()),
                    prompt_cache_key: "test-cache-key".into(),
                    model_call_id: ids::new_id("mc"),
                },
            )
            .unwrap();
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].arguments, r#"{"path":"a.md"}"#);
        // 第二轮：final 正文（验证 output delta 转发）。
        let _ = gateway
            .call_turn(
                &store,
                "run-fw",
                &Budget::default(),
                &CompletionRequest {
                    model: String::new(),
                    system_prompt: "s".into(),
                    messages: vec![ChatMessage {
                        role: "user".into(),
                        content: "hi".into(),
                        ..Default::default()
                    }],
                    max_tokens: 256,
                    response_schema: None,
                    tools_json: Some(crate::tools::provider_tools_json(&["read_file".into()])),
                },
                &modelgw::TurnOpts {
                    cancel: &token,
                    forwarder: Some(recorder.clone()),
                    prompt_cache_key: "test-cache-key".into(),
                    model_call_id: ids::new_id("mc"),
                },
            )
            .unwrap();
        let events = recorder.0.lock().unwrap();
        let tool_deltas: Vec<&str> = events
            .iter()
            .filter(|(k, _)| *k == "run.tool_arguments_delta")
            .map(|(_, v)| v.as_str())
            .collect();
        assert!(tool_deltas.len() >= 2, "参数应分片转发：{tool_deltas:?}");
        // 分片拼回 = 最终参数（聚合完整性）。
        assert_eq!(tool_deltas.concat(), r#"{"path":"a.md"}"#);
        let text: String = events
            .iter()
            .filter(|(k, _)| *k == "run.output_delta")
            .map(|(_, v)| v.as_str())
            .collect();
        assert!(text.contains("流式正文"), "正文 delta 应转发：{text}");
    }

    // ---------- M4：checkpoint v2 / reasoning 持久化 / 缓存观测 ----------

    use std::sync::Arc;

    fn streaming_gateway(fake: &std::sync::Arc<FakeModel>) -> Gateway {
        let gateway = Gateway::with_shared(fake.clone(), Some(fake.clone()));
        let rt = tokio::runtime::Runtime::new().unwrap();
        gateway.set_runtime_handle(rt.handle().clone());
        gateway
    }

    /// checkpoint v2：指纹绑定——存储指纹与当前 Provider 不一致 → 明确回退全新循环。
    #[test]
    fn checkpoint_v2_provider_mismatch_falls_back_to_fresh_loop() {
        let store = setup();
        let fake = std::sync::Arc::new(FakeModel::default());
        fake.enable_native();
        fake.enable_streaming();
        fake.push_response(
            r#"{"action":"run_command","arguments":{"argv":["ls"]},"summary":"执行"}"#,
            5,
            3,
        );
        fake.push_response(r#"{"action":"final","summary":"新循环完成"}"#, 5, 3);
        let gateway = streaming_gateway(&fake);
        let config = RunConfig {
            workitem_id: "wi",
            task_id: "",
            goal: "g",
            manifest_id: "ctx1",
            tool_allowlist: &["run_command".into()],
            idempotency_key: "key-fp-mismatch",
            budget: &RunBudget::default(),
            max_iterations: 10,
        };
        let (run, _) = create_run(&store, &config).unwrap();
        let initial = crate::prompt::assemble(
            &crate::prompt::PromptEnv::default(),
            &["run_command".to_string()],
            &crate::prompt::knowledge_text("", ""),
            "g",
        );
        let mut rollout = crate::rollout::Rollout::open(&store.data_dir, &run.id).ok();
        let out = execute_run(
            &store,
            &gateway,
            &policy_snapshot(),
            None,
            &config,
            &run.id,
            None,
            rollout.take(),
            &initial,
            &CompactPolicy::default(),
            None,
        )
        .unwrap();
        assert_eq!(out.run.status, "paused");
        // 篡改存储指纹（模拟 Provider/模型切换后恢复）。
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE agent_checkpoints SET state=replace(state, '\"providerFingerprint\":\"fake\"', '\"providerFingerprint\":\"sha256:other\"')",
                    [],
                )
                .map_err(sg_store::Error::from)
            })
            .unwrap();
        let rollout2 = crate::rollout::Rollout::open(&store.data_dir, &run.id).ok();
        let resumed = execute_run(
            &store,
            &gateway,
            &policy_snapshot(),
            None,
            &config,
            &run.id,
            None,
            rollout2,
            &initial,
            &CompactPolicy::default(),
            None,
        )
        .unwrap();
        // 明确回退：全新循环重新计划（不跨 Provider 回放旧 transcript）。
        assert_eq!(resumed.run.status, "completed_execution");
        let rollout_path = crate::rollout::Rollout::path_for(&store.data_dir, &run.id);
        let body = std::fs::read_to_string(&rollout_path).unwrap();
        assert!(
            body.contains("checkpoint_provider_mismatch"),
            "应记录指纹不匹配: {body}"
        );
    }

    /// checkpoint v2：损坏的 opaque payload 被丢弃（明确回退），Run 可继续。
    #[test]
    fn corrupted_reasoning_opaque_dropped_on_resume() {
        let store = setup();
        let fake = std::sync::Arc::new(FakeModel::default());
        fake.enable_native();
        fake.enable_streaming();
        fake.set_data_policy(serde_json::json!({"reasoningPersist": "encrypted_at_rest"}));
        fake.set_reasoning_text("SECRET-REASONING-CONTENT");
        fake.push_response(
            r#"{"action":"run_command","arguments":{"argv":["ls"]},"summary":"执行"}"#,
            5,
            3,
        );
        fake.push_response(r#"{"action":"final","summary":"恢复完成"}"#, 5, 3);
        let gateway = streaming_gateway(&fake);
        gateway.set_reasoning_vault(Arc::new(crate::reasoning_state::ReasoningVault::new(
            Arc::new(sg_settings::credentials::InMemoryCredentials::default()),
        )));
        let config = RunConfig {
            workitem_id: "wi",
            task_id: "",
            goal: "g",
            manifest_id: "ctx1",
            tool_allowlist: &["run_command".into()],
            idempotency_key: "key-corrupt-opaque",
            budget: &RunBudget::default(),
            max_iterations: 10,
        };
        let (run, _) = create_run(&store, &config).unwrap();
        let initial = crate::prompt::assemble(
            &crate::prompt::PromptEnv::default(),
            &["run_command".to_string()],
            &crate::prompt::knowledge_text("", ""),
            "g",
        );
        let out = execute_run(
            &store,
            &gateway,
            &policy_snapshot(),
            None,
            &config,
            &run.id,
            None,
            None,
            &initial,
            &CompactPolicy::default(),
            None,
        )
        .unwrap();
        assert_eq!(out.run.status, "paused");
        // 恢复前批准挂起审批（正常恢复语义）。
        let pending = sg_policy::pending(&store, 10).unwrap();
        sg_policy::decide(&store, &pending[0].id, "approved", "tester", "ok").unwrap();
        // 破坏 checkpoint 中的 opaque payload（非法 base64）。
        store
            .with_conn(|c| {
                // 定位 reasoning_opaque 项（index 随 transcript 长度变化）后破坏 payload。
                for idx in 0..32usize {
                    let ty: String = c
                        .query_row(
                            &format!(
                                "SELECT COALESCE(json_extract(state,'$.items[{idx}].type'),'') FROM agent_checkpoints WHERE agent_run_id=?1"
                            ),
                            [&run.id],
                            |r| r.get(0),
                        )
                        .unwrap_or_default();
                    if ty == "reasoning_opaque" {
                        let payload: String = c
                            .query_row(
                                &format!(
                                    "SELECT json_extract(state,'$.items[{idx}].payload') FROM agent_checkpoints WHERE agent_run_id=?1"
                                ),
                                [&run.id],
                                |r| r.get(0),
                            )
                            .unwrap_or_default();
                        c.execute(
                            "UPDATE agent_checkpoints SET state=replace(state, ?2, '!!!not-base64!!!') WHERE agent_run_id=?1",
                            rusqlite::params![run.id, payload],
                        )?;
                        break;
                    }
                }
                Ok(())
            })
            .unwrap();
        let rollout2 = crate::rollout::Rollout::open(&store.data_dir, &run.id).ok();
        let resumed = execute_run(
            &store,
            &gateway,
            &policy_snapshot(),
            None,
            &config,
            &run.id,
            None,
            rollout2,
            &initial,
            &CompactPolicy::default(),
            None,
        )
        .unwrap();
        assert_eq!(resumed.run.status, "completed_execution");
        let rollout_path = crate::rollout::Rollout::path_for(&store.data_dir, &run.id);
        let body = std::fs::read_to_string(&rollout_path).unwrap();
        assert!(body.contains("checkpoint_opaque_dropped"));
    }

    /// M4 reasoning 三态：encrypted / dropped_policy / dropped_key_unavailable；
    /// 明文永不入 checkpoint DB 或 rollout。
    #[test]
    fn reasoning_persist_three_states_and_no_plaintext_leak() {
        let store = setup();
        let fake = std::sync::Arc::new(FakeModel::default());
        fake.enable_native();
        fake.enable_streaming();
        fake.set_reasoning_text("SECRET-REASONING。第二句。");
        for _ in 0..3 {
            fake.push_response(r#"{"action":"final","summary":"ok"}"#, 9, 7);
        }
        let gateway = streaming_gateway(&fake);
        // (a) 显式启用 + vault → encrypted，解密还原明文（仅 vault 侧可解）。
        fake.set_data_policy(serde_json::json!({"reasoningPersist": "encrypted_at_rest"}));
        gateway.set_reasoning_vault(Arc::new(crate::reasoning_state::ReasoningVault::new(
            Arc::new(sg_settings::credentials::InMemoryCredentials::default()),
        )));
        let token = sg_integrations::CancelToken::new();
        let resp = gateway
            .call_turn(
                &store,
                "run-rp",
                &Budget::default(),
                &CompletionRequest {
                    model: String::new(),
                    system_prompt: "s".into(),
                    messages: vec![ChatMessage {
                        role: "user".into(),
                        content: "hi".into(),
                        ..Default::default()
                    }],
                    max_tokens: 64,
                    response_schema: None,
                    tools_json: None,
                },
                &modelgw::TurnOpts {
                    cancel: &token,
                    forwarder: None,
                    prompt_cache_key: "k1".into(),
                    model_call_id: ids::new_id("mc"),
                },
            )
            .unwrap();
        assert_eq!(resp.reasoning_state_status, "encrypted");
        let blob = resp.reasoning_state_encrypted.clone().unwrap();
        // 检查项：明文不在 DB / blob 中。
        assert!(!blob.contains("SECRET-REASONING"));
        // (b) 无数据策略 → dropped_policy（不持久化，不报错）。
        fake.set_data_policy(serde_json::json!({}));
        let resp2 = gateway
            .call_turn(
                &store,
                "run-rp",
                &Budget::default(),
                &CompletionRequest {
                    model: String::new(),
                    system_prompt: "s".into(),
                    messages: vec![ChatMessage {
                        role: "user".into(),
                        content: "hi".into(),
                        ..Default::default()
                    }],
                    max_tokens: 64,
                    response_schema: None,
                    tools_json: None,
                },
                &modelgw::TurnOpts {
                    cancel: &token,
                    forwarder: None,
                    prompt_cache_key: "k1".into(),
                    model_call_id: ids::new_id("mc"),
                },
            )
            .unwrap();
        assert_eq!(resp2.reasoning_state_status, "dropped_policy");
        assert!(resp2.reasoning_state_encrypted.is_none());
        // (c) 启用但未装配 vault → dropped_key_unavailable（明确回退）。
        let gateway2 = streaming_gateway(&fake);
        fake.set_data_policy(serde_json::json!({"reasoningPersist": "encrypted_at_rest"}));
        let resp3 = gateway2
            .call_turn(
                &store,
                "run-rp",
                &Budget::default(),
                &CompletionRequest {
                    model: String::new(),
                    system_prompt: "s".into(),
                    messages: vec![ChatMessage {
                        role: "user".into(),
                        content: "hi".into(),
                        ..Default::default()
                    }],
                    max_tokens: 64,
                    response_schema: None,
                    tools_json: None,
                },
                &modelgw::TurnOpts {
                    cancel: &token,
                    forwarder: None,
                    prompt_cache_key: "k1".into(),
                    model_call_id: ids::new_id("mc"),
                },
            )
            .unwrap();
        assert_eq!(resp3.reasoning_state_status, "dropped_key_unavailable");
        // (d) 解密还原（仅经 vault）。
        let vault = crate::reasoning_state::ReasoningVault::new(Arc::new(
            sg_settings::credentials::InMemoryCredentials::default(),
        ));
        // 注意：此 vault 是新实例（不同密钥），应解密失败（fail-closed 而非错值）。
        assert!(vault.decrypt(&blob).is_err());
    }

    /// M4 缓存观测：model_turns 记录 cached/reasoning tokens 与 prompt_cache_key。
    #[test]
    fn model_turns_record_cache_and_reasoning_metrics() {
        let store = setup();
        let fake = std::sync::Arc::new(FakeModel::default());
        fake.enable_native();
        fake.enable_streaming();
        fake.set_reasoning_text("推理内容。");
        fake.push_response_cached(r#"{"action":"final","summary":"done"}"#, 100, 40, 60);
        let gateway = streaming_gateway(&fake);
        let out = start(
            &store,
            &gateway,
            &policy_snapshot(),
            None,
            &RunConfig {
                workitem_id: "wi",
                task_id: "",
                goal: "g",
                manifest_id: "ctx1",
                tool_allowlist: &["read_file".into()],
                idempotency_key: "key-cache-obs",
                budget: &RunBudget::default(),
                max_iterations: 10,
            },
        )
        .unwrap();
        assert_eq!(out.run.status, "completed_execution");
        let row: (i64, i64, String) = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT cached_tokens, reasoning_tokens, prompt_cache_key FROM model_turns WHERE agent_run_id=?1",
                    [&out.run.id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .map_err(sg_store::Error::from)
            })
            .unwrap();
        assert_eq!(row.0, 60, "cached tokens 落库");
        assert!(row.1 > 0, "reasoning tokens 计量落库");
        assert!(row.2.starts_with("")); // 缓存域键存在（非空由下方断言）
        assert!(!row.2.is_empty());
        // reasoning 明文不出现在任何观测表（model_calls/model_turns 不存正文）。
        let leak: i64 = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM model_turns WHERE agent_run_id=?1 AND (prompt_cache_key LIKE '%推理%' OR protocol LIKE '%推理%')",
                    [&out.run.id],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .unwrap();
        assert_eq!(leak, 0);
    }

    /// M4：压缩摘要 JSON 校验——两次失败保留原 checkpoint 并使 Run 失败。
    #[test]
    fn compaction_invalid_summary_preserves_checkpoint_and_fails() {
        let store = setup();
        let fake = std::sync::Arc::new(FakeModel::default());
        fake.push_response(
            r#"{"action":"read_file","arguments":{"path":"a"},"summary":"读取"}"#,
            5,
            3,
        );
        // 两次压缩调用均返回非法摘要（缺 summary 字段）。
        fake.push_response("not-json", 5, 3);
        fake.push_response(r#"{"facts":[]}"#, 5, 3);
        let gateway = Gateway::new(Box::new(SharedFake(fake.clone())));
        let executor = move |_p: &Proposal| -> Result<String, String> { Ok("x".repeat(40000)) };
        let initial = crate::prompt::assemble(
            &crate::prompt::PromptEnv::default(),
            &["read_file".to_string()],
            &crate::prompt::knowledge_text("", ""),
            "g",
        );
        let config = RunConfig {
            workitem_id: "wi",
            task_id: "",
            goal: "g",
            manifest_id: "ctx1",
            tool_allowlist: &["read_file".into()],
            idempotency_key: "key-compact-invalid",
            budget: &RunBudget::default(),
            max_iterations: 10,
        };
        let (run, _) = create_run(&store, &config).unwrap();
        let err = match execute_run(
            &store,
            &gateway,
            &policy_snapshot(),
            Some(&executor),
            &config,
            &run.id,
            None,
            None,
            &initial,
            &CompactPolicy {
                threshold_tokens: 8000,
                keep_turns: 1,
            },
            None,
        ) {
            Err(e) => e,
            Ok(_) => panic!("非法摘要必须使压缩失败"),
        };
        // 注意：同语句两次 lock() 会自死锁（guard 存活到语句结束），先取出。
        let (calls_n, call_heads) = {
            let calls = fake.calls.lock().unwrap();
            (
                calls.len(),
                calls
                    .iter()
                    .map(|c| c.system_prompt.chars().take(12).collect::<String>())
                    .collect::<Vec<_>>(),
            )
        };
        assert!(
            err.to_string().contains("压缩摘要校验失败"),
            "{err}; calls={calls_n} contents={call_heads:?}"
        );
        // 原 checkpoint 保留（phase=running 回滚点存在）。
        let state: String = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT state FROM agent_checkpoints WHERE agent_run_id=?1 ORDER BY seq DESC LIMIT 1",
                    [&run.id],
                    |r| r.get(0),
                )
                .map_err(sg_store::Error::from)
            })
            .unwrap();
        assert!(state.contains(r#""schemaVersion":2"#));
    }

    /// M4：压缩策略选择矩阵。
    #[test]
    fn select_compaction_matrix() {
        use modelgw::select_compaction;
        // 无快照/无声明 → 本地结构化（保守）。
        assert_eq!(
            select_compaction(None, None),
            modelgw::CompactionStrategy::LocalStructured
        );
        // 声明 provider_opaque 但数据策略未允许 serverState → 本地。
        assert_eq!(
            select_compaction(
                Some(serde_json::json!({"compaction": "provider_opaque"})),
                Some(serde_json::json!({})),
            ),
            modelgw::CompactionStrategy::LocalStructured
        );
        // 两者齐备 → provider_opaque。
        assert_eq!(
            select_compaction(
                Some(serde_json::json!({"compaction": "provider_opaque"})),
                Some(serde_json::json!({"serverState": "allowed"})),
            ),
            modelgw::CompactionStrategy::ProviderOpaque
        );
    }

    /// 按 idempotency_key 反查 run id（错误路径断言用）。
    fn err_run_id(store: &Store, key: &str) -> String {
        store
            .with_conn(|conn| {
                Ok(conn
                    .query_row(
                        "SELECT id FROM agent_runs WHERE idempotency_key=?1",
                        [key],
                        |r| r.get::<_, String>(0),
                    )
                    .ok())
            })
            .unwrap()
            .unwrap()
    }

    #[test]
    fn default_request_carries_output_budget() {
        // 主请求 max_tokens 固定 16384（推理型模型推理也计入）；压缩调用为 8192。
        let store = setup();
        let fake = std::sync::Arc::new(FakeModel::default());
        fake.push_response(r#"{"action":"final","summary":"s"}"#, 5, 3);
        let gateway = Gateway::new(Box::new(SharedFake(fake.clone())));
        start(
            &store,
            &gateway,
            &policy_snapshot(),
            None,
            &RunConfig {
                workitem_id: "wi",
                task_id: "",
                goal: "g",
                manifest_id: "ctx1",
                tool_allowlist: &["read_file".into()],
                idempotency_key: "key-max-tokens",
                budget: &RunBudget::default(),
                max_iterations: 5,
            },
        )
        .unwrap();
        let calls = fake.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].max_tokens, 16384);
    }

    #[test]
    fn budget_tokens_out_stops_calls() {
        // D7：max_tokens_out 台账真实消费——超出后下一调用预算拒绝，Run failed。
        let store = setup();
        let fake = FakeModel::default();
        fake.push_response(
            r#"{"action":"read_file","arguments":{"path":"a"},"summary":"r"}"#,
            5,
            100,
        );
        let gateway = Gateway::new(Box::new(fake));
        let executed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = executed.clone();
        let executor = move |_p: &Proposal| -> Result<String, String> {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok("ok".into())
        };
        let mut budget = RunBudget::default();
        budget.model.max_tokens_out = 50;
        let err = match start(
            &store,
            &gateway,
            &policy_snapshot(),
            Some(&executor),
            &RunConfig {
                workitem_id: "wi",
                task_id: "",
                goal: "g",
                manifest_id: "ctx1",
                tool_allowlist: &["read_file".into()],
                idempotency_key: "key-budget-out",
                budget: &budget,
                max_iterations: 5,
            },
        ) {
            Err(e) => e,
            Ok(_) => panic!("输出预算超出必须使 Run failed"),
        };
        assert!(err.to_string().contains("tokens_out"), "{err}");
        let run = get_run(&store, &err_run_id(&store, "key-budget-out")).unwrap();
        assert_eq!(run.status, "failed");
        assert!(run.result.contains("budget_exceeded: tokens_out"));
        assert_eq!(executed.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn reconcile_interrupted_fails_only_queued_and_running() {
        let store = setup();
        let fake = FakeModel::default();
        // 一个完成（final）、一个暂停（高风险待审批）、一个孤儿 running（崩溃遗留）。
        fake.push_response(r#"{"action":"final","summary":"done"}"#, 5, 3);
        fake.push_response(
            r#"{"action":"run_command","arguments":{"argv":["ls"]},"summary":"x"}"#,
            5,
            3,
        );
        let gateway = Gateway::new(Box::new(fake));
        let executor = |_p: &Proposal| -> Result<String, String> { Ok(String::new()) };
        let allow_read = ["read_file".to_string()];
        let allow_cmd = ["run_command".to_string()];
        let done = start(
            &store,
            &gateway,
            &policy_snapshot(),
            Some(&executor),
            &RunConfig {
                workitem_id: "wi",
                task_id: "",
                goal: "g",
                manifest_id: "ctx1",
                tool_allowlist: &allow_read,
                idempotency_key: "key-rec-done",
                budget: &RunBudget::default(),
                max_iterations: 5,
            },
        )
        .unwrap();
        assert_eq!(done.run.status, "completed_execution");
        let paused = start(
            &store,
            &gateway,
            &policy_snapshot(),
            Some(&executor),
            &RunConfig {
                workitem_id: "wi",
                task_id: "",
                goal: "g",
                manifest_id: "ctx1",
                tool_allowlist: &allow_cmd,
                idempotency_key: "key-rec-paused",
                budget: &RunBudget::default(),
                max_iterations: 5,
            },
        )
        .unwrap();
        assert_eq!(paused.run.status, "paused");
        // 手工造一个 running 遗留行。
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO agent_runs(id, workitem_id, task_id, goal, input_baseline_sha, context_manifest_id,
                        tool_allowlist, budget, policy_snapshot, idempotency_key, status, created_at, updated_at)
                     VALUES ('run_orphan','wi','','g','','ctx1','[]','{}','default','key-rec-orphan','running','t','t')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(reconcile_interrupted(&store).unwrap(), 1);
        assert_eq!(get_run(&store, "run_orphan").unwrap().status, "failed");
        assert!(get_run(&store, "run_orphan")
            .unwrap()
            .result
            .contains("运行中断"));
        assert_eq!(
            get_run(&store, &paused.run.id).unwrap().status,
            "paused",
            "paused 是合法等待审批态，对账不得触碰"
        );
        assert_eq!(
            get_run(&store, &done.run.id).unwrap().status,
            "completed_execution",
            "已完成 Run 对账不得触碰"
        );
        // 幂等：再次对账为 no-op。
        assert_eq!(reconcile_interrupted(&store).unwrap(), 0);
    }

    /// 测试种子：一条 Run + 一条提案（FK 链复用 setup() 的 projects/workitems/manifest）。
    fn seed_run_and_proposal(
        store: &Store,
        run_id: &str,
        proposal_id: &str,
        run_status: &str,
        decision: &str,
    ) {
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO agent_runs(id, workitem_id, task_id, goal, input_baseline_sha, context_manifest_id,
                        tool_allowlist, budget, policy_snapshot, idempotency_key, status, created_at, updated_at)
                     VALUES (?1,'wi','','g','sha','ctx1','[]','{}','default',?2,?3,'t','t')",
                    rusqlite::params![run_id, format!("ik_{run_id}"), run_status],
                )?;
                c.execute(
                    "INSERT INTO tool_proposals(id, agent_run_id, tool, arguments, risk, action_digest,
                        requires_approval, decision, result, created_at)
                     VALUES (?1, ?2, 'mcp__srv__send', '{}', 'high', 'd', 0, ?3, '', 't')",
                    rusqlite::params![proposal_id, run_id, decision],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn proposal_of(id: &str, run_id: &str) -> Proposal {
        Proposal {
            id: id.into(),
            run_id: run_id.into(),
            tool: "mcp__srv__send".into(),
            arguments: "{}".into(),
            risk: "high".into(),
            action_digest: "d".into(),
            decision: "proposed".into(),
            result: String::new(),
            created_at: String::new(),
        }
    }

    fn proposal_decision(store: &Store, id: &str) -> String {
        store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT decision FROM tool_proposals WHERE id=?1",
                    [id],
                    |r| r.get(0),
                )?)
            })
            .unwrap()
    }

    fn outcome_of(store: &Store, proposal_id: &str) -> (String, String) {
        store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT outcome, reconciliation FROM tool_execution_outcomes WHERE proposal_id=?1",
                    [proposal_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .unwrap()
    }

    /// M0-07（EV-009 超时路径）：executor Ok 但带 tool_outcome_unknown 前缀 →
    /// 提案 decision=unknown（不落 executed），outcome 一等 unknown + 对账 pending，
    /// 预算计数含 unknown（消耗了一次执行尝试）。
    #[test]
    fn unknown_outcome_is_first_class_not_executed() {
        let store = setup();
        seed_run_and_proposal(&store, "run_unk", "tp_unk", "running", "proposed");
        let executor = |_p: &Proposal| -> Result<String, String> {
            Ok(format!(
                "{TOOL_OUTCOME_UNKNOWN_PREFIX} 工具 send 在 1s 内未返回，副作用可能已发生且无法确认。"
            ))
        };
        let text =
            run_executor(&store, Some(&executor), &proposal_of("tp_unk", "run_unk")).unwrap();
        assert!(text.starts_with(TOOL_OUTCOME_UNKNOWN_PREFIX));
        assert_eq!(
            proposal_decision(&store, "tp_unk"),
            "unknown",
            "副作用未知的执行不得标 executed"
        );
        assert_eq!(
            outcome_of(&store, "tp_unk"),
            ("unknown".into(), "pending".into())
        );
        assert_eq!(count_tool_calls(&store, "run_unk").unwrap(), 1);
    }

    /// M0-07：正常 Ok → executed + outcome ok（无需对账）；Err → exec_failed + outcome failed。
    #[test]
    fn ok_and_failed_outcomes_recorded() {
        let store = setup();
        seed_run_and_proposal(&store, "run_ok", "tp_ok", "running", "proposed");
        let executor = |_p: &Proposal| -> Result<String, String> { Ok("done".into()) };
        run_executor(&store, Some(&executor), &proposal_of("tp_ok", "run_ok")).unwrap();
        assert_eq!(proposal_decision(&store, "tp_ok"), "executed");
        assert_eq!(outcome_of(&store, "tp_ok"), ("ok".into(), "none".into()));
        seed_run_and_proposal(&store, "run_err", "tp_err", "running", "proposed");
        let failing = |_p: &Proposal| -> Result<String, String> { Err("boom".into()) };
        run_executor(&store, Some(&failing), &proposal_of("tp_err", "run_err")).unwrap();
        assert_eq!(proposal_decision(&store, "tp_err"), "exec_failed");
        assert_eq!(
            outcome_of(&store, "tp_err"),
            ("failed".into(), "none".into())
        );
    }

    /// M0-07（EV-009 restart/reconcile 路径）：被打断 Run 的遗留提案启动对账 →
    /// unknown + 对账 pending；paused Run（合法等待审批）的提案不触碰。
    #[test]
    fn reconcile_marks_interrupted_run_proposals_unknown() {
        let store = setup();
        seed_run_and_proposal(&store, "run_stale", "tp_stale", "running", "approved");
        seed_run_and_proposal(&store, "run_paused", "tp_paused", "paused", "proposed");
        reconcile_interrupted(&store).unwrap();
        assert_eq!(get_run(&store, "run_stale").unwrap().status, "failed");
        assert_eq!(proposal_decision(&store, "tp_stale"), "unknown");
        assert_eq!(
            outcome_of(&store, "tp_stale"),
            ("unknown".into(), "pending".into())
        );
        assert_eq!(get_run(&store, "run_paused").unwrap().status, "paused");
        assert_eq!(proposal_decision(&store, "tp_paused"), "proposed");
        assert!(
            outcome_of_inner(&store, "tp_paused").is_none(),
            "paused 提案不产生 outcome"
        );
    }

    fn outcome_of_inner(store: &Store, proposal_id: &str) -> Option<(String, String)> {
        store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT outcome, reconciliation FROM tool_execution_outcomes WHERE proposal_id=?1",
                    [proposal_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .ok())
            })
            .unwrap()
    }

    /// 共享 FakeModel：请求捕获（calls）对测试可见。
    struct SharedFake(std::sync::Arc<FakeModel>);
    impl sg_integrations::model::ModelProvider for SharedFake {
        fn name(&self) -> &str {
            "fake"
        }
        fn health_check(&self) -> Result<(), String> {
            Ok(())
        }
        fn complete(
            &self,
            req: &sg_integrations::model::CompletionRequest,
        ) -> Result<sg_integrations::model::CompletionResponse, String> {
            self.0.complete(req)
        }
    }

    #[test]
    fn compaction_triggers_preserves_frozen_and_keeps_turns() {
        let store = setup();
        let fake = std::sync::Arc::new(FakeModel::default());
        // 剧本：①read_file 决策 ②压缩摘要 ③final。
        fake.push_response(
            r#"{"action":"read_file","arguments":{"path":"a"},"summary":"读取"}"#,
            5,
            3,
        );
        fake.push_response(
            r#"{"summary":"已读取 a 并分析","facts":["a 有 3 个模块"],"pendingApprovals":[],"executedTools":[{"tool":"read_file","result":"ok"}],"keyFiles":["a"]}"#,
            5,
            3,
        );
        fake.push_response(r#"{"action":"final","summary":"完成"}"#, 5, 3);
        let gateway = Gateway::new(Box::new(SharedFake(fake.clone())));
        // 大结果让第二轮估算超阈值（threshold=8000）。
        let executor = move |_p: &Proposal| -> Result<String, String> { Ok("x".repeat(40000)) };
        let initial = crate::prompt::assemble(
            &crate::prompt::PromptEnv::default(),
            &["read_file".to_string()],
            &crate::prompt::knowledge_text("", ""),
            "g",
        );
        let config = RunConfig {
            workitem_id: "wi",
            task_id: "",
            goal: "g",
            manifest_id: "ctx1",
            tool_allowlist: &["read_file".into()],
            idempotency_key: "key-compact",
            budget: &RunBudget::default(),
            max_iterations: 10,
        };
        let (run, created) = create_run(&store, &config).unwrap();
        assert!(created);
        let out = execute_run(
            &store,
            &gateway,
            &policy_snapshot(),
            Some(&executor),
            &config,
            &run.id,
            None,
            None,
            &initial,
            &CompactPolicy {
                threshold_tokens: 8000,
                keep_turns: 1,
            },
            None,
        )
        .unwrap();
        assert_eq!(out.run.status, "completed_execution");
        // 3 次模型调用：决策 + 压缩 + final（压缩计入预算）。
        assert_eq!(count_model_calls(&store, &run.id).unwrap(), 3);
        let n: i64 = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM events_outbox WHERE type='run.compacted' AND aggregate_id=?1",
                    [&run.id],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(n, 1, "run.compacted 恰好一条");
        // 捕获的请求：压缩调用用压缩指令；压缩后保留 冻结头3 + 摘要1 + 最近1轮2。
        let calls = fake.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 3);
        assert!(calls[1].system_prompt.contains("会话压缩器"));
        let m = &calls[2].messages;
        assert_eq!(m.len(), 3 + 1 + 2, "冻结头+摘要+保留 1 轮");
        assert_eq!(m[3].role, "user");
        assert!(m[3].content.contains("会话已压缩"));
        assert!(m[3].content.contains("已读取 a 并分析"), "摘要内容注入");
        assert_eq!(m[4].role, "assistant");
        assert_eq!(m[5].role, "tool");
        // 冻结头（developer/knowledge/goal）与首次请求逐字节相同（前缀纪律）。
        assert_eq!(
            serde_json::to_string(&m[..3]).unwrap(),
            serde_json::to_string(&calls[0].messages[..3]).unwrap(),
            "冻结头不改写"
        );
    }

    #[test]
    fn compaction_failure_fails_run_with_context_too_large() {
        let store = setup();
        let fake = FakeModel::default();
        fake.push_error("boom context length exceeded");
        let gateway = Gateway::new(Box::new(fake));
        let goal = "g".repeat(60000);
        let initial = crate::prompt::assemble(
            &crate::prompt::PromptEnv::default(),
            &["read_file".to_string()],
            &crate::prompt::knowledge_text("", ""),
            &goal,
        );
        let config = RunConfig {
            workitem_id: "wi",
            task_id: "",
            goal: &goal,
            manifest_id: "ctx1",
            tool_allowlist: &["read_file".into()],
            idempotency_key: "key-compact-fail",
            budget: &RunBudget::default(),
            max_iterations: 5,
        };
        let (run, _) = create_run(&store, &config).unwrap();
        let err = match execute_run(
            &store,
            &gateway,
            &policy_snapshot(),
            None,
            &config,
            &run.id,
            None,
            None,
            &initial,
            &CompactPolicy {
                threshold_tokens: 1000,
                keep_turns: 2,
            },
            None,
        ) {
            Err(e) => e,
            Ok(_) => panic!("压缩失败必须使 Run failed"),
        };
        assert!(err.to_string().contains("context_too_large"), "{err}");
        let final_run = get_run(&store, &run.id).unwrap();
        assert_eq!(final_run.status, "failed");
        assert!(final_run.result.contains("context_too_large"));
    }

    /// P1-1（评审修复）：TurnOpts.model_call_id 即 model_calls 权威行 id——
    /// 与 ledger consumption_key 共用，崩溃对账按 id 命中权威用量。
    #[test]
    fn call_turn_records_model_calls_with_caller_id() {
        let store = setup();
        let fake = FakeModel::default();
        fake.push_response(r#"{"action":"final","summary":"完成"}"#, 5, 3);
        let gateway = Gateway::new(Box::new(fake));
        let token = sg_integrations::CancelToken::new();
        let fixed_id = ids::new_id("mc");
        gateway
            .call_turn(
                &store,
                "run-mcid",
                &Budget::default(),
                &CompletionRequest {
                    model: String::new(),
                    system_prompt: "s".into(),
                    messages: vec![ChatMessage {
                        role: "user".into(),
                        content: "hi".into(),
                        ..Default::default()
                    }],
                    max_tokens: 64,
                    response_schema: None,
                    tools_json: None,
                },
                &modelgw::TurnOpts {
                    cancel: &token,
                    forwarder: None,
                    prompt_cache_key: String::new(),
                    model_call_id: fixed_id.clone(),
                },
            )
            .unwrap();
        let rows: Vec<String> = store
            .with_conn(|c| {
                let mut stmt =
                    c.prepare("SELECT id FROM model_calls WHERE agent_run_id='run-mcid'")?;
                let out = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(out)
            })
            .unwrap();
        assert_eq!(
            rows,
            vec![fixed_id.clone()],
            "权威行 id == 调用方指定 key（且恰一行）"
        );
    }
}
