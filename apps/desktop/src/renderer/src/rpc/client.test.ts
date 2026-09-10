import { afterEach, describe, expect, it, vi } from 'vitest';
import { rpc } from './client';

function bridgeMock(): ReturnType<typeof vi.fn> {
  const mock = vi.fn();
  (window as unknown as { ratiflow: unknown }).ratiflow = { rpc: mock };
  return mock;
}

describe('rpc 信封还原', () => {
  afterEach(() => {
    delete (window as unknown as { ratiflow?: unknown }).ratiflow;
    vi.restoreAllMocks();
  });

  it('成功信封解包 result', async () => {
    bridgeMock().mockResolvedValue({ __sgRpc: true, ok: true, result: { items: [1] } });
    await expect(rpc<{ items: number[] }>('plan.list', {})).resolves.toEqual({ items: [1] });
  });

  it('错误信封还原为 Error，message 保留 feature_disabled 语义供静默降级', async () => {
    bridgeMock().mockResolvedValue({
      __sgRpc: true,
      ok: false,
      error: { message: 'feature_disabled: RATIFLOW_PLAN_DAG 未开启', code: 'invalid_request' },
    });
    const err = (await rpc('plan.get', {}).catch((e: unknown) => e)) as Error & { code?: string };
    expect(err).toBeInstanceOf(Error);
    expect(err.message).toBe('feature_disabled: RATIFLOW_PLAN_DAG 未开启');
    expect(err.code).toBe('invalid_request');
  });

  it('无信封标记的桥接返回原样透传（测试 mock 直连兼容）', async () => {
    bridgeMock().mockResolvedValue({ items: [{ id: 'plan_1' }] });
    await expect(rpc('plan.list', {})).resolves.toEqual({ items: [{ id: 'plan_1' }] });
  });

  it('桥接自身 rejection 原样上抛（白名单等 loud 校验路径不变）', async () => {
    bridgeMock().mockRejectedValue(new Error("Error invoking remote method 'sg:rpc': rpc 方法不在契约白名单内"));
    await expect(rpc('nope', {})).rejects.toThrow('契约白名单');
  });
});
