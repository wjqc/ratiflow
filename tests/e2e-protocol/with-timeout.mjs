#!/usr/bin/env node
// e2e 全程看门狗（缺陷审计 2026-09-07）：core 死锁/挂起时脚本内无超时会永久挂死
// `make e2e`。包装器给每个场景一个硬性总时限，超时 kill 子进程并以失败退出。
import { spawn } from 'node:child_process';
import { argv, exit } from 'node:process';

const secs = Number(argv[2] ?? 300);
const script = argv[3];
if (!script || !Number.isFinite(secs) || secs <= 0) {
  console.error('用法: node with-timeout.mjs <seconds> <script> [args...]');
  exit(2);
}

// 脚本名相对本文件目录解析（Makefile 从仓库根传裸文件名），执行 cwd 为仓库根
//（脚本以仓库根为基准解析 core 二进制/契约路径）。
const scriptPath = new URL(script, import.meta.url).pathname;
const child = spawn(process.execPath, [scriptPath, ...argv.slice(4)], {
  stdio: 'inherit',
  cwd: new URL('../../', import.meta.url).pathname,
});
const timer = setTimeout(() => {
  console.error(`\n✕ e2e 看门狗: ${script} 超过 ${secs}s 未结束，判定失败（core 挂起/死锁嫌疑）`);
  child.kill('SIGKILL');
  exit(1);
}, secs * 1000);

child.on('exit', (code, signal) => {
  clearTimeout(timer);
  if (signal) exit(1);
  else exit(code ?? 1);
});
