import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import '@testing-library/jest-dom/vitest';
import ApprovalsPage from './ApprovalsPage';

// WP-6（RDWS v1.4 A3）三要素组件测试：mock preload bridge，不依赖 Electron/Rust core。
const rpcMock = vi.fn();

function ok(body: unknown) {
  return Promise.resolve(body);
}

const TOOL_APPROVAL = {
  id: 'apr_1',
  subject_type: 'tool_proposal',
  subject_id: 'tp_1',
  risk: 'high',
  status: 'requested',
  reason: 'run run_1 tool apply_patch',
  expires_at: '2099-01-01T00:00:00Z',
  action_digest: 'd',
  impact_digest: 'sha256:abc',
  scope_facts_digest: '',
  digest_schema_version: 1,
  rationale: '需要修改部署脚本以支持回滚参数',
  confidence: 0.87,
  impactCompleteness: 'complete',
  impactNodeCount: 6,
};

describe('ApprovalsPage WP-6 三要素', () => {
  beforeEach(() => {
    cleanup();
    rpcMock.mockReset();
    rpcMock.mockImplementation((method: string) => {
      if (method === 'approval.list') return ok({ items: [TOOL_APPROVAL] });
      return ok({});
    });
    (window as unknown as { sixgates: unknown }).sixgates = {
      rpc: (method: string, params: Record<string, unknown>) => rpcMock(method, params),
      hello: () => Promise.resolve({ ok: true }),
      onEvent: () => () => {},
    };
  });

  afterEach(() => {
    delete (window as unknown as { sixgates?: unknown }).sixgates;
  });

  it('工具提案展示影响面 completeness（服务端权威）', async () => {
    render(<ApprovalsPage onDecided={() => {}} />);
    await waitFor(() =>
      expect(screen.getByTestId('impact-completeness')).toHaveTextContent('全图可达'),
    );
    expect(screen.getByText(/6 个关联节点/)).toBeInTheDocument();
    // 漂移失效提示（双 digest 绑定的治理语义面）。
    expect(screen.getByText(/影响面漂移将使本审批自动失效/)).toBeInTheDocument();
  });

  it('模型理由与置信度带「模型自报」标注（untrusted_display）', async () => {
    render(<ApprovalsPage onDecided={() => {}} />);
    await waitFor(() => expect(screen.getByTestId('model-rationale')).toBeInTheDocument());
    expect(screen.getByTestId('model-rationale')).toHaveTextContent('（模型自报）');
    expect(screen.getByTestId('model-confidence')).toHaveTextContent('87%');
    expect(screen.getByTestId('model-confidence')).toHaveTextContent('不参与自动判定');
  });

  it('旧版审批（未绑定影响面）如实留空，不虚构字段', async () => {
    rpcMock.mockImplementation((method: string) => {
      if (method === 'approval.list') {
        return ok({
          items: [{ ...TOOL_APPROVAL, id: 'apr_2', impact_digest: '', impactCompleteness: undefined, impactNodeCount: undefined, rationale: undefined, confidence: null }],
        });
      }
      return ok({});
    });
    render(<ApprovalsPage onDecided={() => {}} />);
    await waitFor(() => expect(screen.getByText('未绑定（旧版审批）')).toBeInTheDocument());
    expect(screen.queryByTestId('model-rationale')).not.toBeInTheDocument();
    expect(screen.queryByTestId('model-confidence')).not.toBeInTheDocument();
  });
});
