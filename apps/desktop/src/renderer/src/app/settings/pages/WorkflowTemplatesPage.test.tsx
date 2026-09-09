// S13 工作流与关卡模板页：列表/复制草稿/编辑保存/激活 契约单测。
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import '@testing-library/jest-dom/vitest';
import { WorkflowTemplatesPage } from './WorkflowTemplatesPage';

const rpcMock = vi.fn();
function ok(body: unknown) { return Promise.resolve(body); }

const GATES = [
  { gate_id: 'requirements', title: '需求关', purpose: '澄清需求', deliverables: ['doc', 'prd'], acceptance: ['PRD 已确认'], context_policy_ref: null, team_policy_ref: null, workspace_policy_ref: null },
  { gate_id: 'fix', title: '修复关', purpose: '', deliverables: ['code'], acceptance: [], context_policy_ref: null, team_policy_ref: null, workspace_policy_ref: null },
];

const TEMPLATES = [
  {
    id: 'wtpl_1', key: 'six-gate-default', name: '默认六关', createdAt: '', updatedAt: '',
    versions: [
      { id: 'wfv_v1', template_id: 'wtpl_1', version_no: 1, status: 'active', content_digest: 'aabbccdd1122', created_by: 'migration', created_at: '', updated_at: '2026-09-01T10:00:00.000Z' },
      { id: 'wfv_v2', template_id: 'wtpl_1', version_no: 2, status: 'draft', content_digest: 'eeff00112233', created_by: 'local', created_at: '', updated_at: '2026-09-07T10:00:00.000Z' },
    ],
  },
];

