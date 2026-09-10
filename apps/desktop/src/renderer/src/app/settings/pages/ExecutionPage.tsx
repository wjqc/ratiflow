// 执行与沙箱：executor.settings + executor.check 运行自检。改动即保存（无保存按钮）。
import { useCallback, useEffect, useRef, useState } from 'react';
import { rpc } from '../../../rpc/client';
import { SettingsPageHeader } from '../components/SettingsPageHeader';
import { SettingsRow, SettingsToggle } from '../components/SettingsRow';
import { SettingsSection } from '../components/SettingsSection';
import { StatusPill } from '../components/StatusPill';
import { IconZap } from '../../../components/Icons';
import { useAutoSave } from '../hooks/useAutoSave';

interface ExecutorSettings {
  mode?: string;
  memoryMB?: number;
  cpus?: number;
  timeoutSec?: number;
  networkOff?: boolean;
  revision: number;
}
interface CheckStep { name: string; status: string; detail: string }
interface SandboxCapability {
  backend: string;
  version: string;
  blockedReason: string;
  protectionScope?: {
    kind: string;
    writeScope: string;
    network: string;
  } | null;
}
interface CheckResponse { status: string; steps: CheckStep[]; sandbox?: SandboxCapability }

const MODE_HINT: Record<string, string> = {
  auto: '自动探测：内核沙箱（macOS Seatbelt / Linux Landlock）可用即优先，否则 Docker，均不可用则禁用（fail-closed）。',
  docker: '可选强隔离：一次性容器、禁网、资源限制、镜像钉扎（需本机 Docker 常驻）。',
  kernel_restricted: '推荐。内核沙箱（macOS Seatbelt / Linux Landlock）强制路径与网络边界；写仅限受管 worktree 与工件目录，读取类命令零 Docker 依赖。',
  safe_restricted: '本机白名单（非强隔离）；自动写/执行禁用。',
  disabled: '全部执行禁用。',
  unsafe_explicit: '不提供容器隔离；所有执行进入日志与证据标注。',
};

