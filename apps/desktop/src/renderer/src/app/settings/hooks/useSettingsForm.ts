// 设置页共用表单状态：dirty 跟踪 + revision 乐观锁 + 保存/错误/冲突（页面设计 §4.1）。
import { useCallback, useEffect, useRef, useState } from 'react';
import { rpc, rpcErrorMessage } from '../../../rpc/client';

export interface SettingsFormState<T> {
  value: T;
  revision: number;
}

export function useSettingsForm<T>(method: string, params: Record<string, unknown>, initial: T, revOf: (v: T) => number) {
  const [server, setServer] = useState<SettingsFormState<T> | null>(null);
  const [draft, setDraft] = useState<T>(initial);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [conflict, setConflict] = useState(false);
  const [savedAt, setSavedAt] = useState('');
  const loadedOnce = useRef(false);

  const load = useCallback(async () => {
    setLoading(true);
    setError('');
    try {
      const result = await rpc<T>(method, params);
      setServer({ value: result, revision: revOf(result) });
      if (!loadedOnce.current) {
        setDraft(result);
        loadedOnce.current = true;
      }
    } catch (reason) {
      setError(rpcErrorMessage(reason));
    } finally {
      setLoading(false);
    }
    // params 串行化避免对象身份触发重查
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [method, JSON.stringify(params)]);

  useEffect(() => { void load(); }, [load]);

  const dirty = server !== null && JSON.stringify(draft) !== JSON.stringify(server.value);

  const save = useCallback(async (updateMethod: string, buildPatches: (d: T, expected: number) => Record<string, unknown>) => {
    if (!server) return false;
    setSaving(true);
    setError('');
    setConflict(false);
    try {
      await rpc(updateMethod, buildPatches(draft, server.revision));
      setSavedAt(new Date().toLocaleTimeString('zh-CN', { hour12: false }));
      await load();
      setDraft(server.value);
      return true;
    } catch (reason) {
      const msg = rpcErrorMessage(reason);
      setError(msg);
      if (/revision|冲突|conflict/i.test(msg)) {
        setConflict(true);
      }
      return false;
    } finally {
      setSaving(false);
    }
  }, [server, draft, load]);

  return { server, draft, setDraft, dirty, loading, saving, error, conflict, savedAt, save, reload: load };
}

/** 测试连接状态（Testing 独立于保存）。 */
export function useConnectionTest() {
  const [testing, setTesting] = useState(false);
  const [steps, setSteps] = useState<Array<{ name: string; status: string; errorCode: string | null; detail: unknown }>>([]);
  const [overall, setOverall] = useState<'idle' | 'running' | 'ready' | 'degraded' | 'error' | 'action_required'>('idle');

  const run = useCallback(async (method: string, params: Record<string, unknown>) => {
    setTesting(true);
    setSteps([]);
    setOverall('running');
    try {
      const report = await rpc<{ status: string; steps: typeof steps }>(method, params);
      setSteps(report.steps ?? []);
      setOverall((report.status as typeof overall) ?? 'error');
    } catch (reason) {
      setSteps([{ name: 'request', status: 'failed', errorCode: 'RPC', detail: rpcErrorMessage(reason) }]);
      setOverall('error');
    } finally {
      setTesting(false);
    }
  }, []);

  return { testing, steps, overall, run };
}
