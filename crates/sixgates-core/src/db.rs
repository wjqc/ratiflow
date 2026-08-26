//! DB actor：专用线程独占 Store，闭包请求经 mpsc 串行执行（ADR-028）。
//! 纪律：任何 `.await` 不得跨越 DB 借用；长 I/O（模型调用、工具进程）必须在 actor 之外执行。
use sg_store::Store;
use tokio::sync::{mpsc, oneshot};

type Job = Box<dyn FnOnce(&Store) + Send + 'static>;

/// actor 已关闭（job panic 触发进程退出，或通道断开）。
#[derive(Debug)]
pub struct DbClosed;

#[derive(Clone)]
pub struct Db {
    tx: mpsc::Sender<Job>,
}

impl Db {
    /// Store 所有权移入 actor 线程；调用方只持有句柄。
    pub fn spawn(store: Store) -> Self {
        let (tx, mut rx) = mpsc::channel::<Job>(64);
        std::thread::spawn(move || {
            while let Some(job) = rx.blocking_recv() {
                // job panic 不带着中毒状态继续服务：记日志后退出，交给 Electron 崩溃重启策略。
                let panicked =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&store))).is_err();
                if panicked {
                    eprintln!(
                        "{{\"level\":\"fatal\",\"msg\":\"db actor job panicked; exiting for restart\"}}"
                    );
                    std::process::exit(101);
                }
            }
        });
        Self { tx }
    }

    /// 在 actor 线程上执行闭包并等待结果。
    pub async fn call<T, F>(&self, f: F) -> Result<T, DbClosed>
    where
        F: FnOnce(&Store) -> T + Send + 'static,
        T: Send + 'static,
    {
        let (otx, orx) = oneshot::channel();
        let job: Job = Box::new(move |store| {
            let _ = otx.send(f(store));
        });
        self.tx.send(job).await.map_err(|_| DbClosed)?;
        orx.await.map_err(|_| DbClosed)
    }
}
