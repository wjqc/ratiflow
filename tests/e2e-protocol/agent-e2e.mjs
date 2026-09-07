// M0-② 协议级 Agent 生命周期 E2E：agent.start 唯一入口、非阻塞、事件推送、终态与幂等。
// 前置：cargo build --release -p ratiflow-core；无模型环境变量（FakeModel 脚本耗尽 → model_unavailable）。
import { spawn, execSync } from 'node:child_process';
import { appendFileSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import readline from 'node:readline';

const CORE = process.env.CORE_BIN ?? join(process.cwd(), 'target', 'release', 'ratiflow-core');
const dataDir = mkdtempSync(join(tmpdir(), 'sg-agent-e2e-'));

class CoreClient {
  constructor(binary, dataDir, env = {}) {
    this.proc = spawn(binary, ['app-server', '--data-dir', dataDir],
      { stdio: ['pipe', 'pipe', 'ignore'], env: { ...process.env, ...env } });
    this.nextId = 1;
    this.pending = new Map();
    this.events = [];
    this.rl = readline.createInterface({ input: this.proc.stdout });
    this.rl.on('line', (line) => this.onLine(line));
  }
  onLine(line) {
    if (!line.trim()) return;
    let message;
    try { message = JSON.parse(line); } catch { return; }
    if (message.method === 'event' && message.params) {
      this.events.push(message.params);
      return;
    }
    if (message.protocolVersion) { this.hello = message; return; }
    if (message.id && this.pending.has(message.id)) {
      const { resolve, reject } = this.pending.get(message.id);
      this.pending.delete(message.id);
      if (message.error) {
        reject(Object.assign(new Error(message.error.data?.detail ?? message.error.message), { code: message.error.message }));
      } else {
        resolve(message.result);
      }
    }
  }
  call(method, params = {}) {
    const id = String(this.nextId++);
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.proc.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
    });
  }
  async rpc(method, params) {
    for (let i = 0; i < 100 && !this.hello; i++) {
      await new Promise((r) => setTimeout(r, 50));
    }
    return this.call(method, params);
  }
  kill() { this.proc.kill('SIGKILL'); }
}

function assert(condition, label) {
  if (!condition) {
    throw new Error(`agent-e2e 断言失败：${label}`);
  }
  console.log(`  ✓ ${label}`);
}

async function waitStatus(client, runId, predicate, capMs = 30000) {
  const deadline = Date.now() + capMs;
  for (;;) {
    const run = await client.rpc('agent.get', { runId });
    if (predicate(run.status)) return run;
    if (Date.now() > deadline) throw new Error(`等待状态超时：${runId}（当前 ${run.status}）`);
    await new Promise((r) => setTimeout(r, 100));
  }
}

async function waitEvent(client, type, capMs = 5000) {
  const deadline = Date.now() + capMs;
  while (Date.now() < deadline) {
    if (client.events.some((e) => e.type === type)) return true;
    await new Promise((r) => setTimeout(r, 100));
  }
  return false;
}

async function waitTerminal(client, runId, capMs = 15000) {
  const deadline = Date.now() + capMs;
  for (;;) {
    const run = await client.rpc('agent.get', { runId });
    if (['completed_execution', 'failed', 'cancelled'].includes(run.status)) return run;
    if (Date.now() > deadline) throw new Error(`等待终态超时：${runId}（当前 ${run.status}）`);
    await new Promise((r) => setTimeout(r, 100));
  }
}

