import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './e2e',
  timeout: 120_000,
  retries: 0,
  workers: 1, // Electron 实例互斥（core 数据目录与 Keychain 探测不可并行）
  reporter: [['list']],
  use: {
    trace: 'off',
  },
});
