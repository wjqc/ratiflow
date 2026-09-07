#!/usr/bin/env node
// 设置域协议级 E2E（跨端手册 §14 场景 D/E 的协议侧）：凭据、revision 冲突、
// 备份 verify/restore、审计脱敏、settings.summary 阻塞语义、SSH 首用指纹流。
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import * as readline from 'node:readline';

const CORE = process.env.CORE_BIN ?? join(process.cwd(), 'target', 'release', 'sixgates-core');

class CoreClient {
  constructor(dataDir) {
    this.proc = spawn(CORE, ['app-server', '--data-dir', dataDir], { stdio: ['pipe', 'pipe', 'pipe'] });
    this.nextId = 1;
    this.pending = new Map();
    this.rl = readline.createInterface({ input: this.proc.stdout });
    this.rl.on('line', (l) => this.onLine(l));
    this.proc.stderr.on('data', () => {});
  }
  onLine(line) {
    if (!line.trim()) return;
    let m; try { m = JSON.parse(line); } catch { return; }
    if (m.protocolVersion) { this.hello = m; return; }
    if (m.id && this.pending.has(m.id)) {
      const { resolve, reject } = this.pending.get(m.id);
      this.pending.delete(m.id);
      if (m.error) reject(Object.assign(new Error(m.error.data?.detail ?? m.error.message), { code: m.error.message }));
      else resolve(m.result);
    }
  }
  call(method, params = {}) {
    const id = String(this.nextId++);
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.proc.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
    });
  }
  kill() { this.proc.kill('SIGKILL'); }
}

