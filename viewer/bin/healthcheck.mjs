import http from 'node:http';

const port = Number(process.env.TESLATLAS_VIEWER_HEALTH_PORT ?? 4173);

if (!Number.isSafeInteger(port) || port < 1 || port > 65_535) {
  process.stderr.write('invalid health-check port\n');
  process.exit(1);
}

const request = http.get(
  {
    host: '127.0.0.1',
    port,
    path: '/',
    timeout: 2_000,
  },
  (response) => {
    response.resume();
    if (response.statusCode !== 200) {
      process.stderr.write(`Viewer returned HTTP ${response.statusCode ?? 'unknown'}\n`);
      process.exitCode = 1;
    }
  },
);

request.once('timeout', () => request.destroy(new Error('health-check timed out')));
request.once('error', (error) => {
  process.stderr.write(`Viewer health check failed: ${error.message}\n`);
  process.exitCode = 1;
});

