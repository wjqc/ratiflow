// S24 MCP 服务器页：分组渲染 / 启停开关 / 批准 / 撤销两步确认 / 新建表单。
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import '@testing-library/jest-dom/vitest';
import { McpPage } from './McpPage';

const rpcMock = vi.fn();
function ok(body: unknown) { return Promise.resolve(body); }

const ACTIVE = {
  serverId: 'mcp_a', name: 'codegraph', transport: 'stdio',
  command: '/usr/local/bin/codegraph', args: ['serve', '--mcp'],
  status: 'active', probeError: '', enabled: true,
  toolCounts: { active: 2, candidate: 0 },
};
const CANDIDATE = {
  serverId: 'mcp_b', name: 'figma', transport: 'stdio',
  command: 'figma-mcp', args: [],
  status: 'candidate', probeError: '', enabled: true,
  toolCounts: { active: 0, candidate: 3 },
};

function mockList(items: unknown[]) {
  rpcMock.mockImplementation((method: string) => {
    if (method === 'mcp.serverList') return ok({ items });
    return ok({});
  });
}

describe('MCP 服务器页', () => {
  beforeEach(() => {
    cleanup();
    rpcMock.mockReset();
    (window as unknown as { ratiflow: unknown }).ratiflow = {
      rpc: (m: string, p: Record<string, unknown>) => rpcMock(m, p),
      hello: () => Promise.resolve({ ok: true }),
    };
  });
  afterEach(() => { delete (window as unknown as { ratiflow?: unknown }).ratiflow; });

  it('按状态分组渲染并显示计数', async () => {
    mockList([ACTIVE, CANDIDATE]);
    render(<McpPage />);
    await waitFor(() => expect(screen.getByRole('heading', { name: /已安装/ })).toBeInTheDocument());
    expect(screen.getByRole('heading', { name: /待批准/ })).toBeInTheDocument();
    expect(screen.getByText('codegraph')).toBeInTheDocument();
    expect(screen.getByText(/已连接并可用/)).toBeInTheDocument();
    expect(screen.getByText('MCP 2')).toBeInTheDocument();
  });

  it('空列表显示引导文案', async () => {
    mockList([]);
    render(<McpPage />);
    await waitFor(() => expect(screen.getByText(/暂无 MCP 服务器/)).toBeInTheDocument());
  });

  it('旧版内核载荷（无 enabled/toolCounts）不崩溃并渲染候选行', async () => {
    mockList([{
      serverId: 'mcp_old', name: 'codegraph', transport: 'stdio',
      command: 'codegraph', args: ['serve', '--mcp'],
      status: 'candidate', probeError: '',
    }]);
    render(<McpPage />);
    await waitFor(() => expect(screen.getByRole('heading', { name: /待批准/ })).toBeInTheDocument());
    expect(screen.getByText(/候选工具待批准/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: '批准' })).toBeInTheDocument();
  });

  it('活跃服务器开关调用 mcp.serverToggle', async () => {
    mockList([ACTIVE]);
    render(<McpPage />);
    await waitFor(() => expect(screen.getByRole('switch', { name: '启用 codegraph' })).toBeInTheDocument());
    fireEvent.click(screen.getByRole('switch', { name: '启用 codegraph' }));
    await waitFor(() => expect(rpcMock).toHaveBeenCalledWith('mcp.serverToggle', { serverId: 'mcp_a', enabled: false }));
  });

  it('候选服务器走批准 RPC', async () => {
    mockList([CANDIDATE]);
    render(<McpPage />);
    await waitFor(() => expect(screen.getByRole('button', { name: '批准' })).toBeInTheDocument());
    fireEvent.click(screen.getByRole('button', { name: '批准' }));
    await waitFor(() => expect(rpcMock).toHaveBeenCalledWith('mcp.serverApprove', { serverId: 'mcp_b', decidedBy: 'local' }));
  });

  it('撤销需两步确认', async () => {
    mockList([ACTIVE]);
    render(<McpPage />);
    await waitFor(() => expect(screen.getByRole('button', { name: '撤销' })).toBeInTheDocument());
    fireEvent.click(screen.getByRole('button', { name: '撤销' }));
    expect(rpcMock).not.toHaveBeenCalledWith('mcp.serverRemove', expect.anything());
    fireEvent.click(screen.getByRole('button', { name: '确认撤销' }));
    await waitFor(() => expect(rpcMock).toHaveBeenCalledWith('mcp.serverRemove', { serverId: 'mcp_a', decidedBy: 'local', reason: 'settings-ui' }));
  });

  it('新建表单提交 mcp.serverAdd 并校验名称', async () => {
    mockList([]);
    render(<McpPage />);
    await waitFor(() => expect(screen.getByRole('button', { name: '新建' })).toBeInTheDocument());
    fireEvent.click(screen.getByRole('button', { name: '新建' }));
    fireEvent.change(screen.getByLabelText('名称'), { target: { value: '非法名称!' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await waitFor(() => expect(screen.getByText(/名称仅允许字母、数字/)).toBeInTheDocument());
    fireEvent.change(screen.getByLabelText('名称'), { target: { value: 'codegraph' } });
    fireEvent.change(screen.getByLabelText('启动命令'), { target: { value: '/usr/local/bin/codegraph' } });
    fireEvent.change(screen.getByLabelText('启动参数'), { target: { value: 'serve --mcp' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await waitFor(() => expect(rpcMock).toHaveBeenCalledWith('mcp.serverAdd', {
      name: 'codegraph', command: '/usr/local/bin/codegraph', args: ['serve', '--mcp'],
    }));
  });

  it('注册后探针失败显示错误横幅', async () => {
    rpcMock.mockImplementation((method: string) => {
      if (method === 'mcp.serverList') return ok({ items: [] });
      if (method === 'mcp.serverAdd') {
        return ok({ serverId: 'mcp_x', status: 'probe_failed', probeError: '命令不存在' });
      }
      return ok({});
    });
    render(<McpPage />);
    await waitFor(() => expect(screen.getByRole('button', { name: '新建' })).toBeInTheDocument());
    fireEvent.click(screen.getByRole('button', { name: '新建' }));
    fireEvent.change(screen.getByLabelText('名称'), { target: { value: 'bad' } });
    fireEvent.change(screen.getByLabelText('启动命令'), { target: { value: '/no/such/bin' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await waitFor(() => expect(screen.getByText(/探针失败：命令不存在/)).toBeInTheDocument());
  });

  it('JSON 模式解析配置并提交', async () => {
    mockList([]);
    render(<McpPage />);
    await waitFor(() => expect(screen.getByRole('button', { name: '新建' })).toBeInTheDocument());
    fireEvent.click(screen.getByRole('button', { name: '新建' }));
    fireEvent.click(screen.getByRole('tab', { name: 'JSON' }));
    fireEvent.change(screen.getByLabelText('完整配置'), {
      target: { value: JSON.stringify({ codegraph: { type: 'stdio', command: 'codegraph', args: ['serve', '--mcp'] } }) },
    });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await waitFor(() => expect(rpcMock).toHaveBeenCalledWith('mcp.serverAdd', {
      name: 'codegraph', command: 'codegraph', args: ['serve', '--mcp'],
    }));
  });

  // RDWS-006 Windows 产品负例：沙箱限制只针对本地 stdio——远程传输可用。
  it('win32 平台提示 stdio 不可用但保留入口与远程传输', async () => {
    mockList([ACTIVE]);
    (window as unknown as { ratiflow: unknown }).ratiflow = {
      rpc: (m: string, p: Record<string, unknown>) => rpcMock(m, p),
      hello: () => Promise.resolve({ ok: true }),
      platform: () => 'win32' as NodeJS.Platform,
    };
    render(<McpPage />);
    await waitFor(() =>
      expect(screen.getByTestId('stdio-unsupported-banner')).toHaveTextContent('不支持本地 stdio 传输'),
    );
    // 入口保留（远程可用），已有数据照常分组渲染。
    expect(screen.getByRole('button', { name: '新建' })).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: /已安装/ })).toBeInTheDocument();
  });

  // 远程传输表单：streamable-http 注册带 url/headers；sse 同构。
  it('远程表单校验 URL 并提交 transport/url/headers', async () => {
    mockList([]);
    render(<McpPage />);
    await waitFor(() => expect(screen.getByRole('button', { name: '新建' })).toBeInTheDocument());
    fireEvent.click(screen.getByRole('button', { name: '新建' }));
    fireEvent.change(screen.getByLabelText('名称'), { target: { value: 'remote-svc' } });
    fireEvent.change(screen.getByLabelText('MCP 传输类型'), { target: { value: 'streamable-http' } });
    // 空 URL → 前端拦截。
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await waitFor(() =>
      expect(screen.getByText(/远程端点必填（绝对 http\/https URL）/)).toBeInTheDocument(),
    );
    expect(rpcMock.mock.calls.filter(([m]) => m === 'mcp.serverAdd')).toHaveLength(0);
    fireEvent.change(screen.getByLabelText('远程端点'), { target: { value: 'https://example.com/mcp' } });
    fireEvent.change(screen.getByLabelText(/静态头/), { target: { value: '{"Authorization": "Bearer t"}' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await waitFor(() => expect(rpcMock).toHaveBeenCalledWith('mcp.serverAdd', {
      name: 'remote-svc',
      transport: 'streamable-http',
      url: 'https://example.com/mcp',
      headers: { Authorization: 'Bearer t' },
    }));
  });
});
