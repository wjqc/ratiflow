import type { DiagnosticReport } from './types';

async function requestReport(path: string, method: 'GET' | 'POST'): Promise<DiagnosticReport> {
  const response = await fetch(path, {
    method,
    headers: { Accept: 'application/json' },
  });
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
