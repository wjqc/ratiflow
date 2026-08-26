// 知识库默认策略：knowledge.settings.get/update（全局默认；项目来源在项目知识库页管理）。
import { useCallback, useEffect, useState, type FormEvent } from 'react';
import { rpc } from '../../../rpc/client';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { IconCheck } from '../../../components/Icons';

interface KnowledgeSettings {
  maxFileBytes?: number;
  maxFilesPerSource?: number;
  chunkMaxChars?: number;
  secretScanFailClosed?: boolean;
  defaultResultLimit?: number;
  includeTestsByDefault?: boolean;
  contextBudgetBytes?: number;
  instructionFileNames?: string[];
  maxInstructionBytes?: number;
  autoCompactThresholdTokens?: number;
  compactionKeepTurns?: number;
  revision?: number;
}

const EMPTY: Required<Omit<KnowledgeSettings, 'revision'>> = {
  maxFileBytes: 2097152,
  maxFilesPerSource: 500,
  chunkMaxChars: 2000,
  secretScanFailClosed: true,
  defaultResultLimit: 20,
  includeTestsByDefault: false,
  contextBudgetBytes: 65536,
  instructionFileNames: ['SixGates.md', 'AGENTS.md'],
  maxInstructionBytes: 32768,
  autoCompactThresholdTokens: 24000,
  compactionKeepTurns: 2,
};

