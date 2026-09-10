import { act, renderHook, waitFor } from '@testing-library/react';
import { useState } from 'react';
import { beforeEach, expect, it, vi } from 'vitest';
import { rpc } from '../rpc/client';
import { useComposerAssignments } from './useComposerAssignments';
vi.mock('../rpc/client', () => ({ rpc: vi.fn(), rpcErrorMessage: (e: Error) => e.message }));
const mockRpc = vi.mocked(rpc);
beforeEach(() => {
  mockRpc.mockReset();
  mockRpc.mockImplementation(async (method) => {
    if (method === 'agentProfile.list') return { items: [{ id: 'a', name: 'Writer', versions: [{ id: 'a1', versionNo: 1 }, { id: 'a2', versionNo: 2 }] }] };
    if (method === 'skill.activeList') return { items: [{ name: 'Review', versionId: 's1', versionNo: 1 }] };
    return {};
  });
});
const setup = () => renderHook(() => { const [text, setText] = useState('保留正文'); return { text, ...useComposerAssignments(text, setText) }; });
it('freezes selected versions, removes triggers, and persists only changed fields', async () => {
  const { result } = setup();
  await act(async () => result.current.apply('w'));
  expect(mockRpc).not.toHaveBeenCalled();
  act(() => result.current.open('agent'));
  await waitFor(() => expect(result.current.items).toHaveLength(1));
  act(() => result.current.pick(result.current.items[0]));
  expect(result.current.text).toBe('保留正文 ');
  await act(async () => result.current.apply('w'));
  expect(mockRpc).toHaveBeenLastCalledWith('workitem.updateRunDefaults', { workItemId: 'w', agentProfileVersionId: 'a2' });
  act(() => result.current.change('保留正文 /Rev', 9));
  await waitFor(() => expect(result.current.items[0]?.label).toBe('Review'));
  act(() => result.current.pick(result.current.items[0]));
  await act(async () => result.current.apply('w'));
  expect(mockRpc).toHaveBeenLastCalledWith('workitem.updateRunDefaults', { workItemId: 'w', agentProfileVersionId: 'a2', skillVersionIds: ['s1'] });
  act(() => { result.current.removeAgent(); result.current.removeSkill('s1'); });
  await act(async () => result.current.apply('w'));
  expect(mockRpc).toHaveBeenLastCalledWith('workitem.updateRunDefaults', { workItemId: 'w', agentProfileVersionId: null, skillVersionIds: [] });
});
it('reports catalog errors and propagates persistence failure so execution cannot start', async () => {
  const { result } = setup();
  act(() => result.current.open('agent'));
  await waitFor(() => expect(result.current.items).toHaveLength(1));
  act(() => result.current.pick(result.current.items[0]));
  mockRpc.mockRejectedValue(new Error('unavailable'));
  await expect(result.current.apply('w')).rejects.toThrow('unavailable');
  act(() => result.current.open('skill'));
  await waitFor(() => expect(result.current.emptyText).toContain('加载失败：unavailable'));
});
