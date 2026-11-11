import { defineConfig } from '@playwright/test';

const hostPort = Number(process.env.TESLATLAS_VIEWER_HOST_PORT ?? '43130');
const externalServer = process.env.TESLATLAS_VIEWER_EXTERNAL_SERVER === '1';
const pageOrigin = process.env.TESLATLAS_VIEWER_PAGE_ORIGIN;
const browserExecutable = process.env.TESLATLAS_VIEWER_BROWSER_EXECUTABLE;
const rawEvidenceDir = process.env.TESLATLAS_VIEWER_RAW_EVIDENCE_DIR;

if (externalServer && (!pageOrigin || !browserExecutable || !rawEvidenceDir)) {
  throw new Error(
    'TESLATLAS_VIEWER_PAGE_ORIGIN, TESLATLAS_VIEWER_BROWSER_EXECUTABLE, and TESLATLAS_VIEWER_RAW_EVIDENCE_DIR are required with TESLATLAS_VIEWER_EXTERNAL_SERVER=1',
  );
}

export default defineConfig({
  testDir: './e2e',
  fullyParallel: false,
  forbidOnly: true,
  retries: 0,
  workers: 1,
  reporter: [['list']],
  outputDir: externalServer ? rawEvidenceDir : 'test-results/live-hub',
  use: {
    baseURL: externalServer ? pageOrigin : `http://127.0.0.1:${hostPort}`,
    trace: 'off',
    screenshot: 'off',
    video: 'off',
    locale: 'en-GB',
    timezoneId: 'UTC',
    colorScheme: 'light',
    contextOptions: { reducedMotion: 'reduce' },
    ...(externalServer ? { launchOptions: { executablePath: browserExecutable } } : {}),
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
