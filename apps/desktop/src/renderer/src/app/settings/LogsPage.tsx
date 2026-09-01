// S51 日志与故障报告：日志目录（appInfo + openLogs 窄 IPC）+ logs.list 列表 + 去敏 bundle 导出（§5.15）。
import { useCallback, useEffect, useState } from 'react';
import { rpc } from '../../rpc/client';
import type { LogEntry, LogListResult } from './types';
import { SettingsPageHeader } from './components/SettingsPageHeader';
import { SettingsSection } from './components/SettingsSection';
import { StatusPill } from './components/StatusPill';
import { IconDownload, IconFolder, IconRefresh } from '../../components/Icons';

export function LogsPage() {
  const [logDir, setLogDir] = useState<string | null>(null);
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [logsLoading, setLogsLoading] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadLogs = useCallback(async () => {
    setLogsLoading(true);
    setError(null);
    try {
      const res = await rpc<LogListResult>('logs.list', { limit: 100 });
      setLogs(res.items ?? []);
    } catch (e) {
      setError(e instanceof Error ? e.message : '日志列表加载失败');
    } finally {
      setLogsLoading(false);
    }
  }, []);

  useEffect(() => {
    void window.sixgates
      .appInfo()
      .then((info) => setLogDir(info.logDir))
      .catch(() => setError('应用信息读取失败'));
    void loadLogs();
  }, [loadLogs]);

  const openLogs = async () => {
    try {
      await window.sixgates.openLogs();
    } catch (e) {
      setError(e instanceof Error ? e.message : '打开日志目录失败');
    }
  };

  const exportBundle = async () => {
    setExporting(true);
    setError(null);
    try {
      const data = await rpc<Record<string, unknown>>('logs.exportDiagnosticBundle');
      const blob = new Blob([JSON.stringify(data, null, 2)], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `diagnostic-bundle-${new Date().toISOString().slice(0, 10)}.json`;
      a.click();
      URL.revokeObjectURL(url);
    } catch (e) {
      setError(e instanceof Error ? e.message : '导出失败');
    } finally {
      setExporting(false);
    }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="日志与故障报告"
        scope="本地"
        status={<StatusPill kind="ready" />}
        description="日志目录可打开；日志文件列表与去敏故障报告可导出。"
      />

      {error ? <div className="sg-banner sg-banner--error" role="alert">{error}</div> : null}

      <SettingsSection title="日志目录" description="main/core 日志落盘位置；导出前会对秘密键脱敏。">
        <div className="sg-summary-rows">
          <div className="sg-summary-row">
            <span className="sg-summary-label">日志目录</span>
            <span className="sg-summary-status"><StatusPill kind="ready" label="可用" /></span>
            <span className="sg-summary-detail sg-path">{logDir ?? '读取中…'}</span>
          </div>
        </div>
        <div className="sg-row" style={{ marginTop: 12 }}>
          <button className="sg-btn" onClick={() => void openLogs()} disabled={!logDir}>
            <IconFolder size={14} />
            打开日志目录
          </button>
        </div>
      </SettingsSection>

      <SettingsSection
        title="日志文件"
        description="core 日志目录内文件（名称 / 大小）。"
        actions={
          <button className="sg-btn sg-btn--sm" onClick={() => void loadLogs()} disabled={logsLoading}>
            <IconRefresh size={14} />
            刷新
          </button>
        }
      >
        {logsLoading && logs.length === 0 ? (
          <div className="sg-skeleton-rows" aria-busy="true">
            <div className="sg-skeleton-row" />
            <div className="sg-skeleton-row" />
          </div>
        ) : logs.length === 0 ? (
          <div className="sg-empty">
            <span>暂无日志文件</span>
            <span className="sg-hint">Core 尚未落盘日志，或日志目录不可达。</span>
          </div>
        ) : (
          <table className="sg-table">
            <thead>
              <tr>
                <th>文件名</th>
                <th style={{ width: 120 }}>大小</th>
              </tr>
            </thead>
            <tbody>
              {logs.map((l) => (
                <tr key={l.path}>
                  <td className="sg-code" style={{ fontWeight: 500 }}>{l.name}</td>
                  <td className="sg-muted">{formatBytes(l.sizeBytes)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </SettingsSection>

      <SettingsSection title="故障报告导出" description="版本 + 诊断 + 最近审计 + 日志引用；全文脱敏后打包为 JSON。">
        <div className="sg-row">
          <button className="sg-btn" onClick={() => void exportBundle()} disabled={exporting}>
            <IconDownload size={14} />
            {exporting ? '导出中…' : '导出故障报告'}
          </button>
          <span className="sg-hint">导出内容经脱敏处理，可安全粘贴到问题反馈。</span>
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
