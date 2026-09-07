import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, readFile, readdir, realpath, rename, rm, stat, symlink, writeFile } from 'node:fs/promises';
import http from 'node:http';
import os from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import test from 'node:test';

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

function run(command, arguments_, options = {}) {
  return new Promise((resolveRun, reject) => {
    const child = spawn(command, arguments_, {
      cwd: options.cwd ?? projectRoot,
      env: options.env ?? process.env,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';
    const timer = setTimeout(() => {
      child.kill('SIGKILL');
      reject(new Error(`${command} timed out`));
    }, options.timeout ?? 60_000);
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    child.once('error', (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.once('exit', (code, signal) => {
      clearTimeout(timer);
      resolveRun({ code, signal, stdout, stderr });
    });
  });
}

async function npm(arguments_, options) {
  return run('npm', arguments_, options);
}

async function waitForListening(child) {
  return new Promise((resolveListening, reject) => {
    let stdout = '';
    let stderr = '';
    const timer = setTimeout(() => {
      reject(new Error(`Viewer CLI did not listen: ${stderr}`));
    }, 10_000);
    const inspect = (chunk) => {
      stdout += chunk;
      const match = stdout.match(/Teslatlas Viewer listening on (http:\/\/[^\s]+)\n/u);
      if (match !== null) {
        clearTimeout(timer);
        resolveListening({ origin: match[1], stdout: () => stdout, stderr: () => stderr });
      }
    };
    child.stdout.on('data', inspect);
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    child.once('exit', (code, signal) => {
      clearTimeout(timer);
      reject(new Error(`Viewer CLI exited before listening (${code ?? signal}): ${stderr}`));
    });
    child.once('error', reject);
  });
}

async function waitForExit(child, timeout = 10_000) {
  return new Promise((resolveExit, reject) => {
    const timer = setTimeout(() => {
      child.kill('SIGKILL');
      reject(new Error('Viewer CLI did not exit'));
    }, timeout);
    child.once('exit', (code, signal) => {
      clearTimeout(timer);
      resolveExit({ code, signal });
    });
    child.once('error', reject);
  });
}

function rawRequest(origin, path, method = 'GET') {
  const url = new URL(origin);
  return new Promise((resolveRequest, reject) => {
    const request = http.request({
      host: url.hostname,
      port: url.port,
      path,
      method,
    }, (response) => {
      const chunks = [];
      response.on('data', (chunk) => chunks.push(chunk));
      response.on('end', () => resolveRequest({
        status: response.statusCode,
        headers: response.headers,
        body: Buffer.concat(chunks),
      }));
    });
    request.once('error', reject);
    request.end();
  });
}

async function bundleFingerprint(root) {
  const files = [];
  async function visit(directory) {
    for (const entry of (await readdir(directory, { withFileTypes: true }))
      .sort((left, right) => left.name.localeCompare(right.name))) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) await visit(path);
      else if (entry.isFile()) {
        files.push({
          path: path.slice(root.length + 1),
          sha256: sha256(await readFile(path)),
        });
      } else {
        throw new Error(`non-regular bundle entry: ${path}`);
      }
    }
  }
  await visit(root);
  files.sort((left, right) => left.path.localeCompare(right.path));
  return {
    files,
    manifestSha256: sha256(Buffer.from(`${JSON.stringify(files)}\n`, 'utf8')),
  };
}

test('packed Viewer CLI serves only its installed built assets and shuts down cleanly', async () => {
  const temporary = await mkdtemp(join(os.tmpdir(), 'teslatlas-viewer-cli-'));
  let server;
  try {
    let packagePath = process.env.TESLATLAS_VIEWER_PACKAGE;
    if (packagePath === undefined) {
      const packed = await npm(['pack', '--silent', '--json', '--pack-destination', temporary]);
      assert.equal(packed.code, 0, packed.stderr);
      const jsonStart = packed.stdout.lastIndexOf('\n[');
      const packResult = JSON.parse(
        jsonStart === -1 ? packed.stdout : packed.stdout.slice(jsonStart + 1),
      );
      packagePath = join(temporary, packResult[0].filename);
    }
    packagePath = resolve(packagePath);
    const installRoot = temporary;
    await writeFile(join(temporary, 'package.json'), '{"private":true}\n');
    const installed = await npm(
      ['install', '--no-audit', '--no-fund', '--omit=dev', packagePath],
      { cwd: temporary },
    );
    assert.equal(installed.code, 0, installed.stderr);

    const binary = join(temporary, 'node_modules', '.bin', 'teslatlas-viewer');
    const binaryMetadata = await stat(binary);
    assert.equal(binaryMetadata.isFile() || binaryMetadata.isSymbolicLink(), true);
    const packageRoot = resolve(dirname(await realpath(binary)), '..');
    const packageMetadata = JSON.parse(await readFile(join(packageRoot, 'package.json'), 'utf8'));

    const help = await run(binary, ['--help'], { cwd: installRoot });
    assert.equal(help.code, 0, help.stderr);
    assert.match(help.stdout, /--host/u);
    assert.match(help.stdout, /--port/u);
    const version = await run(binary, ['--version'], { cwd: installRoot });
    assert.equal(version.code, 0, version.stderr);
    assert.equal(version.stdout.trim(), packageMetadata.version);

    server = spawn(binary, ['--host', '127.0.0.1', '--port', '0'], {
      cwd: installRoot,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    const listening = await waitForListening(server);
    const origin = listening.origin;
    const bound = new URL(origin);
    assert.equal(bound.hostname, '127.0.0.1');
    assert.notEqual(bound.port, '0');

    const index = await rawRequest(origin, '/');
    assert.equal(index.status, 200);
    assert.match(index.headers['content-type'], /^text\/html/u);
    assert.match(index.body.toString('utf8'), /<div id="root"><\/div>/u);
    const assetPath = index.body.toString('utf8').match(/src="(\/assets\/[^"]+\.js)"/u)?.[1];
    assert.ok(assetPath);
    const asset = await rawRequest(origin, assetPath);
    assert.equal(asset.status, 200);
    assert.match(asset.headers['content-type'], /javascript/u);
    assert.ok(asset.body.length > 1_000);
    const versionResponse = await rawRequest(origin, '/version.json');
    assert.equal(versionResponse.status, 200);
    assert.equal(
      JSON.parse(versionResponse.body.toString('utf8')).product_version,
      packageMetadata.version,
    );
    const head = await rawRequest(origin, '/', 'HEAD');
    assert.equal(head.status, 200);
    assert.equal(head.body.length, 0);
    assert.equal(Number(head.headers['content-length']), index.body.length);
    assert.equal((await rawRequest(origin, '/app/current')).status, 200);
    assert.equal((await rawRequest(origin, '/assets')).status, 404);
    assert.equal((await rawRequest(origin, '/assets/missing.js')).status, 404);
    const methodRejected = await rawRequest(origin, '/', 'POST');
    assert.equal(methodRejected.status, 405);
    assert.equal(methodRejected.headers.allow, 'GET, HEAD');
    assert.equal((await rawRequest(origin, '/%2e%2e/package.json')).status, 403);
    assert.equal((await rawRequest(origin, '/..%2fpackage.json')).status, 403);
    assert.equal((await rawRequest(origin, '/%252e%252e%252fpackage.json')).status, 403);

    const outside = join(temporary, 'outside.txt');
    await writeFile(outside, 'must not be served\n');
    const escapeLink = join(packageRoot, 'dist', 'escape.txt');
    await symlink(outside, escapeLink);
    assert.equal((await rawRequest(origin, '/escape.txt')).status, 403);
    await rm(escapeLink);

    const indexPath = join(packageRoot, 'dist', 'index.html');
    const hiddenIndex = join(packageRoot, 'dist', 'index.original-test');
    const outsideIndex = join(temporary, 'outside-index.html');
    const outsideSentinel = 'VIEWER_TEST_OUTSIDE_INDEX_SENTINEL\n';
    await writeFile(outsideIndex, outsideSentinel);
    await rename(indexPath, hiddenIndex);
    await symlink(outsideIndex, indexPath);
    try {
      for (const path of ['/', '/app/current', '/index.html']) {
        const response = await rawRequest(origin, path);
        assert.equal(response.status, 403);
        assert.equal(response.body.includes(outsideSentinel), false);
      }
    } finally {
      await rm(indexPath);
      await rename(hiddenIndex, indexPath);
    }
    assert.equal((await rawRequest(origin, '/')).status, 200);
    assert.equal((await rawRequest(origin, '/app/current')).status, 200);
    assert.equal((await rawRequest(origin, '/index.html')).status, 200);

    const conflict = await run(binary, ['--host', '127.0.0.1', '--port', bound.port]);
    assert.equal(conflict.code, 1);
    assert.match(conflict.stderr, /address .* already in use/iu);

    server.kill('SIGTERM');
    const stopped = await waitForExit(server);
    server = undefined;
    assert.equal(stopped.code, 0);
    assert.equal(stopped.signal, null);

    const interruptedServer = spawn(binary, ['--port', '0'], {
      cwd: installRoot,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    const interruptedListening = await waitForListening(interruptedServer);
    assert.equal(new URL(interruptedListening.origin).hostname, '127.0.0.1');
    interruptedServer.kill('SIGINT');
    const interrupted = await waitForExit(interruptedServer);
    assert.equal(interrupted.code, 0);
    assert.equal(interrupted.signal, null);

    const dist = join(packageRoot, 'dist');
    const hiddenDist = join(packageRoot, 'dist.missing-test');
    await rename(dist, hiddenDist);
    try {
      const missing = await run(binary, ['--host', '127.0.0.1', '--port', '0']);
      assert.equal(missing.code, 1);
      assert.match(missing.stderr, /built Viewer assets are missing/iu);
    } finally {
      await rename(hiddenDist, dist);
    }

    await rename(indexPath, hiddenIndex);
    await symlink(outsideIndex, indexPath);
    try {
      const unsafeIndex = await run(binary, ['--host', '127.0.0.1', '--port', '0']);
      assert.equal(unsafeIndex.code, 1);
      assert.match(unsafeIndex.stderr, /built Viewer assets are missing/iu);
      assert.equal(unsafeIndex.stdout.includes('listening'), false);
    } finally {
      await rm(indexPath);
      await rename(hiddenIndex, indexPath);
    }

    const outsideBundle = join(temporary, 'outside-bundle');
    await rename(dist, hiddenDist);
    await rename(hiddenDist, outsideBundle);
    await symlink(outsideBundle, dist, 'dir');
    try {
      const escapedRoot = await run(binary, ['--host', '127.0.0.1', '--port', '0']);
      assert.equal(escapedRoot.code, 1);
      assert.match(escapedRoot.stderr, /built Viewer assets are missing/iu);
    } finally {
      await rm(dist);
      await rename(outsideBundle, dist);
    }

    const archiveEntriesResult = await run('tar', ['-tzf', packagePath]);
    assert.equal(archiveEntriesResult.code, 0, archiveEntriesResult.stderr);
    const archiveEntries = archiveEntriesResult.stdout.trim().split('\n').sort();
    assert.ok(archiveEntries.includes('package/bin/teslatlas-viewer.mjs'));
    assert.ok(archiveEntries.includes('package/dist/index.html'));
    assert.equal(archiveEntries.some((path) => path.includes('node_modules/')), false);
    assert.equal(archiveEntries.some((path) => /(credential|secret|\.cache|test-results|playwright-report)/iu.test(path)), false);

    const npmVersion = await npm(['--version']);
    assert.equal(npmVersion.code, 0, npmVersion.stderr);
    const receipt = {
      schemaVersion: 1,
      package: {
        name: packageMetadata.name,
        version: packageMetadata.version,
        sha256: sha256(await readFile(packagePath)),
        entries: archiveEntries,
      },
      bundle: await bundleFingerprint(join(packageRoot, 'dist')),
      runtime: { node: process.version, npm: npmVersion.stdout.trim() },
      boundOrigin: origin,
      checks: {
        help: true,
        version: true,
        indexGet: true,
        referencedAssetGet: true,
        versionGet: true,
        head: true,
        methodRejected: true,
        spaFallback: true,
        missingAsset404: true,
        traversalRejected: true,
        outsideSymlinkRejected: true,
        portConflictRejected: true,
        sigtermExitCode: 0,
        sigintExitCode: 0,
        defaultLoopback: true,
        missingBuildRejected: true,
        assetRootSymlinkRejected: true,
        indexSymlinkRejectedAfterStartup: true,
        unsafeIndexRejectedAtStartup: true,
      },
    };
    if (process.env.TESLATLAS_VIEWER_CLI_RECEIPT !== undefined) {
      await writeFile(
        process.env.TESLATLAS_VIEWER_CLI_RECEIPT,
        `${JSON.stringify(receipt, null, 2)}\n`,
        { mode: 0o600, flag: 'wx' },
      );
    }
  } finally {
    if (server !== undefined) {
      server.kill('SIGKILL');
      await waitForExit(server).catch(() => undefined);
    }
    await rm(temporary, { recursive: true, force: true });
  }
});
