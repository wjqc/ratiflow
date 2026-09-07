//! sixgates-core app-server：JSON-RPC 2.0 over stdio（tokio 运行时，ADR-028）。
//! stdout 只输出协议消息（单写者任务）；stderr 输出结构化日志（main 轮转写文件）。
//! 子命令：app-server（默认）| migrate-v2 --from <dir> --to <dir>

mod db;
mod deltas;
mod dispatch;
mod model_source;

extern "C" {
    fn getppid() -> i32;
}

/// safety: getppid 是 POSIX 纯查询。
unsafe fn libc_getppid() -> i32 {
    getppid()
}
mod automation_dispatch;
mod commands;
mod memory_dispatch;
mod migrate;
mod plan_dispatch;
mod plan_runtime;
mod settings_dispatch;
mod state;
mod tool_exec;
mod trace_dispatch;
mod workflow_dispatch;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sg_protocol::{
    err_response, event_notification, ok_response, ErrorCode, Hello, RpcError, RpcMessage,
};
use sg_store::{outbox, Store};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "migrate-v2") {
        let from = flag_value(&args, "--from").unwrap_or_else(|| "./data".into());
        let to = flag_value(&args, "--to").unwrap_or_else(|| "./data-v3".into());
        match migrate::migrate_v2(&from, &to, env!("CARGO_PKG_VERSION")) {
            Ok(report) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).unwrap_or_default()
                );
            }
            Err(e) => {
                eprintln!("migrate-v2 failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let data_dir = flag_value(&args, "--data-dir")
        .unwrap_or_else(|| std::env::var("SIXGATES_DATA_DIR").unwrap_or_else(|_| "./data".into()));
    let store = match Store::open(std::path::Path::new(&data_dir), env!("CARGO_PKG_VERSION")) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{{\"level\":\"fatal\",\"msg\":\"store open failed: {e}\"}}");
            std::process::exit(1);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("{{\"level\":\"fatal\",\"msg\":\"tokio runtime init failed: {e}\"}}");
            std::process::exit(1);
        }
    };
    // Run 任务专属连接（M0-②）：WAL 多连接；RPC 走 DB actor 连接，Run 循环走本连接。
    let run_store = match Store::open(std::path::Path::new(&data_dir), env!("CARGO_PKG_VERSION")) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("{{\"level\":\"fatal\",\"msg\":\"run store open failed: {e}\"}}");
            std::process::exit(1);
        }
    };
    runtime.block_on(run_server(store, run_store, env!("CARGO_PKG_VERSION")));
}

