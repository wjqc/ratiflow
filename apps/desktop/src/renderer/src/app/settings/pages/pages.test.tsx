// 新接线的 9 个设置页：mock bridge 下渲染不崩溃 + 空/加载状态展示。
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import '@testing-library/jest-dom/vitest';
import type React from 'react';
import { GeneralPage } from './GeneralPage';
import { KnowledgeDefaultsPage } from './KnowledgeDefaultsPage';
import { ModelsPage } from './ModelsPage';
import { ToolsPage } from './ToolsPage';
import { ExecutionPage } from './ExecutionPage';
import { IntegrationsPage } from './IntegrationsPage';
import { SkillsPage } from './SkillsPage';
import { MemoryPage } from './MemoryPage';
import { DiagnosticsPage } from '../DiagnosticsPage';
import { SETTINGS_NAV } from '../settings-routes';
import SettingsShell from '../SettingsShell';

const rpcMock = vi.fn();
function ok(body: unknown) { return Promise.resolve(body); }

describe('设置页接线', () => {
  beforeEach(() => {
    cleanup();
    rpcMock.mockReset();
    rpcMock.mockImplementation((method: string) => {
      switch (method) {
        case 'settings.get': return ok({ items: [] });
        case 'knowledge.settings.get': return ok({ revision: 0 });
        case 'modelProfile.list': case 'gitlabProfile.list': case 'sshTarget.list':
        case 'credentialRef.list': case 'tool.list': case 'executionProfile.list':        case 'modelProvider.presets':
          return ok({ items: [] });
        case 'executor.settings.get': return ok({ revision: 0 });
        case 'project.list': return ok({ items: [{ id: 'prj_1', name: '示例项目', archivedAt: null }] });
        case 'memory.settingsGet':
          return ok({
            projectId: 'prj_1', enabled: false, captureMode: 'off',
            maxEntries: 8, maxBytes: 12288, staleAfterDays: 180, revision: 1,
            updatedAt: '2026-09-04T09:30:00.000Z', updatedBy: 'local',
          });
        case 'memory.list':
          return ok({ projectId: 'prj_1', items: [], counts: { active: 0 }, cursor: null });
        case 'skill.list':
          return ok({
            items: [
              { id: 'skill_1', name: 'deploy-check', description: '部署检查清单', bodyBytes: 120, enabled: true, source: 'manual', agentProfileId: null, agentName: null, revision: 1, createdAt: '', updatedAt: '' },
              { id: 'skill_2', name: 'prd-writer', description: '', bodyBytes: 80, enabled: false, source: 'import', agentProfileId: 'agent_1', agentName: '文档 Agent', revision: 2, createdAt: '', updatedAt: '' },
            ],
          });
        case 'agentProfile.list':
          // 契约形状与后端 dispatch 一致：versions 恒为数组（缺失即契约漂移）。
          return ok({ items: [{ id: 'agent_1', name: '文档 Agent', adapter_kind: 'local_harness', enabled: true, versions: [] }] });
        case 'model.usage':
          return ok({
            tokensIn: 12000,
            tokensOut: 3000,
            cachedTokens: 4800,
            cacheHitRatio: 0.6,
            compactions: 2,
            compactionBeforeEst: 18000,
            compactionAfterEst: 6200,
            daily: [
              { day: '2026-09-01', tokensIn: 4000, totalTokens: 5000, cachedTokens: 1600, cacheHitRatio: 0.4 },
              { day: '2026-09-02', tokensIn: 0, totalTokens: 0, cachedTokens: 0, cacheHitRatio: null },
              { day: '2026-09-03', tokensIn: 8000, totalTokens: 10000, cachedTokens: 3200, cacheHitRatio: 0.4 },
            ],
          });
        // 治理总览四指标：形状与 workflow::metrics 输出对齐（缺失字段会在渲染层崩溃）。
        case 'metrics.overview':
          return ok({
            windowDays: 30,
            orphanRate: { totalNodes: 3, orphanCount: 1, rate: 0.33, insufficientData: false },
            loopRate: { completedReworks: 2, reworkedWorkitems: 1, workitemsWithReleases: 2, averageReworkCount: 1.5, reworkWorkitemRate: 0.5, insufficientData: false },
            approvalLayers: { dimensions: ['subjectType', 'risk'], layers: [{ subjectType: 'workitem', risk: 'medium', approved: 3, rejected: 1, pending: 0, expired: 0, changesRequested: 0, passRate: 0.75, sample: 4, insufficientData: false, rubberStampSuspect: false }] },
            aiSuggestionAdoption: { decided: 4, accepted: 2, rate: 0.5, insufficientData: false },
          });
        case 'triage.list':
          return ok({ items: [], knowledgeBlocked: [] });
        default: return ok({});
      }
    });
    (window as unknown as { ratiflow: unknown }).ratiflow = {
      rpc: (m: string, p: Record<string, unknown>) => rpcMock(m, p),
      hello: () => Promise.resolve({ ok: true }),
      selectFile: () => Promise.resolve(null),
      selectDirectory: () => Promise.resolve(null),
      openExternal: () => Promise.resolve(),
      appInfo: () => Promise.resolve({ desktopVersion: 'test', logDir: '/tmp', userDataDir: '/tmp' }),
      openLogs: () => Promise.resolve(),
      onEvent: () => () => {},
    };
  });
  afterEach(() => { delete (window as unknown as { ratiflow?: unknown }).ratiflow; });

  it.each([
    ['常规', GeneralPage, '管理应用启动'],
    ['知识默认', KnowledgeDefaultsPage, '不添加项目来源'],
    ['模型', ModelsPage, 'Keychain'],
    ['工具', ToolsPage, 'ActionDigest 绑定'],
    ['执行', ExecutionPage, '显式不安全'],
  ] as Array<[string, () => React.ReactElement, string]>)('%s 页渲染且含安全提示', async (_label, Page, hint) => {
    render(<Page />);
    await waitFor(() => expect(screen.getAllByText(new RegExp(hint.slice(0, 4))).length).toBeGreaterThan(0));
  });

  it('外部集成页同页含 GitLab 与 SSH 两个区块，新增表单在区块内展开', async () => {
    render(<IntegrationsPage />);
    await waitFor(() => expect(screen.getByText('暂无 GitLab 实例')).toBeInTheDocument());
    expect(screen.getByRole('heading', { name: '外部集成' })).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'GitLab' })).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'SSH 目标机' })).toBeInTheDocument();
    await waitFor(() => expect(screen.getByText('暂无 SSH 目标')).toBeInTheDocument());
    // 两个空态并存 → 确认不是两页而是同页两区块。
    expect(screen.getByText('部署关依赖至少一个已确认指纹的目标。')).toBeInTheDocument();
    // 新增表单在区块内切换，不改变区块标题；秘密直填落 Keychain，不再有凭据引用 ID 输入。
    fireEvent.click(screen.getByRole('button', { name: '新增实例' }));
    expect(screen.getByLabelText('名称 *')).toBeInTheDocument();
    expect(screen.getByLabelText('访问令牌（Token）')).toBeInTheDocument();
    expect(screen.queryByPlaceholderText('cr_…')).not.toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'GitLab' })).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: '取消' }));
    fireEvent.click(screen.getByRole('button', { name: '新增目标' }));
    expect(screen.getByLabelText('Host *')).toBeInTheDocument();
    expect(screen.getByLabelText('访问凭证（密码或私钥）')).toBeInTheDocument();
  });

  it('常规页合并外观设置并自动保存两个设置域（无保存按钮）', async () => {
    rpcMock.mockImplementation((method: string) => {
      if (method === 'settings.get') {
        return ok({
          items: [
            { key: 'app.general', value: { language: 'zh-CN' }, revision: 2 },
            { key: 'app.appearance', value: { density: 'comfortable' }, revision: 3 },
          ],
        });
      }
      if (method === 'settings.update') {
        return ok({
          items: [
            { key: 'app.general', revision: 3 },
            { key: 'app.appearance', revision: 4 },
          ],
        });
      }
      return ok({});
    });
    const appearanceEvents: unknown[] = [];
    const onAppearance = (event: Event) => appearanceEvents.push((event as CustomEvent).detail);
    window.addEventListener('sg:appearance-changed', onAppearance);
    render(<GeneralPage />);
    await waitFor(() => expect(screen.getByRole('heading', { name: /^外观$/ })).toBeInTheDocument());
    fireEvent.change(screen.getByLabelText('语言'), { target: { value: 'system' } });
    fireEvent.change(screen.getByLabelText('界面密度'), { target: { value: 'compact' } });
    // 改动即保存：无「保存更改」按钮，自动触发 settings.update。
    expect(screen.queryByRole('button', { name: '保存更改' })).not.toBeInTheDocument();
    await waitFor(() => expect(rpcMock).toHaveBeenCalledWith(
      'settings.update',
      expect.objectContaining({
        patches: expect.arrayContaining([
          expect.objectContaining({ key: 'app.general', expectedRevision: 2 }),
          expect.objectContaining({ key: 'app.appearance', expectedRevision: 3 }),
        ]),
      }),
    ));
    expect(appearanceEvents).toHaveLength(1);
    window.removeEventListener('sg:appearance-changed', onAppearance);
  });

  it('S12 记忆页：开关/计数/空状态', async () => {
    render(<MemoryPage />);
    await waitFor(() => expect(screen.getByRole('switch', { name: /启用记忆注入/ })).toBeInTheDocument());
    await waitFor(() => expect(screen.getByText(/0 条记忆/)).toBeInTheDocument());
    // 默认项目未开启 → 开关未勾选（enabled=false 来自 settingsGet）。
    const sw = screen.getByRole('switch', { name: /启用记忆注入/ }) as HTMLInputElement;
    expect(sw.checked).toBe(false);
    await waitFor(() => expect(screen.getByText(/当前项目未开启记忆/)).toBeInTheDocument());
    // 无全局门禁：设置加载完成后开关可操作。
    expect(sw.disabled).toBe(false);
  });

  it('S25 技能页：列表/搜索/启停开关', async () => {
    render(<SkillsPage />);
    await waitFor(() => expect(screen.getByText('deploy-check')).toBeInTheDocument());
    expect(screen.getByText('2 个技能')).toBeInTheDocument();
    // 未启用技能也在列表中（开关未勾选）。
    expect(screen.getByText('prd-writer')).toBeInTheDocument();
    const toggles = screen.getAllByRole('switch');
    expect(toggles.length).toBe(2);
    fireEvent.click(toggles[0]);
    await waitFor(() =>
      expect(rpcMock).toHaveBeenCalledWith('skill.setEnabled', expect.objectContaining({ skillId: 'skill_1', enabled: false })),
    );
    // 绑定选择器：显示范围描述；改绑发送 skill.update（显式 null=全局）。
    expect(screen.getByText(/仅 文档 Agent 生效/)).toBeInTheDocument();
    const selects = screen.getAllByRole('combobox', { name: /生效范围/ });
    expect(selects).toHaveLength(2);
    fireEvent.change(selects[1], { target: { value: '' } });
    await waitFor(() =>
      expect(rpcMock).toHaveBeenCalledWith('skill.update', expect.objectContaining({ skillId: 'skill_2', agentProfileId: null })),
    );
  });

  it('S25 技能页：市场 tab 扁平技能列表（搜索 + 分页，开关=安装）', async () => {
    rpcMock.mockImplementation((method: string) => {
      if (method === 'skill.list') {
        return ok({ items: [
          { id: 'skill_1', name: 'deploy-check', description: '', bodyBytes: 1, enabled: true, source: 'manual', agentProfileId: null, agentName: null, revision: 1, createdAt: '', updatedAt: '' },
        ] });
      }
      if (method === 'skill.marketList') {
        return ok({ items: [
          { id: 'mkt_1', name: '远端技能库', kind: 'remote_git', rootPath: 'https://example.com/r.git', marketplaceId: '', enabled: true, revision: 1, resolvedRoot: 'https://example.com/r.git', marketplaceName: 'r', description: '', pluginCount: 0, skills: [
            { name: 'remote-alpha', dirName: 'remote-alpha', description: '远程技能', plugin: '', version: 'abcd1234' },
          ], plugins: [], error: '' },
        ] });
      }
      return ok({});
    });
    render(<SkillsPage />);
    await waitFor(() => expect(screen.getByText('deploy-check')).toBeInTheDocument());
    // 切到市场 tab：不显示市场源名，直接平铺技能行。
    fireEvent.click(screen.getByRole('tab', { name: /市场/ }));
    const sw = await waitFor(() => screen.getByRole('switch', { name: '安装技能 remote-alpha' }));
    expect(screen.queryByText('远端技能库')).not.toBeInTheDocument();
    expect(sw).not.toBeChecked();
    // 开关打开 = marketImport。
    fireEvent.click(sw);
    await waitFor(() =>
      expect(rpcMock).toHaveBeenCalledWith('skill.marketImport', expect.objectContaining({ sourceId: 'mkt_1', skillName: 'remote-alpha' })),
    );
  });

  it('S25 技能页：市场技能本地缓存——刷新成功落缓存，拉取失败仍显示上次列表', async () => {
    // 测试环境无 localStorage：打桩到内存 Map（组件按同 key 读写）。
    const store = new Map<string, string>();
    vi.stubGlobal('localStorage', {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
      removeItem: (k: string) => void store.delete(k),
      clear: () => void store.clear(),
    });
    const marketOk = { items: [
      { id: 'mkt_1', name: '远端技能库', kind: 'remote_git', rootPath: 'https://example.com/r.git', marketplaceId: '', enabled: true, revision: 1, resolvedRoot: '', marketplaceName: 'r', description: '', pluginCount: 0, skills: [
        { name: 'cached-skill', dirName: 'cached-skill', description: '缓存技能', plugin: '', version: 'abcd1234' },
      ], plugins: [], error: '' },
    ] };
    // 第一次：刷新成功 → 列表出现且写入 localStorage。
    rpcMock.mockImplementation((method: string) => {
      if (method === 'skill.list') return ok({ items: [] });
      if (method === 'skill.marketList') return ok(marketOk);
      return ok({});
    });
    const first = render(<SkillsPage />);
    fireEvent.click(screen.getByRole('tab', { name: /市场/ }));
    await waitFor(() => expect(screen.getByText('cached-skill')).toBeInTheDocument());
    await waitFor(() => expect(localStorage.getItem('ratiflow.skill.market.v1')).toBeTruthy());
    first.unmount();

    // 第二次：marketList 拒绝（离线）→ 仍显示缓存列表，不出「还没有市场源」空态。
    rpcMock.mockImplementation((method: string) => {
      if (method === 'skill.list') return ok({ items: [] });
      if (method === 'skill.marketList') return Promise.reject(new Error('网络不可达'));
      return ok({});
    });
    render(<SkillsPage />);
    fireEvent.click(screen.getByRole('tab', { name: /市场/ }));
    await waitFor(() => expect(screen.getByText('cached-skill')).toBeInTheDocument());
    expect(screen.queryByText('还没有市场源')).not.toBeInTheDocument();
    // 缓存行的安装开关照常可用（走缓存里的 sourceId）。
    fireEvent.click(screen.getByRole('switch', { name: '安装技能 cached-skill' }));
    await waitFor(() =>
      expect(rpcMock).toHaveBeenCalledWith('skill.marketImport', expect.objectContaining({ sourceId: 'mkt_1', skillName: 'cached-skill' })),
    );
  });

  it('工具页策略行来自 tool.list', async () => {
    rpcMock.mockImplementation((method: string) => {
      if (method === 'tool.list') {
        return ok({ items: [{ tool_id: 'read_file', enabled: true, risk: 'low', requires_approval: false, network: 'deny', revision: 1 }] });
      }
      return ok({});
    });
    render(<ToolsPage />);
    await waitFor(() => expect(screen.getByText('read_file')).toBeInTheDocument());
  });

  it('知识默认页统一行式布局：标题在左、控件在右', async () => {
    const { container } = render(<KnowledgeDefaultsPage />);
    await waitFor(() => expect(screen.getByText('最大文件大小（MB）')).toBeInTheDocument());
    const items = container.querySelectorAll('.sg-set-item');
    expect(items.length).toBeGreaterThanOrEqual(9);
    expect(container.querySelectorAll('.sg-set-item-control')).toHaveLength(items.length);
    expect(screen.getByLabelText('指令文件名（逗号分隔，按序探测）')).toBeInTheDocument();
    // 布尔设置用胶囊开关（不再出现裸复选框网格）。
    expect(container.querySelectorAll('.sg-setting-toggle').length).toBeGreaterThanOrEqual(2);
  });

  it('使用统计只保留总/缓存命中/命中率三个指标并渲染每日折线图', async () => {
    render(<DiagnosticsPage />);
    await waitFor(() => expect(screen.getByText('15,000')).toBeInTheDocument());
    const grid = screen.getByLabelText('模型 Token 用量统计');
    expect(grid.children).toHaveLength(3);
    expect(screen.getByText('4,800')).toBeInTheDocument();
    expect(screen.getByText('60%')).toBeInTheDocument();
    expect(screen.queryByText('输入 Token')).not.toBeInTheDocument();
    expect(screen.queryByText('输出 Token')).not.toBeInTheDocument();
    expect(screen.queryByText('2 次')).not.toBeInTheDocument();
    // 每日折线图：总/命中两条连续序列；命中率遇空日断线（此处仅剩孤立点，无连线）。
    const chart = screen.getByRole('img', { name: /近 30 天模型每日 Token 用量/ });
    expect(chart.querySelectorAll('polyline')).toHaveLength(2);
    expect(chart.querySelectorAll('circle')).toHaveLength(8); // 每天总/命中两点，第1、3天各加命中率点
    expect(screen.getByText('每日趋势')).toBeInTheDocument();
    // 上下文压缩区块已移除。
    expect(screen.queryByText('上下文压缩', { selector: 'h2' })).not.toBeInTheDocument();
    expect(screen.queryByText(/压缩前后/)).not.toBeInTheDocument();
    expect(screen.queryByText('本地运行')).not.toBeInTheDocument();
    expect(screen.queryByText('外部集成', { selector: 'h2' })).not.toBeInTheDocument();
  });

  it('使用统计近 30 天无调用量时折线图给空状态', async () => {
    rpcMock.mockImplementation((method: string) => {
      if (method === 'model.usage') {
        return ok({
          tokensIn: 0, tokensOut: 0, cachedTokens: 0, cacheHitRatio: null,
          daily: [],
        });
      }
      return ok({});
    });
    render(<DiagnosticsPage />);
    await waitFor(() => expect(screen.getByText('近 30 天暂无每日模型调用量。')).toBeInTheDocument());
    expect(screen.queryByRole('img', { name: /近 30 天模型每日 Token 用量/ })).not.toBeInTheDocument();
  });
  it('设置壳：导航表内 real 路由全部渲染且不落「未接线」占位（skills 接线回归）', async () => {
    const realIds = SETTINGS_NAV.flatMap((g) => g.items.map((i) => i.id))
      .filter((id) => id !== 'editors');
    for (const id of realIds) {
      cleanup();
      const { container } = render(<SettingsShell section={id} />);
      await waitFor(() => expect(container.textContent).not.toBe(''));
      expect(container.textContent, `路由 ${id} 落入未接线占位`).not.toContain('页面未接线');
    }
  });
});


