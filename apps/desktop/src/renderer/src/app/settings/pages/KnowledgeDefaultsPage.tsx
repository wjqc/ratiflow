// S11 知识库默认策略：knowledge.settings.get/update（全局默认；项目覆盖在项目知识库页）。
import { useCallback, useEffect, useState } from 'react';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { rpc, rpcErrorMessage } from '../../../rpc/client';

interface KnowledgeSettings {
  defaultExcluded?: string[];
  maxFileBytes?: number;
  maxFilesPerSource?: number;
  chunkMaxChars?: number;
  secretScanFailClosed?: boolean;
  defaultResultLimit?: number;
  includeTestsByDefault?: boolean;
  contextBudgetBytes?: number;
  revision?: number;
}

export function KnowledgeDefaultsPage() {
  const [value, setValue] = useState<KnowledgeSettings | null>(null);
  const [draft, setDraft] = useState<KnowledgeSettings>({});
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [conflict, setConflict] = useState(false);

  const load = useCallback(async () => {
    setLoading(true); setError('');
    try {
      const result = await rpc<KnowledgeSettings>('knowledge.settings.get', {});
      setValue(result); setDraft(result);
    } catch (reason) { setError(rpcErrorMessage(reason)); }
    finally { setLoading(false); }
  }, []);
  useEffect(() => { void load(); }, [load]);

  const dirty = JSON.stringify(draft) !== JSON.stringify(value);
  const set = (key: keyof KnowledgeSettings, v: unknown) => setDraft({ ...draft, [key]: v });

  const save = async () => {
    setSaving(true); setError(''); setConflict(false);
    try {
      await rpc('knowledge.settings.update', {
        settings: draft, expectedRevision: value?.revision ?? 0,
      });
      await load();
    } catch (reason) {
      const msg = rpcErrorMessage(reason);
      setError(msg);
      if (/revision|冲突/i.test(msg)) setConflict(true);
    } finally { setSaving(false); }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="知识库默认策略" scope="全局"
        status={loading ? <StatusPill kind="checking" /> : dirty ? <StatusPill kind="pending" label="有未保存更改" /> : <StatusPill kind="ready" label="已保存" />}
        description="全局默认的扫描、分块、秘密扫描与检索策略。此处不添加项目来源——项目来源在项目知识库页管理。"
        actions={<button className="sg-btn sg-btn--primary" disabled={!dirty || saving} onClick={() => void save()}>{saving ? '保存中…' : '保存更改'}</button>}
      />
      {error ? <div className="sg-banner sg-banner--error" role="alert">{error}</div> : null}
      {conflict ? <div className="sg-banner sg-banner--error" role="alert">策略已被其他会话修改；刷新后重新编辑。</div> : null}
      <SettingsSection title="扫描边界" description="默认排除项避免已取代架构污染检索">
        <p className="sg-hint">固定排除：.git、node_modules、target、dist、docs/history/**、legacy/**</p>
        <label className="sg-set-field">
          <span className="sg-set-label">最大文件大小（MB）</span>
          <input type="number" min={1} max={64} value={Math.floor((draft.maxFileBytes ?? 2097152) / 1048576)}
            onChange={(e) => set('maxFileBytes', Number(e.target.value) * 1048576)} />
        </label>
        <label className="sg-set-field">
          <span className="sg-set-label">单来源文件上限</span>
          <input type="number" min={10} max={5000} value={draft.maxFilesPerSource ?? 500}
            onChange={(e) => set('maxFilesPerSource', Number(e.target.value))} />
        </label>
      </SettingsSection>
      <SettingsSection title="分块与秘密" description="秘密扫描失败时禁止内容进入模型（fail closed）">
        <label className="sg-set-field">
          <span className="sg-set-label">分块最大字符数</span>
          <input type="number" min={500} max={8000} value={draft.chunkMaxChars ?? 2000}
            onChange={(e) => set('chunkMaxChars', Number(e.target.value))} />
        </label>
        <label className="sg-set-field">
          <input type="checkbox" checked={draft.secretScanFailClosed !== false} onChange={(e) => set('secretScanFailClosed', e.target.checked)} />
          <span>秘密扫描 fail closed（命中秘密不返回原文）</span>
        </label>
      </SettingsSection>
      <SettingsSection title="检索与上下文预算">
        <label className="sg-set-field">
          <span className="sg-set-label">默认结果数</span>
          <input type="number" min={5} max={50} value={draft.defaultResultLimit ?? 20}
            onChange={(e) => set('defaultResultLimit', Number(e.target.value))} />
        </label>
        <label className="sg-set-field">
          <input type="checkbox" checked={draft.includeTestsByDefault === true} onChange={(e) => set('includeTestsByDefault', e.target.checked)} />
          <span>默认包含测试文件（默认排除，勾选后仍受排除理由标注）</span>
        </label>
        <label className="sg-set-field">
          <span className="sg-set-label">上下文预算（KB）</span>
          <input type="number" min={16} max={1024} value={Math.floor((draft.contextBudgetBytes ?? 65536) / 1024)}
            onChange={(e) => set('contextBudgetBytes', Number(e.target.value) * 1024)} />
        </label>
      </SettingsSection>
    </div>
  );
}
