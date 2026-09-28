import assert from 'node:assert/strict';
import { mkdtemp, cp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import test from 'node:test';

import { verifySdkArtifact } from './verify-sdk-artifact.mjs';

const projectRoot = resolve(import.meta.dirname, '..');

test('admits the exact vendored and installed SDK contents', async () => {
  const result = await verifySdkArtifact({ projectRoot });

  assert.equal(
    result.tarballSha256,
    '1e62303c17d558002001de0aeccb90e50c3644c76f46ced4e492b2960ec6f4da',
  );
  assert.equal(
    result.installedContentManifestSha256,
    '853cd8fc6bc5b1d063b599cdf26c7543c8d41f4bcf0ab3c84fa90c1bd8002cf8',
  );
  assert.equal(result.installedMemberCount, 88);
});

test('rejects a changed transitive installed SDK file', async () => {
  const temporary = await mkdtemp(join(tmpdir(), 'viewer-sdk-gate-'));
  const packageRoot = join(temporary, 'sdk');
  try {
    await cp(join(projectRoot, 'node_modules/@teslatlas/sdk'), packageRoot, {
      recursive: true,
    });
    const clientPath = join(packageRoot, 'dist/hub/client.js');
    const bytes = await readFile(clientPath);
    await writeFile(clientPath, Buffer.concat([bytes, Buffer.from('\n// changed\n')]));

    await assert.rejects(
      verifySdkArtifact({ projectRoot, packageRoot }),
      /installed SDK member does not match the vendored tarball/u,
    );
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
});
