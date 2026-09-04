// S12 记忆编辑器：标题/类型/正文(Markdown)/标签。新建可直接成为 active（用户确认即生效）；
// 编辑时类型锁定；revision conflict 时保留本地草稿并给出重载入口（§3.6）。
import { useState, type FormEvent } from 'react';
import { MEMORY_KIND_LABEL, type MemoryKind } from '../types';

export interface MemoryDraft {
  title: string;
  kind: MemoryKind;
  body: string;
  tags: string;
}

export const EMPTY_DRAFT: MemoryDraft = { title: '', kind: 'lesson', body: '', tags: '' };

export function MemoryEditor({
  mode,
  initial,
  submitting,
  conflict,
  submitting_label = '保存',
  onSubmit,
  onCancel,
}: {
  mode: 'create' | 'edit';
  initial: MemoryDraft;
  submitting: boolean;
  conflict: boolean;
  submitting_label?: string;
  onSubmit: (draft: MemoryDraft) => void;
  onCancel: () => void;
}) {
  const [draft, setDraft] = useState<MemoryDraft>(initial);
  const set = (patch: Partial<MemoryDraft>) => setDraft((d) => ({ ...d, ...patch }));

  const submit = (e: FormEvent) => {
    e.preventDefault();
    onSubmit({
      ...draft,
      title: draft.title.trim(),
      body: draft.body,
      tags: draft.tags.trim(),
    });
  };

  return (
    <form className="sg-memory-editor" onSubmit={submit} aria-label={mode === 'create' ? '新建记忆' : '编辑记忆'}>
      {conflict ? (
        <div role="alert" className="sg-memory-banner sg-memory-banner--warn">
          该记忆已被其他会话修改（revision 冲突）。下方草稿已保留，可复制后
          <button type="button" className="sg-btn" onClick={onCancel}>
            重新加载服务器版本
          </button>
          再合并。
        </div>
      ) : null}
      <label className="sg-field">
        <span>标题</span>
        <input
          value={draft.title}
          onChange={(e) => set({ title: e.target.value })}
          required
          maxLength={120}
          placeholder="一句话结论，如：部署前必须检查健康端点"
        />
      </label>
      <label className="sg-field">
        <span>类型</span>
        <select
          value={draft.kind}
          disabled={mode === 'edit'}
          onChange={(e) => set({ kind: e.target.value as MemoryKind })}
        >
          {(Object.keys(MEMORY_KIND_LABEL) as MemoryKind[]).map((k) => (
            <option key={k} value={k}>
              {MEMORY_KIND_LABEL[k]}
            </option>
          ))}
        </select>
        {mode === 'edit' ? <span className="sg-hint">类型创建后固定</span> : null}
      </label>
      <label className="sg-field">
        <span>正文（Markdown）</span>
        <textarea
          rows={8}
          value={draft.body}
          onChange={(e) => set({ body: e.target.value })}
          required
          placeholder="会被 Secret 扫描；命中高风险秘密将被拒绝写入"
        />
      </label>
      <label className="sg-field">
        <span>标签（逗号分隔，可选）</span>
        <input value={draft.tags} onChange={(e) => set({ tags: e.target.value })} placeholder="deploy, storage" />
      </label>
      <div className="sg-memory-editor-actions">
        <button type="submit" className="sg-btn sg-btn--primary" disabled={submitting}>
          {submitting ? '保存中…' : submitting_label}
        </button>
        <button type="button" className="sg-btn" onClick={onCancel} disabled={submitting}>
          取消
        </button>
      </div>
    </form>
  );
}