export function KnowledgeDefaultsPage() {
  const [value, setValue] = useState<KnowledgeSettings | null>(null);
  const [draft, setDraft] = useState<KnowledgeSettings>(EMPTY);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);

  const load = useCallback(async () => {
    setLoading(true); setError(null);
    try {
      const res = await rpc<KnowledgeSettings>('knowledge.settings.get', {});
      const { revision: resRevision, ...rest } = res;
      const merged = { ...EMPTY, ...rest };
      setValue(merged);
      setDraft(merged);
      setRevision(typeof resRevision === 'number' ? resRevision : 0);
    } catch (e) {
      setError(e instanceof Error ? e.message : '知识策略加载失败');
    } finally { setLoading(false); }
  }, []);

  useEffect(() => { void load(); }, [load]);

  const dirty = JSON.stringify(draft) !== JSON.stringify(value);
  const set = (key: keyof KnowledgeSettings, v: unknown) => setDraft((d) => ({ ...d, [key]: v }));

  const save = async (e: FormEvent) => {
    e.preventDefault();
    setSaving(true); setError(null); setNotice(null);
    try {
      const res = await rpc<{ revision: number }>('knowledge.settings.update', {
        settings: { ...draft, revision: revision + 1 },
        expectedRevision: revision,
      });
      setRevision(res.revision);
      setNotice('知识默认策略已保存');
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : '保存失败');
    } finally { setSaving(false); }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="知识库默认策略"
        scope="全局"
        status={loading ? <StatusPill kind="checking" /> : dirty ? <StatusPill kind="pending" label="有未保存更改" /> : <StatusPill kind="ready" label="已保存" />}
        description="扫描、分块、秘密扫描与检索的全局默认。此处不添加项目来源——项目来源在项目知识库页管理。"
      />

      {error ? <div className="sg-banner sg-banner--error" role="alert">操作失败：{error}</div> : null}
      {notice ? <div className="sg-banner sg-banner--info" role="status">{notice}</div> : null}

      <form onSubmit={save}>
        <SettingsSection title="扫描边界" description="固定排除 .git、node_modules、target、dist、docs/history/**、legacy/**">
          <div className="sg-card sg-set-form">
            <div className="sg-form-grid--2">
              <div className="sg-field">
                <label htmlFor="kb-maxfile">最大文件大小（MB）</label>
                <input id="kb-maxfile" className="sg-input" type="number" min={1} max={64}
                  value={Math.floor((draft.maxFileBytes ?? EMPTY.maxFileBytes) / 1048576)}
                  onChange={(e) => set('maxFileBytes', Number(e.target.value) * 1048576)} />
              </div>
              <div className="sg-field">
                <label htmlFor="kb-maxfiles">单来源文件上限</label>
                <input id="kb-maxfiles" className="sg-input" type="number" min={10} max={5000}
                  value={draft.maxFilesPerSource ?? EMPTY.maxFilesPerSource}
                  onChange={(e) => set('maxFilesPerSource', Number(e.target.value))} />
              </div>
            </div>
          </div>
        </SettingsSection>

        <SettingsSection title="分块与秘密" description="秘密命中时禁止内容进入模型（fail closed）">
          <div className="sg-card sg-set-form">
            <div className="sg-form-grid--2">
              <div className="sg-field">
                <label htmlFor="kb-chunk">分块最大字符数</label>
                <input id="kb-chunk" className="sg-input" type="number" min={500} max={8000}
                  value={draft.chunkMaxChars ?? EMPTY.chunkMaxChars}
                  onChange={(e) => set('chunkMaxChars', Number(e.target.value))} />
              </div>
              <div className="sg-field">
                <label htmlFor="kb-failclosed">秘密扫描</label>
                <label className="sg-row" style={{ gap: 6, cursor: 'pointer' }}>
                  <input id="kb-failclosed" type="checkbox" checked={draft.secretScanFailClosed !== false}
                    onChange={(e) => set('secretScanFailClosed', e.target.checked)} />
                  <span className="sg-hint" style={{ margin: 0 }}>fail closed（命中不返回原文）</span>
                </label>
              </div>
            </div>
          </div>
        </SettingsSection>

        <SettingsSection title="项目指令文件（F07）" description="Agent 启动时按 全局→项目根→docs/ 聚合注入；层内容过秘密扫描，超限截断。生效预览可在任意项目上调用 context.instructions。">
          <div className="sg-card sg-set-form">
            <div className="sg-form-grid--2">
              <div className="sg-field">
                <label htmlFor="kb-instrfiles">指令文件名（逗号分隔，按序探测）</label>
                <input id="kb-instrfiles" className="sg-input" type="text"
                  value={(draft.instructionFileNames ?? EMPTY.instructionFileNames).join(', ')}
                  onChange={(e) => set('instructionFileNames', e.target.value.split(',').map((x) => x.trim()).filter(Boolean))} />
              </div>
              <div className="sg-field">
                <label htmlFor="kb-instrbytes">指令总上限（KB）</label>
                <input id="kb-instrbytes" className="sg-input" type="number" min={1} max={256}
                  value={Math.floor((draft.maxInstructionBytes ?? EMPTY.maxInstructionBytes) / 1024)}
                  onChange={(e) => set('maxInstructionBytes', Number(e.target.value) * 1024)} />
              </div>
            </div>
          </div>
        </SettingsSection>

        <SettingsSection title="上下文压缩（F09）" description="输入估算超过阈值时自动压缩会话历史（保留冻结头与最近 K 轮工具往返，system 段不变）。">
          <div className="sg-card sg-set-form">
            <div className="sg-form-grid--2">
              <div className="sg-field">
                <label htmlFor="kb-compactthr">自动压缩阈值（tokens，估算 chars/4）</label>
                <input id="kb-compactthr" className="sg-input" type="number" min={1000} max={200000}
                  value={draft.autoCompactThresholdTokens ?? EMPTY.autoCompactThresholdTokens}
                  onChange={(e) => set('autoCompactThresholdTokens', Number(e.target.value))} />
              </div>
              <div className="sg-field">
                <label htmlFor="kb-compactkeep">保留最近工具轮数 K</label>
                <input id="kb-compactkeep" className="sg-input" type="number" min={0} max={10}
                  value={draft.compactionKeepTurns ?? EMPTY.compactionKeepTurns}
                  onChange={(e) => set('compactionKeepTurns', Number(e.target.value))} />
              </div>
            </div>
          </div>
        </SettingsSection>

        <SettingsSection title="检索与上下文预算">
          <div className="sg-card sg-set-form">
            <div className="sg-form-grid--2">
              <div className="sg-field">
                <label htmlFor="kb-limit">默认结果数</label>
                <input id="kb-limit" className="sg-input" type="number" min={5} max={50}
                  value={draft.defaultResultLimit ?? EMPTY.defaultResultLimit}
                  onChange={(e) => set('defaultResultLimit', Number(e.target.value))} />
              </div>
              <div className="sg-field">
                <label htmlFor="kb-budget">上下文预算（KB）</label>
                <input id="kb-budget" className="sg-input" type="number" min={16} max={1024}
                  value={Math.floor((draft.contextBudgetBytes ?? EMPTY.contextBudgetBytes) / 1024)}
                  onChange={(e) => set('contextBudgetBytes', Number(e.target.value) * 1024)} />
              </div>
            </div>
            <div className="sg-field">
              <label htmlFor="kb-tests" style={{ cursor: 'pointer' }}>
                <input id="kb-tests" type="checkbox" checked={draft.includeTestsByDefault === true}
                  onChange={(e) => set('includeTestsByDefault', e.target.checked)} />
                默认包含测试文件（默认排除）
              </label>
            </div>
            <div className="sg-row">
              <button type="submit" className="sg-btn sg-btn--primary" disabled={!dirty || saving}>
                <IconCheck size={14} />
                {saving ? '保存中…' : '保存更改'}
              </button>
              <span className="sg-hint">项目可在项目知识库页覆盖这些默认值。</span>
            </div>
          </div>
        </SettingsSection>
      </form>
    </div>
  );
}
