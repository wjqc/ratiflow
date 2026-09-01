//! Agent Run + 模型网关（v2 ADR-022 行为等价）：
//! 模型只能产出 ToolCallProposal；出网内容统一裁剪/脱敏/预算；completed_execution ≠ 过关。
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sg_integrations::model::{ChatMessage, CompletionRequest};
use sg_policy::{self, PolicyError, Snapshot};
use sg_store::{ids, outbox, timefmt, Error, Store};

pub mod instructions;
pub mod modelgw;
pub mod profile;
pub mod prompt;
pub mod rollout;
pub mod router;
pub mod schema;
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
#[allow(clippy::too_many_arguments)]
pub fn execute_run(
    store: &Store,
    gateway: &Gateway,
    policy: &Snapshot,
    executor: Option<&ToolExecutor>,
    config: &RunConfig<'_>,
    run_id: &str,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    mut rollout: Option<crate::rollout::Rollout>,
    initial: &prompt::InitialTurn,
    compact: &CompactPolicy,
) -> Result<RunOutput, Error> {
    let RunConfig {
        goal: _goal,
        tool_allowlist: _tool_allowlist,
        budget,
        max_iterations,
        ..
    } = *config;
    let system_prompt = initial.system_prompt.clone();
    let mut run = get_run(store, run_id)?;
    let cancelled = |cancel: Option<&std::sync::atomic::AtomicBool>| {
        cancel
            .map(|f| f.load(std::sync::atomic::Ordering::SeqCst))
            .unwrap_or(false)
    };

    let (mut messages, start_iteration, mut pending_proposal) =
        load_checkpoint(store, run_id).unwrap_or_else(|| (initial.prefix.clone(), 0, None));
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
        });
        messages.push(ChatMessage {
            role: "tool".into(),
            content: result_text.clone(),
        });
        log_rollout(
            &mut rollout,
            "tool_result",
            json!({"tool": proposal.tool, "preview": tools::truncate_output(&result_text, 400)}),
        );
    }

    #[allow(unused_assignments)]
    for iteration in start_iteration..max_iterations {
        if cancelled(cancel) {
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
        let request = CompletionRequest {
            model: String::new(),
            system_prompt: system_prompt.clone(),
            messages: messages.clone(),
            max_tokens: 4096,
            response_schema: None,
        };
        log_rollout(
            &mut rollout,
            "model_request",
            json!({"messages": messages.len(), "estTokens": estimate_tokens(&request)}),
        );
        let response = match gateway.call(store, run_id, &budget.model, &request) {
            Ok(resp) => resp,
            Err(e) => {
                if e.contains("budget_exceeded") {
                    run.result = e.clone();
                    set_status(store, &mut run, "failed", "run.failed")?;
                    finish(store, &run)?;
                    return Err(Error::Message(e));
                }
                let lower = e.to_lowercase();
                run.result = if lower.contains("context")
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

        let decision = match parse_decision(&response.content) {
            Some(d) => d,
            None => {
                messages.push(ChatMessage {
                    role: "assistant".into(),
                    content: response.content,
                });
                messages.push(ChatMessage {
                    role: "user".into(),
                    content: "输出必须是 JSON（schema 见系统提示）；请重新输出决策对象。".into(),
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
            checkpoint(store, run_id, iteration, &messages, None);
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

        if cancelled(cancel) {
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
                messages.push(ChatMessage {
                    role: "assistant".into(),
                    content: format!("tool {}({})", decision.action, decision.arguments),
                });
                checkpoint(store, run_id, iteration, &messages, Some(&proposal_id));
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
                messages.push(ChatMessage {
                    role: "assistant".into(),
                    content: format!("tool {}({})", decision.action, decision.arguments),
                });
                messages.push(ChatMessage {
                    role: "tool".into(),
                    content: output_text,
                });
                checkpoint(store, run_id, iteration, &messages, None);
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
) -> Result<usize, Error> {
    // 回滚点（phase=running；压缩失败可从此恢复重试）。
    checkpoint(store, &run.id, iteration, messages, None);
    let compress_system = "你是会话压缩器。把对话历史压缩为结构化摘要 JSON：         {\"summary\":\"...\",\"facts\":[\"...\"],\"pendingApprovals\":[],\
         \"executedTools\":[{\"tool\":\"..\",\"result\":\"..\"}],\"keyFiles\":[\"..\"]}\u{3002}         必须保留：任务目标、当前状态、未决审批、已执行工具与结论、关键文件路径。只输出 JSON。";
    let request = CompletionRequest {
        model: String::new(),
        system_prompt: compress_system.into(),
        messages: messages.clone(),
        max_tokens: 2048,
        response_schema: None,
    };
    let resp = match gateway.call(store, &run.id, budget, &request) {
        Ok(r) => r,
        Err(_) => gateway
            .call(store, &run.id, budget, &request)
            .map_err(|e| Error::Message(format!("context_too_large: 压缩调用失败 {e}")))?,
    };
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

fn estimate_tokens(req: &CompletionRequest) -> usize {
    let chars =
        req.system_prompt.len() + req.messages.iter().map(|m| m.content.len()).sum::<usize>();
    chars / 4 + 64
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
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO tool_proposals(id, agent_run_id, tool, arguments, risk, action_digest, requires_approval, decision, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,0,'proposed',?7)",
            rusqlite::params![proposal.id, proposal.run_id, proposal.tool, proposal.arguments, proposal.risk, proposal.action_digest, proposal.created_at],
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

/// 执行提案并落 executed/exec_failed 标记，返回给模型的文本。
fn run_executor(
    store: &Store,
    executor: Option<&ToolExecutor>,
    proposal: &Proposal,
) -> Result<String, Error> {
    match executor {
        Some(exec) => match exec(proposal) {
            Ok(result) => {
                mark_proposal(store, proposal, "executed", &result)?;
                Ok(result)
            }
            Err(e) => {
                mark_proposal(store, proposal, "exec_failed", &e)?;
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

fn set_status(store: &Store, run: &mut AgentRun, to: &str, event: &str) -> Result<(), Error> {
    run.status = to.into();
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE agent_runs SET status=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![to, timefmt::now(), run.id],
        )?;
        Ok(())
    })?;
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

fn checkpoint(
    store: &Store,
    run_id: &str,
    seq: usize,
    messages: &[ChatMessage],
    pending: Option<&str>,
) {
    // M1/F03：checkpoint 载荷带恢复元数据（phase/pending 提案）；messages 结构不变。
    let body = serde_json::json!({
        "messages": messages,
        "iteration": seq,
        "pending_proposal_id": pending,
        "phase": if pending.is_some() { "waiting_approval" } else { "running" },
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

pub fn cancel(store: &Store, id: &str) -> Result<(), Error> {
    let mut run = get_run(store, id)?;
    if matches!(
        run.status.as_str(),
        "completed_execution" | "failed" | "cancelled"
    ) {
        return Ok(());
    }
    set_status(store, &mut run, "cancelled", "run.cancelled")?;
    Ok(())
}

/// 最新 checkpoint 的恢复元数据（M0 旧格式 messages 数组不可恢复，返回 None 走全新循环）。
fn load_checkpoint(
    store: &Store,
    run_id: &str,
) -> Option<(Vec<ChatMessage>, usize, Option<String>)> {
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
    let arr = v.get("messages")?.as_array()?;
    let mut messages = Vec::with_capacity(arr.len());
    for m in arr {
        messages.push(ChatMessage {
            role: m.get("role")?.as_str()?.into(),
            content: m.get("content")?.as_str()?.into(),
        });
    }
    let iteration = v.get("iteration").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
    let pending = v
        .get("pending_proposal_id")
        .and_then(|x| x.as_str())
        .map(String::from);
    Some((messages, iteration, pending))
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
                    tool: r.get(2)?,
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
pub fn count_tool_calls(store: &Store, run_id: &str) -> Result<i64, Error> {
    store.with_conn(|conn| {
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM tool_proposals WHERE agent_run_id=?1 AND decision IN ('executed','exec_failed')",
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

pub fn proposals(store: &Store, run_id: &str) -> Result<Vec<Proposal>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, agent_run_id, tool, arguments, risk, action_digest, decision, COALESCE(result,''), created_at
             FROM tool_proposals WHERE agent_run_id=?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map([run_id], |r| {
            Ok(Proposal {
                id: r.get(0)?, run_id: r.get(1)?, tool: r.get(2)?, arguments: r.get(3)?,
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
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
        let flag = Arc::new(AtomicBool::new(false));
        let f2 = flag.clone();
        let executed = Arc::new(AtomicUsize::new(0));
        let counter = executed.clone();
        let executor = move |_p: &Proposal| -> Result<String, String> {
            counter.fetch_add(1, Ordering::SeqCst);
            // 首个工具执行后请求取消：下一迭代边界应观察到并收尾。
            f2.store(true, Ordering::SeqCst);
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
            Some(&flag),
            None,
            &initial,
            &CompactPolicy::default(),
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
        ) {
            Err(e) => e,
            Ok(_) => panic!("压缩失败必须使 Run failed"),
        };
        assert!(err.to_string().contains("context_too_large"), "{err}");
        let final_run = get_run(&store, &run.id).unwrap();
        assert_eq!(final_run.status, "failed");
        assert!(final_run.result.contains("context_too_large"));
    }
}
