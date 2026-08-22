//! sixgates-core app-server：JSON-RPC 2.0 over stdio。
//! stdout 只输出协议消息；stderr 输出结构化日志（main 轮转写文件）。
//! 子命令：app-server（默认）| migrate-v2 --from <dir> --to <dir>

mod dispatch;

extern "C" {
    fn getppid() -> i32;
}

/// safety: getppid 是 POSIX 纯查询。
unsafe fn libc_getppid() -> i32 {
    getppid()
}
mod migrate;
mod settings_dispatch;
mod state;

use std::io::{BufRead, Write};
use std::sync::atomic::Ordering;

use serde_json::json;
use sg_protocol::{
    err_response, event_notification, hello_compatible, ok_response, Hello, Request, RpcMessage,
};
use sg_store::{outbox, Store};

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
    let app = state::AppState::new(store, env!("CARGO_PKG_VERSION"));

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    // hello 握手（main 校验 protocolVersion；不兼容时不创建业务窗口）。
    let hello = Hello {
        protocolVersion: sg_protocol::PROTOCOL_VERSION.into(),
        coreVersion: env!("CARGO_PKG_VERSION").into(),
        schemaVersion: app.store.schema_version().unwrap_or(0),
        capabilities: vec![
            "workitem".into(),
            "knowledge".into(),
            "agent".into(),
            "deployment".into(),
        ],
    };
    let _ = writeln!(out, "{}", serde_json::to_string(&hello).unwrap_or_default());
    let _ = out.flush();

    // sidecar 崩溃恢复语义：启动即 quick_check + 恢复检查点扫描。
    if let Err(e) = app.store.quick_check() {
        eprintln!("{{\"level\":\"error\",\"msg\":\"quick_check: {e}\"}}");
    }
    // 初始事件水位。
    if let Ok(latest) = outbox::latest_sequence(&app.store) {
        app.last_pushed.store(latest, Ordering::SeqCst);
    }

    // parent-death watchdog：Electron 退出后 core 必须自动终止（P0 F 生命周期）。
    std::thread::spawn(|| loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        // getppid 变为 1（init/launchd）说明父进程已死。
        if unsafe { libc_getppid() } == 1 {
            eprintln!("{{\"level\":\"info\",\"msg\":\"parent exited; core self-terminating\"}}");
            std::process::exit(0);
        }
    });

    let stdin = std::io::stdin();
    let mut pending_shutdown = false;
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.len() > sg_protocol::MAX_MESSAGE_BYTES {
            let _ = writeln!(
                out,
                "{}",
                err_response(
                    None,
                    sg_protocol::RpcError::new(
                        sg_protocol::ErrorCode::InvalidRequest,
                        "message exceeds 8 MiB limit"
                    )
                )
                .to_line()
            );
            let _ = out.flush();
            continue;
        }
        let message: RpcMessage = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                let _ = writeln!(
                    out,
                    "{}",
                    err_response(
                        None,
                        sg_protocol::RpcError::new(
                            sg_protocol::ErrorCode::ParseError,
                            e.to_string()
                        )
                    )
                    .to_line()
                );
                let _ = out.flush();
                continue;
            }
        };
        match message {
            RpcMessage::Request(req) => {
                let id = req.id.clone();
                let response = handle_request(&app, &req);
                let line = match response {
                    Ok(value) => ok_response(id, value).to_line(),
                    Err(e) => err_response(id, e).to_line(),
                };
                let _ = writeln!(out, "{line}");
                // 增量推送事件 notification。
                push_events(&app, &mut out);
                let _ = out.flush();
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

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn handle_request(
    app: &state::AppState,
    req: &Request,
) -> Result<serde_json::Value, sg_protocol::RpcError> {
    let params = req.params.clone().unwrap_or_else(|| json!({}));
    dispatch::dispatch(app, &req.method, &params)
}

fn push_events(app: &state::AppState, out: &mut std::io::StdoutLock<'static>) {
    let last = app.last_pushed.load(Ordering::SeqCst);
    if let Ok(latest) = outbox::latest_sequence(&app.store) {
        if latest > last {
            if let Ok(events) = outbox::replay(&app.store, last, 500) {
                for event in events {
                    let notification = event_notification(event);
                    let _ = writeln!(out, "{}", notification.to_line());
                }
            }
            app.last_pushed.store(latest, Ordering::SeqCst);
        }
    }
}

// 供测试引用的兼容性断言。
#[allow(unused)]
fn assert_hello_contract(hello: &Hello) -> bool {
    hello_compatible(hello)
}
