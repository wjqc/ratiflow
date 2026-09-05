//! F11/M4：Harness 回归矩阵（integration）。九剧本索引：
//! ①happy ②retry-nudge ③审批暂停→恢复 ④压缩触发 ⑤截断 ⑥中途取消
//! ⑦预算耗尽（本文件）⑧前缀稳定性 ⑨rollout kind 序列（本文件）。
//! ①②③④⑥⑧ 见 crates/agent/src/lib.rs 与 prompt.rs 的单元测试；
//! ⑤ 见 tools.rs（truncate_marks_and_caps）。
use sg_agent::rollout::Rollout;
use sg_agent::{create_run, execute_run, get_run, CompactPolicy, Gateway, RunBudget, RunConfig};
use sg_integrations::model::FakeModel;
use sg_policy::{Risk, Snapshot, ToolRule};
use sg_store::Store;

fn setup(tag: &str) -> Store {
    let dir = std::env::temp_dir().join(format!("sg-hr-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir, "test").unwrap();
    let now = sg_store::timefmt::now();
    store
        .with_conn(|c| {
            c.execute(
                "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                 VALUES ('pj','u','n','p','main',?1)",
                [&now],
            )?;
            c.execute(
                "INSERT INTO workitems(id, project_id, title, created_at, updated_at)
                 VALUES ('wi','pj','t',?1,?1)",
                [&now],
            )?;
            c.execute(
                "INSERT INTO context_manifests(id, workitem_id, scope, data_policy, created_at)
                 VALUES ('ctx1','wi','{}','standard',?1)",
                [&now],
            )?;
            Ok(())
        })
        .unwrap();
    store
}

fn policy() -> Snapshot {
    Snapshot {
        tool_rules: vec![ToolRule {
            tool: "read_file".into(),
            risk: Risk::Low,
            requires_approval: false,
            data_level: "internal".into(),
            max_result_bytes: 65536,
            timeout_sec: 60,
        }],
        approval_ttl_secs: 3600,
    }
}

/// ⑦ 预算耗尽：模型调用数超 Budget.max_calls → run failed（result 含 budget_exceeded）。
#[test]
fn budget_exhausted_fails_run() {
    let store = setup("budget");
    let fake = FakeModel::default();
    fake.push_response(
        r#"{"action":"read_file","arguments":{"path":"a"},"summary":"读"}"#,
        5,
        3,
    );
    fake.push_response(r#"{"action":"final","summary":"done"}"#, 5, 3);
    let gateway = Gateway::new(Box::new(fake));
    let budget = RunBudget {
        max_tool_calls: 10,
        max_duration_sec: 600,
        model: sg_agent::Budget {
            max_calls: 1,
            ..Default::default()
        },
    };
    let config = RunConfig {
        workitem_id: "wi",
        task_id: "",
        goal: "g",
        manifest_id: "ctx1",
        tool_allowlist: &["read_file".into()],
        idempotency_key: "hr-budget",
        budget: &budget,
        max_iterations: 10,
    };
    let (run, _) = create_run(&store, &config).unwrap();
    let initial = sg_agent::prompt::assemble(
        &sg_agent::prompt::PromptEnv::default(),
        &["read_file".to_string()],
        &sg_agent::prompt::knowledge_text("", ""),
        "g",
    );
    let executor = |_p: &sg_agent::Proposal| -> Result<String, String> { Ok("ok".into()) };
    let result = execute_run(
        &store,
        &gateway,
        &policy(),
        Some(&executor),
        &config,
        &run.id,
        None,
        None,
        &initial,
        &CompactPolicy::default(),
        None,
    );
    assert!(result.is_err(), "预算耗尽必须失败");
    let final_run = get_run(&store, &run.id).unwrap();
    assert_eq!(final_run.status, "failed");
    assert!(
        final_run.result.contains("budget_exceeded"),
        "{}",
        final_run.result
    );
    // 恰好 1 次成功调用（第 2 次被预算拒绝，不产生 model_calls 行）。
    assert_eq!(sg_agent::count_model_calls(&store, &run.id).unwrap(), 1);
}

/// ⑨ rollout kind 序列：一次 [工具轮 + final] 的 Run，事件序精确匹配。
#[test]
fn rollout_kind_sequence_exact() {
    let store = setup("rollout");
    let dir = store.data_dir.clone();
    let fake = FakeModel::default();
    fake.push_response(
        r#"{"action":"read_file","arguments":{"path":"a"},"summary":"读"}"#,
        5,
        3,
    );
    fake.push_response(r#"{"action":"final","summary":"done"}"#, 5, 3);
    let gateway = Gateway::new(Box::new(fake));
    let config = RunConfig {
        workitem_id: "wi",
        task_id: "",
        goal: "g",
        manifest_id: "ctx1",
        tool_allowlist: &["read_file".into()],
        idempotency_key: "hr-rollout",
        budget: &RunBudget::default(),
        max_iterations: 5,
    };
    let (run, _) = create_run(&store, &config).unwrap();
    let initial = sg_agent::prompt::assemble(
        &sg_agent::prompt::PromptEnv::default(),
        &["read_file".to_string()],
        &sg_agent::prompt::knowledge_text("", ""),
        "g",
    );
    let executor = |_p: &sg_agent::Proposal| -> Result<String, String> { Ok("ok".into()) };
    let rollout = Rollout::open(&dir, &run.id).unwrap();
    let out = execute_run(
        &store,
        &gateway,
        &policy(),
        Some(&executor),
        &config,
        &run.id,
        None,
        Some(rollout),
        &initial,
        &CompactPolicy::default(),
        None,
    )
    .unwrap();
    assert_eq!(out.run.status, "completed_execution");
    let path = Rollout::path_for(&dir, &run.id);
    let body = std::fs::read_to_string(&path).unwrap();
    let kinds: Vec<String> = body
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .map(|v| v["kind"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        kinds,
        vec![
            // M4：Run 启动冻结压缩策略（provider_opaque→local_structured→fail 链）。
            "compaction_strategy",
            "instructions_assembled",
            "run_started",
            "model_request",
            "model_response",
            "tool_proposed",
            "tool_result",
            "checkpoint_saved",
            "turn_completed",
            "model_request",
            "model_response",
            "run_finished",
        ],
        "rollout kind 序列精确匹配"
    );
    // 无秘密形状。
    assert!(!body.contains("glpat-") && !body.contains("AKIA"));
}
