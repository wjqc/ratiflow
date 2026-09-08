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
    rpcMock.mockImplementation((method: string) => {
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

  it('胶囊上下文行：工作区与关卡选择器同排内联；不再有 label/PRD 提示与管理按钮', async () => {
    rpcMock.mockImplementation((method: string) => {
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
    // 胶囊行：工作区胶囊与关卡胶囊都在 sg-nt-context-pills 容器内。
    const pills = document.querySelector('.sg-nt-context-pills');
    expect(pills).not.toBeNull();
    expect(pills!.querySelector('[aria-label="关卡模板"]')).not.toBeNull();
    expect(screen.getByText('工作区')).toBeInTheDocument();
    expect(screen.getByText('关卡')).toBeInTheDocument();
    // 删除项：旧 label / PRD 提示文字 / 管理按钮 不复存在。
    expect(screen.queryByText(/PRD 将结合此工作区的代码与知识库起草/)).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: '管理关卡模板' })).not.toBeInTheDocument();
  });

  it('交付流程按所选模板动态展示：标签带「按所选模板」标注', async () => {
    rpcMock.mockImplementation((method: string) => {
      if (method === 'workflowTemplate.list') {
        return Promise.resolve({
          items: [
            { key: 'six-gate-default', name: '默认六关', versions: [{ status: 'active' }] },
            { key: 'tri-gate', name: '三关精简', versions: [{ status: 'active' }] },
          ],
        });
      }
      if (method === 'workflowTemplate.get') {
        return Promise.resolve({
          activeVersion: {
            gates: [
              { gate_id: 'requirements', title: '需求关', purpose: '澄清' },
              { gate_id: 'development', title: '开发关', purpose: '编码' },
              { gate_id: 'verification', title: '验证关', purpose: '验收' },
            ],
          },
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
    // 默认模板：本地六关常量。
    await waitFor(() => expect(screen.getByText(/交付流程（6 关）/)).toBeInTheDocument());
    fireEvent.change(screen.getByLabelText('关卡模板'), { target: { value: 'tri-gate' } });
    // 切到自定义模板 → 读激活版本定义，3 关 + 「按所选模板」标注。
    await waitFor(() => expect(screen.getByText(/交付流程（3 关 · 按所选模板）/)).toBeInTheDocument());
    expect(screen.getByText('需求关')).toBeInTheDocument();
    expect(screen.getByText('开发关')).toBeInTheDocument();
    expect(screen.getByText('验证关')).toBeInTheDocument();
  });
});
