import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { draftPrd, friendlyAgentError } from './prdDraft';

describe('PRD 自动起草', () => {
  const rpcMock = vi.fn();
  let artifactCreated = false;

  beforeEach(() => {
    artifactCreated = false;
    rpcMock.mockReset();
    rpcMock.mockImplementation((method: string) => {
      switch (method) {
        case 'artifact.list':
          return Promise.resolve({
            items: artifactCreated ? [{ id: 'ar_1', kind: 'prd', title: 'PRD' }] : [],
          });
        case 'artifact.create':
          artifactCreated = true;
          return Promise.resolve({ id: 'ar_1', kind: 'prd', title: 'PRD' });
        case 'stage.startActivity':
          return Promise.resolve({ runId: 'run_1' });
        case 'agent.get':
          return Promise.resolve({ id: 'run_1', status: 'completed_execution', result: '# PRD\n\n已起草' });
        case 'artifact.listRevisions':
          return Promise.resolve({ items: [] });
        case 'trace.coverage':
          return Promise.resolve({ items: [{ requirementKey: 'REQ-001', status: 'active' }] });
        case 'artifact.createDraft':
          return Promise.resolve({ id: 'rev_1' });
        default:
          return Promise.resolve({});
      }
    });
    (window as unknown as { sixgates: unknown }).sixgates = {
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
    delete (window as unknown as { sixgates?: unknown }).sixgates;
  });

  it('创建 PRD 工件、启动 Agent 并自动保存草稿', async () => {
    const content = await draftPrd('wi_1', '支持工作区选择', 'auto-prd-wi_1');

    expect(content).toContain('# PRD');
    expect(rpcMock).toHaveBeenCalledWith(
      'stage.startActivity',
      expect.objectContaining({
        workItemId: 'wi_1',
        gate: 'requirements',
        idempotencyKey: 'auto-prd-wi_1',
      }),
    );
    expect(rpcMock).toHaveBeenCalledWith(
      'artifact.createDraft',
      expect.objectContaining({
        artifactId: 'ar_1',
        content: '# PRD\n\n已起草',
        requirementKeys: ['REQ-001'],
      }),
    );
  });

  it('把内部模型错误转换为可操作提示', () => {
    expect(friendlyAgentError('model:model_unavailable: script exhausted')).toContain('设置与诊断');
    expect(friendlyAgentError('model_timeout: timed out')).toContain('需求已经保存');
  });
});
