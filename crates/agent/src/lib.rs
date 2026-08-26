//! Agent Run + 模型网关（v2 ADR-022 行为等价）：
//! 模型只能产出 ToolCallProposal；出网内容统一裁剪/脱敏/预算；completed_execution ≠ 过关。
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sg_integrations::model::{ChatMessage, CompletionRequest};
use sg_policy::{self, PolicyError, Snapshot};
use sg_store::{ids, outbox, timefmt, Error, Store};

pub mod modelgw;
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
    execute_run(store, gateway, policy, executor, config, &run.id, None)
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
    set_status(store, &mut run, "running")?;
    Ok((run, true))
}

/// 执行既有 Run 的循环（M0-② 起 Run 由装配层在独立任务/连接上驱动）。
pub fn execute_run(
    store: &Store,
    gateway: &Gateway,
    policy: &Snapshot,
    executor: Option<&ToolExecutor>,
    config: &RunConfig<'_>,
    run_id: &str,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<RunOutput, Error> {
    let RunConfig {
        goal,
        tool_allowlist,
        budget,
        max_iterations,
        ..
    } = *config;
    let mut run = get_run(store, run_id)?;
    let cancelled = |cancel: Option<&std::sync::atomic::AtomicBool>| {
        cancel
            .map(|f| f.load(std::sync::atomic::Ordering::SeqCst))
            .unwrap_or(false)
    };

    let mut messages = vec![ChatMessage {
        role: "user".into(),
        content: goal.into(),
    }];
    let mut tool_calls = 0i64;
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(budget.max_duration_sec.max(1) as u64);
    #[allow(unused_assignments)]
    for iteration in 0..max_iterations {
        if cancelled(cancel) {
            run.result = "已取消（用户请求）".into();
            set_status(store, &mut run, "cancelled")?;
            finish(store, &run)?;
            return Ok(RunOutput {
                run,
                output: String::new(),
            });
        }
        if std::time::Instant::now() > deadline {
            run.result = "budget: duration exceeded".into();
            set_status(store, &mut run, "failed")?;
            finish(store, &run)?;
            return Err(Error::Message("budget_exhausted: duration".into()));
        }
        let request = CompletionRequest {
            model: String::new(),
            system_prompt: system_prompt_for(tool_allowlist),
            messages: messages.clone(),
            max_tokens: 4096,
            response_schema: None,
        };
        let response = match gateway.call(store, run_id, &budget.model, &request) {
            Ok(resp) => resp,
            Err(e) => {
                if e.contains("budget_exceeded") {
                    run.result = e.clone();
                    set_status(store, &mut run, "failed")?;
                    finish(store, &run)?;
                    return Err(Error::Message(e));
                }
                run.result = format!("model: {e}");
                set_status(store, &mut run, "failed")?;
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

        if decision.action == "final" {
            run.result = decision.summary.clone();
            set_status(store, &mut run, "completed_execution")?;
            checkpoint(store, run_id, iteration, &messages);
            finish(store, &run)?;
            return Ok(RunOutput {
                run,
                output: decision.summary,
            });
        }

        if tool_calls >= budget.max_tool_calls {
            run.result = "budget: tool calls exhausted".into();
            set_status(store, &mut run, "failed")?;
            finish(store, &run)?;
            return Err(Error::Message("budget_exhausted: tool calls".into()));
        }

        if cancelled(cancel) {
            run.result = "已取消（用户请求）".into();
            set_status(store, &mut run, "cancelled")?;
            finish(store, &run)?;
            return Ok(RunOutput {
                run,
                output: String::new(),
            });
        }

        let (_outcome, output_text) =
            propose_and_execute(store, policy, executor, &run, &decision)?;
        tool_calls += 1;
        let _ = &output_text;
        messages.push(ChatMessage {
            role: "assistant".into(),
            content: format!("tool {}({})", decision.action, decision.arguments),
        });
        messages.push(ChatMessage {
            role: "tool".into(),
            content: output_text,
        });
        checkpoint(store, run_id, iteration, &messages);
    }

    run.result = "max iterations reached without final answer".into();
    set_status(store, &mut run, "failed")?;
    finish(store, &run)?;
    Err(Error::Message("agent loop exhausted".into()))
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
    Some(Decision {
        action: value["action"].as_str()?.into(),
        arguments: value["arguments"].to_string(),
        summary: value["summary"].as_str().unwrap_or_default().into(),
    })
}

fn system_prompt_for(allowlist: &[String]) -> String {
    format!(
        "你是 SixGates 交付 Agent。每轮输出一个 JSON 对象：{{\"action\":\"<tool|final>\",\"arguments\":{{...}},\"summary\":\"...\"}}。可用工具：{allowlist:?}。完成任务时 action=final 并在 summary 给出结果。你不能直接执行工具；系统会校验并执行提案。"
    )
}

fn propose_and_execute(
    store: &Store,
    policy: &Snapshot,
    executor: Option<&ToolExecutor>,
    run: &AgentRun,
    decision: &Decision,
) -> Result<(String, String), Error> {
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

    let needs_approval = match sg_policy::evaluate(policy, &decision.action, &digest) {
        Ok(_) => false,
        Err(PolicyError::ApprovalRequired) => true,
        Err(e) => {
            mark_proposal(store, &proposal, "rejected", &e.to_string())?;
            return Ok((e.to_string(), format!("工具被策略拒绝：{e}")));
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
        )?;
        mark_proposal(store, &proposal, "rejected", "approval_required")?;
        return Ok((
            "approval_required".into(),
            "该工具需要人工审批；审批通过后重新运行任务。".into(),
        ));
    }

    match executor {
        Some(exec) => match exec(&proposal) {
            Ok(result) => {
                mark_proposal(store, &proposal, "executed", &result)?;
                Ok((result.clone(), result))
            }
            Err(e) => {
                mark_proposal(store, &proposal, "exec_failed", &e)?;
                Ok((e.clone(), format!("执行失败：{e}")))
            }
        },
        None => {
            mark_proposal(store, &proposal, "rejected", "no executor")?;
            Ok(("no executor".into(), "执行器不可用。".into()))
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

fn set_status(store: &Store, run: &mut AgentRun, to: &str) -> Result<(), Error> {
    run.status = to.into();
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE agent_runs SET status=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![to, timefmt::now(), run.id],
        )?;
        Ok(())
    })?;
    // running 的对外事件名是 run.started（M0-② 契约）；其余沿用 run.<status>。
    let event_type = if to == "running" {
        "run.started".to_string()
    } else {
        format!("run.{to}")
    };
    outbox::emit(
        store,
        "agent_run",
        &run.id,
        &event_type,
        json!({"workitemId": run.workitem_id}),
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

fn checkpoint(store: &Store, run_id: &str, seq: usize, messages: &[ChatMessage]) {
    let body = serde_json::to_string(messages).unwrap_or_default();
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
    set_status(store, &mut run, "cancelled")?;
    Ok(())
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
    fn high_risk_requires_approval_never_executes() {
        let store = setup();
        let fake = FakeModel::default();
        fake.push_response(
            r#"{"action":"run_command","arguments":{"argv":["ls"]},"summary":"执行"}"#,
            5,
            3,
        );
        fake.push_response(r#"{"action":"final","summary":"等待审批"}"#, 5, 3);
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
        assert_eq!(
            executed.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "高风险工具未获批准不得执行"
        );
        let pending = sg_policy::pending(&store, 10).unwrap();
        assert_eq!(pending.len(), 1);
        let _ = out;
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
        let out = execute_run(
            &store,
            &gateway,
            &policy_snapshot(),
            Some(&executor),
            &config,
            &run.id,
            Some(&flag),
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
}
