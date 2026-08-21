import { useCallback, useEffect, useState } from 'react';
import { createWorkItem, listWorkItems } from './api';
import type { WorkItem } from './types';
import { gateLabels } from './types';

interface Props {
  projectId: string;
  onOpenDetail: (id: string) => void;
}

export default function WorkItemsPage({ projectId, onOpenDetail }: Props) {
  const [items, setItems] = useState<WorkItem[]>([]);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(true);
  const [title, setTitle] = useState('');
  const [issueIid, setIssueIid] = useState('');

  const refresh = useCallback(async () => {
    if (!projectId) {
      setItems([]);
      setLoading(false);
      return;
    }
    setLoading(true);
    setError('');
    try {
      const page = await listWorkItems(projectId);
      setItems(page.items);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '加载工作项失败');
    } finally {
      setLoading(false);
    }
  }, [projectId]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!title.trim()) {
      return;
    }
    try {
      await createWorkItem({
        projectId,
        title: title.trim(),
        gitlabIssueIid: issueIid.trim() || undefined,
      });
      setTitle('');
      setIssueIid('');
      await refresh();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '创建工作项失败');
    }
  };

  return (
    <div className="stack">
      <div className="page-heading">
        <div>
          <h1>工作项</h1>
          <p>从 GitLab Issue 或本机输入创建需求，六关状态按门禁证据推进。</p>
        </div>
      </div>
      {error ? <div className="error-banner" role="alert">{error}</div> : null}

      <form className="panel create-form" onSubmit={submit} aria-label="创建工作项">
        <label className="field">
          <span>标题</span>
          <input value={title} onChange={(e) => setTitle(e.target.value)} placeholder="例如：支持 SSO 登录" required />
        </label>
        <label className="field field-narrow">
          <span>GitLab Issue IID</span>
          <input value={issueIid} onChange={(e) => setIssueIid(e.target.value)} placeholder="42" inputMode="numeric" />
        </label>
        <button className="primary-button" type="submit">创建工作项</button>
      </form>

      <section className="panel" aria-labelledby="workitem-list-title">
        <h2 id="workitem-list-title">工作项列表</h2>
        {loading ? (
          <div className="panel loading-panel">正在读取工作项…</div>
        ) : items.length === 0 ? (
          <div className="empty-state">尚无工作项；创建第一个需求开始六关流程。</div>
        ) : (
          <div className="integration-list">
            <div className="integration-header" aria-hidden="true">
              <span>标题</span><span>当前关</span><span>Issue</span><span>操作</span>
            </div>
            {items.map((item) => (
              <div className="integration-row" key={item.id}>
                <div className="service-name">{item.title}</div>
                <div><span className={`stage-chip stage-${item.currentGate}`}>{gateLabels[item.currentGate]}</span></div>
                <div className="detail">{item.gitlabIssueIid ? `#${item.gitlabIssueIid}` : '—'}</div>
                <div>
                  <button className="row-action" type="button" onClick={() => onOpenDetail(item.id)}>查看六关</button>
                </div>
              </div>
            ))}
          </div>
        )}
      </section>
    </div>
  );
}
