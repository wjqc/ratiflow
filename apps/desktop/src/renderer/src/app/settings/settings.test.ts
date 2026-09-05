// 设置中心关键路径单测：路由表完整性 / 脱敏 / S00 阻塞推导 / S50 修复入口。
import { describe, expect, it } from 'vitest';
import {
  DEFAULT_SETTINGS_ROUTE,
  SETTINGS_ADVANCED_ITEMS,
  SETTINGS_NAV,
  SETTINGS_PRIMARY_ITEMS,
  isSettingsRouteId,
  settingsRouteMeta,
} from './settings-routes';
import { redactSecrets, REDACTED } from '../../lib/redact';

describe('settings-routes', () => {
  it('14 页路由全部注册且 id 唯一', () => {
    const ids = SETTINGS_NAV.flatMap((g) => g.items.map((i) => i.id));
    expect(ids.length).toBe(14);
    expect(new Set(ids).size).toBe(14);
  });

  it('S24 MCP 服务器路由真实接入且位于 Agent 分组', () => {
    const mcp = SETTINGS_NAV.flatMap((g) => g.items).find((i) => i.id === 'mcp');
    expect(mcp?.code).toBe('S24');
    expect(mcp?.name).toBe('MCP 服务器');
    expect(mcp?.availability).toBe('real');
    expect(isSettingsRouteId('mcp')).toBe(true);
  });

  it('S12 项目记忆路由真实接入且位于工作区分组', () => {
    const memory = SETTINGS_NAV.flatMap((g) => g.items).find((i) => i.id === 'memory');
    expect(memory?.code).toBe('S12');
    expect(memory?.name).toBe('项目记忆');
    expect(memory?.availability).toBe('real');
    expect(isSettingsRouteId('memory')).toBe(true);
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

  it('常用导航保持精简，外部集成为高级设置末项，GitLab/SSH 不再是独立路由', () => {
    expect(SETTINGS_PRIMARY_ITEMS.map((item) => item.id)).toEqual([
      'app-general', 'projects', 'models',
    ]);
    expect(SETTINGS_ADVANCED_ITEMS.at(-1)?.id).toBe('integrations');
    expect(SETTINGS_ADVANCED_ITEMS.some((item) => item.id === 'diagnostics')).toBe(true);
    expect(SETTINGS_ADVANCED_ITEMS.some((item) => item.id === 'editors')).toBe(false);
    const allNames = SETTINGS_NAV.flatMap((group) => group.items).map((item) => String(item.id));
    expect(allNames).not.toContain('gitlab');
    expect(allNames).not.toContain('ssh');
    expect(allNames).not.toContain('credentials');
    expect(allNames.some((item) => ['overview', 'audit', 'logs'].includes(item))).toBe(false);
  });

  it('isSettingsRouteId 校验未知值并回退默认', () => {
    expect(isSettingsRouteId('overview')).toBe(false);
    expect(isSettingsRouteId('appearance')).toBe(false);
    expect(isSettingsRouteId('audit')).toBe(false);
    expect(isSettingsRouteId('logs')).toBe(false);
    expect(isSettingsRouteId('diagnostics')).toBe(true);
    expect(isSettingsRouteId('gitlab')).toBe(false);
    expect(isSettingsRouteId('ssh')).toBe(false);
    expect(isSettingsRouteId('credentials')).toBe(false);
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
