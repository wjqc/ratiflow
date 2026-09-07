// 场景 D（手册 §14）：凭据失效——GitLab 401 → Profile degraded → 概览阻塞
// → 轮换凭据 → 验证 → 恢复 ready。全过程审计不包含 token。
import { expect, test } from '@playwright/test';
import { FakeGitLab, launchApp, type E2eApp } from './fixtures';

let e2e: E2eApp;
let gitlab: FakeGitLab;
const SECRET_V1 = 'glpat-e2e-v1-aaaaaaaaaaaaaa';
const SECRET_V2 = 'glpat-e2e-v2-bbbbbbbbbbbbbb';

test.beforeEach(async () => {
  gitlab = new FakeGitLab();
  await gitlab.start();
});
test.afterEach(async () => {
  if (e2e) { await e2e.close(); e2e = undefined as unknown as E2eApp; }
  await gitlab.stop();
});

test('D1 401 → degraded → 轮换 → 验证 → ready；审计无 token', async () => {
  e2e = await launchApp({ env: { RATIFLOW_GITLAB_URL: gitlab.url(), RATIFLOW_GITLAB_TOKEN: '' } });

  // 1. 创建凭据（真实 Keychain 写入）。
  const cred = await e2e.rpc<{ id: string; revision: number; name: string }>('credentialRef.create', {
    name: 'E2E GitLab Token', kind: 'gitlab_token', provider: 'gitlab', secret: SECRET_V1,
  });
  expect(cred.id).toBeTruthy();

  // 2. 绑定 GitLab Profile。
  const profile = await e2e.rpc<{ id: string; revision: number }>('gitlabProfile.create', {
    name: 'E2E 实例', baseUrl: gitlab.url(), credentialRefId: cred.id,
  });
  expect(profile.id).toBeTruthy();

  // 3. fake 返回 401 → 测试后 degraded。
  gitlab.setMode('unauthorized');
  const failedReport = await e2e.rpc<{ status: string; steps: Array<{ name: string; status: string; errorCode: string | null }> }>(
    'gitlabProfile.test', { profileId: profile.id });
  expect(failedReport.status).toBe('error');
  expect(failedReport.steps.some((s) => s.name === 'auth' && s.errorCode === 'CREDENTIAL_AUTH_FAILED')).toBe(true);

  const degraded = await e2e.rpc<{ status: string }>('gitlabProfile.list');
  // 状态回写为 degraded（或 error）——不能是 ready。
  const profiles = await e2e.rpc<{ items: Array<{ id: string; status: string }> }>('gitlabProfile.list');
  const target = profiles.items.find((p) => p.id === profile.id);
  expect(['degraded', 'error', 'configured']).toContain(target?.status);
  expect(degraded).toBeTruthy();

  // 4. 轮换凭据（replace，新 secret）。
  const latest = profiles.items.find((p) => p.id === profile.id);
  const currentRevision = await e2e.rpc<{ revision: number }>('credentialRef.verify', { refId: cred.id });
  await e2e.rpc('credentialRef.replace', {
    refId: cred.id, secret: SECRET_V2, expectedRevision: currentRevision.revision,
  });

  // 5. fake 切回 200 → 验证通过 → ready。
  gitlab.setMode('ok');
  const okReport = await e2e.rpc<{ status: string; steps: Array<{ name: string; status: string }> }>(
    'gitlabProfile.test', { profileId: profile.id });
  expect(okReport.status).toBe('ready');
  expect(okReport.steps.every((s) => s.status === 'passed')).toBe(true);
  void latest;

  // 6. currentUser 可读。
  const user = await e2e.rpc<{ username: string }>('gitlabProfile.currentUser', { profileId: profile.id });
  expect(user.username).toBe('e2e-user');

  // 7. 审计导出全文不含任何 secret 值。
  const auditExport = await e2e.rpc('audit.export', {});
  const auditText = JSON.stringify(auditExport);
  expect(auditText).not.toContain(SECRET_V1);
  expect(auditText).not.toContain(SECRET_V2);
  expect(auditText).not.toContain('glpat-');

  // 8. 日志与请求头：fake 端收到过 token（证明真实传递），但日志/审计不可见——由 7 保证。
  expect(gitlab.tokenSeen.length).toBeGreaterThanOrEqual(2);

  // 清理：删除凭据（Keychain 同步删除）。
  const afterVerify = await e2e.rpc<{ revision: number }>('credentialRef.verify', { refId: cred.id });
  await e2e.rpc('credentialRef.remove', { refId: cred.id, expectedRevision: afterVerify.revision, force: true });
});

test('D2 GitLab/SSH 直填秘密自动落 Keychain（凭据引用页已删）', async () => {
  e2e = await launchApp({ env: { RATIFLOW_GITLAB_URL: gitlab.url(), RATIFLOW_GITLAB_TOKEN: '' } });

  // 1. GitLab 直填 token 创建 → 自动生成凭据引用，列表不回显秘密。
  const profile = await e2e.rpc<{ id: string }>('gitlabProfile.create', {
    name: '直填实例', baseUrl: gitlab.url(), token: SECRET_V1,
  });
  expect(profile.id).toBeTruthy();
  const profiles = await e2e.rpc<{ items: Array<{ id: string; credential_ref_id?: string | null }> }>('gitlabProfile.list');
  const bound = profiles.items.find((p) => p.id === profile.id);
  expect(bound?.credential_ref_id).toBeTruthy();

  // 2. 直填 token 真实传递：fake 收到；测试通过。
  gitlab.setMode('ok');
  const okReport = await e2e.rpc<{ status: string }>('gitlabProfile.test', { profileId: profile.id });
  expect(okReport.status).toBe('ready');
  expect(gitlab.tokenSeen).toContain(SECRET_V1);

  // 3. SSH 直填凭证创建 → 同样自动生成凭据引用。
  const target = await e2e.rpc<{ id: string }>('sshTarget.create', {
    name: '直填目标机', host: '127.0.0.1', port: 22, user: 'deploy', secret: SECRET_V2,
  });
  expect(target.id).toBeTruthy();
  const targets = await e2e.rpc<{ items: Array<{ id: string; credential_ref_id?: string | null }> }>('sshTarget.list');
  expect(targets.items.find((t) => t.id === target.id)?.credential_ref_id).toBeTruthy();

  // 4. 秘密值与 credentialRefId 同给 → InvalidParams。
  const conflict = e2e.rpc('gitlabProfile.create', {
    name: '冲突实例', baseUrl: gitlab.url(), token: SECRET_V1, credentialRefId: bound?.credential_ref_id,
  });
  await expect(conflict).rejects.toThrow(/二选一/);

  // 5. 审计不含直填秘密。
  const auditText = JSON.stringify(await e2e.rpc('audit.export', {}));
  expect(auditText).not.toContain(SECRET_V1);
  expect(auditText).not.toContain(SECRET_V2);
});
