import type {
  AgentRunInfo,
  Approval,
  Artifact,
  DeploymentInfo,
  DiagnosticReport,
  Evidence,
  GateResultInfo,
  PassportInfo,
  RevisionInfo,
  Stage,
  WorkItem,
} from './types';

// 会话令牌只存在本机 localStorage（loopback 单用户模型；无 storage 环境退化为内存）。
const TOKEN_KEY = 'sixgates.session-token';
let memoryToken = '';

export function storedToken(): string {
  try {
    return window.localStorage?.getItem(TOKEN_KEY) ?? memoryToken;
  } catch {
    return memoryToken;
  }
}

function storeToken(token: string): void {
  memoryToken = token;
  try {
    window.localStorage?.setItem(TOKEN_KEY, token);
  } catch {
    // 无 localStorage（如测试环境）时仅保存在内存。
  }
}

export async function ensureSession(): Promise<void> {
  if (storedToken() !== '') {
    return;
  }
  const response = await fetch('/api/v1/auth/sessions', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ label: 'web-console' }),
  });
  if (!response.ok) {
    throw new Error('创建本机会话失败');
  }
  const body = (await response.json()) as { token: string };
  storeToken(body.token);
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  await ensureSession();
  const response = await fetch(path, {
    ...init,
    headers: {
      ...(init?.body ? { 'Content-Type': 'application/json' } : {}),
      Authorization: `Bearer ${storedToken()}`,
      ...(init?.headers ?? {}),
    },
  });
  if (!response.ok) {
    let detail = `HTTP ${response.status}`;
    try {
      const problem = (await response.json()) as { detail?: string };
      if (problem.detail) {
        detail = problem.detail;
      }
    } catch {
      // problem+json 解析失败时保留状态码描述
    }
    throw new Error(detail);
  }
  return (await response.json()) as T;
}

// --- 诊断（无需会话） ---

async function requestReport(path: string, method: 'GET' | 'POST'): Promise<DiagnosticReport> {
  const response = await fetch(path, { method, headers: { Accept: 'application/json' } });
  if (!response.ok) {
    throw new Error(`诊断请求失败（HTTP ${response.status}）`);
  }
  return (await response.json()) as DiagnosticReport;
}

export function loadDiagnostics(): Promise<DiagnosticReport> {
  return requestReport('/api/v1/diagnostics', 'GET');
}

export function recheckDiagnostics(): Promise<DiagnosticReport> {
  return requestReport('/api/v1/diagnostics/recheck', 'POST');
}

// --- 工作项 ---

export function listWorkItems(projectId: string): Promise<{ items: WorkItem[]; nextCursor: string }> {
  return request(`/api/v1/workitems?projectId=${encodeURIComponent(projectId)}`);
}

export function createWorkItem(input: {
  projectId: string;
  title: string;
  description?: string;
  gitlabIssueIid?: string;
}): Promise<WorkItem> {
  return request('/api/v1/workitems', { method: 'POST', body: JSON.stringify(input) });
}

export function getWorkItem(id: string): Promise<{ workItem: WorkItem; stages: Stage[] }> {
  return request(`/api/v1/workitems/${id}`);
}

export function evaluateGate(workItemId: string, gate: string): Promise<GateResultInfo> {
  return request(`/api/v1/workitems/${workItemId}/gates/${gate}/evaluate`, { method: 'POST' });
}

// --- 工件 / 证据 ---

export function listArtifacts(workItemId: string): Promise<{ items: Artifact[] }> {
  return request(`/api/v1/workitems/${workItemId}/artifacts`);
}

export function listEvidence(workItemId: string): Promise<{ items: Evidence[] }> {
  return request(`/api/v1/workitems/${workItemId}/evidences`);
}

export function recordEvidence(
  workItemId: string,
  input: { gate: string; kind: string; title: string; source: string },
): Promise<Evidence> {
  return request(`/api/v1/workitems/${workItemId}/evidences`, {
    method: 'POST',
    body: JSON.stringify(input),
  });
}

export function verifyEvidence(evidenceId: string, verifiedBy: string): Promise<unknown> {
  return request(`/api/v1/evidences/${evidenceId}/verify`, {
    method: 'POST',
    body: JSON.stringify({ verifiedBy }),
  });
}

// --- 审批 ---

export function listApprovals(): Promise<{ items: Approval[] }> {
  return request('/api/v1/approvals');
}

export function decideApproval(id: string, decision: 'approved' | 'rejected', decidedBy: string): Promise<Approval> {
  return request(`/api/v1/approvals/${id}/decide`, {
    method: 'POST',
    body: JSON.stringify({ decision, decidedBy, reason: decision === 'approved' ? 'Web 控制台批准' : 'Web 控制台拒绝' }),
  });
}

// --- 文牒 / 部署 ---

export function issuePassport(workItemId: string): Promise<PassportInfo> {
  return request(`/api/v1/workitems/${workItemId}/passports`, { method: 'POST', body: JSON.stringify({}) });
}

export function latestPassport(workItemId: string): Promise<PassportInfo> {
  return request(`/api/v1/workitems/${workItemId}/passports/latest`);
}

export function getDeployment(id: string): Promise<DeploymentInfo> {
  return request(`/api/v1/deployments/${id}`);
}

// --- 需求导入（FR-REQ-001） ---

export function importIssue(input: {
  projectId: string;
  issueIid: string;
  gitlabProjectId: string;
}): Promise<WorkItem> {
  return request('/api/v1/workitems/import-issue', { method: 'POST', body: JSON.stringify(input) });
}

// --- 工件操作（闯关工作台） ---

