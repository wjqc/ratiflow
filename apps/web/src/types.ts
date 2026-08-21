export type DiagnosticStatus = 'ready' | 'needs_configuration' | 'checking' | 'unavailable';

export interface DiagnosticItem {
  id: string;
  label: string;
  status: DiagnosticStatus;
  detail: string;
  required: boolean;
}

export interface DiagnosticReport {
  generatedAt: string;
  ready: boolean;
  integrations: DiagnosticItem[];
  local: DiagnosticItem[];
}
