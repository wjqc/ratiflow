//! capture 状态机与候选裁决单测（实施方案 §13 M4 / §14.1）。
//! 重点：unknown 不透明重试、不重复候选、拒绝不再推荐、接受可编辑并激活。
use serde_json::{json, Value};

use sg_memory as mem;
use sg_memory::capture::CandidateParsed;
use sg_store::Store;

struct Temp {
    path: std::path::PathBuf,
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn tempdir(tag: &str) -> Temp {
    let path = std::env::temp_dir().join(format!(
        "{tag}-{}-{}",
        std::process::id(),
        sg_store::ids::new_id("t")
    ));
    std::fs::create_dir_all(&path).unwrap();
    Temp { path }
}

fn open_store(tag: &str) -> (Store, Temp) {
    let t = tempdir(tag);
    (Store::open(&t.path, "test").unwrap(), t)
}

fn add_project(store: &Store, id: &str) {
    store
        .with_conn(|c| {
            c.execute(
                "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                 VALUES (?1, 'u', 'ns-' || ?1, 'prj-' || ?1, 'main', ?2)",
                rusqlite::params![id, sg_store::timefmt::now()],
            )?;
            c.execute(
                "INSERT INTO workitems(id, project_id, title, created_at, updated_at)
                 VALUES ('wi-' || ?1, ?1, 't', ?2, ?2)",
                rusqlite::params![id, sg_store::timefmt::now()],
            )?;
            c.execute(
                "INSERT INTO context_manifests(id, workitem_id, scope, data_policy, created_at)
                 VALUES ('cm-' || ?1, 'wi-' || ?1, '{}', 'standard', ?2)",
                rusqlite::params![id, sg_store::timefmt::now()],
            )?;
            c.execute(
                "INSERT INTO agent_runs(
                    id, workitem_id, goal, input_baseline_sha, context_manifest_id,
                    budget, policy_snapshot, idempotency_key, status, created_at, updated_at
                 ) VALUES ('run-' || ?1, 'wi-' || ?1, '部署 检查', 'sha', 'cm-' || ?1, '{}', '{}',
                           'idem-' || ?1, ?3, ?2, ?2)",
                rusqlite::params![id, sg_store::timefmt::now(), "completed_execution"],
            )?;
            Ok(())
        })
        .unwrap();
}

fn enable_suggest(store: &Store, project: &str) {
    sg_memory::set_feature_enabled(store, true, "tester").unwrap();
    sg_memory::settings_update(
        store,
        project,
        &sg_memory::SettingsPatch {
            enabled: Some(true),
            capture_mode: Some("suggest".into()),
            ..Default::default()
        },
        1,
        &sg_store::ids::new_id("k"),
    )
    .unwrap();
}

/// 排队条件：仅 completed_execution 且 capture_mode=suggest（MEM-019）。
#[test]
fn start_job_gate_conditions() {
    let (store, _t) = open_store("sg-cap-gate");
    add_project(&store, "pj");

    // capture_mode=off → memory_disabled。
    sg_memory::set_feature_enabled(&store, true, "tester").unwrap();
    assert_err_token(
        mem::capture::start_job(&store, "pj", "run-pj", "k1", "", 0),
        "memory_disabled",
    );

    // run 不存在 → not_found（先于模式检查）。
    assert_err_token(
        mem::capture::start_job(&store, "pj", "run-missing", "k1", "", 0),
        "not_found",
    );

    // suggest + completed → pending。
    enable_suggest(&store, "pj");
    let job = mem::capture::start_job(&store, "pj", "run-pj", "k1", "", 0).unwrap();
    assert_eq!(job["status"], "pending");
    assert!(job["jobId"].as_str().unwrap().starts_with("memjob_"));
}

fn assert_err_token<T>(result: Result<T, sg_store::Error>, token: &str) {
    let msg = result.err().expect("期望失败").to_string();
    assert!(msg.starts_with(token), "期望 {token}，实际 {msg}");
}

