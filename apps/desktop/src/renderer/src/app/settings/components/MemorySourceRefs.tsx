// S12 来源引用视图：kind + 定位 + 关系 + 摘要哈希前缀（MEM-018 追溯；不显示任何正文）。
import type { MemorySourceRefView } from '../types';

const KIND_LABEL: Record<string, string> = {
  manual: '人工输入',
  run: 'Run',
  workitem: '工作项',
  artifact: '工件',
  evidence: '证据',
  requirement: '需求',
  import: '导入',
};

const RELATION_LABEL: Record<string, string> = {
  derived_from: '派生自',
  summarizes: '摘要自',
  corrects: '修正',
};

export function MemorySourceRefs({ sources }: { sources: MemorySourceRefView[] }) {
  if (sources.length === 0) {
    return <p className="sg-hint">（无来源记录）</p>;
  }
  return (
    <ul className="sg-memory-sources" aria-label="来源引用">
      {sources.map((s, i) => (
        <li key={`${s.sourceKind}-${i}`} className="sg-memory-source">
          <span className="sg-memory-chip">{KIND_LABEL[s.sourceKind] ?? s.sourceKind}</span>
          <span className="sg-memory-source-loc">{s.locator || s.sourceId || '（未登记定位）'}</span>
          <span className="sg-memory-source-meta">
            {RELATION_LABEL[s.relation] ?? s.relation}
            {s.sourceDigest ? ` · ${s.sourceDigest.slice(0, 15)}…` : ''}
          </span>
        </li>
      ))}
    </ul>
  );
}
