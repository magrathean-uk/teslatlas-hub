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
    '138d1e6924828e655f3fc8c58731467983a3a0e0086ec927c182b7cdd7a09340',
  );
  assert.equal(
    result.installedContentManifestSha256,
    'dc3b8cf27fdd514474b048dda0587a60f094e32e1ab9cab185608695dd7b5ea4',
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
