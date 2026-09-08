// S03 更新与关于：版本区真实（core.version + appInfo）+ update.check/status 展示；实际更新由 Electron main 执行。
import { useEffect, useState } from 'react';
import { rpc } from '../../rpc/client';
import { redactedJson } from '../../lib/redact';
import type { CoreVersionInfo, UpdateCheckInfo, UpdateStatusInfo } from './types';
import { SettingsPageHeader } from './components/SettingsPageHeader';
import { SettingsSection } from './components/SettingsSection';
import { StatusPill } from './components/StatusPill';
import { IconCheck, IconDoc } from '../../components/Icons';

interface AppInfo {
  desktopVersion: string;
  logDir: string;
  userDataDir: string;
}

export function UpdatesPage() {
  const [version, setVersion] = useState<CoreVersionInfo | null>(null);
  const [appInfo, setAppInfo] = useState<AppInfo | null>(null);
  const [updateCheck, setUpdateCheck] = useState<UpdateCheckInfo | null>(null);
  const [updateStatus, setUpdateStatus] = useState<UpdateStatusInfo | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    rpc<CoreVersionInfo>('core.version')
      .then(setVersion)
      .catch((e) => setLoadError(e instanceof Error ? e.message : 'core 版本读取失败'));
    void window.ratiflow
      .appInfo()
      .then(setAppInfo)
      .catch(() => setAppInfo(null));
    rpc<UpdateCheckInfo>('update.check')
      .then(setUpdateCheck)
      .catch(() => setUpdateCheck(null));
    rpc<UpdateStatusInfo>('update.status')
      .then(setUpdateStatus)
      .catch(() => setUpdateStatus(null));
  }, []);

  const copyDiagnostics = async () => {
    const payload = {
      desktop: appInfo?.desktopVersion ?? 'unknown',
      core: version,
      capturedAt: new Date().toISOString(),
    };
    try {
      await navigator.clipboard.writeText(redactedJson(payload));
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      setLoadError('复制失败：剪贴板不可用');
    }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="更新与关于"
        description="版本、数据与日志目录信息真实可读；更新执行由 Electron main（autoUpdater）负责。"
      />

      {loadError ? <div className="sg-banner sg-banner--error" role="alert">{loadError}</div> : null}

      <SettingsSection title="版本信息">
        <div className="sg-kv">
          <span className="sg-kv-k">桌面端</span>
          <span>{appInfo?.desktopVersion ?? '读取中…'}</span>
          <span className="sg-kv-k">Rust Core</span>
          <span>{version ? `v${version.coreVersion}` : loadError ? '不可用' : '读取中…'}</span>
          <span className="sg-kv-k">协议版本</span>
          <span>{version ? `v${version.protocolVersion}` : '—'}</span>
          <span className="sg-kv-k">schema 版本</span>
          <span>{version ? `v${version.schemaVersion}` : '—'}</span>
          <span className="sg-kv-k">数据目录</span>
          <span className="sg-path">{appInfo?.userDataDir ?? '—'}</span>
          <span className="sg-kv-k">日志目录</span>
          <span className="sg-path">{appInfo?.logDir ?? '—'}</span>
        </div>
        <div className="sg-row" style={{ marginTop: 12 }}>
          <button className="sg-btn" onClick={() => void copyDiagnostics()}>
            {copied ? <IconCheck size={14} /> : <IconDoc size={14} />}
            {copied ? '已复制' : '复制诊断信息'}
          </button>
          <span className="sg-hint">内容经脱敏处理，可安全粘贴到问题反馈。</span>
        </div>
      </SettingsSection>

      <SettingsSection title="更新通道">
        <div className="sg-summary-rows">
          <div className="sg-summary-row">
            <span className="sg-summary-label">通道</span>
            <span className="sg-summary-status"><StatusPill kind="ready" label={updateCheck?.channel ?? 'stable'} /></span>
            <span className="sg-summary-detail sg-muted">当前分发通道</span>
          </div>
          <div className="sg-summary-row">
            <span className="sg-summary-label">自动检查</span>
            <span className="sg-summary-status">
              <StatusPill kind={updateCheck?.autoCheck ? 'ready' : 'readonly'} label={updateCheck?.autoCheck ? '开启' : '关闭'} />
            </span>
            <span className="sg-summary-detail sg-muted">启动时检查更新</span>
          </div>
          <div className="sg-summary-row">
            <span className="sg-summary-label">自动下载</span>
            <span className="sg-summary-status">
              <StatusPill kind={updateCheck?.autoDownload ? 'ready' : 'readonly'} label={updateCheck?.autoDownload ? '开启' : '关闭'} />
            </span>
            <span className="sg-summary-detail sg-muted">发现更新后自动下载</span>
          </div>
          <div className="sg-summary-row">
            <span className="sg-summary-label">当前版本</span>
            <span className="sg-summary-status">
              <StatusPill kind="ready" label={updateCheck?.currentVersion ?? '—'} />
            </span>
            <span className="sg-summary-detail sg-muted">最新版 {updateCheck?.latestVersion ?? '—'}</span>
          </div>
          <div className="sg-summary-row">
            <span className="sg-summary-label">更新可用</span>
            <span className="sg-summary-status">
              <StatusPill kind={updateCheck?.updateAvailable ? 'ready' : 'readonly'} label={updateCheck?.updateAvailable ? '有可用更新' : '已是最新'} />
            </span>
            <span className="sg-summary-detail sg-muted">{updateCheck?.note ?? ''}</span>
          </div>
        </div>
        <p className="sg-hint" style={{ margin: '8px 0 0' }}>
          实际更新下载与安装由桌面端 autoUpdater 执行；core 仅报告版本与通道状态。
        </p>
      </SettingsSection>

      <SettingsSection title="组件状态">
        <div className="sg-kv">
          <span className="sg-kv-k">桌面端</span>
          <span>{updateStatus?.desktop ?? '—'}</span>
          <span className="sg-kv-k">Rust Core</span>
          <span>{updateStatus?.core ?? '—'}</span>
          <span className="sg-kv-k">协议</span>
          <span>{updateStatus ? `v${updateStatus.protocol}` : '—'}</span>
          <span className="sg-kv-k">schema</span>
          <span>{updateStatus ? `v${updateStatus.schema}` : '—'}</span>
          <span className="sg-kv-k">签名验证</span>
          <span>
            <StatusPill kind={updateStatus?.signatureVerified ? 'ready' : 'readonly'} label={updateStatus?.signatureVerified ? '已通过' : '未验证'} />
          </span>
        </div>
        {updateStatus?.note ? <p className="sg-hint" style={{ margin: '8px 0 0' }}>{updateStatus.note}</p> : null}
      </SettingsSection>
    </div>
  );
}
