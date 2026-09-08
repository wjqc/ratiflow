// S41 备份与恢复：backup.create/list/verify/restore/delete 已真实接入（§5.10）。
import { useCallback, useEffect, useState } from 'react';
import { rpc } from '../../rpc/client';
import { formatDateTime } from '../../lib/format';
import type {
  BackupDeleteResult,
  BackupListResult,
  BackupRecord,
  BackupRestoreOutcome,
  CoreVersionInfo,
} from './types';
import { SettingsPageHeader } from './components/SettingsPageHeader';
import { SettingsSection } from './components/SettingsSection';
import { StatusPill } from './components/StatusPill';
import { IconDb, IconDownload, IconRefresh } from '../../components/Icons';
import { useTwoStepConfirm } from './components/useTwoStepConfirm';

function statusKind(status: string): 'ready' | 'pending' | 'error' {
  if (status === 'verified') return 'ready';
  if (status === 'corrupt' || status === 'incompatible') return 'error';
  return 'pending';
}

export function BackupPage() {
  const [version, setVersion] = useState<CoreVersionInfo | null>(null);
  const [dataDir, setDataDir] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [manifest, setManifest] = useState<BackupRecord | null>(null);
  const [history, setHistory] = useState<BackupRecord[]>([]);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [info, setInfo] = useState<string | null>(null);
  // 删除/恢复都是破坏性操作：行内两步确认（window.confirm 在本环境会被自动放行，禁用）。
  const [pendingConfirm, requestConfirm] = useTwoStepConfirm();

  useEffect(() => {
    rpc<CoreVersionInfo>('core.version')
      .then(setVersion)
      .catch(() => setVersion(null));
    void window.ratiflow
      .appInfo()
      .then((info) => setDataDir(info.userDataDir))
      .catch(() => setDataDir(null));
  }, []);

  const loadHistory = useCallback(async () => {
    setHistoryLoading(true);
    setError(null);
    try {
      const res = await rpc<BackupListResult>('backup.list');
      setHistory(res.items ?? []);
    } catch (e) {
      setError(e instanceof Error ? e.message : '备份历史加载失败');
    } finally {
      setHistoryLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadHistory();
  }, [loadHistory]);

  const createBackup = async () => {
    setCreating(true);
    setError(null);
    setInfo(null);
    try {
      const m = await rpc<BackupRecord>('backup.create');
      setManifest(m);
      await loadHistory();
    } catch (e) {
      setError(e instanceof Error ? e.message : '备份失败');
    } finally {
      setCreating(false);
    }
  };

  const verify = async (id: string) => {
    setBusyId(id);
    setError(null);
    setInfo(null);
    try {
      const rec = await rpc<BackupRecord>('backup.verify', { backupId: id });
      await loadHistory();
      setInfo(`校验完成：${rec.status}${rec.problems ? `（${rec.problems}）` : ''}`);
    } catch (e) {
      setError(e instanceof Error ? e.message : '校验失败');
    } finally {
      setBusyId(null);
    }
  };

  const restore = async (id: string) => {
    setBusyId(id);
    setError(null);
    setInfo(null);
    try {
      const out = await rpc<BackupRestoreOutcome>('backup.restore', { backupId: id });
      await loadHistory();
      setInfo(
        out.requiresRestart
          ? '恢复成功，已写入本地数据库。请重启应用以加载备份数据（Core 持有旧连接）。'
          : '恢复成功',
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : '恢复失败');
    } finally {
      setBusyId(null);
    }
  };

  const remove = (id: string) =>
    requestConfirm(`backup:${id}`, () => {
      void doRemove(id);
    });

  const doRemove = async (id: string) => {
    setBusyId(id);
    setError(null);
    setInfo(null);
    try {
      const res = await rpc<BackupDeleteResult>('backup.delete', { backupId: id });
      setInfo(res.status === 'deleted' ? '备份已删除' : '删除完成');
      await loadHistory();
    } catch (e) {
      setError(e instanceof Error ? e.message : '删除失败');
    } finally {
      setBusyId(null);
    }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="备份与恢复"
        description="备份为带 manifest 与 sha256 校验的本地快照；恢复前须校验通过，恢复后需重启应用。"
      />

      {error ? <div className="sg-banner sg-banner--error" role="alert">操作失败：{error}</div> : null}
      {info ? <div className="sg-banner sg-banner--info" role="status">{info}</div> : null}

      <SettingsSection title="备份状态">
        <div className="sg-summary-rows">
          <div className="sg-summary-row">
            <span className="sg-summary-label">数据目录</span>
            <span className="sg-summary-status"><StatusPill kind="ready" label="本地" /></span>
            <span className="sg-summary-detail sg-path">{dataDir ?? '读取中…'}</span>
          </div>
          <div className="sg-summary-row">
            <span className="sg-summary-label">schema 版本</span>
            <span className="sg-summary-status"><StatusPill kind="ready" label={version ? `v${version.schemaVersion}` : '读取中'} /></span>
            <span className="sg-summary-detail sg-muted">备份与同 schema 版本的 core 配套</span>
          </div>
        </div>
      </SettingsSection>

      <SettingsSection title="立即创建备份" description="写入 backups/ 目录并返回 manifest；不中断当前工作。">
        <div className="sg-row">
          <button className="sg-btn sg-btn--primary" onClick={() => void createBackup()} disabled={creating}>
            <IconDb size={14} />
            {creating ? '备份中…' : '创建备份'}
          </button>
        </div>
        {manifest ? (
          <div className="sg-card" style={{ marginTop: 12 }} role="status" aria-live="polite">
            <div className="sg-row" style={{ fontWeight: 600, marginBottom: 8 }}>
              <StatusPill kind="ready" label="备份成功" />
              <span className="sg-muted">{formatDateTime(manifest.created_at)}</span>
            </div>
            <div className="sg-kv">
              <span className="sg-kv-k">路径</span>
              <span className="sg-path">{manifest.path}</span>
              <span className="sg-kv-k">sha256</span>
              <span className="sg-path">{manifest.digest}</span>
              <span className="sg-kv-k">schema</span>
              <span>v{manifest.schema_version}（core {manifest.manifest.ratiflowVersion}）</span>
              <span className="sg-kv-k">对象数</span>
              <span>
                {Object.entries(manifest.manifest.objectsCount ?? {})
                  .map(([k, v]) => `${k}:${v}`)
                  .join(' · ') || '—'}
              </span>
            </div>
          </div>
        ) : null}
      </SettingsSection>

      <SettingsSection
        title="备份历史与恢复"
        description="校验（sha256/schema/rollouts）→ 通过后恢复；恢复成功需重启应用生效。"
        actions={
          <button className="sg-btn sg-btn--sm" onClick={() => void loadHistory()} disabled={historyLoading}>
            <IconRefresh size={14} />
            刷新
          </button>
        }
      >
        {historyLoading && history.length === 0 ? (
          <div className="sg-skeleton-rows" aria-busy="true">
            <div className="sg-skeleton-row" />
            <div className="sg-skeleton-row" />
          </div>
        ) : history.length === 0 ? (
          <div className="sg-empty">
            <IconDb size={28} style={{ color: 'var(--sg-border-strong)' }} />
            <span>暂无备份记录</span>
            <span className="sg-hint">点击上方「创建备份」生成第一条本地快照。</span>
          </div>
        ) : (
          <table className="sg-table">
            <thead>
              <tr>
                <th style={{ width: 140 }}>创建时间</th>
                <th style={{ width: 80 }}>大小</th>
                <th style={{ width: 70 }}>schema</th>
                <th style={{ width: 90 }}>状态</th>
                <th style={{ width: 200 }}>操作</th>
              </tr>
            </thead>
            <tbody>
              {history.map((b) => {
                return (
                  <tr key={b.id}>
                    <td className="sg-muted" title={b.id}>{formatDateTime(b.created_at)}</td>
                    <td className="sg-muted">{formatBytes(b.size_bytes)}</td>
                    <td className="sg-muted">v{b.schema_version}</td>
                    <td>
                      <StatusPill kind={statusKind(b.status)} label={b.status} />
                      {b.verified ? <span className="sg-hint"> · 已校验</span> : null}
                    </td>
                    <td>
                      <div className="sg-row" style={{ gap: 6 }}>
                        <button
                          className="sg-btn sg-btn--sm"
                          disabled={busyId !== null}
                          onClick={() => void verify(b.id)}
                          title="重算 sha256 并检查 schema/rollouts 一致性"
                        >
                          {busyId === b.id ? '处理中…' : '校验'}
                        </button>
                        <button
                          className="sg-btn sg-btn--sm sg-btn--danger"
                          disabled={busyId !== null || b.status !== 'verified'}
                          onClick={() => requestConfirm(`restore:${b.id}`, () => void restore(b.id))}
                          title={
                            b.status !== 'verified'
                              ? '仅已校验通过的备份可恢复；先执行校验'
                              : pendingConfirm === `restore:${b.id}`
                                ? '再次点击确认恢复（将覆盖当前本地数据库）'
                                : '恢复该备份'
                          }
                        >
                          {busyId === b.id ? '处理中…' : pendingConfirm === `restore:${b.id}` ? '确认恢复？' : '恢复'}
                        </button>
                        <button
                          className="sg-btn sg-btn--sm"
                          disabled={busyId !== null}
                          onClick={() => remove(b.id)}
                          title={pendingConfirm === `backup:${b.id}` ? '再次点击确认删除' : '删除记录与快照文件（不可撤销）'}
                        >
                          {busyId === b.id ? '处理中…' : pendingConfirm === `backup:${b.id}` ? '确认删除？' : '删除'}
                        </button>
                      </div>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
        <div className="sg-row" style={{ marginTop: 12 }}>
          <button className="sg-btn" disabled title="恢复操作在列表中按备份执行（须先校验通过）">
            <IconDownload size={14} />
            从备份恢复
          </button>
          <span className="sg-hint">恢复为危险操作：先校验 → 二次确认 → 成功后重启应用。</span>
        </div>
      </SettingsSection>
    </div>
  );
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}
