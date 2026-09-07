//! 高频 UI delta 通道（ADR-033 M2）：
//! - delta 是易失体验事件：不写 outbox/SQLite，只经 Core stdout 独立通道推送；
//! - 30–50ms 合并（40ms 窗口），64KiB 背压上限（超限丢弃新 delta 并计数）；
//! - per-run 单调 seq（renderer 依序渲染、检测乱序/重复）；
//! - 终态事实不经本通道——renderer 断线/重建后用 agent.get/trace 对账。

use std::collections::HashMap;
use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;

/// 合并窗口（方案 §5 M2：30–50ms）。
pub const COALESCE_WINDOW_MS: u64 = 40;
/// 缓冲背压上限（字节）：超过即丢弃后续 delta（丢帧优于阻塞 Run 循环）。
const BUFFER_CAP_BYTES: usize = 64 << 10;
/// 生产者→合并器通道深度；满即 try_send 丢弃（第二级背压）。
const CHANNEL_DEPTH: usize = 256;

/// 生产者侧投递项（kind 即事件类型字符串，避免 core 依赖 agent 枚举序列化）。
pub struct DeltaItem {
    pub run_id: String,
    pub workitem_id: String,
    pub kind: &'static str,
    pub text: String,
}

/// Run 任务侧的发送端（clone 进任务）。
#[derive(Clone)]
pub struct DeltaHub {
    tx: mpsc::Sender<DeltaItem>,
}

impl DeltaHub {
    /// 尽力而为投递：通道满即丢弃（返回是否投递成功，仅供测试/观测）。
    pub fn publish(&self, item: DeltaItem) -> bool {
        self.tx.try_send(item).is_ok()
    }
}

/// 合并器句柄（持有接收端；coalescer 任务随 attach 启动）。
pub struct DeltaCoalescer {
    rx: mpsc::Receiver<DeltaItem>,
}

/// 建 hub + 合并器（在 app 启动时调用一次）。
pub fn channel() -> (DeltaHub, DeltaCoalescer) {
    let (tx, rx) = mpsc::channel(CHANNEL_DEPTH);
    (DeltaHub { tx }, DeltaCoalescer { rx })
}

/// per-run 合并缓冲。
#[derive(Default)]
struct RunBuffer {
    text: String,
    dropped_bytes: u64,
    workitem_id: String,
}