const client = new CoreClient(CORE, dataDir);
try {
  assert((await client.rpc('core.version')).protocolVersion === '1', 'hello/版本握手');

  const project = await client.rpc('project.create', {
    gitlabInstance: 'x', namespace: 'n', project: 'p', name: 'agent-e2e',
  });
  const wi = await client.rpc('workitem.create', { projectId: project.id, title: '生命周期', description: '' });
  const manifest = await client.rpc('context.create', {
    projectId: project.id, workItemId: wi.id, query: 'e2e', selectedSources: [],
  });

  // 1) 唯一入口：立即返回 runId（<500ms，本地不含模型往返）。
  const t0 = Date.now();
  const started = await client.rpc('agent.start', {
    workItemId: wi.id, goal: '生命周期验证', contextManifestId: manifest.id,
    toolAllowlist: ['read_file'], idempotencyKey: 'agent-e2e-1',
  });
  assert(!!started.runId, `agent.start 返回 runId（${Date.now() - t0}ms）`);
  assert(Date.now() - t0 < 500, 'agent.start 不执行循环（立即返回）');

  // 2) 非阻塞：Run 进行中其余 RPC 立即返回。
  const t1 = Date.now();
  await client.rpc('project.list');
  assert(Date.now() - t1 < 300, 'Run 期间 project.list 立即返回（读循环+DB actor 双并发）');

  // 3) 未配模型 → 终态 failed（model_unavailable），绝不 completed_execution。
  const run = await waitTerminal(client, started.runId);
  assert(run.status === 'failed', `终态 failed（result=${run.result}）`);
  assert(run.result.includes('model_unavailable'), '失败原因含 model_unavailable');

  // 4) 幂等：同 idempotencyKey 重放返回同一 runId，不再派发任务。
  const replay = await client.rpc('agent.start', {
    workItemId: wi.id, goal: '生命周期验证', contextManifestId: manifest.id,
    toolAllowlist: ['read_file'], idempotencyKey: 'agent-e2e-1',
  });
  assert(replay.runId === started.runId, '幂等重放返回同一 runId');

  // 5) 取消语义：终态 Run 再取消 → 返回既有终态（不重复事件）。
  const cancel = await client.rpc('agent.cancel', { runId: started.runId });
  assert(cancel.status === 'failed', '终态 Run 取消幂等（返回既有终态）');

  // 6) 事件通知：run.started / run.failed 到达（flush 双路：响应后即时 + 250ms 兜底）。
  for (let i = 0; i < 20 && client.events.filter((e) => e.type === 'run.started').length === 0; i++) {
    await new Promise((r) => setTimeout(r, 100));
  }
  const types = client.events.map((e) => e.type);
  assert(types.includes('run.started'), '收到 run.started 通知');
  assert(types.includes('run.failed'), '收到 run.failed 通知');
  assert(types.filter((t) => t === 'run.failed').length === 1, 'run.failed 恰好一条');

  // 7) 契约唯一性：agent.run 已从契约移除 → 调用返回 method_not_found。
  const removed = await client.call('agent.run', {}).catch((e) => e);
  assert(removed && removed.code === 'method_not_found', 'agent.run 已移除（method_not_found）');

  console.log('\nAgent 生命周期 E2E（基础）通过。');

  // ===== 工具执行场景（F05/M0-③）：脚本模型 + safe_restricted + 真实项目根 =====
  const projDir = mkdtempSync(join(tmpdir(), 'sg-agent-e2e-proj-'));
  writeFileSync(join(projDir, 'NOTES.md'), 'hello-m03');
  // P0-4：可写执行必须在受管 worktree 内——夹具项目需为 git 仓库（否则 run_command 被正确拒绝）。
  execSync(`git -C "${projDir}" init -b main`, { stdio: 'ignore' });
  execSync(`git -C "${projDir}" config user.email e2e@ratiflow.local`, { stdio: 'ignore' });
  execSync(`git -C "${projDir}" config user.name e2e`, { stdio: 'ignore' });
  writeFileSync(join(projDir, 'AGENTS.md'), '项目约定：注释使用中文（M2-PROJECT-MARK）。');
  execSync(`git -C "${projDir}" add .`, { stdio: 'ignore' });
  execSync(`git -C "${projDir}" commit -m init`, { stdio: 'ignore' });
  const script = [
    { content: '{"action":"read_file","arguments":{"path":"NOTES.md"},"summary":"读取"}', tokensIn: 10, tokensOut: 5 },
    { content: '{"action":"search_knowledge","arguments":{"query":"hello","limit":3},"summary":"检索"}', tokensIn: 10, tokensOut: 5 },
    { content: '{"action":"run_command","arguments":{"argv":["wc","-c","NOTES.md"]},"summary":"统计"}', tokensIn: 10, tokensOut: 5 },
    { content: '{"action":"write_file","arguments":{"path":"drafts/out.md","content":"# M0-3 草稿"},"summary":"写草稿"}', tokensIn: 10, tokensOut: 5 },
    { content: '{"action":"final","summary":"工具链验证完成"}', tokensIn: 10, tokensOut: 5 },
    // 拒绝场景（第二个 Run）：run_command → 暂停 → 拒绝（后续不应被消费）
    { content: '{"action":"run_command","arguments":{"argv":["wc","-c","NOTES.md"]},"summary":"统计"}', tokensIn: 10, tokensOut: 5 },
    // M3/F10 场景3：toolPolicy 免批覆盖后 run_command 直接执行
    { content: '{"action":"run_command","arguments":{"argv":["wc","-c","NOTES.md"]},"summary":"免批执行"}', tokensIn: 10, tokensOut: 5 },
    { content: '{"action":"final","summary":"免批完成"}', tokensIn: 10, tokensOut: 5 },
  ];
  const scriptPath = join(tmpdir(), `sg-agent-e2e-script-${Date.now()}.json`);
  writeFileSync(scriptPath, JSON.stringify(script));
  const dataDir2 = mkdtempSync(join(tmpdir(), 'sg-agent-e2e2-'));
  const toolClient = new CoreClient(CORE, dataDir2, {
    RATIFLOW_EXEC_MODE: 'safe_restricted',     // 确定性执行模式（无 Docker 依赖）
    RATIFLOW_FAKE_MODEL_SCRIPT: scriptPath,    // 脚本模型（未配置真实模型时生效）
  });
  // F07：全局指令层（数据目录根）。
  writeFileSync(join(dataDir2, 'Ratiflow.md'), '全局约定：交付遵循 M2-GLOBAL-MARK 规范。');
  try {
    const project = await toolClient.rpc('project.create', {
      gitlabInstance: 'x', namespace: 'n', project: 'p', name: '工具链', localRoot: projDir,
    });
    const wi = await toolClient.rpc('workitem.create', { projectId: project.id, title: '工具链验证', description: '' });
    // F08：先扫描知识源，再建 manifest（included 项来自检索命中）。
    const source = await toolClient.rpc('knowledge.create', {
      projectId: project.id, kind: 'repo_path', name: 'proj', locator: projDir,
    });
    await toolClient.rpc('knowledge.scan', { sourceId: source.id, projectRoot: projDir });
    const manifest = await toolClient.rpc('context.create', {
      projectId: project.id, workItemId: wi.id, query: 'hello', selectedSources: [source.id],
    });
    // F07：项目指令层（P0-4 后 Agent 读 worktree——AGENTS.md 须在基线提交内，见上方 git init）。

    // F07：context.instructions 分层预览（全局→项目根）。
    const instrPreview = await toolClient.rpc('context.instructions', { projectId: project.id });
    const instrLabels = (instrPreview.layers ?? []).map((l) => l.label);
    assert(instrLabels.includes('global:Ratiflow.md'), `context.instructions 含全局层（${instrLabels}）`);
    assert(instrLabels.includes('project:AGENTS.md'), `context.instructions 含项目层（${instrLabels}）`);
    assert((instrPreview.promptBytes?.system ?? 0) > 0, 'context.instructions 带装配字节占比');

    // tool.list 由运行时注册表驱动（含 search_knowledge 与限制字段）。
    const tools = await toolClient.rpc('tool.list');
    const names = tools.items.map((t) => t.tool_id ?? t.name);
    assert(names.includes('search_knowledge'), 'tool.list 含注册表新工具 search_knowledge');
    assert(tools.items.find((t) => (t.tool_id ?? t.name) === 'run_command').max_result_bytes > 0, 'tool.list 带限制字段 max_result_bytes');

    // ===== M1/F03：run_command 高风险 → Run 暂停（而非作废/继续）=====
    const started = await toolClient.rpc('agent.start', {
      workItemId: wi.id, goal: '读文件→检索→命令→写草稿', contextManifestId: manifest.id,
      toolAllowlist: ['read_file', 'search_knowledge', 'run_command', 'write_file'],
      idempotencyKey: 'agent-e2e-tools-1',
    });
    // F06/F07/F08：装配摘要——指令层≥2（全局+项目）、manifest 知识项≥1、段落字节占比。
    const instr = started.instructions ?? {};
    assert((instr.instructionLayers ?? []).length >= 2, `指令层≥2（${JSON.stringify((instr.instructionLayers ?? []).map((l) => l.label))}）`);
    assert((instr.knowledgeItems ?? 0) >= 1, `manifest 知识项注入（${instr.knowledgeItems}）`);
    assert((instr.promptBytes?.knowledge ?? 0) > 0, '知识层字节占比存在');
    let run = await waitStatus(toolClient, started.runId, (st) =>
      ['paused', 'completed_execution', 'failed', 'cancelled'].includes(st));
    assert(run.status === 'paused', `run_command 触发暂停（实际 ${run.status}：${run.result}）`);
    assert(await waitEvent(toolClient, 'run.waiting_approval'), '收到 run.waiting_approval 通知');

    let props = (await toolClient.rpc('agent.proposals', { runId: started.runId })).items;
    let byTool = Object.fromEntries(props.map((p) => [p.tool, p]));
    assert(props.length === 3, `暂停时 3 个提案（实际 ${props.length}）`);
    assert(byTool.read_file.decision === 'executed' && byTool.read_file.result.includes('hello-m03'),
      'read_file 带真实参数读到真实内容');
    assert(byTool.search_knowledge.decision === 'executed' && byTool.search_knowledge.result.includes('NOTES.md'),
      'search_knowledge 库内检索命中项目文件');
    assert(byTool.run_command.decision === 'proposed', 'run_command 提案保持 proposed（挂起）');

    // 审批通过 → 恢复 → 完成；模型调用恰好 5 次（无重复计费）。
    let pending = await toolClient.rpc('approval.list', { limit: 10 });
    const appr = pending.items.find((a) => a.subject_type === 'tool_proposal') ?? pending.items[0];
    const decided = await toolClient.rpc('approval.decide', {
      approvalId: appr.id, decision: 'approved', decidedBy: 'e2e', reason: 'M1 恢复验证',
    });
    assert(decided?.run?.status === 'resuming', 'approval.decide 返回恢复联动');
    run = await waitTerminal(toolClient, started.runId, 30000);
    assert(run.status === 'completed_execution', `恢复后终态 completed（实际 ${run.status}：${run.result}）`);
    assert(await waitEvent(toolClient, 'run.resumed'), '收到 run.resumed 通知');

    props = (await toolClient.rpc('agent.proposals', { runId: started.runId })).items;
    byTool = Object.fromEntries(props.map((p) => [p.tool, p]));
    assert(props.length === 4, `4 个提案（实际 ${props.length}）`);
    assert(byTool.run_command.decision === 'executed' && byTool.run_command.result.includes('NOTES.md'),
      '恢复后执行挂起提案（wc -c 真实输出）');
    assert(byTool.write_file.decision === 'executed' && byTool.write_file.result.includes('written'),
      'write_file 落工件草稿区');
    const got = await toolClient.rpc('agent.get', { runId: started.runId });
    assert(got.modelCalls === 5, `模型调用恰好 5 次、不重复计费（实际 ${got.modelCalls}）`);
    assert(got.rollout && got.rollout.lines >= 8, `rollout 摘要存在（${got.rollout?.lines ?? 0} 行）`);

    // ===== M1/F03：拒绝路径 → Run failed（approval_rejected）=====
    const wi2 = await toolClient.rpc('workitem.create', { projectId: project.id, title: '拒绝路径', description: '' });
    const manifest2 = await toolClient.rpc('context.create', {
      projectId: project.id, workItemId: wi2.id, query: 'e2e', selectedSources: [],
    });
    const started2 = await toolClient.rpc('agent.start', {
      workItemId: wi2.id, goal: '将被拒绝', contextManifestId: manifest2.id,
      toolAllowlist: ['run_command'], idempotencyKey: 'agent-e2e-tools-2',
    });
    const run2 = await waitStatus(toolClient, started2.runId, (st) => st === 'paused');
    pending = await toolClient.rpc('approval.list', { limit: 10 });
    const appr2 = pending.items.find((a) => a.subject_type === 'tool_proposal') ?? pending.items[0];
    const rejected = await toolClient.rpc('approval.decide', {
      approvalId: appr2.id, decision: 'rejected', decidedBy: 'e2e', reason: '风险过大',
    });
    assert(rejected?.run?.status === 'failed', '拒绝联动返回 failed');
    const final2 = await toolClient.rpc('agent.get', { runId: started2.runId });
    assert(final2.status === 'failed' && final2.result.includes('审批拒绝'), `Run 以审批拒绝收尾（${final2.result}）`);

    // ===== M1/F04：rollout 入备份 + 秘密探针 + 篡改检测 =====
    const bk = await toolClient.rpc('backup.create', {});
    let verified = await toolClient.rpc('backup.verify', { backupId: bk.id });
    assert(verified.status === 'verified', `备份 verify 通过（${verified.status}）`);
    assert((verified.manifest.rolloutsCount ?? 0) >= 2, `manifest 带 rolloutsCount（${verified.manifest.rolloutsCount}）`);
    // 秘密探针：扫备份目录全部文件（db + rollouts/）
    const backupsDir = join(verified.path, '..');
    let secretHits = 0;
    for (const entry of readdirSync(backupsDir, { recursive: true })) {
      const full = join(backupsDir, entry.toString());
      let stat;
      try { stat = statSync(full); } catch { continue; }
      if (!stat.isFile()) continue;
      const body = readFileSync(full, 'utf8');
      if (/(sk-[A-Za-z0-9]{20}|ghp_[A-Za-z0-9]{20}|glpat-[A-Za-z0-9_-]{20}|AKIA[0-9A-Z]{16}|BEGIN [A-Z ]*PRIVATE KEY)/.test(body)) {
        secretHits += 1;
      }
    }
    assert(secretHits === 0, `备份目录秘密探针零命中（命中 ${secretHits}）`);
    // 篡改一个 rollout 字节 → verify corrupt
    const rolloutsDir = verified.path.replace(/\.db$/, '.rollouts');
    const rolloutFiles = readdirSync(rolloutsDir);
    appendFileSync(join(rolloutsDir, rolloutFiles[0]), 'tamper\n');
    const corrupt = await toolClient.rpc('backup.verify', { backupId: bk.id });
    assert(corrupt.status === 'corrupt', `篡改 rollout 后 verify corrupt（${corrupt.status}）`);

    // ===== M3/F10 场景3：toolPolicy 免批覆盖 + 真实权限快照落库 =====
    const tl = await toolClient.rpc('tool.list');
    const rc = tl.items.find((t) => t.tool_id === 'run_command');
    await toolClient.rpc('toolPolicy.update', {
      toolId: 'run_command', expectedRevision: rc.revision ?? 0, requiresApproval: false,
    });
    const wi3 = await toolClient.rpc('workitem.create', { projectId: project.id, title: '免批覆盖', description: '' });
    const manifest3 = await toolClient.rpc('context.create', {
      projectId: project.id, workItemId: wi3.id, query: 'hello', selectedSources: [source.id],
    });
    const s3 = await toolClient.rpc('agent.start', {
      workItemId: wi3.id, goal: '免批执行', contextManifestId: manifest3.id,
      toolAllowlist: ['run_command'], idempotencyKey: 'agent-e2e-tools-3',
    });
    assert(s3.instructions?.modeSource === 'env', `执行模式来源标记（${s3.instructions?.modeSource}）`);
    const run3 = await waitTerminal(toolClient, s3.runId, 30000);
    assert(run3.status === 'completed_execution', `免批后直接完成、无暂停（实际 ${run3.status}：${run3.result}）`);
    const props3 = (await toolClient.rpc('agent.proposals', { runId: s3.runId })).items;
    assert(props3.length === 1 && props3[0].tool === 'run_command' && props3[0].decision === 'executed',
      '免批覆盖后 run_command 直接执行');
    const g3 = await toolClient.rpc('agent.get', { runId: s3.runId });
    assert(g3.policySnapshot && g3.policySnapshot !== 'default' && g3.policySnapshot.includes('run_command'),
      '真实权限快照落库（非 default 占位）');
    assert(g3.policySnapshot.includes('settings'), '快照含设置来源标记');
    console.log('\nAgent 工具链 E2E 通过。');

    // ===== M3/F10 场景4：executionProfile 设置域模式来源（独立 core，无 env 覆盖）=====
    const dataDir4 = mkdtempSync(join(tmpdir(), 'sg-agent-e2e4-'));
    const projDir4 = mkdtempSync(join(tmpdir(), 'sg-agent-e2e4-proj-'));
    writeFileSync(join(projDir4, 'NOTES.md'), 'hello-m3');
    const script4 = [
      { content: '{"action":"read_file","arguments":{"path":"NOTES.md"},"summary":"读取A"}', tokensIn: 10, tokensOut: 5 },
      { content: '{"action":"final","summary":"A完成"}', tokensIn: 10, tokensOut: 5 },
      { content: '{"action":"read_file","arguments":{"path":"NOTES.md"},"summary":"读取B"}', tokensIn: 10, tokensOut: 5 },
      { content: '{"action":"final","summary":"B完成"}', tokensIn: 10, tokensOut: 5 },
    ];
    const script4Path = join(tmpdir(), `sg-agent-e2e4-script-${Date.now()}.json`);
    writeFileSync(script4Path, JSON.stringify(script4));
    const modeClient = new CoreClient(CORE, dataDir4, {
      RATIFLOW_FAKE_MODEL_SCRIPT: script4Path, // 无 EXEC_MODE：默认来源=探测
    });
    try {
      const project4 = await modeClient.rpc('project.create', {
        gitlabInstance: 'x', namespace: 'n', project: 'p', name: '模式来源', localRoot: projDir4,
      });
      const source4 = await modeClient.rpc('knowledge.create', {
        projectId: project4.id, kind: 'repo_path', name: 'p4', locator: projDir4,
      });
      await modeClient.rpc('knowledge.scan', { sourceId: source4.id, projectRoot: projDir4 });
      const wi4 = await modeClient.rpc('workitem.create', { projectId: project4.id, title: '模式来源', description: '' });
      const manifest4 = await modeClient.rpc('context.create', {
        projectId: project4.id, workItemId: wi4.id, query: 'hello', selectedSources: [source4.id],
      });
      // Run A：默认探测来源（Docker 读不到宿主文件 / Disabled 拒绝）——都读不到 hello-m3。
      const sa = await modeClient.rpc('agent.start', {
        workItemId: wi4.id, goal: 'A', contextManifestId: manifest4.id,
        toolAllowlist: ['read_file'], idempotencyKey: 'mode-a',
      });
      const runA = await waitTerminal(modeClient, sa.runId, 30000);
      assert(runA.status === 'completed_execution', `Run A 完成（${runA.status}）`);
      const propA = (await modeClient.rpc('agent.proposals', { runId: sa.runId })).items[0];
      assert(!propA.result.includes('hello-m3'), `默认来源（探测）读不到宿主文件内容（decision=${propA.decision}）`);
      // 设置域生效：executionProfile → safe_restricted（env 缺省时覆盖探测）。
      await modeClient.rpc('executor.settings.update', {
        settings: { mode: 'safe_restricted', unsafeConfirmed: false }, expectedRevision: 0,
      });
      const eff = await modeClient.rpc('executor.settings.get', {});
      assert(eff.effective?.mode === 'safe_restricted' && eff.effective?.source === 'settings',
        `生效模式来源=设置（${JSON.stringify(eff.effective)}）`);
      const sb = await modeClient.rpc('agent.start', {
        workItemId: wi4.id, goal: 'B', contextManifestId: manifest4.id,
        toolAllowlist: ['read_file'], idempotencyKey: 'mode-b',
      });
      const runB = await waitTerminal(modeClient, sb.runId, 30000);
      assert(runB.status === 'completed_execution', `Run B 完成（${runB.status}）`);
      const propB = (await modeClient.rpc('agent.proposals', { runId: sb.runId })).items[0];
      assert(propB.decision === 'executed' && propB.result.includes('hello-m3'),
        `设置来源生效：read_file 读到真实内容（decision=${propB.decision}）`);
      console.log('Agent 模式来源 E2E 通过。');

      // M4：model.usage 观测 RPC——token/缓存/延迟聚合与压缩计数。
      const usage = await modeClient.rpc('model.usage', { runId: sb.runId });
      assert(usage.calls >= 1, `model.usage 应有调用轮次（${usage.calls}）`);
      assert(typeof usage.tokensIn === 'number' && typeof usage.tokensOut === 'number');
      assert(usage.cachedTokens === 0, `fake 脚本无缓存命中（${usage.cachedTokens}）`);
      assert(usage.compactions === 0, `无压缩事件（${usage.compactions}）`);
      const usageAll = await modeClient.rpc('model.usage', {});
      assert(usageAll.calls >= usage.calls, '全局聚合 ≥ 单 Run');
      console.log('模型用量观测（model.usage）E2E 通过。');
    } finally {
      modeClient.kill();
      rmSync(dataDir4, { recursive: true, force: true });
      rmSync(projDir4, { recursive: true, force: true });
      rmSync(script4Path, { force: true });
    }
  } finally {
    toolClient.kill();
    rmSync(dataDir2, { recursive: true, force: true });
    rmSync(projDir, { recursive: true, force: true });
    rmSync(scriptPath, { force: true });
  }
} finally {
  client.kill();
  rmSync(dataDir, { recursive: true, force: true });
}