/// 幂等：同 key 同内容返回原 job；同 key 异 run → conflict。
#[test]
fn start_job_idempotent() {
    let (store, _t) = open_store("sg-cap-idem");
    add_project(&store, "pj");
    enable_suggest(&store, "pj");
    let a = mem::capture::start_job(&store, "pj", "run-pj", "k1", "", 0).unwrap();
    let b = mem::capture::start_job(&store, "pj", "run-pj", "k1", "", 0).unwrap();
    assert_eq!(a["jobId"], b["jobId"]);

    // 异指纹（不同 run）同 key → 冲突。
    store
        .with_conn(|c| {
            c.execute(
                "INSERT INTO agent_runs(
                    id, workitem_id, goal, input_baseline_sha, context_manifest_id,
                    budget, policy_snapshot, idempotency_key, status, created_at, updated_at
                 ) VALUES ('run-pj2', 'wi-pj', 'g', 'sha', 'cm-pj', '{}', '{}', 'idem-2', 'completed_execution', ?1, ?1)",
                [sg_store::timefmt::now()],
            )?;
            Ok(())
        })
        .unwrap();
    assert_err_token(
        mem::capture::start_job(&store, "pj", "run-pj2", "k1", "", 0),
        "memory_conflict",
    );
}

fn candidate() -> CandidateParsed {
    CandidateParsed {
        title: "部署前必须检查健康端点".into(),
        kind: "lesson".into(),
        summary: "健康端点检查是部署前置条件。".into(),
        body: "部署前必须检查健康端点；未通过则中止发布。依据：G8 验证记录。".into(),
    }
}

/// 成功路径：候选落库；同内容重复响应去重；reject 后不再推荐（MEM-020/§8.2）。
#[test]
fn succeeded_dedupes_and_reject_blocks_recommendation() {
    let (store, _t) = open_store("sg-cap-dedup");
    add_project(&store, "pj");
    enable_suggest(&store, "pj");
    let job1 = mem::capture::start_job(&store, "pj", "run-pj", "k1", "", 0).unwrap();
    let j1 = job1["jobId"].as_str().unwrap().to_string();
    assert!(mem::capture::mark_in_flight(&store, &j1).unwrap());
    let out = mem::capture::mark_succeeded(&store, &j1, &candidate(), 10, 20).unwrap();
    assert_eq!(out["status"], "succeeded");
    let candidate_id = out["candidate"].as_str().unwrap().to_string();

    // 拒绝该候选。
    let decided = mem::candidate::decide(
        &store,
        "pj",
        &candidate_id,
        "reject",
        None,
        None,
        "decide-1",
    )
    .unwrap();
    assert_eq!(decided["status"], "rejected");

    // 同内容再次捕获 → 去重（不再推荐）。
    let job2 = mem::capture::start_job(&store, "pj", "run-pj", "k2", "", 0).unwrap();
    let j2 = job2["jobId"].as_str().unwrap().to_string();
    mem::capture::mark_in_flight(&store, &j2).unwrap();
    let out2 = mem::capture::mark_succeeded(&store, &j2, &candidate(), 10, 20).unwrap();
    assert_eq!(out2["deduped"], json!(true));
    assert!(out2["candidate"].is_null());

    // pending 列表为空（候选被拒 + 新响应被去重）。
    let list = mem::candidate::list(&store, "pj", None, 50).unwrap();
    assert_eq!(list["items"].as_array().unwrap().len(), 0);
}

