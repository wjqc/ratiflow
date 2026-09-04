// S12 记忆抽屉：预览/编辑/版本/来源/使用记录 + 置顶/归档/恢复/两步清除。
// 焦点：打开进入标题，Esc 关闭；purge 走 purgePreview 一次性 token（禁用 window.confirm）。
import { useEffect, useRef, useState } from 'react';
import { rpc } from '../../../rpc/client';
import { renderMarkdown } from '../../../lib/markdown';
import { rpcErrText, rpcErrToken } from '../../../lib/rpcError';
import { SettingsSection } from './SettingsSection';
import { StatusPill } from './StatusPill';
import { MemoryEditor, EMPTY_DRAFT, type MemoryDraft } from './MemoryEditor';
import { MemorySourceRefs } from './MemorySourceRefs';
import { formatTime } from './MemoryList';
import {
  MEMORY_KIND_LABEL,
  MEMORY_STATUS_LABEL,
  type MemoryDetail,
  type MemoryPurgePreviewInfo,
} from '../types';

function uuid(): string {
  return typeof crypto.randomUUID === 'function'
    ? crypto.randomUUID()
    : `k-${Math.random().toString(36).slice(2)}${Date.now()}`;
}

function errCode(e: unknown): string {
  return rpcErrToken(e) ?? '';
}

function errText(e: unknown): string {
  return rpcErrText(e);
}