describe('工作流与关卡模板页', () => {
  beforeEach(() => {
    cleanup();
    rpcMock.mockReset();
    rpcMock.mockImplementation((method: string, params: Record<string, unknown>) => {
      switch (method) {
        case 'workflowTemplate.list':
          return ok({ items: TEMPLATES });
        case 'workflowTemplate.get':
          if (params.versionId) {
            return ok({
              version: { version: TEMPLATES[0].versions.find((v) => v.id === params.versionId), gates: GATES },
            });
          }
          return ok({ activeVersion: { version: TEMPLATES[0].versions[0], gates: GATES } });
        case 'workflowTemplate.create':
          return ok({ template: { id: 'wtpl_1', key: 'six-gate-default', name: '默认六关' }, version: { id: 'wfv_v3', version_no: 3, status: 'draft' } });
        case 'workflowTemplate.updateDraft':
          return ok({ id: params.versionId, version_no: 2, status: 'draft' });
        case 'workflowTemplate.activate':
          return ok({ id: params.versionId, version_no: 2, status: 'active' });
        default:
          return ok({});
      }
    });
    (window as unknown as { ratiflow: unknown }).ratiflow = {
      rpc: (m: string, p: Record<string, unknown>) => rpcMock(m, p),
      hello: () => Promise.resolve({ ok: true }),
      selectFile: () => Promise.resolve(null),
      selectDirectory: () => Promise.resolve(null),
      openExternal: () => Promise.resolve(),
      appInfo: () => Promise.resolve({ desktopVersion: 'test', logDir: '', userDataDir: '' }),
      openLogs: () => Promise.resolve(),
      onEvent: () => () => undefined,
    };
  });
  afterEach(() => { delete (window as unknown as { ratiflow?: unknown }).ratiflow; });

  it('列表展示模板与版本状态；默认模板不提供弃用入口', async () => {
    render(<WorkflowTemplatesPage />);
    await waitFor(() => expect(screen.getByText('默认六关')).toBeInTheDocument());
    expect(screen.getByText('six-gate-default（默认）')).toBeInTheDocument();
    expect(screen.getByText('使用中')).toBeInTheDocument();
    expect(screen.getByText('草稿')).toBeInTheDocument();
    // 默认模板弃用会令 default_active_version_id 拒服务，页面必须隐藏弃用按钮。
    expect(screen.queryByRole('button', { name: '弃用' })).not.toBeInTheDocument();
    expect(screen.getByText('有未激活的草稿 v2，点击上方「编辑」继续调整。')).toBeInTheDocument();
  });

  it('复制为新草稿：以激活版本 gates 调 create（同 key 追加 draft）', async () => {
    render(<WorkflowTemplatesPage />);
    await waitFor(() => expect(screen.getByText('默认六关')).toBeInTheDocument());
    fireEvent.click(screen.getByRole('button', { name: '复制为新草稿' }));
    await waitFor(() =>
      expect(rpcMock).toHaveBeenCalledWith(
        'workflowTemplate.create',
        expect.objectContaining({
          key: 'six-gate-default',
          gates: expect.arrayContaining([
            expect.objectContaining({ gateId: 'requirements', deliverables: ['doc', 'prd'] }),
          ]),
        }),
      ),
    );
  });

  it('草稿编辑：按 versionId 读回，保存发送 camelCase gates 到 updateDraft；保存并激活追加 activate', async () => {
    render(<WorkflowTemplatesPage />);
    await waitFor(() => expect(screen.getByText('默认六关')).toBeInTheDocument());
    fireEvent.click(screen.getByRole('button', { name: '编辑' }));
    // 读草稿定义走 versionId 参数（本次功能新增的后端读取口）。
    await waitFor(() =>
      expect(rpcMock).toHaveBeenCalledWith('workflowTemplate.get', { templateId: 'wtpl_1', versionId: 'wfv_v2' }),
    );
    const title = await screen.findByDisplayValue('需求关');
    fireEvent.change(title, { target: { value: '需求澄清关' } });
    fireEvent.click(screen.getByRole('button', { name: '保存草稿' }));
    await waitFor(() =>
      expect(rpcMock).toHaveBeenCalledWith(
        'workflowTemplate.updateDraft',
        expect.objectContaining({
          versionId: 'wfv_v2',
          gates: expect.arrayContaining([
            expect.objectContaining({ gateId: 'requirements', title: '需求澄清关', acceptance: ['PRD 已确认'] }),
          ]),
        }),
      ),
    );
    expect(rpcMock).not.toHaveBeenCalledWith('workflowTemplate.activate', expect.anything());
    // 保存并激活：先 updateDraft 再 activate，一并在案。
    fireEvent.click(screen.getByRole('button', { name: '保存并激活' }));
    await waitFor(() =>
      expect(rpcMock).toHaveBeenCalledWith('workflowTemplate.activate', { versionId: 'wfv_v2' }),
    );
  });

  it('功能门控关闭（feature_disabled）时页面降级提示且不展示新建入口', async () => {
    rpcMock.mockImplementation(() => Promise.reject(new Error('feature_disabled: RATIFLOW_WORKFLOW_TEMPLATE_V2 未开启')));
    render(<WorkflowTemplatesPage />);
    await waitFor(() =>
      expect(screen.getByText(/关卡模板功能已关闭/)).toBeInTheDocument(),
    );
    expect(screen.getByRole('button', { name: /新建模板/ })).toBeDisabled();
  });

  it('查看流程：非草稿版本可只读展开关卡定义', async () => {
    render(<WorkflowTemplatesPage />);
    await waitFor(() => expect(screen.getByText('默认六关')).toBeInTheDocument());
    fireEvent.click(screen.getByRole('button', { name: '查看流程' }));
    await waitFor(() =>
      expect(rpcMock).toHaveBeenCalledWith('workflowTemplate.get', { templateId: 'wtpl_1', versionId: 'wfv_v1' }),
    );
    expect(await screen.findByText(/第 1 关 · 需求关/)).toBeInTheDocument();
    expect(screen.getByText(/交付物：doc、prd/)).toBeInTheDocument();
    expect(screen.getByText('PRD 已确认')).toBeInTheDocument();
    // 只读查看不出现编辑控件。
    expect(screen.queryByRole('button', { name: '添加关卡' })).not.toBeInTheDocument();
  });
});
