// S41 备份与恢复：backup.create 已真实接入；历史/校验/恢复待 ZCode 契约（§5.10）。
import { useEffect, useState } from 'react';
import { rpc } from '../../rpc/client';
import { formatDateTime } from '../../lib/format';
import type { BackupRecord, CoreVersionInfo } from './types';
import { SettingsPageHeader } from './components/SettingsPageHeader';
import { SettingsSection } from './components/SettingsSection';
import { StatusPill } from './components/StatusPill';
import { ContractPending } from './components/ContractPending';
import { IconDb, IconDownload } from '../../components/Icons';

export function BackupPage() {
  const [version, setVersion] = useState<CoreVersionInfo | null>(null);
  const [dataDir, setDataDir] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [manifest, setManifest] = useState<BackupRecord | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    rpc<CoreVersionInfo>('core.version')
      .then(setVersion)
      .catch(() => setVersion(null));
    void window.sixgates
      .appInfo()
      .then((info) => setDataDir(info.userDataDir))
      .catch(() => setDataDir(null));
  }, []);

  const createBackup = async () => {
    setCreating(true);
    setError(null);
    try {
      const m = await rpc<BackupRecord>('backup.create');
      setManifest(m);
    } catch (e) {
      setError(e instanceof Error ? e.message : '备份失败');
    } finally {
      setCreating(false);
    }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="备份与恢复"
        scope="本地"
        status={<StatusPill kind="partial" />}
        description="备份为带 manifest 与 sha256 校验的本地快照；恢复/历史/校验待契约开放。"
      />

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
        {error ? <div className="sg-banner sg-banner--error" role="alert" style={{ marginTop: 10 }}>备份失败：{error}</div> : null}
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
              <span>v{manifest.schema_version}（core {manifest.manifest.sixgatesVersion}）</span>
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

      <SettingsSection title="备份历史与恢复">
        <ContractPending
          features={[
            '备份列表：manifest 元数据（时间、大小、schema、对象数）',
            '备份校验：sha256 与 schema 一致性检查',
            '恢复流程：选择备份 → 校验 → 二次确认 → 恢复 → 结果提示',
            '删除备份与清理策略',
          ]}
          awaiting={['backup.list', 'backup.verify', 'backup.restore', 'backup.delete']}
          fallback="当前可手动复制 backups/ 目录作为冷备份；恢复需 Core 独占锁，请等待契约开放。"
        />
        <div className="sg-row" style={{ marginTop: 12 }}>
          <button className="sg-btn" disabled title="backup.list 契约待 ZCode 提交">
            <IconDownload size={14} />
            从备份恢复
          </button>
          <span className="sg-hint">恢复前需备份校验通过 + 二次确认（危险操作带）。</span>
        </div>
      </SettingsSection>
    </div>
  );
}
