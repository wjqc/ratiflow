// S03 更新与关于：版本区真实（core.version + appInfo）；检查更新待 update.* 契约（§5.3）。
import { useEffect, useState } from 'react';
import { rpc } from '../../rpc/client';
import { redactedJson } from '../../lib/redact';
import type { CoreVersionInfo } from './types';
import { SettingsPageHeader } from './components/SettingsPageHeader';
import { SettingsSection } from './components/SettingsSection';
import { StatusPill } from './components/StatusPill';
import { ContractPending } from './components/ContractPending';
import { IconCheck, IconDoc } from '../../components/Icons';

interface AppInfo {
  desktopVersion: string;
  logDir: string;
  userDataDir: string;
}

export function UpdatesPage() {
  const [version, setVersion] = useState<CoreVersionInfo | null>(null);
  const [appInfo, setAppInfo] = useState<AppInfo | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    rpc<CoreVersionInfo>('core.version')
      .then(setVersion)
      .catch((e) => setLoadError(e instanceof Error ? e.message : 'core 版本读取失败'));
    void window.sixgates
      .appInfo()
      .then(setAppInfo)
      .catch(() => setAppInfo(null));
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
        scope="本地"
        status={version ? <StatusPill kind="ready" label="运行中" /> : <StatusPill kind="checking" />}
        description="版本、数据与日志目录信息真实可读；自动更新通道待契约开放。"
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
        <ContractPending
          features={[
            '更新通道选择（stable / beta）',
            '检查更新与更新包状态展示',
            '版本历史与更新后自动迁移确认',
          ]}
          awaiting={['update.check', 'update.status', 'update.apply']}
          fallback="当前版本随安装包分发；升级后 schema 迁移由 core 启动时自动执行并写入审计。"
        />
      </SettingsSection>
    </div>
  );
}