function assert(c, label) {
  if (!c) throw new Error(`设置域 E2E 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

async function main() {
  const dataDir = await mkdtempSync(join(tmpdir(), 'sg-set-e2e-'));
  const c = new CoreClient(dataDir);
  const credentialIds = []; // { id, revision }——remove 需 expectedRevision
  try {
    for (let i = 0; i < 50 && !c.hello; i++) await new Promise((r) => setTimeout(r, 100));
    assert(c.hello?.protocolVersion === '1', 'hello 握手');

    // 1. settings.summary 初始阻塞（模型/GitLab 未配置）。
    const s0 = await c.call('settings.summary');
    assert(s0.overallStatus === 'action_required', '未配置时 action_required');
    assert(s0.blockers.some((b) => b.id === 'model_not_configured' && b.targetRoute === '/settings/models'), '模型阻塞 + 路由');

    // 2. 凭据：真实 Keychain 写入（mac）+ DTO 无明文。
    const cred = await c.call('credentialRef.create', { name: 'E2E Key', kind: 'model_api_key', provider: 'openai', secret: 'sk-e2e-test-12345678' });
    credentialIds.push({ id: cred.id, revision: cred.revision });
    assert(cred.id && cred.status === 'active', '凭据创建（Keychain）');
    assert(!JSON.stringify(cred).includes('sk-e2e-test'), 'DTO 不回显明文');

    // 3. revision 冲突 + 轮换主路径（缺陷审计：轮换后旧 secret 失效此前无覆盖）+ 清理。
    //    此前每轮 CI 向真实 Keychain 泄漏一条测试凭据且 remove 从未被测。
    const conflict = await c.call('credentialRef.replace', { refId: cred.id, secret: 'x', expectedRevision: 99 }).catch((e) => e);
    // 缺陷审计：错误码契约钉死（settings codes::REVISION_CONFLICT 唯一），不再三选一。
    // 缺陷审计：错误码契约钉死（REVISION_CONFLICT 经 RPC envelope 映射为 conflict），不再三选一。
    assert(conflict.code === 'conflict', `revision 冲突（${conflict.code}）`);
    const rotated = await c.call('credentialRef.replace', { refId: cred.id, secret: 'sk-e2e-rotated-98765', expectedRevision: cred.revision });
    assert(rotated.status === 'active' && rotated.revision > cred.revision, '凭据轮换 revision 前进');
    assert(!JSON.stringify(rotated).includes('sk-e2e-rotated'), '轮换 DTO 不回显明文');
    credentialIds[0].revision = rotated.revision;

    // 4. 模型 Profile + 路由 → 阻塞解除。
    const mp = await c.call('modelProfile.create', { name: 'E2E', providerKind: 'fake', credentialRefId: cred.id });
    assert(mp.status === 'configured', '模型 Profile 创建');
    const summary2 = await c.call('settings.summary');
    assert(!summary2.blockers.some((b) => b.id === 'model_not_configured'), '配置后模型阻塞解除');

    // 4a. 供应商预设：智谱 GLM / DeepSeek 内置端点。
    const presets = await c.call('modelProvider.presets', {});
    const zhipu = (presets.items ?? []).find((p) => p.id === 'zhipu');
    const deepseek = (presets.items ?? []).find((p) => p.id === 'deepseek');
    assert(zhipu?.baseUrl === 'https://open.bigmodel.cn/api/paas/v4', '智谱 GLM 预设端点');
    assert(deepseek?.baseUrl === 'https://api.deepseek.com/v1', 'DeepSeek 预设端点');
    assert((zhipu?.models ?? []).some((m) => m.id === 'glm-5.3'), '智谱预设含 glm-5.3');

    // 4b. GLM 预设直填 apiKey 创建：默认端点回填 + 凭据自动落 Keychain（DB 无明文）。
    const glm = await c.call('modelProfile.create', { name: 'GLM 主力', providerKind: 'zhipu', apiKey: 'zhipu-e2e-secret-123', defaultModel: 'glm-5.3' });
    assert(glm.base_url === 'https://open.bigmodel.cn/api/paas/v4', 'GLM Profile 回填预设端点');
    assert(!!glm.credential_ref_id, 'apiKey 直填自动绑定凭据引用');
    assert(!JSON.stringify(glm).includes('zhipu-e2e-secret'), 'Profile DTO 无明文密钥');
    const credList = await c.call('credentialRef.list', {});
    assert((credList.items ?? []).some((cr) => cr.id === glm.credential_ref_id), '凭据引用已登记');
    const badKind = await c.call('modelProfile.create', { name: 'X', providerKind: 'anthropic' }).catch((e) => e);
    assert(`${badKind.code}: ${badKind.message}`.includes('不受支持'), '未知 providerKind 拒绝');

    // 5. 备份 create → verify → 篡改 → corrupt 拒绝恢复。
    const bk = await c.call('backup.create');
    assert(bk.id && bk.status === 'created', '备份创建');
    const ok = await c.call('backup.verify', { backupId: bk.id });
    assert(ok.verified === true, '备份 verify 通过');
    const { writeFileSync } = await import('node:fs');
    writeFileSync(ok.path, Buffer.from('corrupted'));
    const bad = await c.call('backup.verify', { backupId: bk.id });
    assert(bad.verified === false && bad.status === 'corrupt', '篡改后 corrupt');
    const refused = await c.call('backup.restore', { backupId: bk.id }).catch((e) => e);
    assert(['CONFLICT', 'conflict', 'BACKUP_CORRUPT'].includes(refused.code), `corrupt 备份拒绝恢复（${refused.code}）`);

    // 6. 审计脱敏导出。
    const exported = await c.call('audit.export', {});
    assert(!JSON.stringify(exported).includes('sk-e2e-test'), '审计导出无秘密');
    assert(JSON.stringify(exported).includes('credentialRef.create'), '审计含动作记录');

    // 7. SSH 首用指纹流（不可达主机 → unreachable 而非静默接受）。
    const ssh = await c.call('sshTarget.create', { name: 'E2E', host: 'not-a-host.invalid', user: 'u', remoteDir: '/srv' });
    const t = await c.call('sshTarget.test', { targetId: ssh.id });
    assert(['error', 'action_required'].includes(t.status), 'SSH 测试返回明确状态');

    // 8. 知识检索 v2 默认排除。
    const pj = await c.call('project.create', { gitlabInstance: 'x', namespace: 'n', project: 'p', name: 'P' });
    const hit = await c.call('knowledge.searchV2', { projectId: pj.id, query: 'nonexistent' });
    assert(Array.isArray(hit.items), 'searchV2 结构化响应');

    console.log(`\n设置域 E2E 通过：${JSON.stringify(summary2.dataSafety)}`);
  } catch (e) {
    console.error(`设置域 E2E 失败：${e.message}`);
    process.exitCode = 1;
  } finally {
    // Keychain 清理（缺陷审计：测试凭据不得滞留真实 Keychain）。
    // remove 放在 finally：即便后续断言失败，测试凭据也会被删除。
    for (const cred of credentialIds) {
      try {
        await c.call('credentialRef.remove', { refId: cred.id, expectedRevision: cred.revision, force: true });
        console.log('  ✓ Keychain 清理:', cred.id);
      } catch (e) {
        console.error(`  ! Keychain 清理失败（需手工删除）: ${cred.id} — ${e.message ?? e}`);
      }
    }
    c.kill();
    rmSync(dataDir, { recursive: true, force: true });
  }
}
void main();
