// 设置中心关键路径单测：路由表完整性 / 脱敏 / S00 阻塞推导 / S50 修复入口。
import { describe, expect, it } from 'vitest';
import {
  DEFAULT_SETTINGS_ROUTE,
  SETTINGS_NAV,
  isSettingsRouteId,
  settingsRouteMeta,
} from './settings-routes';
import { deriveBlockers } from './OverviewPage';
import { FIX_TARGET } from './DiagnosticsPage';
import { redactSecrets, REDACTED } from '../../lib/redact';
import type { DiagnosticsReport } from './types';

describe('settings-routes', () => {
  it('17 页路由全部注册且 id 唯一', () => {
    const ids = SETTINGS_NAV.flatMap((g) => g.items.map((i) => i.id));
    expect(ids.length).toBe(17);
    expect(new Set(ids).size).toBe(17);
  });

  it('每页都有中文名与 S 编号，分组非空', () => {
    for (const group of SETTINGS_NAV) {
      expect(group.label.length).toBeGreaterThan(0);
      for (const item of group.items) {
        expect(item.name.length).toBeGreaterThan(0);
        expect(item.code).toMatch(/^S\d{2}$/);
        expect(settingsRouteMeta(item.id).id).toBe(item.id);
      }
    }
  });

  it('isSettingsRouteId 校验未知值并回退默认', () => {
    expect(isSettingsRouteId('overview')).toBe(true);
    expect(isSettingsRouteId('diagnostics')).toBe(true);
    expect(isSettingsRouteId('bogus')).toBe(false);
    expect(isSettingsRouteId(undefined)).toBe(false);
    expect(isSettingsRouteId(DEFAULT_SETTINGS_ROUTE)).toBe(true);
  });
});

describe('redactSecrets', () => {
  it('命中秘密键名的值被替换，普通字段保留', () => {
    const input = {
      baseUrl: 'https://gitlab.example.com',
      token: 'glpat-xyz',
      nested: { apiKey: 'sk-123', list: [{ password: 'p', note: 'ok' }] },
    };
    const out = redactSecrets(input);
    expect(out.baseUrl).toBe('https://gitlab.example.com');
    expect(out.token).toBe(REDACTED);
    expect(out.nested.apiKey).toBe(REDACTED);
    expect(out.nested.list[0].password).toBe(REDACTED);
    expect(out.nested.list[0].note).toBe('ok');
  });

  it('authorization/private_key/credential 等变体也命中', () => {
    const out = redactSecrets({ Authorization: 'Bearer x', private_key: 'k', credentialId: 'c1' });
    expect(out.Authorization).toBe(REDACTED);
    expect(out.private_key).toBe(REDACTED);
    expect(out.credentialId).toBe(REDACTED);
  });

  it('非对象原样返回', () => {
    expect(redactSecrets('token')).toBe('token');
    expect(redactSecrets(null)).toBe(null);
    expect(redactSecrets(42)).toBe(42);
  });
});

describe('S00 deriveBlockers', () => {
  const report = (statuses: Array<'ready' | 'pending' | 'error' | 'disabled'>): DiagnosticsReport => ({
    generatedAt: '2026-08-22T10:00:00Z',
    local: [],
    integrations: [
      { id: 'gitlab', name: 'GitLab', status: statuses[0], detail: '' },
      { id: 'model', name: '模型', status: statuses[1], detail: '' },
      { id: 'ssh', name: 'SSH', status: statuses[2], detail: '' },
    ],
  });

  it('全部就绪时无阻塞', () => {
    expect(deriveBlockers(report(['ready', 'ready', 'ready']))).toHaveLength(0);
  });

  it('未就绪项映射到目标设置页', () => {
    const blockers = deriveBlockers(report(['error', 'pending', 'ready']));
    expect(blockers.map((b) => b.section)).toEqual(['gitlab', 'models']);
  });

  it('空报告不产生阻塞', () => {
    expect(deriveBlockers(null)).toHaveLength(0);
  });
});

describe('S50 FIX_TARGET', () => {
  it('六个检查项均有修复目标且为合法路由', () => {
    for (const id of ['gitlab', 'model', 'ssh', 'sqlite', 'core', 'executor']) {
      expect(isSettingsRouteId(FIX_TARGET[id])).toBe(true);
    }
  });
});