impl DeltaCoalescer {
    /// 驱动合并循环：收 delta → 按 (run, kind) 聚合 → 每 40ms 以 stdout 单写者
    /// 通道 try_send 推送（stdout 拥堵时丢 delta，永不阻塞/反压 Run 循环）。
    pub async fn run(mut self, wtx: mpsc::Sender<String>) {
        let mut tick = tokio::time::interval(Duration::from_millis(COALESCE_WINDOW_MS));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut buffers: HashMap<(String, &'static str), RunBuffer> = HashMap::new();
        let mut seqs: HashMap<String, i64> = HashMap::new();
        let mut buffered_bytes: usize = 0;
        loop {
            let item = tokio::select! {
                _ = tick.tick() => {
                    // 窗口到：冲刷所有非空缓冲。
                    for ((run_id, kind), buf) in buffers.iter_mut() {
                        if buf.text.is_empty() && buf.dropped_bytes == 0 {
                            continue;
                        }
                        let seq = seqs.entry(run_id.clone()).or_default();
                        *seq += 1;
                        let line = sg_protocol::event_notification(json!({
                            "sequence": *seq,
                            "aggregateType": "agent_run",
                            "aggregateId": run_id,
                            "type": kind,
                            "payload": {
                                "workItemId": buf.workitem_id,
                                "text": buf.text,
                                "droppedBytes": buf.dropped_bytes,
                            },
                            "occurredAt": sg_store::timefmt::now(),
                            "volatile": true,
                        }))
                        .to_line();
                        buffered_bytes -= buf.text.len();
                        buf.text.clear();
                        buf.dropped_bytes = 0;
                        // 丢帧优于阻塞：stdout 通道满时放弃本批 delta。
                        let _ = wtx.try_send(line);
                    }
                    // 清理已冲刷的空缓冲，防长会话下 map 无界增长。
                    buffers.retain(|_, b| !b.text.is_empty() || b.dropped_bytes > 0);
                    continue;
                }
                item = self.rx.recv() => item,
            };
            let Some(item) = item else { break };
            // 64KiB 背压上限：缓冲超限即丢弃（计数，下批 flush 携带）。
            if buffered_bytes + item.text.len() > BUFFER_CAP_BYTES {
                let key = (item.run_id.clone(), item.kind);
                let buf = buffers.entry(key).or_default();
                buf.workitem_id = item.workitem_id;
                buf.dropped_bytes += item.text.len() as u64;
                continue;
            }
            buffered_bytes += item.text.len();
            let key = (item.run_id.clone(), item.kind);
            let buf = buffers.entry(key).or_default();
            buf.workitem_id = item.workitem_id;
            buf.text.push_str(&item.text);
        }
    }
}

/// Run 任务侧转发器：实现 agent 网关的 TurnDeltaForwarder（阻塞线程安全，
/// try_send 语义——丢弃优于反压）。
pub struct HubForwarder {
    hub: DeltaHub,
    run_id: String,
    workitem_id: String,
}

impl HubForwarder {
    pub fn new(hub: DeltaHub, run_id: &str, workitem_id: &str) -> Self {
        Self {
            hub,
            run_id: run_id.to_string(),
            workitem_id: workitem_id.to_string(),
        }
    }
}

impl sg_agent::modelgw::TurnDeltaForwarder for HubForwarder {
    fn forward(&self, _run_id: &str, kind: sg_agent::modelgw::DeltaKind, text: &str) {
        let _ = self.hub.publish(DeltaItem {
            run_id: self.run_id.clone(),
            workitem_id: self.workitem_id.clone(),
            kind: kind.event_type(),
            text: text.to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合并语义：同 run 同 kind 的连续 delta 在一个窗口内合成一条事件；
    /// per-run seq 单调递增、无重复。
    #[tokio::test]
    async fn coalesces_deltas_within_window_with_monotonic_seq() {
        let (hub, coalescer) = channel();
        let (wtx, mut wrx) = mpsc::channel(64);
        tokio::spawn(coalescer.run(wtx));
        for piece in ["你", "好", "，", "世", "界"] {
            assert!(hub.publish(DeltaItem {
                run_id: "run1".into(),
                workitem_id: "wi".into(),
                kind: "run.output_delta",
                text: piece.into(),
            }));
        }
        // 一个窗口内只应收到 1 条合并事件。
        let line = tokio::time::timeout(Duration::from_millis(200), wrx.recv())
            .await
            .expect("flush within window")
            .expect("line");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["method"], "event");
        let params = &v["params"];
        assert_eq!(params["type"], "run.output_delta");
        assert_eq!(params["aggregateId"], "run1");
        assert_eq!(params["sequence"], 1);
        assert_eq!(params["payload"]["text"], "你好，世界");
        assert_eq!(params["volatile"], true);
        // 第二窗口：seq 递增。
        assert!(hub.publish(DeltaItem {
            run_id: "run1".into(),
            workitem_id: "wi".into(),
            kind: "run.output_delta",
            text: "!".into(),
        }));
        let line = tokio::time::timeout(Duration::from_millis(300), wrx.recv())
            .await
            .unwrap()
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["params"]["sequence"], 2);
        assert_eq!(v["params"]["payload"]["text"], "!");
    }

    /// 64KiB 背压上限：超限 delta 被丢弃且 droppedBytes 在下批事件中如实上报。
    #[tokio::test]
    async fn backpressure_cap_drops_and_reports() {
        let (hub, coalescer) = channel();
        let (wtx, mut wrx) = mpsc::channel(64);
        tokio::spawn(coalescer.run(wtx));
        let big = "x".repeat(50 << 10);
        // 两笔共 100KiB > 64KiB：第二笔应被丢弃。
        for _ in 0..2 {
            assert!(hub.publish(DeltaItem {
                run_id: "run-bp".into(),
                workitem_id: "wi".into(),
                kind: "run.output_delta",
                text: big.clone(),
            }));
        }
        let line = tokio::time::timeout(Duration::from_millis(300), wrx.recv())
            .await
            .unwrap()
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["params"]["payload"]["text"], big);
        assert_eq!(v["params"]["payload"]["droppedBytes"], big.len() as u64);
    }

    /// 乱序/重复序号防线：不同 kind 各自合并但共享 per-run seq，仍严格递增。
    #[tokio::test]
    async fn seq_shared_across_kinds_is_monotonic() {
        let (hub, coalescer) = channel();
        let (wtx, mut wrx) = mpsc::channel(64);
        tokio::spawn(coalescer.run(wtx));
        assert!(hub.publish(DeltaItem {
            run_id: "r".into(),
            workitem_id: "wi".into(),
            kind: "run.output_delta",
            text: "a".into(),
        }));
        assert!(hub.publish(DeltaItem {
            run_id: "r".into(),
            workitem_id: "wi".into(),
            kind: "run.tool_arguments_delta",
            text: "{\"".into(),
        }));
        let mut seqs = Vec::new();
        for _ in 0..2 {
            let line = tokio::time::timeout(Duration::from_millis(300), wrx.recv())
                .await
                .unwrap()
                .unwrap();
            let v: serde_json::Value = serde_json::from_str(&line).unwrap();
            seqs.push(v["params"]["sequence"].as_i64().unwrap());
        }
        seqs.sort();
        assert_eq!(seqs, vec![1, 2], "per-run seq 单调无重复");
    }
}
