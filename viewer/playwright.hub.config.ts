import { defineConfig } from '@playwright/test';

const hostPort = Number(process.env.TESLATLAS_VIEWER_HOST_PORT ?? '43130');

export default defineConfig({
  testDir: './e2e',
  fullyParallel: false,
  forbidOnly: true,
  retries: 0,
  workers: 1,
  reporter: [['list']],
  outputDir: 'test-results/live-hub',
  use: {
    trace: 'off',
    screenshot: 'off',
    video: 'off',
    locale: 'en-GB',
    timezoneId: 'UTC',
    colorScheme: 'light',
    contextOptions: { reducedMotion: 'reduce' },
  },
  webServer: {
    command: `npm run preview -- --host 127.0.0.1 --port ${hostPort}`,
    url: `http://127.0.0.1:${hostPort}`,
    reuseExistingServer: false,
  },
});
