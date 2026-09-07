#!/usr/bin/env node
// WP-4 Git 仓库导入协议级 E2E（RDWS v1.4 / RDWS-007/008）：
// ① importAdd→probe 审批→worker probing→候选→activation 审批→active 全链（真实
//    本地 git 远端 + 真实沙箱探针）；② 两次批准前零 checkout（零代码执行）；
// ③ 源 checkout 冻结后只读；④ 同 ref 新 SHA = 新行新候选；⑤ revoke 目录清理 +
//    server 级联 revoked；⑥ 空仓库受限 fetch（HEAD==pinnedSha 且无默认分支残留）。
// call 前 tool_source_drift 的篡改拒绝在 ratiflow-core 单测（mcp_import_call_tests）
// 覆盖（需要 executor 直调）。前置：cargo build --release；本机 git + python3。
import { execSync, spawn } from 'node:child_process';
import { existsSync, mkdtempSync, rmSync, writeFileSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import * as readline from 'node:readline';

const CORE = process.env.CORE_BIN ?? join(process.cwd(), 'target', 'release', 'ratiflow-core');
const HAS_PY = (() => { try { execSync('python3 --version', { stdio: 'ignore', timeout: 10000 }); return true; } catch { return false; } })();
const HAS_GIT = (() => { try { execSync('git --version', { stdio: 'ignore', timeout: 10000 }); return true; } catch { return false; } })();
if (!HAS_PY || !HAS_GIT) {
  console.error('✕ mcp-import-e2e 需要 git + python3（fail-loud，不静默 skip）');
  process.exit(1);
}

class CoreClient {
  constructor(dataDir, env = {}) {
    this.proc = spawn(CORE, ['app-server', '--data-dir', dataDir], { stdio: ['pipe', 'pipe', 'ignore'], env: { ...process.env, ...env } });
    this.nextId = 1;
    this.pending = new Map();
    this.rl = readline.createInterface({ input: this.proc.stdout });
    this.rl.on('line', (l) => this.onLine(l));
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
  async hello_(timeoutMs = 5000) {
    const deadline = Date.now() + timeoutMs;
    while (!this.hello) {
      if (Date.now() > deadline) throw new Error('等待 hello 超时');
      await new Promise((r) => setTimeout(r, 25));
    }
    return this.hello;
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
  if (!c) throw new Error(`mcp-import-e2e 断言失败：${label}`);
  console.log(`  ✓ ${label}`);
}

const sh = (cmd, cwd) => execSync(cmd, { cwd, timeout: 60000 }).toString().trim();

const MANIFEST = JSON.stringify({
  schemaVersion: 1,
  entrypoint: { command: 'server.py', args: [] },
  sandbox: { network: false, writableDirs: [] },
  timeouts: { probeSec: 10, callSec: 10 },
  deps: { manager: 'none' },
});

const SERVER_PY = `#!/usr/bin/env python3
import sys, json
def send(obj):
    sys.stdout.write(json.dumps(obj) + "\\n"); sys.stdout.flush()
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    req = json.loads(line)
    m = req.get("method"); i = req.get("id")
    if m == "initialize":
        send({"jsonrpc":"2.0","id":i,"result":{"serverInfo":{"name":"imported-e2e","version":"1.0"},"protocolVersion":"2024-11-05"}})
    elif m == "notifications/initialized":
        continue
    elif m == "tools/list":
        send({"jsonrpc":"2.0","id":i,"result":{"tools":[
            {"name":"read_thing","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":True}},
            {"name":"send_thing","inputSchema":{"type":"object"}}]}})
    elif m == "tools/call":
        send({"jsonrpc":"2.0","id":i,"result":{"content":[{"type":"text","text":"ok"}],"isError":False}})
    else:
        if i is not None:
            send({"jsonrpc":"2.0","id":i,"error":{"code":-32601,"message":"nf"}})
`;

function makeRemote(root) {
  const repo = join(root, 'remote');
  mkdirSync(repo, { recursive: true });
  writeFileSync(join(repo, 'ratiflow-mcp.json'), MANIFEST);
  writeFileSync(join(repo, 'server.py'), SERVER_PY);
  mkdirSync(join(repo, 'lib'), { recursive: true });
  writeFileSync(join(repo, 'lib', 'helper.py'), 'DATA = 1\n');
  sh('git init -q -b main', repo);
  sh('git add .', repo);
  sh('git update-index --chmod=+x server.py', repo);
  const gitName = '-c user.name=e2e -c user.email=e2e@test';
  sh(`git ${gitName} commit -q -m init`, repo);
  return repo;
}

async function waitStatus(c, importId, statuses, capMs = 30000) {
  const deadline = Date.now() + capMs;
  for (;;) {
    const imp = await c.call('mcp.importGet', { importId });
    if (statuses.includes(imp.status)) return imp;
    if (Date.now() > deadline) throw new Error(`等待导入状态 ${statuses} 超时（当前 ${imp.status}: ${imp.error}）`);
    await new Promise((r) => setTimeout(r, 500));
  }
}

async function approvalFor(c, importId, subjectType) {
  const list = await c.call('approval.list', {});
  const hit = (list.items ?? []).find((a) => a.subject_type === subjectType && a.subject_id === importId);
  return hit?.id;
}

async function main() {
  const work = mkdtempSync(join(tmpdir(), 'sg-mcp-import-'));
  const dataDir = join(work, 'data');
  mkdirSync(dataDir, { recursive: true });
  const remote = makeRemote(work);
  const sha1 = sh('git rev-parse HEAD', remote);
  const c = new CoreClient(dataDir, { RATIFLOW_MCP_GIT_IMPORT: '1' });
  const fail = (e) => { console.error(`E2E 失败：${e.message}`); c?.kill(); process.exit(1); };

  try {
    await c.hello_();
    // ① importAdd：解析 ref→SHA + probe 审批。
    const added = await c.call('mcp.importAdd', { repoUrl: `file://${remote}`, ref: sha1, idempotencyKey: 'imp-e2e-1' });
    assert(added.status === 'awaiting_probe_approval' && added.pinnedSha === sha1, '① importAdd 冻结 SHA + 待探针审批');
    const impDir = join(dataDir, 'mcp-imports', added.importId);
    assert(!existsSync(impDir), '① 两次批准前零 checkout（零代码执行）');
    // ② probe 批准 → worker（5s tick）clone+冻结+沙箱探针 → awaiting_activation。
    const probeApr = await approvalFor(c, added.importId, 'mcp_import_probe');
    assert(!!probeApr, '② probe 审批在审批中心');
    await c.call('mcp.importDecide', { importId: added.importId, decision: 'approved', decidedBy: 'admin', reason: 'e2e', idempotencyKey: 'imp-dec-1' });
    const cand = await waitStatus(c, added.importId, ['awaiting_activation', 'failed']);
    assert(cand.status === 'awaiting_activation', `② probing→候选（${cand.error}）`);
    const freeze = cand.contentFreeze ?? {};
    assert(freeze.commit_tree_digest?.length === 40 && freeze.dirty === false && freeze.import_id === added.importId, '② 冻结五元组落库');
    // ⑥ 空仓库受限 fetch：HEAD==pinnedSha；无默认分支抓取痕迹。
    const checkout = join(impDir, 'checkout');
    assert(sh('git rev-parse HEAD', checkout) === sha1, '⑥ HEAD==pinnedSha');
    const branches = sh('git branch -a', checkout);
    assert(!branches.includes('main') && !branches.includes('remotes/'), `⑥ 未抓默认分支（${branches}）`);
    // ③ 源 checkout 冻结后只读。
    let readonly = false;
    try { writeFileSync(join(checkout, 'lib', 'helper.py'), 'DATA = 99\n'); } catch { readonly = true; }
    assert(readonly, '③ 源 checkout 冻结后只读');
    // ④ activation 批准 → active + server 注册 + 工具灌入。
    const actApr = await approvalFor(c, added.importId, 'mcp_import_activate');
    assert(!!actApr, '④ activation 审批在审批中心');
    const act = await c.call('mcp.importDecide', { importId: added.importId, decision: 'approved', decidedBy: 'admin', reason: 'e2e', idempotencyKey: 'imp-dec-2' });
    assert(act.status === 'active' && !!act.serverId, '④ 激活 → active + server 注册');
    const tools = await c.call('mcp.toolsList', {});
    const names = (tools.items ?? []).map((t) => `${t.serverName}__${t.toolName}`);
    assert(names.includes(`${act.serverName}__read_thing`), `④ 候选工具灌入注册表（${names}）`);
    // ⑤ 同 ref 新 SHA = 新行新候选（旧 active 不动）。
    writeFileSync(join(remote, 'lib', 'helper.py'), 'DATA = 2\n');
    sh('git add . && git -c user.name=e2e -c user.email=e2e@test commit -q -m bump', remote);
    const sha2 = sh('git rev-parse HEAD', remote);
    const added2 = await c.call('mcp.importAdd', { repoUrl: `file://${remote}`, ref: sha2, idempotencyKey: 'imp-e2e-2' });
    assert(added2.importId !== added.importId, '⑤ 同 ref 新 SHA 产新候选（旧 active 不动）');
    const old = await c.call('mcp.importGet', { importId: added.importId });
    assert(old.status === 'active', '⑤ 旧 active 不受新候选影响');
    // ⑥ revoke：目录清理 + server 级联 revoked。
    const revoked = await c.call('mcp.importRevoke', { importId: added.importId, decidedBy: 'admin', reason: 'e2e 收尾', idempotencyKey: 'imp-rv-1' });
    assert(revoked.status === 'revoked', '⑥ revoke → revoked');
    assert(!existsSync(impDir), '⑥ revoke 目录清理');
    const toolsAfter = await c.call('mcp.toolsList', {});
    assert(
      !(toolsAfter.items ?? []).some((t) => t.serverName === act.serverName && t.status === 'active'),
      '⑥ server 级联 revoked（无活跃工具）',
    );
    console.log('mcp-import-e2e 全部通过');
  } catch (error) {
    fail(error);
  } finally {
    c.kill();
    rmSync(work, { recursive: true, force: true });
  }
}

main().catch((e) => { console.error(`E2E 失败：${e.message}`); process.exit(1); });
