import { useEffect, useRef, useState } from 'react';
import type { KeyboardEvent } from 'react';
import { rpc, rpcErrorMessage } from '../rpc/client';
import type { PaletteItem } from './QuickPalette';

type Choice = PaletteItem & { versionId: string };
type Kind = 'agent' | 'skill';

/** Detail-page assignments use the same frozen task defaults as NewTaskPage. */
export function useComposerAssignments(text: string, setText: (text: string) => void) {
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const [picker, setPicker] = useState<{ kind: Kind; start: number; end: number; query: string } | null>(null);
  const [options, setOptions] = useState<Choice[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');
  const [highlight, setHighlight] = useState(0);
  const [agent, setAgent] = useState<Choice | null>(null);
  const [skills, setSkills] = useState<Choice[]>([]);
  const [agentChanged, setAgentChanged] = useState(false);
  const [skillsChanged, setSkillsChanged] = useState(false);

  useEffect(() => {
    if (!picker) return;
    let cancelled = false;
    setLoading(true);
    setError('');
    setOptions([]);
    const load = async (): Promise<Choice[]> => {
      if (picker.kind === 'skill') {
        const result = await rpc<{ items: { name: string; versionId: string; versionNo: number }[] }>('skill.activeList', {});
        return result.items.map((s) => ({ id: s.versionId, label: s.name, hint: `v${s.versionNo}`, versionId: s.versionId }));
      }
      const result = await rpc<{ items: { id: string; name: string; enabled?: boolean; versions?: { id: string; versionNo: number }[] }[] }>('agentProfile.list', {});
      return result.items.filter((a) => a.enabled !== false && a.versions?.length).map((a) => {
        const latest = a.versions!.reduce((x, y) => x.versionNo > y.versionNo ? x : y);
        return { id: a.id, label: a.name, hint: 'Agent', versionId: latest.id };
      });
    };
    void load().then((items) => { if (!cancelled) setOptions(items); }).catch((reason) => {
      if (!cancelled) setError(`加载失败：${rpcErrorMessage(reason)}`);
    }).finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [picker?.kind]);

  const items = options.filter((item) => item.label.toLowerCase().includes(picker?.query.toLowerCase() ?? ''));
  const open = (kind: Kind) => {
    const base = text.trimEnd();
    const value = `${base}${base ? ' ' : ''}${kind === 'agent' ? '@' : '/'}`;
    setText(value);
    setPicker({ kind, start: value.length - 1, end: value.length, query: '' });
    setHighlight(0);
    inputRef.current?.focus();
  };
  const change = (value: string, caret: number) => {
    setText(value);
    const match = /(?:^|\s)([@/])([^\s@/]*)$/.exec(value.slice(0, caret));
    setPicker(match ? { kind: match[1] === '@' ? 'agent' : 'skill', start: caret - match[2].length - 1, end: caret, query: match[2] } : null);
    setHighlight(0);
  };
  const pick = (item: PaletteItem) => {
    if (!picker) return;
    const choice = options.find((option) => option.id === item.id);
    if (!choice) return;
    if (picker.kind === 'agent') { setAgent(choice); setAgentChanged(true); }
    else {
      setSkills((prev) => prev.some((s) => s.versionId === choice.versionId) ? prev : [...prev, choice]);
      setSkillsChanged(true);
    }
    setText(text.slice(0, picker.start) + text.slice(picker.end));
    setPicker(null);
    inputRef.current?.focus();
  };
  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (!picker) return false;
    if (event.key === 'Escape') { event.preventDefault(); setPicker(null); return true; }
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      setHighlight((i) => (i + (event.key === 'ArrowDown' ? 1 : -1) + Math.max(items.length, 1)) % Math.max(items.length, 1));
      return true;
    }
    if (event.key === 'Enter') {
      event.preventDefault();
      if (items.length) pick(items[highlight % items.length]);
      return true;
    }
    return false;
  };
  const apply = async (workItemId: string) => {
    if (!agentChanged && !skillsChanged) return;
    await rpc('workitem.updateRunDefaults', {
      workItemId,
      ...(agentChanged ? { agentProfileVersionId: agent?.versionId ?? null } : {}),
      ...(skillsChanged ? { skillVersionIds: skills.map((s) => s.versionId) } : {}),
    });
  };
  return {
    inputRef, picker, items, highlight, agent, skills, open, change, pick, onKeyDown, apply,
    close: () => setPicker(null),
    removeAgent: () => { setAgent(null); setAgentChanged(true); },
    removeSkill: (id: string) => { setSkills((prev) => prev.filter((s) => s.versionId !== id)); setSkillsChanged(true); },
    emptyText: loading ? '加载中…' : error || (picker?.kind === 'agent' ? '暂无匹配的可用 Agent，请在设置中配置。' : '暂无匹配的已启用技能，请在设置中配置。'),
  };
}
