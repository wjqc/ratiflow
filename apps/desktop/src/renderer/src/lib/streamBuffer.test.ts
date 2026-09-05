// M2：流式增量缓冲单测——合并、乱序/重复序号丢弃、终态清理。
import { describe, expect, it } from 'vitest';
import { streamBuffer } from './streamBuffer';
import type { TimelineEvent } from '../rpc/client';

function delta(sequence: number, text: string, type = 'run.output_delta'): TimelineEvent {
  return {
    sequence,
    type,
    aggregateId: 'run-1',
    occurredAt: '2026-09-04T00:00:00.000Z',
    summary: '',
    payload: { text },
    volatile: true,
  };
}

describe('streamBuffer', () => {
  it('absorbs delta events and merges text by kind', () => {
    expect(streamBuffer.ingest(delta(1, '你好'))).toBe(true);
    expect(streamBuffer.ingest(delta(2, '，世界', 'run.tool_arguments_delta'))).toBe(true);
    const snap = streamBuffer.snapshot('run-1');
    expect(snap.text).toBe('你好');
    expect(snap.toolArgs).toBe('，世界');
    expect(snap.seq).toBe(2);
    expect(streamBuffer.ingest({ ...delta(3, 'x'), type: 'run.cancelled' })).toBe(false);
  });

  it('drops out-of-order and duplicate sequences (renderer-side guard)', () => {
    streamBuffer.clear('run-oo');
    expect(streamBuffer.ingest({ ...delta(5, 'a'), aggregateId: 'run-oo' })).toBe(true);
    expect(streamBuffer.ingest({ ...delta(5, 'dup'), aggregateId: 'run-oo' })).toBe(true);
    expect(streamBuffer.ingest({ ...delta(4, 'stale'), aggregateId: 'run-oo' })).toBe(true);
    expect(streamBuffer.ingest({ ...delta(6, 'b'), aggregateId: 'run-oo' })).toBe(true);
    expect(streamBuffer.snapshot('run-oo').text).toBe('ab');
  });

  it('notifies subscribers and clears on terminal state', () => {
    streamBuffer.clear('run-s');
    const seen: string[] = [];
    const off = streamBuffer.subscribe('run-s', (s) => seen.push(s.text));
    streamBuffer.ingest(delta(1, '流'));
    streamBuffer.ingest(delta(2, '式'));
    off();
    streamBuffer.clear('run-s');
    expect(streamBuffer.snapshot('run-s').text).toBe('');
  });

  it('tracks droppedBytes from backpressure reporting', () => {
    streamBuffer.clear('run-d');
    const ok = streamBuffer.ingest({
      ...delta(1, 'ab'),
      aggregateId: 'run-d',
      payload: { text: 'ab', droppedBytes: 128 },
    });
    expect(ok).toBe(true);
    expect(streamBuffer.snapshot('run-d').droppedBytes).toBe(128);
  });
});
