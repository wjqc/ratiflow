import { rpc, rpcErrorMessage, waitForRunTerminal } from '../rpc/client';

interface ArtifactInfo {
  id: string;
  kind: string;
}

interface RevisionInfo {
  id: string;
  status: string;
  etag: string;
}

interface PendingPrdDraft {
  runId?: string;
  state: 'running' | 'failed';
  error?: string;
}

const pendingKey = (workItemId: string) => `sg:auto-prd:${workItemId}`;

export function friendlyAgentError(value: unknown): string {
  const raw = rpcErrorMessage(value);
  if (raw.includes('script exhausted') || raw.includes('未找到可用模型')) {
    return '没有可用模型。请先到“设置与诊断 → 模型”完成连接测试，然后重试。';
  }
  if (raw.includes('api key missing') || raw.includes('CREDENTIAL_MISSING')) {
    return '模型凭据不可用。请重新保存 API Key 并测试连接。';
  }
  if (raw.includes('model_rate_limited') || raw.includes('MODEL_RATE_LIMITED')) {
    return '模型请求过于频繁，请稍后重试或切换模型。';
  }
  if (raw.includes('model_timeout') || raw.includes('TIMEOUT')) {
    return '模型响应超时。需求已经保存，你可以直接重试起草。';
  }
  if (raw.includes('model_unavailable') || raw.includes('MODEL_UNAVAILABLE')) {
    return '当前模型无法生成内容。请测试连接、切换模型后重试。';
  }
  return raw.replace(/^Agent 状态\s+failed[:：]\s*/i, '');
}

export async function ensurePrdArtifact(workItemId: string): Promise<ArtifactInfo> {
  const page = await rpc<{ items: ArtifactInfo[] }>('artifact.list', { workItemId });
  const existing = page.items.find((item) => item.kind === 'prd');
  if (existing) return existing;
  return rpc<ArtifactInfo>('artifact.create', {
    workItemId,
    kind: 'prd',
    title: 'PRD',
  });
}

export async function startPrdDraft(
  workItemId: string,
  requirementText: string,
  idempotencyKey: string,
): Promise<string> {
  await ensurePrdArtifact(workItemId);
  const goal = [
    '根据用户需求与当前项目知识库起草一份可评审的 PRD。',
    '必须包含：背景与目标、范围、非目标、用户故事、功能需求、验收标准、风险与待确认项。',
    '直接输出 Markdown 正文，不要解释起草过程。',
    requirementText.trim() ? `\n用户需求：\n${requirementText.trim()}` : '',
  ].join('\n');
  const started = await rpc<{ runId: string }>('stage.startActivity', {
    workItemId,
    gate: 'requirements',
    goal,
    toolAllowlist: ['read_file', 'search_knowledge'],
    idempotencyKey,
  });
  return started.runId;
}

export async function finishPrdDraft(workItemId: string, runId: string): Promise<string> {
  const run = await waitForRunTerminal(runId);
  if (run.status !== 'completed_execution') {
    throw new Error(friendlyAgentError(`Agent 状态 ${run.status}：${run.result}`));
  }
  const artifact = await ensurePrdArtifact(workItemId);
  const page = await rpc<{ items: RevisionInfo[] }>('artifact.listRevisions', {
    artifactId: artifact.id,
  });
  const current = page.items.find((item) => item.status !== 'superseded') ?? null;
  if (current?.status === 'draft') {
    await rpc('artifact.updateDraft', {
      revisionId: current.id,
      etag: current.etag,
      content: run.result,
    });
  } else {
    await rpc('artifact.createDraft', {
      artifactId: artifact.id,
      content: run.result,
      requirementKeys: await activeRequirementKeys(workItemId),
    });
  }
  return run.result;
}

export async function draftPrd(
  workItemId: string,
  requirementText: string,
  idempotencyKey: string,
): Promise<string> {
  const runId = await startPrdDraft(workItemId, requirementText, idempotencyKey);
  return finishPrdDraft(workItemId, runId);
}

export function rememberAutomaticPrd(workItemId: string, pending: PendingPrdDraft): void {
  try {
    localStorage.setItem(pendingKey(workItemId), JSON.stringify(pending));
  } catch {
    // 本地存储不可用时，任务本身仍已创建；工作台允许手动重试。
  }
}

export function readAutomaticPrd(workItemId: string): PendingPrdDraft | null {
  try {
    const raw = localStorage.getItem(pendingKey(workItemId));
    return raw ? (JSON.parse(raw) as PendingPrdDraft) : null;
  } catch {
    return null;
  }
}

export function clearAutomaticPrd(workItemId: string): void {
  try {
    localStorage.removeItem(pendingKey(workItemId));
  } catch {
    // ignore
  }
}

async function activeRequirementKeys(workItemId: string): Promise<string[]> {
  try {
    const coverage = await rpc<{ items: { requirementKey: string; status: string }[] }>(
      'trace.coverage',
      { workItemId },
    );
    return coverage.items
      .filter((item) => item.status === 'active')
      .map((item) => item.requirementKey);
  } catch {
    return [];
  }
}
