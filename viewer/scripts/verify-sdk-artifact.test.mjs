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
    'd1ab6ba0ede3a24ae12ed4151db0c90bf957fa19f5640cc4323bd368e565e8bb',
  );
  assert.equal(
    result.installedContentManifestSha256,
    '2b8b4c2b73ea4ca6dce50092d56172bb3114c55034890ee1d8ecea8831786bb5',
  );
  assert.equal(result.installedMemberCount, 80);
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
