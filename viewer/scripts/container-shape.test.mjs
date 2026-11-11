import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const projectRoot = join(dirname(fileURLToPath(import.meta.url)), '..');

test('Docker runtime has a non-root static-service health check without persistence', async () => {
  const dockerfile = await readFile(join(projectRoot, 'Dockerfile'), 'utf8');
  const compose = await readFile(join(projectRoot, 'compose.yaml'), 'utf8');

  assert.match(
    dockerfile,
    /FROM node:26\.10\.0-bookworm-slim@sha256:662933cf47f013bc8e4beb31a6116448427a82057ba7c42c97e4c5ba766504c2 AS runtime/u,
  );
  assert.match(dockerfile, /npm install --global npm@12\.1\.0/u);
  assert.match(dockerfile, /COPY --from=build --chown=node:node \/build\/bin\/healthcheck\.mjs \.\/bin\/healthcheck\.mjs/u);
  assert.match(dockerfile, /USER node\n/u);
  assert.match(
    dockerfile,
    /HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 CMD \["node", "bin\/healthcheck\.mjs"\]/u,
  );
  assert.doesNotMatch(dockerfile, /^VOLUME\b/mu);
  assert.match(dockerfile, /EXPOSE 4173\n/u);

  assert.match(compose, /127\.0\.0\.1:4173:4173/u);
  assert.match(compose, /restart: unless-stopped/u);
  assert.doesNotMatch(compose, /^\s+volumes:/mu);
});
