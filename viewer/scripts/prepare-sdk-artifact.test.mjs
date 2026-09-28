import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import test from 'node:test';
import { normalizeNpmPackReport } from './npm-pack-report.mjs';

import { prepareSdkArtifact } from './prepare-sdk-artifact.mjs';

const sdkSource = process.env.TESLATLAS_SDK_SOURCE;
const nodeExecutable = process.env.TESLATLAS_SDK_NODE;
const archiveSha256 = '138d1e6924828e655f3fc8c58731467983a3a0e0086ec927c182b7cdd7a09340';

const packReport = {
  name: '@teslatlas/sdk',
  version: '2026.36.2',
  filename: 'teslatlas-sdk-2026.36.2.tgz',
  entryCount: 88,
};

test('normalizes npm 12 package-keyed pack output and historical array output', () => {
  assert.deepEqual(
    normalizeNpmPackReport({ '@teslatlas/sdk': packReport }, '@teslatlas/sdk'),
    packReport,
  );
  assert.deepEqual(normalizeNpmPackReport([packReport], '@teslatlas/sdk'), packReport);
  assert.throws(
    () => normalizeNpmPackReport({ '@other/sdk': packReport }, '@teslatlas/sdk'),
    /shape is invalid/u,
  );
});

test('builds the pinned SDK archive from clean source without a checked-in tarball', {
  skip: !sdkSource || !nodeExecutable,
}, async () => {
  const projectRoot = await mkdtemp(join(tmpdir(), 'viewer-sdk-prepare-'));
  try {
    await mkdir(join(projectRoot, 'artifacts'));
    await writeFile(
      join(projectRoot, 'artifacts/teslatlas-sdk.json'),
      `${JSON.stringify({
        schema_version: 1,
        package_name: '@teslatlas/sdk',
        package_version: '2026.36.2',
        tarball: 'artifacts/teslatlas-sdk-2026.36.2.tgz',
      tarball_sha256: archiveSha256,
      installed_member_count: 88,
      source: {
        commit: '5662abf8cabbd078f3512b2fb8aa02d22da6b5cf',
        node_version: 'v26.10.0',
        npm_version: '12.1.0',
      },
      })}\n`,
    );

    const result = await prepareSdkArtifact({
      projectRoot,
      sdkSource: resolve(sdkSource),
      nodeExecutable: resolve(nodeExecutable),
    });

    assert.equal(result.tarballSha256, archiveSha256);
    assert.equal(result.memberCount, 88);
    const tarball = join(projectRoot, 'artifacts/teslatlas-sdk-2026.36.2.tgz');
    assert.equal((await readFile(tarball)).byteLength, 285_000);
    assert.equal(
      execFileSync('tar', ['-tzf', tarball], { encoding: 'utf8' })
        .trim()
        .split('\n').length,
      88,
    );
  } finally {
    await rm(projectRoot, { recursive: true, force: true });
  }
});
