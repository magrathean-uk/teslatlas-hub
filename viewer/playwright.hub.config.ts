import { defineConfig } from '@playwright/test';

const hostPort = Number(process.env.TESLATLAS_VIEWER_HOST_PORT ?? '43130');
const externalServer = process.env.TESLATLAS_VIEWER_EXTERNAL_SERVER === '1';
const pageOrigin = process.env.TESLATLAS_VIEWER_PAGE_ORIGIN;

if (externalServer && !pageOrigin) {
  throw new Error(
    'TESLATLAS_VIEWER_PAGE_ORIGIN is required with TESLATLAS_VIEWER_EXTERNAL_SERVER=1',
  );
}

export default defineConfig({
  testDir: './e2e',
  fullyParallel: false,
  forbidOnly: true,
  retries: 0,
  workers: 1,
  reporter: [['list']],
  outputDir: 'test-results/live-hub',
  use: {
    baseURL: externalServer ? pageOrigin : `http://127.0.0.1:${hostPort}`,
    trace: 'off',
    screenshot: 'off',
    video: 'off',
    locale: 'en-GB',
    timezoneId: 'UTC',
    colorScheme: 'light',
    contextOptions: { reducedMotion: 'reduce' },
  },
  ...(externalServer
    ? {}
    : {
        webServer: {
          command: `node bin/teslatlas-viewer.mjs --host 127.0.0.1 --port ${hostPort}`,
          url: `http://127.0.0.1:${hostPort}`,
          reuseExistingServer: false,
        },
      }),
});
