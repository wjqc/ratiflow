import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { join, resolve } from 'node:path';

const desktopRoot = process.cwd();
const monorepoRoot = resolve(desktopRoot, '..', '..');
const npmCommand = process.platform === 'win32' ? 'npm.cmd' : 'npm';

function run(command, args, options = {}) {
  return new Promise((resolveRun, rejectRun) => {
    const child = spawn(command, args, {
      cwd: desktopRoot,
      env: process.env,
      stdio: 'inherit',
      ...options,
    });
    child.once('error', rejectRun);
    child.once('exit', (code, signal) => {
      if (signal) {
        process.kill(process.pid, signal);
        return;
      }
      if (code !== 0) {
        rejectRun(new Error(`${command} exited with code ${code ?? 'unknown'}`));
        return;
      }
      resolveRun();
    });
  });
}

await run(npmCommand, ['run', 'build']);

if (process.platform === 'darwin') {
  const builder = join(monorepoRoot, 'node_modules', '.bin', 'electron-builder');
  await run(builder, ['--dir']);

  const appBundle = join(desktopRoot, 'release', `mac-${process.arch}`, 'Ratiflow.app');
  const executable = join(appBundle, 'Contents', 'MacOS', 'Ratiflow');
  if (!existsSync(executable)) {
    throw new Error(`Ratiflow development bundle is missing: ${executable}`);
  }
  await run(executable, []);
} else {
  const electron = join(monorepoRoot, 'node_modules', '.bin', process.platform === 'win32' ? 'electron.cmd' : 'electron');
  await run(electron, ['.']);
}
