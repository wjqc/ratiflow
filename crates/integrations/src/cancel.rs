//! 可取消令牌（ADR-033 M2）：RunRegistry 与 Provider abort 的统一取消原语。
//!
//! 取代 `AtomicBool`：除同步轮询（迭代边界）外，还提供 async 等待原语
//! [`CancelToken::cancelled`]，使阻塞中的模型 HTTP/SSE 请求能经 `tokio::select!`
//! 即时中止（放弃 future 即关闭连接 socket）——"取消只能等下一轮"就此终结。

use std::sync::Arc;

/// 可克隆的取消令牌。cancel 幂等；等待者即时唤醒（watch 通道，无轮询）。
#[derive(Clone)]
pub struct CancelToken {
    inner: Arc<Inner>,
}

struct Inner {
    tx: tokio::sync::watch::Sender<bool>,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl CancelToken {
    pub fn new() -> Self {
        let (tx, _rx) = tokio::sync::watch::channel(false);
        Self {
            inner: Arc::new(Inner { tx }),
        }
    }

    /// 置位取消。幂等，可从任意线程调用（包括 DB actor / RPC 线程）。
    pub fn cancel(&self) {
        self.inner.tx.send_modify(|v| *v = true);
    }

    /// 同步检查（agent 循环迭代边界、工具执行前）。
    pub fn is_cancelled(&self) -> bool {
        *self.inner.tx.borrow()
    }

    /// 异步等待取消（select 分支用）。已取消时立即返回。
    pub async fn cancelled(&self) {
        let mut rx = self.inner.tx.subscribe();
        loop {
            if *rx.borrow() {
                return;
            }
            match rx.changed().await {
                Ok(()) => continue,
                // 发送端全灭只发生在进程回收期，等价取消语义（调用方在收尾）。
                Err(_) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn cancel_wakes_waiter_immediately() {
        let token = CancelToken::new();
        let waiter = token.clone();
        let t = tokio::spawn(async move {
            tokio::select! {
                _ = waiter.cancelled() => "cancelled",
                _ = tokio::time::sleep(Duration::from_secs(10)) => "timeout",
            }
        });
        token.cancel();
        let started = std::time::Instant::now();
        assert_eq!(t.await.unwrap(), "cancelled");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn cancel_is_idempotent_and_sync_visible() {
        let token = CancelToken::new();
        assert!(!token.is_cancelled());
        token.cancel();
        token.cancel();
        assert!(token.is_cancelled());
    }
}
