import type {
  Approval,
  Artifact,
  DeploymentInfo,
  DiagnosticReport,
  Evidence,
  GateResultInfo,
  PassportInfo,
  Stage,
  WorkItem,
} from './types';

// 会话令牌只存在本机 localStorage（loopback 单用户模型）。
const TOKEN_KEY = 'sixgates.session-token';

export function storedToken(): string {
  return window.localStorage.getItem(TOKEN_KEY) ?? '';
}

function storeToken(token: string): void {
  window.localStorage.setItem(TOKEN_KEY, token);
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
