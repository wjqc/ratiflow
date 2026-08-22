// S51 日志与故障报告：日志目录真实（appInfo + openLogs 窄 IPC）；日志浏览/导出待契约（§5.15）。
import { useEffect, useState } from 'react';
import { SettingsPageHeader } from './components/SettingsPageHeader';
import { SettingsSection } from './components/SettingsSection';
import { StatusPill } from './components/StatusPill';
import { ContractPending } from './components/ContractPending';
import { IconFolder } from '../../components/Icons';

export function LogsPage() {
  const [logDir, setLogDir] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    void window.sixgates
      .appInfo()
      .then((info) => setLogDir(info.logDir))
      .catch(() => setError('应用信息读取失败'));
  }, []);

  const openLogs = async () => {
    try {
      await window.sixgates.openLogs();
    } catch (e) {
      setError(e instanceof Error ? e.message : '打开日志目录失败');
    }
  };

  return (
    <div className="sg-set-page">
      <SettingsPageHeader
        title="日志与故障报告"
        scope="本地"
        status={<StatusPill kind="partial" />}
        description="日志目录真实可打开；结构化日志浏览、级别筛选与故障报告导出待契约开放。"
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

      <SettingsSection title="日志浏览与故障报告">
        <ContractPending
          features={[
            '结构化日志列表：级别 / 模块 / 时间筛选',
            'core 重启状态与最近重启原因',
            '故障报告导出：版本 + 诊断 + 脱敏日志打包',
          ]}
          awaiting={['logs.list', 'logs.export', 'core.restartStatus']}
          fallback="当前可直接打开日志目录查看 sixgates-main.log；core 重启上限与诊断模式见「运行与集成诊断」。"
        />
      </SettingsSection>
    </div>
  );
}