export function ExecutionPage() {
  const [draft, setDraft] = useState<ExecutorSettings>({ revision: 0 });
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const [check, setCheck] = useState<CheckResponse | null>(null);
  const revisionRef = useRef(0);
  const draftRef = useRef(draft);
  draftRef.current = draft;

  const load = useCallback(async () => {
    setLoading(true); setError(null);
    try {
      const res = await rpc<ExecutorSettings>('executor.settings.get', {});
      revisionRef.current = typeof res.revision === 'number' ? res.revision : 0;
      setDraft(res);
    } catch (e) {
      setError(e instanceof Error ? e.message : '执行设置加载失败');
    } finally { setLoading(false); }
  }, []);
  useEffect(() => { void load(); }, [load]);

  const persist = async () => {
    const current = draftRef.current;
    const expected = revisionRef.current;
    try {
      const res = await rpc<{ revision: number }>('executor.settings.update', {
        settings: { ...current, unsafeConfirmed: current.mode === 'unsafe_explicit' },
        expectedRevision: expected,
      });
      revisionRef.current = typeof res.revision === 'number' ? res.revision : expected + 1;
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : '保存失败');
    }
  };

  const { schedule } = useAutoSave(persist);

  const set = (key: keyof ExecutorSettings, v: unknown, immediate = false) => {
    setDraft((d) => ({ ...d, [key]: v }));
    schedule(immediate ? 0 : 400);
  };

  const runCheck = async () => {
    setChecking(true); setError(null);
    try {
      setCheck(await rpc<CheckResponse>('executor.check', {}));
    } catch (e) {
      setError(e instanceof Error ? e.message : '自检失败');
    } finally { setChecking(false); }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="执行与沙箱"
        description="Agent 命令执行模式与资源限制。"
      />

      {error ? <div className="sg-banner sg-banner--error" role="alert">操作失败：{error}</div> : null}

      <SettingsSection title="模式与资源">
        <div className="sg-setting-list">
          <SettingsRow title="执行模式" htmlFor="ex-mode" description={MODE_HINT[draft.mode ?? 'auto']}>
            <select
              id="ex-mode"
              className="sg-select"
              value={draft.mode ?? 'auto'}
              onChange={(e) => {
                const next = e.target.value;
                if (next === 'unsafe_explicit') {
                  const ok = window.confirm('不安全模式不提供容器级隔离。确定切换到显式不安全模式吗？');
                  if (!ok) return;
                }
                // auto = 清空显式模式，跟随探测（内核沙箱优先）；core 以 null 表示未固定。
                set('mode', next === 'auto' ? null : next, true);
              }}
            >
              <option value="auto">自动探测（内核沙箱优先）</option>
              <option value="docker">Docker（可选强隔离）</option>
              <option value="kernel_restricted">内核沙箱（推荐）</option>
              <option value="safe_restricted">本机白名单（非强隔离）</option>
              <option value="disabled">禁用</option>
              <option value="unsafe_explicit">显式不安全（需确认）</option>
            </select>
          </SettingsRow>
          {draft.mode === 'safe_restricted' ? (
            <div className="sg-banner sg-banner--warning sg-set-item-banner" role="alert">
              本机白名单（非强隔离）：仅按命令名白名单执行，无内核强制隔离。
            </div>
          ) : null}
          {draft.mode === 'unsafe_explicit' ? (
            <div className="sg-banner sg-banner--warning sg-set-item-banner" role="alert">
              不安全模式不提供容器级隔离；已二次确认，改动即时生效。
            </div>
          ) : null}
          <SettingsRow title="内存（MB）" htmlFor="ex-mem">
            <input id="ex-mem" className="sg-input" type="number" min={128} max={8192}
              value={draft.memoryMB ?? 256} onChange={(e) => set('memoryMB', Number(e.target.value))} />
          </SettingsRow>
          <SettingsRow title="CPU" htmlFor="ex-cpu">
            <input id="ex-cpu" className="sg-input" type="number" min={1} max={8} step={0.5}
              value={draft.cpus ?? 1} onChange={(e) => set('cpus', Number(e.target.value))} />
          </SettingsRow>
          <SettingsRow title="超时（秒）" htmlFor="ex-timeout">
            <input id="ex-timeout" className="sg-input" type="number" min={10} max={3600}
              value={draft.timeoutSec ?? 120} onChange={(e) => set('timeoutSec', Number(e.target.value))} />
          </SettingsRow>
          <SettingsRow title="默认禁网" description="工具执行默认切断网络出口">
            <SettingsToggle label="默认禁网" checked={draft.networkOff !== false}
              onChange={(checked) => set('networkOff', checked, true)} />
          </SettingsRow>
        </div>
      </SettingsSection>

      <SettingsSection title="运行自检" description="内核沙箱验证（允许面读/敏感面拒/禁网）或一次性容器全流程">
        <div className="sg-card sg-set-form">
          <button className="sg-btn" disabled={checking} onClick={() => void runCheck()} aria-live="polite">
            <IconZap size={14} />
            {checking ? '自检运行中…' : '运行自检'}
          </button>
          {check ? (
            <>
              {check.sandbox ? (
                <div className="sg-hint" style={{ marginTop: 8 }} role="note">
                  沙箱后端：{check.sandbox.backend}
                  {check.sandbox.version ? `（${check.sandbox.version}）` : ''}
                  {check.sandbox.blockedReason ? ` — 阻塞：${check.sandbox.blockedReason}` : ''}
                  {check.sandbox.protectionScope ? `；实际保护范围：${check.sandbox.protectionScope.kind}；写限 ${check.sandbox.protectionScope.writeScope}` : ''}
                </div>
              ) : null}
              <table className="sg-table" style={{ marginTop: 8 }} aria-label="自检步骤">
                <thead><tr><th>步骤</th><th>状态</th><th>详情</th></tr></thead>
                <tbody>
                  {check.steps.map((s) => (
                    <tr key={s.name}>
                      <td>{s.name}</td>
                      <td><StatusPill kind={s.status === 'passed' ? 'ready' : 'error'} label={s.status} /></td>
                      <td className="sg-muted">{s.detail.slice(0, 80)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </>
          ) : null}
        </div>
      </SettingsSection>
    </div>
  );
}
