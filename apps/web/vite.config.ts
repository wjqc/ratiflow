import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  build: {
    outDir: '../../internal/web/dist',
    emptyOutDir: true,
  },
  server: {
    proxy: {
      '/api': 'http://127.0.0.1:7666',
      '/healthz': 'http://127.0.0.1:7666',
    },
  },
  test: {
    environment: 'jsdom',
    setupFiles: './src/test-setup.ts',
  },
});
