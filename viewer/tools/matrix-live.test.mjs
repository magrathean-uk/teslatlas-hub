import { chmod, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { spawn } from 'node:child_process';
import test from 'node:test';
import assert from 'node:assert/strict';

import { validateViewerWire } from './matrix-live.mjs';

const launcher = path.resolve(new URL('./matrix-live.mjs', import.meta.url).pathname);

function run(args) {
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [launcher, ...args], { cwd: path.dirname(launcher), stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => { stdout += String(chunk); });
    child.stderr.on('data', (chunk) => { stderr += String(chunk); });
    child.once('close', (code, signal) => resolve({ code, signal, stdout, stderr }));
  });
}

test('matrix launcher help is side-effect free', async () => {
  const result = await run(['--help']);
  assert.equal(result.code, 0);
  assert.match(result.stdout, /SessionInput JSON/u);
  assert.equal(result.stderr, '');
});

test('matrix launcher rejects missing and malformed SessionInput', async () => {
  const missing = await run([]);
  assert.equal(missing.code, 1);
  assert.match(missing.stderr, /one private SessionInput path/u);

  const root = await mkdtemp(path.join(tmpdir(), 'teslatlas-viewer-matrix-'));
  try {
    const duplicate = path.join(root, 'duplicate.json');
    await writeFile(duplicate, '{"schema_version":1,"schema_version":1}\n', { mode: 0o600 });
    await chmod(duplicate, 0o600);
    const rejectedDuplicate = await run([duplicate]);
    assert.equal(rejectedDuplicate.code, 1);
    assert.match(rejectedDuplicate.stderr, /JSON input|session input/u);

    const wrongShape = path.join(root, 'wrong-shape.json');
    await writeFile(wrongShape, '{"schema_version":1}\n', { mode: 0o600 });
    await chmod(wrongShape, 0o600);
    const rejectedShape = await run([wrongShape]);
    assert.equal(rejectedShape.code, 1);
    assert.match(rejectedShape.stderr, /session input fields/u);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('Viewer wire binds the cross-origin installed page, CA, browser and private reservations', () => {
  const wire = {
    page: {
      origin: 'https://127.0.0.1:18481',
      server_authority: 'runner-owned-installed-viewer',
      artifact_role: 'viewer_package_tarball',
    },
    hub: {
      public_origin: 'https://127.0.0.1:18480',
      cors_allowed_origin: 'https://127.0.0.1:18481',
      cross_origin: true,
    },
    trusted_ca: {
      certificate: {
        id: 'viewer_trusted_ca',
        root: { path: '/tmp/viewer-wire/root-ca.pem', sha256: 'a'.repeat(64) },
        local: { path: '/tmp/viewer-wire/local-ca.pem', sha256: 'a'.repeat(64) },
      },
      certificate_der_sha256: 'b'.repeat(64),
    },
    browser: {
      authority: 'runner-bound-executable',
      engine: 'chromium',
      version: '151.0.7922.34',
      executable: { path: '/tmp/viewer-wire/chromium', sha256: 'c'.repeat(64) },
    },
    reservations: {
      raw_evidence_dir: '/tmp/viewer-wire/outputs/raw-evidence',
      browser_log: '/tmp/viewer-wire/outputs/browser.json',
      close_record: '/tmp/viewer-wire/outputs/close.json',
      supplement: '/tmp/viewer-wire/outputs/supplement.json',
    },
  };

  assert.deepEqual(validateViewerWire(wire), wire);
  assert.throws(
    () => validateViewerWire({ ...wire, hub: { ...wire.hub, cors_allowed_origin: wire.hub.public_origin } }),
    /CORS binding is invalid/u,
  );
  assert.throws(
    () => validateViewerWire({ ...wire, page: { ...wire.page, origin: 'http://127.0.0.1:18481' } }),
    /origin is invalid/u,
  );
  assert.throws(
    () => validateViewerWire({
      ...wire,
      trusted_ca: {
        ...wire.trusted_ca,
        certificate: {
          ...wire.trusted_ca.certificate,
          local: { ...wire.trusted_ca.certificate.local, sha256: 'd'.repeat(64) },
        },
      },
    }),
    /root\/local digest differs/u,
  );
  assert.throws(
    () => validateViewerWire({
      ...wire,
      reservations: { ...wire.reservations, browser_log: '/tmp/another/browser.json' },
    }),
    /distinct siblings/u,
  );
});
