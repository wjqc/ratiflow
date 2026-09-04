// S12 待确认区（ADR-032 M4）：候选沉淀列表 + 候选 Drawer（接受前可编辑；拒绝两步确认）。
// 候选本身永不被检索注入；接受才写正式 active 记忆（MEM-020）。
import { useCallback, useEffect, useState } from 'react';
import { rpc } from '../../../rpc/client';
import { useTwoStepConfirm } from './useTwoStepConfirm';
import { MemoryEditor, type MemoryDraft } from './MemoryEditor';
import { StatusPill } from './StatusPill';
import { MEMORY_KIND_LABEL, type MemoryKind } from '../types';
import { rpcErrText } from '../../../lib/rpcError';

interface CandidateItem {
  candidateId: string;
  jobId: string;
  kind: MemoryKind;
  title: string;
  summary: string;
  status: string;
  createdAt: string;
  runId: string;
  body?: string;
  bodyError?: string;
}

function uuid(): string {
  return typeof crypto.randomUUID === 'function'
    ? crypto.randomUUID()
    : `k-${Math.random().toString(36).slice(2)}${Date.now()}`;
}

export function MemoryCandidates({
  projectId,
  refreshKey,
  onChanged,
}: {
  projectId: string;
  refreshKey: number;
  onChanged: () => void;
}) {
  const [items, setItems] = useState<CandidateItem[] | null>(null);
  const [editing, setEditing] = useState<CandidateItem | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pendingId, requestConfirm] = useTwoStepConfirm();

  const load = useCallback(async () => {
    try {
      const res = await rpc<{ items: CandidateItem[] }>('memory.candidateList', { projectId, limit: 50 });
      setItems(res.items ?? []);
    } catch {
      setItems([]);
    }
  }, [projectId]);

  useEffect(() => {
    if (projectId) void load();
  }, [projectId, refreshKey, load]);

  const reject = (candidate: CandidateItem) => {
    setBusy(true);
    setError(null);
    rpc('memory.candidateDecide', {
      projectId, candidateId: candidate.candidateId, decision: 'reject', idempotencyKey: uuid(),
    })
      .then(() => {
        setEditing(null);
        void load();
        onChanged();
      })
      .catch((e) => setError(rpcErrText(e) || '拒绝失败'))
      .finally(() => setBusy(false));
  };

  const accept = (candidate: CandidateItem, draft: MemoryDraft) => {
    setBusy(true);
    setError(null);
    rpc('memory.candidateDecide', {
      projectId,
      candidateId: candidate.candidateId,
      decision: 'accept',
      editedContent: draft.body,
      editedTitle: draft.title,
      idempotencyKey: uuid(),
    })
      .then(() => {
        setEditing(null);
        void load();
        onChanged();
      })
      .catch((e) => setError(rpcErrText(e) || '接受失败'))
      .finally(() => setBusy(false));
  };

  if (items === null || items.length === 0) return null;

  return (
    <div className="sg-memory-candidates" aria-label="待确认候选">
      <div className="sg-memory-candidates-head">
        <StatusPill kind="pending" label="待确认候选" />
        <span className="sg-hint">
          来自 Agent Run 的候选结论需要你裁决；候选在被接受前不会进入任何 Run 上下文。
        </span>
      </div>
      {error ? (
        <div role="alert" className="sg-memory-banner sg-memory-banner--error">{error}</div>
      ) : null}
      <ul className="sg-memory-list">
        {items.map((c) => (
          <li key={c.candidateId} className="sg-memory-row sg-memory-cand-row">
            <span className="sg-memory-row-main">
              <span className="sg-memory-row-title">{c.title}</span>
              <span className="sg-memory-row-meta">
                <span className="sg-memory-chip">{MEMORY_KIND_LABEL[c.kind] ?? c.kind}</span>
                <span>来自 Run {c.runId.slice(0, 12)}…</span>
                {c.summary ? <span>· {c.summary}</span> : null}
              </span>
            </span>
            <span className="sg-memory-cand-actions">
              <button type="button" className="sg-btn" disabled={busy} onClick={() => setEditing(c)}>
                接受并编辑…
              </button>
              <button
                type="button"
                className="sg-btn sg-memory-danger"
                disabled={busy}
                onClick={() =>
                  requestConfirm(c.candidateId, () => reject(c))
                }
              >
                {pendingId === c.candidateId ? '再次点击确认拒绝' : '拒绝'}
              </button>
            </span>
          </li>
        ))}
      </ul>

      {editing ? (
        <>
          <div className="sg-memory-drawer-mask" onClick={() => setEditing(null)} aria-hidden="true" />
          <aside className="sg-memory-drawer" role="dialog" aria-modal="true" aria-labelledby="sg-cand-drawer-title">
            <div className="sg-memory-drawer-head">
              <h2 id="sg-cand-drawer-title" tabIndex={0} className="sg-memory-drawer-title">
                接受候选（可编辑）
              </h2>
              <StatusPill kind="pending" label="待确认" />
              <button type="button" className="sg-btn" aria-label="关闭候选详情" onClick={() => setEditing(null)}>
                ✕
              </button>
            </div>
            <div className="sg-memory-drawer-body">
              <p className="sg-hint">
                接受后写入正式已确认记忆并可能进入新 Run 上下文；来源将记录为 Run {editing.runId.slice(0, 12)}…。
              </p>
              <MemoryEditor
                mode="create"
                initial={{
                  title: editing.title,
                  kind: editing.kind,
                  body: editing.body ?? '',
                  tags: '',
                }}
                submitting={busy}
                conflict={false}
                submitting_label="接受（写入已确认记忆）"
                onSubmit={(draft) => accept(editing, draft)}
                onCancel={() => setEditing(null)}
              />
            </div>
          </aside>
        </>
      ) : null}
    </div>
  );
}