async fn run_server(store: Store, run_store: Arc<Store>, core_version: &'static str) {
    // sidecar 崩溃恢复语义：启动即 quick_check。
    // hello 的 schemaVersion 与初始事件水位必须在 Store 移入 DB actor 前读取。
    if let Err(e) = store.quick_check() {
        eprintln!("{{\"level\":\"error\",\"msg\":\"quick_check: {e}\"}}");
    }
    // 项目记忆候选捕获 reconciliation（ADR-032 M4 / §8.2）：
    // 遗留 in_flight 无可靠 Provider 查询能力 → unknown；pending 留待显式重试。
    match sg_memory::capture::reconcile_broken_in_flight(&store) {
        Ok(n) if n > 0 => {
            eprintln!("{{\"level\":\"info\",\"msg\":\"memory capture reconciled: {n} in_flight -> unknown\"}}");
        }
        Ok(_) => {}
        Err(e) => eprintln!("{{\"level\":\"error\",\"msg\":\"memory capture reconcile: {e}\"}}"),
    }
    // 项目记忆 FTS 投影启动检查（ADR-032 M1）：不一致才重建；失败仅告警不阻断。
    match sg_memory::ensure_fts_consistent(&store) {
        Ok(report) => {
            if report["rebuild"] == serde_json::json!(true) {
                eprintln!("{{\"level\":\"info\",\"msg\":\"memory fts rebuilt: {report}\"}}");
            }
        }
        Err(e) => eprintln!("{{\"level\":\"error\",\"msg\":\"memory fts check: {e}\"}}"),
    }
    // M1 谱系底座：legacy docs → synthetic requirement revision（unverified）。
    // 幂等回填；失败不阻断启动（表为 additive，仅告警）。
    if crate::dispatch::trace_writes_enabled() {
        match sg_workitem::requirements::backfill_legacy(&store) {
            Ok(n) if n > 0 => {
                eprintln!(
                    "{{\"level\":\"info\",\"msg\":\"legacy requirement backfill: {n} workitems\"}}"
                );
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!(
                    "{{\"level\":\"warn\",\"msg\":\"legacy requirement backfill failed: {e}\"}}"
                );
            }
        }
        match sg_workitem::attempt::backfill_legacy(&store) {
            Ok(n) if n > 0 => {
                eprintln!("{{\"level\":\"info\",\"msg\":\"legacy stage attempt backfill: {n}\"}}");
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("{{\"level\":\"warn\",\"msg\":\"legacy attempt backfill failed: {e}\"}}");
            }
        }
    }
    // Context 存量 data migration（RFC v1.0 §8.2）：legacy_pending manifest 重建冻结；
    // 失败不阻断启动（job 停留 pending/failed，下次启动续跑）。
    match sg_context::legacy_migrate::run(&store, 500) {
        Ok(summary) => {
            eprintln!("{{\"level\":\"info\",\"msg\":\"context legacy migration: {summary}\"}}");
        }
        Err(e) => {
            eprintln!("{{\"level\":\"warn\",\"msg\":\"context legacy migration deferred: {e}\"}}");
        }
    }

    // 放行崩溃恢复：审批已决但推进未完成的请求补完（蓝图 §4.2 原子语义补偿）。
    match sg_workitem::release::resume_pending(&store) {
        Ok(n) if n > 0 => {
            eprintln!("{{\"level\":\"info\",\"msg\":\"release resume: {n} completed\"}}");
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("{{\"level\":\"warn\",\"msg\":\"release resume failed: {e}\"}}");
        }
    }
    // 回滚崩溃恢复（AC-SW-12）：executing 中断的回滚幂等补完，快照 digest 不变。
    match sg_workitem::rollback::resume(&store) {
        Ok(n) if n > 0 => {
            eprintln!("{{\"level\":\"info\",\"msg\":\"rollback resume: {n} completed\"}}");
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("{{\"level\":\"warn\",\"msg\":\"rollback resume failed: {e}\"}}");
        }
    }
    // M6-04：自动化孤儿触发收敛（重复启动不重复执行——receipt 已去重，只标终态）。
    if std::env::var("SIXGATES_AUTOMATIONS").ok().as_deref() == Some("1") {
        match sg_workflow::automation::reconcile_orphans(&store) {
            Ok(n) if n > 0 => {
                eprintln!("{{\"level\":\"info\",\"msg\":\"automation orphan reconcile: {n}\"}}");
            }
            Ok(_) => {}
            Err(e) => eprintln!("{{\"level\":\"warn\",\"msg\":\"automation reconcile: {e}\"}}"),
        }
    }
    // Agent Run 启动对账：崩溃/重启遗留的 queued/running 标 failed(interrupted)，
    // 前端轮询立即见终态，不再挂满超时窗口。
    match sg_agent::reconcile_interrupted(&store) {
        Ok(n) if n > 0 => {
            eprintln!("{{\"level\":\"info\",\"msg\":\"agent run reconcile: {n} interrupted\"}}");
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("{{\"level\":\"warn\",\"msg\":\"agent run reconcile failed: {e}\"}}");
        }
    }
    let schema_version = store.schema_version().unwrap_or(0);
    let initial_seq = outbox::latest_sequence(&store).unwrap_or(0);

    // M4：内置通用 AgentProfile（固定版本与 digest，幂等）。
    if let Err(e) = sg_agent::profile::ensure_builtin_generic(&store) {
        eprintln!("{{\"level\":\"warn\",\"msg\":\"builtin generic profile init failed: {e}\"}}");
    }
    // F11/M4：工具注册表过 tool-definition 契约校验（违例 fail-fast，不带着非法注册表服务）。
    if let Err(e) = sg_agent::schema::validate_registry(&sg_agent::tools::registry()) {
        eprintln!("{{\"level\":\"fatal\",\"msg\":\"tool registry schema violation: {e}\"}}");
        std::process::exit(1);
    }
    let db = db::Db::spawn(store);

    // stdout 单写者任务：hello、响应、事件通知统一经 mpsc 排队写出，无并发交错。
    let (wtx, mut wrx) = tokio::sync::mpsc::channel::<String>(256);
    tokio::spawn(async move {
        let mut writer = tokio::io::BufWriter::new(tokio::io::stdout());
        while let Some(line) = wrx.recv().await {
            if writer.write_all(line.as_bytes()).await.is_err()
                || writer.write_all(b"\n").await.is_err()
            {
                break;
            }
            let _ = writer.flush().await;
        }
    });

    // M2 高频 UI delta 通道：40ms 合并 + 64KiB 背压上限，经同一 stdout 单写者
    // 发出（volatile 事件，不落 outbox/SQLite；丢帧不产生假终态）。
    let (delta_hub, delta_coalescer) = deltas::channel();
    tokio::spawn(delta_coalescer.run(wtx.clone()));

    let app = Arc::new(state::AppState::new(
        db,
        run_store,
        initial_seq,
        core_version,
        delta_hub,
    ));

    // hello 握手（main 校验 protocolVersion；不兼容时不创建业务窗口）。
    let hello = Hello {
        protocolVersion: sg_protocol::PROTOCOL_VERSION.into(),
        coreVersion: core_version.into(),
        schemaVersion: schema_version,
        capabilities: vec![
            "workitem".into(),
            "knowledge".into(),
            "agent".into(),
            "deployment".into(),
        ],
    };
    send(&wtx, serde_json::to_string(&hello).unwrap_or_default()).await;

    // parent-death watchdog：Electron 退出后 core 必须自动终止（P0 F 生命周期）。
    std::thread::spawn(|| loop {
        std::thread::sleep(Duration::from_secs(2));
        // getppid 变为 1（init/launchd）说明父进程已死。
        if unsafe { libc_getppid() } == 1 {
            eprintln!("{{\"level\":\"info\",\"msg\":\"parent exited; core self-terminating\"}}");
            std::process::exit(0);
        }
    });

    // outbox flush 任务：250ms 兜底推送（空闲也推；响应后另有即时 flush）。
    {
        let app = app.clone();
        let wtx = wtx.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(250));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                flush_once(&app, &wtx).await;
            }
        });
    }

    // M6-04：自动化调度 timer（SIXGATES_AUTOMATIONS=1；5s tick）。
    if std::env::var("SIXGATES_AUTOMATIONS").ok().as_deref() == Some("1") {
        let app_timer = app.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(5));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let result = app_timer.db.call(automation_dispatch::fire_due).await;
                match result {
                    Ok(Ok(fired)) if !fired.is_empty() => {
                        for (id, status, note) in &fired {
                            eprintln!("{{\"level\":\"info\",\"msg\":\"automation {id} -> {status}: {note}\"}}");
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    // stdin 读循环：纯分发；请求体在 DB actor 上串行执行。
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut pending_shutdown = false;
    while let Ok(Some(line)) = lines.next_line().await {
        if line.len() > sg_protocol::MAX_MESSAGE_BYTES {
            send(
                &wtx,
                err_response(
                    None,
                    RpcError::new(ErrorCode::InvalidRequest, "message exceeds 8 MiB limit"),
                )
                .to_line(),
            )
            .await;
            continue;
        }
        let message: RpcMessage = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                send(
                    &wtx,
                    err_response(None, RpcError::new(ErrorCode::ParseError, e.to_string()))
                        .to_line(),
                )
                .await;
                continue;
            }
        };
        match message {
            RpcMessage::Request(req) => {
                let id = req.id.clone();
                let params = req.params.clone().unwrap_or_else(|| json!({}));
                let app2 = app.clone();
                let method = req.method.clone();
                let result: Result<serde_json::Value, RpcError> = app
                    .db
                    .call(move |store| dispatch::dispatch(&app2, store, &method, &params))
                    .await
                    .unwrap_or_else(|_| {
                        Err(RpcError::new(
                            ErrorCode::InternalError,
                            "db actor unavailable",
                        ))
                    });
                let line = match result {
                    Ok(value) => ok_response(id, value).to_line(),
                    Err(e) => err_response(id, e).to_line(),
                };
                send(&wtx, line).await;
                // 响应后即时推送（延续既有 piggy-back 语义，消除 250ms 时延）。
                flush_once(&app, &wtx).await;
                if pending_shutdown {
                    break;
                }
            }
            RpcMessage::Notification(n) => {
                if n.method == "shutdown" {
                    pending_shutdown = true;
                }
                if pending_shutdown {
                    break;
                }
            }
            RpcMessage::Response(_) => {
                eprintln!("{{\"level\":\"warn\",\"msg\":\"unexpected response from main\"}}");
            }
        }
    }
    eprintln!("{{\"level\":\"info\",\"msg\":\"core exiting\"}}");
}

