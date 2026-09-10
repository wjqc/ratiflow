import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import '@testing-library/jest-dom/vitest';
import NewTaskPage from './NewTaskPage';

describe('新建任务', () => {
  const rpcMock = vi.fn();
  const onCreated = vi.fn();

  beforeEach(() => {
    cleanup();
    rpcMock.mockReset();
    onCreated.mockReset();
    rpcMock.mockImplementation((method: string, params?: { templateId?: string }) => {
      switch (method) {
        case 'workitem.create': return Promise.resolve({ id: 'wi_new' });
        case 'artifact.list': return Promise.resolve({ items: [] });
        case 'artifact.create': return Promise.resolve({ id: 'ar_prd', kind: 'prd' });
        case 'stage.startActivity': return Promise.resolve({ runId: 'run_prd' });
        default: return Promise.resolve({});
      }
    });
    (window as unknown as { ratiflow: unknown }).ratiflow = {
      rpc: (method: string, params: Record<string, unknown>) => rpcMock(method, params),
      hello: () => Promise.resolve({ ok: true }),
      selectFile: () => Promise.resolve(null),
      selectDirectory: () => Promise.resolve(null),
      openExternal: () => Promise.resolve(),
      appInfo: () => Promise.resolve({ desktopVersion: 'test', logDir: '', userDataDir: '' }),
      openLogs: () => Promise.resolve(),
      onEvent: () => () => undefined,
    };
  });

  afterEach(() => {
    delete (window as unknown as { ratiflow?: unknown }).ratiflow;
  });

  it('提交文字需求后立即启动 PRD 起草并进入任务', async () => {
    render(
      <NewTaskPage
        projectId="pj_1"
        projects={[{ id: 'pj_1', name: 'Ratiflow' }]}
        onCreated={onCreated}
        onWorkspaceChanged={() => undefined}
        onOpenRemote={() => undefined}
        onBack={() => undefined}
      />,
    );

    fireEvent.change(screen.getByLabelText('需求描述'), {
      target: { value: '支持可搜索的项目工作区选择器' },
    });
    fireEvent.click(screen.getByRole('button', { name: '创建并进入需求关' }));

    await waitFor(() => expect(onCreated).toHaveBeenCalledWith('wi_new', 'pj_1'));
    expect(rpcMock).toHaveBeenCalledWith(
      'stage.startActivity',
      expect.objectContaining({
        workItemId: 'wi_new',
        gate: 'requirements',
        idempotencyKey: 'auto-prd-wi_new',
      }),
    );
  });

  it('底部上下文栏展示工作区、本地状态、关卡模板与流程入口', async () => {
    rpcMock.mockImplementation((method: string, params?: { templateId?: string }) => {
      if (method === 'workflowTemplate.list') {
        return Promise.resolve({
          items: [{ key: 'six-gate-default', name: '默认六关', versions: [{ status: 'active' }] }],
        });
      }
      return Promise.resolve({});
    });
    render(
      <NewTaskPage
        projectId="pj_1"
        projects={[{ id: 'pj_1', name: 'Ratiflow' }]}
        onCreated={onCreated}
        onWorkspaceChanged={() => undefined}
        onOpenRemote={() => undefined}
        onBack={() => undefined}
      />,
    );
    await waitFor(() => expect(screen.getByLabelText('关卡模板')).toBeInTheDocument());
    expect(screen.getByRole('heading', { name: '你想在 ratiflow 中完成什么？' })).toBeInTheDocument();
    const pills = document.querySelector('.sg-nt-context-pills');
    expect(pills).not.toBeNull();
    expect(pills!.querySelector('[aria-label="关卡模板"]')).not.toBeNull();
    expect(screen.getByText('本地')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /查看流程/ })).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByText(/PRD 将结合此工作区的代码与知识库起草/)).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: '管理关卡模板' })).not.toBeInTheDocument();
  });

  it('交付流程按所选模板动态展示：标签带「按所选模板」标注', async () => {
    rpcMock.mockImplementation((method: string, params?: { templateId?: string }) => {
      if (method === 'workflowTemplate.list') {
        return Promise.resolve({
          items: [
            { key: 'six-gate-default', name: '默认六关', versions: [{ status: 'active' }] },
            { key: 'tri-gate', name: '三关精简', versions: [{ status: 'active' }] },
          ],
        });
      }
      if (method === 'workflowTemplate.get') {
        const gates =
          params?.templateId === 'tri-gate'
            ? [
                { gate_id: 'requirements', title: '需求关', purpose: '澄清' },
                { gate_id: 'development', title: '开发关', purpose: '编码' },
                { gate_id: 'verification', title: '验证关', purpose: '验收' },
              ]
            : [
                { gate_id: 'requirements', title: '需求关', purpose: '澄清' },
                { gate_id: 'design', title: '设计关', purpose: '方案' },
                { gate_id: 'development', title: '开发关', purpose: '编码' },
                { gate_id: 'testing', title: '测试关', purpose: '验证' },
                { gate_id: 'deployment', title: '部署关', purpose: '发布' },
                { gate_id: 'verification', title: '验证关', purpose: '验收' },
              ];
        return Promise.resolve({ activeVersion: { gates } });
      }
      return Promise.resolve({});
    });
    render(
      <NewTaskPage
        projectId="pj_1"
        projects={[{ id: 'pj_1', name: 'Ratiflow' }]}
        onCreated={onCreated}
        onWorkspaceChanged={() => undefined}
        onOpenRemote={() => undefined}
        onBack={() => undefined}
      />,
    );
    await waitFor(() => expect(screen.getByLabelText('关卡模板')).toBeInTheDocument());
    expect(screen.queryByText(/交付流程（6 关 · 按所选模板）/)).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: /查看流程/ }));
    expect(screen.getByText(/交付流程（6 关 · 按所选模板）/)).toBeInTheDocument();
    fireEvent.click(screen.getByLabelText('关卡模板'));
    fireEvent.click(screen.getByRole('button', { name: '三关精简' }));
    expect(screen.queryByRole('dialog', { name: '选择关卡模板' })).not.toBeInTheDocument();
    // 切到自定义模板 → 读激活版本定义，3 关 + 「按所选模板」标注。
    await waitFor(() => expect(screen.getByText(/交付流程（3 关 · 按所选模板）/)).toBeInTheDocument());
    expect(screen.getByText('需求关')).toBeInTheDocument();
    expect(screen.getByText('开发关')).toBeInTheDocument();
    expect(screen.getByText('验证关')).toBeInTheDocument();
  });

  it('"/" 选技能：浮层拾取后以胶囊呈现，提交携带冻结版本 id', async () => {
    rpcMock.mockImplementation((method: string, params?: { templateId?: string }) => {
      if (method === 'skill.activeList') {
        return Promise.resolve({
          items: [
            { skillId: 'skill_a', name: 'side-effect-safety', versionId: 'skv_a', versionNo: 2 },
            { skillId: 'skill_b', name: 'database-sql', versionId: 'skv_b', versionNo: 1 },
          ],
        });
      }
      if (method === 'workitem.create') return Promise.resolve({ id: 'wi_new' });
      if (method === 'stage.startActivity') return Promise.resolve({ runId: 'run_prd' });
      return Promise.resolve({});
    });
    render(
      <NewTaskPage
        projectId="pj_1"
        projects={[{ id: 'pj_1', name: 'Ratiflow' }]}
        onCreated={onCreated}
        onWorkspaceChanged={() => undefined}
        onOpenRemote={() => undefined}
        onBack={() => undefined}
      />,
    );
    const input = screen.getByLabelText('需求描述');
    // 空格后输入 "/" 触发技能浮层。
    fireEvent.change(input, { target: { value: '支付网关重构 /' } });
    const listbox = await screen.findByRole('listbox', { name: '快捷选择' });
    expect(listbox).toBeInTheDocument();
    // 过滤：输入 si 只剩 side-effect-safety。
    fireEvent.change(input, { target: { value: '支付网关重构 /si' } });
    await waitFor(() =>
      expect(screen.queryByRole('option', { name: /database-sql/ })).not.toBeInTheDocument(),
    );
    // Enter 拾取高亮项：触发文本摘除、胶囊呈现。
    fireEvent.keyDown(input, { key: 'Enter' });
    await waitFor(() => expect(screen.queryByRole('listbox')).not.toBeInTheDocument());
    expect(screen.getByText(/side-effect-safety/)).toBeInTheDocument();
    expect(input).toHaveValue('支付网关重构 ');
    // 提交：workitem.create 携带冻结版本 id。
    fireEvent.click(screen.getByRole('button', { name: '创建并进入需求关' }));
    await waitFor(() => expect(onCreated).toHaveBeenCalledWith('wi_new', 'pj_1'));
    expect(rpcMock).toHaveBeenCalledWith(
      'workitem.create',
      expect.objectContaining({ skillVersionIds: ['skv_a'] }),
    );
    // 胶囊可移除。
    fireEvent.click(screen.getByRole('button', { name: '移除技能 side-effect-safety' }));
    expect(screen.queryByText(/side-effect-safety/)).not.toBeInTheDocument();
  });

  it('"@" 指 Agent：拾取后胶囊呈现，提交携带任务级默认 profile 版本', async () => {
    rpcMock.mockImplementation((method: string, params?: { templateId?: string }) => {
      if (method === 'agentProfile.list') {
        return Promise.resolve({
          items: [
            {
              id: 'apr_1',
              name: '规范执行者',
              enabled: true,
              versions: [
                { id: 'apv_1', versionNo: 1 },
                { id: 'apv_2', versionNo: 2 },
              ],
            },
            { id: 'apr_2', name: '已停用', enabled: false, versions: [{ id: 'apv_9', versionNo: 1 }] },
          ],
        });
      }
      if (method === 'workitem.create') return Promise.resolve({ id: 'wi_new' });
      if (method === 'stage.startActivity') return Promise.resolve({ runId: 'run_prd' });
      return Promise.resolve({});
    });
    render(
      <NewTaskPage
        projectId="pj_1"
        projects={[{ id: 'pj_1', name: 'Ratiflow' }]}
        onCreated={onCreated}
        onWorkspaceChanged={() => undefined}
        onOpenRemote={() => undefined}
        onBack={() => undefined}
      />,
    );
    const input = screen.getByLabelText('需求描述');
    fireEvent.change(input, { target: { value: '重构任务 @' } });
    await screen.findByRole('listbox', { name: '快捷选择' });
    // 已停用 profile 不进选择面；最新版本 v2 被冻结。
    fireEvent.keyDown(input, { key: 'Enter' });
    await waitFor(() => expect(screen.getByText(/规范执行者/)).toBeInTheDocument());
    fireEvent.click(screen.getByRole('button', { name: '创建并进入需求关' }));
    await waitFor(() => expect(onCreated).toHaveBeenCalledWith('wi_new', 'pj_1'));
    expect(rpcMock).toHaveBeenCalledWith(
      'workitem.create',
      expect.objectContaining({ agentProfileVersionId: 'apv_2' }),
    );
  });

  it('聚焦空输入框弹触发提示菜单，点选技能行直接唤起浮层', async () => {
    rpcMock.mockImplementation((method: string, params?: { templateId?: string }) => {
      if (method === 'skill.activeList') {
        return Promise.resolve({
          items: [{ skillId: 'skill_a', name: 'side-effect-safety', versionId: 'skv_a', versionNo: 1 }],
        });
      }
      if (method === 'workitem.create') return Promise.resolve({ id: 'wi_new' });
      if (method === 'stage.startActivity') return Promise.resolve({ runId: 'run_prd' });
      return Promise.resolve({});
    });
    render(
      <NewTaskPage
        projectId="pj_1"
        projects={[{ id: 'pj_1', name: 'Ratiflow' }]}
        onCreated={onCreated}
        onWorkspaceChanged={() => undefined}
        onOpenRemote={() => undefined}
        onBack={() => undefined}
      />,
    );
    // placeholder 不再携带触发提示。
    expect(screen.getByPlaceholderText('描述你的需求…')).toBeInTheDocument();
    const input = screen.getByLabelText('需求描述');
    // 聚焦空输入框 → 提示菜单（附件 / @ Agent / / 技能）。
    fireEvent.focus(input);
    const menu = await screen.findByRole('menu', { name: '输入提示' });
    expect(menu).toBeInTheDocument();
    expect(screen.getAllByRole('menuitem').length).toBe(3);
    // 点选「使用 / 选择技能」→ 插入触发符并唤起技能浮层，菜单关闭。
    fireEvent.mouseDown(screen.getByRole('menuitem', { name: /选择技能/ }));
    await screen.findByRole('listbox', { name: '快捷选择' });
    expect(screen.queryByRole('menu', { name: '输入提示' })).not.toBeInTheDocument();
    expect(input).toHaveValue('/');
    // 输入文字后菜单不再出现（仅空输入聚焦时提示）。
    fireEvent.change(input, { target: { value: '支付网关重构' } });
    fireEvent.blur(input);
    fireEvent.focus(input);
    expect(screen.queryByRole('menu', { name: '输入提示' })).not.toBeInTheDocument();
  });

  it('Escape 关闭浮层不拾取；正文中的普通 "/" 不误触发', async () => {
    rpcMock.mockImplementation((method: string, params?: { templateId?: string }) => {
      if (method === 'skill.activeList') {
        return Promise.resolve({
          items: [{ skillId: 'skill_a', name: 'side-effect-safety', versionId: 'skv_a', versionNo: 1 }],
        });
      }
      if (method === 'workitem.create') return Promise.resolve({ id: 'wi_new' });
      if (method === 'stage.startActivity') return Promise.resolve({ runId: 'run_prd' });
      return Promise.resolve({});
    });
    render(
      <NewTaskPage
        projectId="pj_1"
        projects={[{ id: 'pj_1', name: 'Ratiflow' }]}
        onCreated={onCreated}
        onWorkspaceChanged={() => undefined}
        onOpenRemote={() => undefined}
        onBack={() => undefined}
      />,
    );
    const input = screen.getByLabelText('需求描述');
    // 词中 "/"（前一个字符非空白）不触发。
    fireEvent.change(input, { target: { value: '读/写分离' } });
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
    // 行首 "/" 触发后 Escape 关闭、无胶囊。
    fireEvent.change(input, { target: { value: '/' } });
    await screen.findByRole('listbox', { name: '快捷选择' });
    fireEvent.keyDown(input, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('listbox')).not.toBeInTheDocument());
    expect(screen.queryByText(/side-effect-safety/)).not.toBeInTheDocument();
  });
});