export function createDraft(
  artifactId: string,
  content: string,
): Promise<{ revision: RevisionInfo; etag: string }> {
  return fetchWithETag('/api/v1/artifacts/' + artifactId + '/revisions', {
    method: 'POST',
    body: JSON.stringify({ content }),
  });
}

export function updateDraft(
  revisionId: string,
  ifMatch: string,
  content: string,
): Promise<{ revision: RevisionInfo; etag: string }> {
  return fetchWithETag('/api/v1/revisions/' + revisionId, {
    method: 'PUT',
    headers: { 'If-Match': ifMatch },
    body: JSON.stringify({ content }),
  });
}

async function fetchWithETag(path: string, init: RequestInit): Promise<{ revision: RevisionInfo; etag: string }> {
  await ensureSession();
  const response = await fetch(path, {
    ...init,
    headers: {
      'Content-Type': 'application/json',
      Authorization: `Bearer ${storedToken()}`,
      ...(init?.headers ?? {}),
    },
  });
  if (!response.ok) {
    throw new Error(await problemDetail(response));
  }
  const revision = (await response.json()) as RevisionInfo;
  return { revision, etag: response.headers.get('ETag') ?? revision.etag };
}

async function problemDetail(response: Response): Promise<string> {
  try {
    const problem = (await response.json()) as { detail?: string };
    if (problem.detail) {
      return problem.detail;
    }
  } catch {
    // ignore
  }
  return `HTTP ${response.status}`;
}

export function revisionContent(revisionId: string): Promise<string> {
  return requestText(`/api/v1/revisions/${revisionId}/content`);
}

async function requestText(path: string): Promise<string> {
  await ensureSession();
  const response = await fetch(path, { headers: { Authorization: `Bearer ${storedToken()}` } });
  if (!response.ok) {
    throw new Error(await problemDetail(response));
  }
  return response.text();
}

export function addReview(
  revisionId: string,
  reviewer: string,
  verdict: 'approved' | 'rejected' | 'changes_requested',
): Promise<unknown> {
  return request(`/api/v1/revisions/${revisionId}/reviews`, {
    method: 'POST',
    body: JSON.stringify({ reviewer, verdict, comment: 'Web 工作台评审' }),
  });
}

export function freezeBaseline(
  workItemId: string,
  gate: string,
  revisionIds: string[],
): Promise<unknown> {
  return request(`/api/v1/workitems/${workItemId}/baselines`, {
    method: 'POST',
    body: JSON.stringify({ gate, revisionIds }),
  });
}

export function startAgentRun(
  workItemId: string,
  goal: string,
  manifestId: string,
  toolAllowlist: string[],
): Promise<AgentRunInfo> {
  return request(`/api/v1/workitems/${workItemId}/agent-runs`, {
    method: 'POST',
    body: JSON.stringify({
      goal,
      contextManifestId: manifestId,
      toolAllowlist,
      idempotencyKey: `web-${Date.now()}`,
    }),
  });
}

export function listProposals(runId: string): Promise<{ items: Array<{ tool: string; decision: string; risk: string }> }> {
  return request(`/api/v1/agent-runs/${runId}/proposals`);
}

export function submitDeployment(id: string): Promise<unknown> {
  return request(`/api/v1/deployments/${id}/submit`, { method: 'POST' });
}

export function deploy(id: string): Promise<DeploymentInfo> {
  return request(`/api/v1/deployments/${id}/deploy`, { method: 'POST' });
}

export function verifyDeployment(id: string): Promise<DeploymentInfo> {
  return request(`/api/v1/deployments/${id}/verify`, { method: 'POST' });
}

export function createDeployment(
  workItemId: string,
  plan: Record<string, unknown>,
): Promise<DeploymentInfo> {
  return request('/api/v1/deployments', {
    method: 'POST',
    body: JSON.stringify({ workItemId, plan }),
  });
}

export function createArtifact(workItemId: string, kind: string, title: string): Promise<Artifact> {
  return request(`/api/v1/workitems/${workItemId}/artifacts`, {
    method: 'POST',
    body: JSON.stringify({ kind, title }),
  });
}

export function listRevisions(artifactId: string): Promise<{ items: RevisionInfo[] }> {
  return request(`/api/v1/artifacts/${artifactId}/revisions`);
}

export function createManifest(workItemId: string): Promise<{ id: string }> {
  return request(`/api/v1/workitems/${workItemId}/context-manifests`, {
    method: 'POST',
    body: JSON.stringify({ scope: { maxContextBytes: 262144 }, dataPolicy: 'standard' }),
  });
}

// ensureManifest：为 Agent Run 准备上下文清单（幂等缓存）。
const manifestCache = new Map<string, string>();

export async function ensureManifest(workItemId: string): Promise<string> {
  const cached = manifestCache.get(workItemId);
  if (cached) {
    return cached;
  }
  const manifest = await createManifest(workItemId);
  manifestCache.set(workItemId, manifest.id);
  return manifest.id;
}

export async function startAgentRunWithManifest(
  workItemId: string,
  goal: string,
  toolAllowlist: string[],
): Promise<AgentRunInfo> {
  const manifestId = await ensureManifest(workItemId);
  return startAgentRun(workItemId, goal, manifestId, toolAllowlist);
}

// --- 需求文档（工作目录 data/docs/） ---

export function importDocument(input: {
  projectId: string;
  filename: string;
  content: string;
}): Promise<WorkItem> {
  return request('/api/v1/workitems/import-document', { method: 'POST', body: JSON.stringify(input) });
}

export function listDocuments(workItemId: string): Promise<{ items: string[] }> {
  return request(`/api/v1/workitems/${workItemId}/documents`);
}

export function documentContent(workItemId: string, name: string): Promise<string> {
  return requestText(`/api/v1/workitems/${workItemId}/documents/${encodeURIComponent(name)}`);
}