/// 接受：编辑内容写正式 active entry；二次裁决拒绝；接受后注入可见（协议层另有 E2E）。
#[test]
fn accept_edits_and_activates() {
    let (store, _t) = open_store("sg-cap-accept");
    add_project(&store, "pj");
    enable_suggest(&store, "pj");
    let job = mem::capture::start_job(&store, "pj", "run-pj", "k1", "", 0).unwrap();
    let j = job["jobId"].as_str().unwrap().to_string();
    mem::capture::mark_in_flight(&store, &j).unwrap();
    let out = mem::capture::mark_succeeded(&store, &j, &candidate(), 10, 20).unwrap();
    let candidate_id = out["candidate"].as_str().unwrap().to_string();

    // 未知 job 的候选裁决 → not_found。
    assert_err_token(
        mem::candidate::decide(
            &store,
            "pj",
            "memc_missing",
            "accept",
            Some("x"),
            None,
            "dk0",
        ),
        "not_found",
    );

    let decided = mem::candidate::decide(
        &store,
        "pj",
        &candidate_id,
        "accept",
        Some("编辑后的结论正文。"),
        Some("部署检查清单"),
        "decide-1",
    )
    .unwrap();
    assert_eq!(decided["status"], "accepted");
    let memory_id = decided["memoryId"].as_str().unwrap().to_string();

    let detail = mem::repository::detail(&store, "pj", &memory_id, None).unwrap();
    assert_eq!(detail["status"], "active");
    assert_eq!(detail["body"], "编辑后的结论正文。");
    assert_eq!(detail["title"], "部署检查清单");
    assert_eq!(detail["sources"][0]["sourceKind"], "run");

    // 二次裁决 → invalid_state。
    assert_err_token(
        mem::candidate::decide(
            &store,
            "pj",
            &candidate_id,
            "reject",
            None,
            None,
            "decide-2",
        ),
        "memory_invalid_state",
    );
}

/// unknown：reconcile 将 in_flight 判为 unknown；终态后 mark_terminal 不再改写；
/// unknown 不生成候选、不透明重试（MEM-021）。
#[test]
fn unknown_terminal_and_reconcile() {
    let (store, _t) = open_store("sg-cap-unknown");
    add_project(&store, "pj");
    enable_suggest(&store, "pj");
    let job = mem::capture::start_job(&store, "pj", "run-pj", "k1", "", 0).unwrap();
    let j = job["jobId"].as_str().unwrap().to_string();
    mem::capture::mark_in_flight(&store, &j).unwrap();

    // 进程崩溃模拟：启动 reconciliation 将 in_flight → unknown。
    let n = mem::capture::reconcile_broken_in_flight(&store).unwrap();
    assert_eq!(n, 1);
    let got = mem::capture::get_job(&store, "pj", &j).unwrap();
    assert_eq!(got["job"]["status"], "unknown");
    assert_eq!(got["job"]["errorCode"], "MEMORY_CAPTURE_UNKNOWN");
    assert_eq!(got["candidates"].as_array().unwrap().len(), 0);

    // 终态不可改写（无透明重试语义）。
    mem::capture::mark_terminal(&store, &j, "failed", "MEMORY_CAPTURE_FAILED").unwrap();
    let got = mem::capture::get_job(&store, "pj", &j).unwrap();
    assert_eq!(got["job"]["status"], "unknown");

    // 分类：超时/连接类 → unknown；其余 → failed。
    assert_eq!(
        mem::capture::classify_provider_error("model_timeout after 30s").0,
        "unknown"
    );
    assert_eq!(
        mem::capture::classify_provider_error("connection reset by peer").0,
        "unknown"
    );
    assert_eq!(
        mem::capture::classify_provider_error("model_unavailable").0,
        "failed"
    );
}

/// schema 校验：非法 kind/空正文/超限正文拒绝（确定性 failed）。
#[test]
fn parse_candidate_schema_validation() {
    let ok = mem::capture::parse_candidate_response(
        "前置说明 {\"title\":\"T\",\"kind\":\"fact\",\"body\":\"B 正文\"} 尾部",
        12288,
    )
    .unwrap();
    assert_eq!(ok.title, "T");
    assert_eq!(ok.kind, "fact");

    assert_err_token(
        mem::capture::parse_candidate_response(
            "{\"title\":\"T\",\"kind\":\"weird\",\"body\":\"B\"}",
            12288,
        ),
        "memory_invalid_state",
    );
    assert_err_token(
        mem::capture::parse_candidate_response(
            "{\"title\":\"T\",\"kind\":\"fact\",\"body\":\"\"}",
            12288,
        ),
        "memory_invalid_state",
    );
    let big = "x".repeat(13000);
    assert_err_token(
        mem::capture::parse_candidate_response(
            &format!("{{\"title\":\"T\",\"kind\":\"fact\",\"body\":\"{big}\"}}"),
            12288,
        ),
        "memory_invalid_state",
    );
    assert_err_token(
        mem::capture::parse_candidate_response("不是 JSON", 12288),
        "memory_invalid_state",
    );
    let _ = Value::Null;
}