export function MemoryDrawer({
  projectId,
  memoryId,
  createMode,
  readOnly,
  onClose,
  onChanged,
}: {
  projectId: string;
  memoryId: string | null;
  createMode: boolean;
  readOnly: boolean;
  onClose: () => void;
  onChanged: () => void;
}) {
  const [detail, setDetail] = useState<MemoryDetail | null>(null);
  const [loading, setLoading] = useState(!createMode);
  const [mode, setMode] = useState<'view' | 'edit' | 'create'>(createMode ? 'create' : 'view');
  const [submitting, setSubmitting] = useState(false);
  const [conflict, setConflict] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [purgePanel, setPurgePanel] = useState<MemoryPurgePreviewInfo | null>(null);
  const [purging, setPurging] = useState(false);
  const titleRef = useRef<HTMLHeadingElement>(null);

  useEffect(() => {
    titleRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const load = async () => {
    if (!memoryId) return;
    setLoading(true);
    setError(null);
    try {
      setDetail(await rpc<MemoryDetail>('memory.get', { projectId, memoryId }));
    } catch (e) {
      setError(errText(e) || '加载失败');
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    if (!createMode) void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [memoryId, projectId]);

  const mutate = async (fn: () => Promise<unknown>): Promise<boolean> => {
    setSubmitting(true);
    setError(null);
    setNotice(null);
    try {
      await fn();
      await load();
      onChanged();
      return true;
    } catch (e) {
      setError(errText(e) || '操作失败');
      return false;
    } finally {
      setSubmitting(false);
    }
  };

  const saveCreate = (draft: MemoryDraft) => {
    void mutate(() =>
      rpc('memory.create', {
        projectId,
        title: draft.title,
        kind: draft.kind,
        body: draft.body,
        tags: draft.tags ? draft.tags.split(/[,，]/).map((t) => t.trim()).filter(Boolean) : [],
        idempotencyKey: uuid(),
      }),
    ).then((ok) => {
      if (ok) {
        onChanged();
        onClose();
      }
    });
  };

  // 冲突精度：保存失败且 code=memory_conflict 时保留草稿。
  const onSaveEdit = (draft: MemoryDraft) => {
    if (!detail) return;
    setSubmitting(true);
    setError(null);
    setNotice(null);
    rpc('memory.update', {
      projectId,
      memoryId: detail.id,
      title: draft.title,
      body: draft.body,
      tags: draft.tags ? draft.tags.split(/[,，]/).map((t) => t.trim()).filter(Boolean) : [],
      expectedRevision: detail.revision,
      idempotencyKey: uuid(),
    })
      .then(async () => {
        setConflict(false);
        setMode('view');
        setNotice('已保存为新版本');
        await load();
        onChanged();
      })
      .catch((e) => {
        if (errCode(e) === 'memory_conflict') {
          setConflict(true); // 保留草稿（MemoryEditor 内部 state 未卸载）
        } else {
          setError(errText(e) || '保存失败');
        }
      })
      .finally(() => setSubmitting(false));
  };

  const runPurge = () => {
    if (!purgePanel?.confirmationToken || !detail) return;
    setPurging(true);
    rpc('memory.purge', {
      projectId,
      memoryId: detail.id,
      expectedRevision: detail.revision,
      confirmationToken: purgePanel.confirmationToken,
      idempotencyKey: uuid(),
    })
      .then(async () => {
        setPurgePanel(null);
        setNotice('正文已清除，仅保留审计墓碑');
        await load();
        onChanged();
      })
      .catch((e) => setError(errText(e) || '清除失败'))
      .finally(() => setPurging(false));
  };

  const heading = createMode ? '新建记忆' : detail?.title || detail?.slug || '记忆详情';

  return (
    <>
      <div className="sg-memory-drawer-mask" onClick={onClose} aria-hidden="true" />
      <aside
        className="sg-memory-drawer"
        role="dialog"
        aria-modal="true"
        aria-labelledby="sg-memory-drawer-title"
      >
        <div className="sg-memory-drawer-head">
          <h2 id="sg-memory-drawer-title" tabIndex={-1} ref={titleRef} className="sg-memory-drawer-title">
            {heading}
          </h2>
          {detail ? (
            <StatusPill kind={detail.status === 'active' ? 'ready' : detail.status === 'conflicted' ? 'error' : 'readonly'} label={MEMORY_STATUS_LABEL[detail.status]} />
          ) : null}
          <button type="button" className="sg-btn" aria-label="关闭记忆详情" onClick={onClose}>
            ✕
          </button>
        </div>

        <div className="sg-memory-drawer-body">
          {loading ? <p className="sg-hint">加载中…</p> : null}
          {error ? (
            <div role="alert" className="sg-memory-banner sg-memory-banner--error">
              {error}
              {error && errCode({ message: error }) === 'memory_secret_detected' ? (
                <p className="sg-hint">命中高风险秘密的正文不会被写入，也不会回显；请移除秘密后重试。</p>
              ) : null}
            </div>
          ) : null}
          {notice ? <div className="sg-memory-banner sg-memory-banner--ok">{notice}</div> : null}

          {mode === 'create' ? (
            <MemoryEditor
              mode="create"
              initial={EMPTY_DRAFT}
              submitting={submitting}
              conflict={false}
              submitting_label="创建（直接生效）"
              onSubmit={saveCreate}
              onCancel={onClose}
            />
          ) : null}

          {!createMode && detail ? (
            <>
              {detail.status === 'conflicted' ? (
                <div role="alert" className="sg-memory-banner sg-memory-banner--warn">
                  同主题存在相互矛盾的记忆，已停止自动注入；请修订或归档其一以解除冲突。
                </div>
              ) : null}
              {detail.status === 'purged' ? (
                <div className="sg-memory-banner sg-memory-banner--warn">
                  正文已清除（{detail.purgedAt ? formatTime(detail.purgedAt) : '时间未知'}），仅保留不含正文的审计墓碑。
                  旧备份与共享对象中的同内容不受本次清除影响。
                </div>
              ) : null}

              {mode === 'edit' ? (
                <MemoryEditor
                  mode="edit"
                  initial={{
                    title: detail.title,
                    kind: detail.kind,
                    body: detail.body ?? '',
                    tags: detail.tags.join(', '),
                  }}
                  submitting={submitting}
                  conflict={conflict}
                  onSubmit={onSaveEdit}
                  onCancel={() => {
                    setConflict(false);
                    setMode('view');
                  }}
                />
              ) : (
                <>
                  <div className="sg-memory-meta">
                    <span className="sg-memory-chip">{MEMORY_KIND_LABEL[detail.kind]}</span>
                    <span className="sg-hint">
                      slug {detail.slug} · v{detail.revisionNo} ·{' '}
                      {detail.contentSha256 ? `${detail.contentSha256.slice(0, 12)}…` : ''}
                    </span>
                  </div>
                  {detail.bodyState === 'purged' ? (
                    <p className="sg-memory-purged-body">正文已清除</p>
                  ) : (
                    <div
                      className="sg-memory-preview sg-markdown"
                      // 正文渲染前已转义原生 HTML（lib/markdown），记忆是上下文数据不是指令。
                      dangerouslySetInnerHTML={{ __html: renderMarkdown(detail.body ?? '') }}
                    />
                  )}
                  {detail.tags.length > 0 ? (
                    <p className="sg-memory-tags">
                      {detail.tags.map((t) => (
                        <span key={t} className="sg-memory-chip">
                          #{t}
                        </span>
                      ))}
                    </p>
                  ) : null}
                </>
              )}

              {!readOnly ? (
                <div className="sg-memory-actions">
                  {mode === 'view' && detail.status !== 'purged' ? (
                    <button type="button" className="sg-btn" onClick={() => setMode('edit')}>
                      编辑
                    </button>
                  ) : null}
                  <button
                    type="button"
                    className="sg-btn"
                    disabled={submitting || detail.status === 'purged'}
                    onClick={() =>
                      void mutate(() =>
                        rpc('memory.pin', {
                          projectId,
                          memoryId: detail.id,
                          pinned: !detail.pinned,
                          expectedRevision: detail.revision,
                          idempotencyKey: uuid(),
                        }),
                      )
                    }
                  >
                    {detail.pinned ? '取消置顶' : '置顶'}
                  </button>
                  {detail.status === 'archived' || detail.status === 'conflicted' ? (
                    <button
                      type="button"
                      className="sg-btn"
                      disabled={submitting}
                      onClick={() =>
                        void mutate(() =>
                          rpc('memory.restore', {
                            projectId,
                            memoryId: detail.id,
                            expectedRevision: detail.revision,
                            idempotencyKey: uuid(),
                          }),
                        )
                      }
                    >
                      恢复
                    </button>
                  ) : null}
                  {detail.status !== 'archived' && detail.status !== 'purged' ? (
                    <button
                      type="button"
                      className="sg-btn"
                      disabled={submitting}
                      onClick={() =>
                        void mutate(() =>
                          rpc('memory.archive', {
                            projectId,
                            memoryId: detail.id,
                            expectedRevision: detail.revision,
                            idempotencyKey: uuid(),
                          }),
                        )
                      }
                    >
                      归档
                    </button>
                  ) : null}
                  {detail.status !== 'purged' ? (
                    <button
                      type="button"
                      className="sg-btn sg-memory-danger"
                      disabled={submitting}
                      onClick={() => {
                        setError(null);
                        rpc<MemoryPurgePreviewInfo>('memory.purgePreview', {
                          projectId,
                          memoryId: detail.id,
                        })
                          .then(setPurgePanel)
                          .catch((e) => setError(errText(e) || '预览失败'));
                      }}
                    >
                      清除内容…
                    </button>
                  ) : null}
                </div>
              ) : (
                <p className="sg-hint">当前项目已归档或功能关闭：可浏览，不可修改。</p>
              )}

              {purgePanel ? (
                <SettingsSection title="清除影响预览" description="清除不等于从所有备份或其他领域中抹除同一内容。">
                  <ul className="sg-memory-purge-facts">
                    <li>共享引用：知识 {purgePanel.objectRefs.sharedWithKnowledge ?? 0} · 工件/证据 {purgePanel.objectRefs.sharedWithArtifacts ?? 0} · 附件 {purgePanel.objectRefs.sharedWithAttachments ?? 0}</li>
                    <li>曾被 {purgePanel.manifestRefs.count} 个 Context Manifest 使用</li>
                    <li>可能残留于 {purgePanel.backupRefs.likelyContained} 份历史备份</li>
                    <li>{purgePanel.backupRefs.note}</li>
                  </ul>
                  {purgePanel.blockers.length > 0 ? (
                    <div role="alert" className="sg-memory-banner sg-memory-banner--warn">
                      {purgePanel.blockers.map((b) => (
                        <p key={b.code}>{b.detail}</p>
                      ))}
                      本次仅能解除记忆侧引用。
                    </div>
                  ) : (
                    <button type="button" className="sg-btn sg-memory-danger" disabled={purging} onClick={runPurge}>
                      {purging ? '清除中…' : '确认清除（两步确认）'}
                    </button>
                  )}
                  <div className="sg-memory-editor-actions">
                    <button type="button" className="sg-btn" onClick={() => setPurgePanel(null)}>
                      取消清除
                    </button>
                  </div>
                </SettingsSection>
              ) : null}

              <SettingsSection title="版本历史">
                <ul className="sg-memory-revisions">
                  {detail.revisions.map((r) => (
                    <li key={r.revisionNo}>
                      v{r.revisionNo} · {r.title} · {formatTime(r.createdAt)}
                      {r.purgedAt ? ' · 已清除' : ''}
                      <span className="sg-hint"> {r.contentSha256.slice(0, 12)}…</span>
                    </li>
                  ))}
                </ul>
              </SettingsSection>

              <SettingsSection title="来源">
                <MemorySourceRefs sources={detail.sources} />
              </SettingsSection>

              <SettingsSection title="最近使用">
                {detail.usage.length === 0 ? (
                  <p className="sg-hint">尚无 Run 采用记录</p>
                ) : (
                  <ul className="sg-memory-revisions">
                    {detail.usage.map((u, i) => (
                      <li key={`${u.manifestId}-${i}`}>
                        manifest {u.manifestId} · v{u.revisionNo} · {formatTime(u.selectedAt)}
                      </li>
                    ))}
                  </ul>
                )}
              </SettingsSection>
            </>
          ) : null}
        </div>
      </aside>
    </>
  );
}