async fn send(tx: &tokio::sync::mpsc::Sender<String>, line: String) {
    let _ = tx.send(line).await;
}

/// 增量推送 outbox 事件（读循环与 250ms 兜底任务共用）。
/// CAS 认领水位区间：并发调用只有一方推送，避免重复；replay 失败不推进水位（下轮重试，不丢事件）。
async fn flush_once(app: &Arc<state::AppState>, wtx: &tokio::sync::mpsc::Sender<String>) {
    let last = app.last_pushed.load(Ordering::SeqCst);
    let batch: Option<(i64, Vec<serde_json::Value>)> = app
        .db
        .call(move |store| {
            let latest = outbox::latest_sequence(store).ok()?;
            if latest > last {
                Some((latest, outbox::replay(store, last, 500).ok()?))
            } else {
                None
            }
        })
        .await
        .ok()
        .flatten();
    if let Some((latest, events)) = batch {
        if app
            .last_pushed
            .compare_exchange(last, latest, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return; // 另一次 flush 已认领该区间
        }
        for event in events {
            send(wtx, event_notification(event).to_line()).await;
        }
    }
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

// 供测试引用的兼容性断言。
#[allow(unused)]
fn assert_hello_contract(hello: &Hello) -> bool {
    sg_protocol::hello_compatible(hello)
}
