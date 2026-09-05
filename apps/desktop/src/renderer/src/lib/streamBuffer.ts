// M2 流式增量缓冲：订阅 sg:event 的易失 delta（run.output_delta /
// run.tool_arguments_delta / run.reasoning_summary_delta），按 run 聚合，
// 供对话区增量渲染。delta 不可靠（断线/背压丢帧属预期）：终态事实一律以
// agent.get / agent.trace 对账重建，本缓冲只在运行中提供"正在打字"视图。

import type { TimelineEvent } from '../rpc/client';

const DELTA_TYPES = new Set([
  'run.output_delta',
  'run.tool_arguments_delta',
  'run.reasoning_summary_delta',
]);

export interface RunStreamSnapshot {
  /** 增量正文（运行中渲染；完成帧落库后由 run.result 取代）。 */
  text: string;
  /** 当前工具参数流（折叠展示，不参与正文）。 */
  toolArgs: string;
  /** 最近一次 delta 的 per-run 序号（乱序/重复防线）。 */
  seq: number;
  /** 被背压丢弃的字节数（如实展示"可能有省略"）。 */
  droppedBytes: number;
}

type Listener = (snapshot: RunStreamSnapshot) => void;

const EMPTY: RunStreamSnapshot = { text: '', toolArgs: '', seq: 0, droppedBytes: 0 };

class StreamBuffer {
  private buffers = new Map<string, RunStreamSnapshot>();
  private listeners = new Map<string, Set<Listener>>();

  /** 顶层事件订阅入口：是 delta 则吸收（返回 true），调用方不再触发全量刷新。 */
  ingest(event: TimelineEvent): boolean {
    if (!DELTA_TYPES.has(event.type)) {
      return false;
    }
    const runId = event.aggregateId;
    if (!runId) {
      return true;
    }
    const prev = this.buffers.get(runId) ?? EMPTY;
    // per-run seq 单调：乱序/重复 delta 直接丢弃。
    if (event.sequence <= prev.seq) {
      return true;
    }
    const text = event.payload?.text ?? '';
    const next: RunStreamSnapshot = {
      text: event.type === 'run.output_delta' ? prev.text + text : prev.text,
      toolArgs:
        event.type === 'run.tool_arguments_delta' ? prev.toolArgs + text : prev.toolArgs,
      seq: event.sequence,
      droppedBytes: prev.droppedBytes + (event.payload?.droppedBytes ?? 0),
    };
    this.buffers.set(runId, next);
    this.listeners.get(runId)?.forEach((l) => l(next));
    return true;
  }

  snapshot(runId: string): RunStreamSnapshot {
    return this.buffers.get(runId) ?? EMPTY;
  }

  /** 订阅某 run 的流式视图；返回退订函数。 */
  subscribe(runId: string, listener: Listener): () => void {
    let set = this.listeners.get(runId);
    if (!set) {
      set = new Set();
      this.listeners.set(runId, set);
    }
    set.add(listener);
    listener(this.snapshot(runId));
    return () => {
      set?.delete(listener);
      if (set && set.size === 0) {
        this.listeners.delete(runId);
      }
    };
  }

  /** 终态后清缓冲（结果以 run.result 为准）。 */
  clear(runId: string): void {
    this.buffers.delete(runId);
    this.listeners.get(runId)?.forEach((l) => l(EMPTY));
  }
}

export const streamBuffer = new StreamBuffer();

export function isDeltaEvent(event: TimelineEvent): boolean {
  return DELTA_TYPES.has(event.type);
}
