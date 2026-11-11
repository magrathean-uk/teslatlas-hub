#!/usr/bin/env node

import { createReadStream } from 'node:fs';
import { lstat, readFile, realpath, stat } from 'node:fs/promises';
import http from 'node:http';
import { dirname, extname, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const packageMetadata = JSON.parse(
  await readFile(resolve(packageRoot, 'package.json'), 'utf8'),
);

const help = `Teslatlas Viewer ${packageMetadata.version}

Serve the packaged Viewer as a local static web app.

Usage: teslatlas-viewer [options]

Options:
  --host <address>  Bind address (default: 127.0.0.1)
  --port <number>   TCP port, or 0 for an assigned port (default: 4173)
  -h, --help        Show this help
  -V, --version     Show the package version
`;

function argumentValue(arguments_, index, name) {
  const value = arguments_[index + 1];
  if (value === undefined || value.startsWith('-')) {
    throw new Error(`${name} requires a value`);
  }
  return value;
}

function parseArguments(arguments_) {
  const options = { host: '127.0.0.1', port: 4173 };
  for (let index = 0; index < arguments_.length; index += 1) {
    const argument = arguments_[index];
    if (argument === '-h' || argument === '--help') return { action: 'help' };
    if (argument === '-V' || argument === '--version') return { action: 'version' };
    if (argument === '--host') {
      options.host = argumentValue(arguments_, index, '--host');
      index += 1;
      continue;
    }
    if (argument.startsWith('--host=')) {
      options.host = argument.slice('--host='.length);
      continue;
    }
    if (argument === '--port') {
      options.port = Number(argumentValue(arguments_, index, '--port'));
      index += 1;
      continue;
    }
    if (argument.startsWith('--port=')) {
      options.port = Number(argument.slice('--port='.length));
      continue;
    }
    throw new Error(`unknown option: ${argument}`);
  }
  if (options.host.length === 0 || /\s/u.test(options.host)) {
    throw new Error('--host must be a non-empty address without whitespace');
  }
  if (!Number.isSafeInteger(options.port) || options.port < 0 || options.port > 65_535) {
    throw new Error('--port must be an integer from 0 through 65535');
  }
  return { action: 'serve', ...options };
}

function isInside(root, path) {
  return path === root || path.startsWith(`${root}${sep}`);
}

function decodeRequestPath(target) {
  if (!target.startsWith('/')) throw Object.assign(new Error('invalid request target'), { status: 400 });
  let value = target.split('?', 1)[0];
  for (let pass = 0; pass < 4; pass += 1) {
    let decoded;
    try {
      decoded = decodeURIComponent(value);
    } catch {
      throw Object.assign(new Error('malformed URL encoding'), { status: 400 });
    }
    if (
      decoded.includes('\0') ||
      decoded.includes('\\') ||
      decoded.split('/').some((segment) => segment === '..')
    ) {
      throw Object.assign(new Error('path traversal is forbidden'), { status: 403 });
    }
    if (decoded === value) return decoded;
    value = decoded;
  }
  if (/%(?:2e|2f|5c)/iu.test(value)) {
    throw Object.assign(new Error('encoded path traversal is forbidden'), { status: 403 });
  }
  return value;
}

async function assertNearestExistingPath(root, candidate) {
  let cursor = candidate;
  while (true) {
    try {
      const existing = await realpath(cursor);
      if (!isInside(root, existing)) {
        throw Object.assign(new Error('symlink escape is forbidden'), { status: 403 });
      }
      return;
    } catch (error) {
      if (error?.status === 403) throw error;
      if (error?.code !== 'ENOENT') throw error;
      const parent = dirname(cursor);
      if (parent === cursor) throw error;
      cursor = parent;
    }
  }
}

async function resolveContainedFile(root, candidate) {
  await assertNearestExistingPath(root, candidate);
  try {
    const candidateMetadata = await lstat(candidate);
    if (candidateMetadata.isFile() || candidateMetadata.isSymbolicLink()) {
      const actual = await realpath(candidate);
      if (!isInside(root, actual) || !(await stat(actual)).isFile()) {
        throw Object.assign(new Error('symlink escape is forbidden'), { status: 403 });
      }
      return actual;
    }
  } catch (error) {
    if (error?.status === 403) throw error;
    if (error?.code !== 'ENOENT') throw error;
  }
  return null;
}

const contentTypes = new Map([
  ['.css', 'text/css; charset=utf-8'],
  ['.html', 'text/html; charset=utf-8'],
  ['.ico', 'image/x-icon'],
  ['.jpeg', 'image/jpeg'],
  ['.jpg', 'image/jpeg'],
  ['.js', 'text/javascript; charset=utf-8'],
  ['.json', 'application/json; charset=utf-8'],
  ['.png', 'image/png'],
  ['.svg', 'image/svg+xml'],
  ['.webp', 'image/webp'],
  ['.woff2', 'font/woff2'],
]);

function sendText(response, method, status, message, headers = {}) {
  const body = Buffer.from(`${message}\n`, 'utf8');
  response.writeHead(status, {
    'Content-Type': 'text/plain; charset=utf-8',
    'Content-Length': body.length,
    'X-Content-Type-Options': 'nosniff',
    ...headers,
  });
  response.end(method === 'HEAD' ? undefined : body);
}

async function prepareAssetRoot() {
  const configured = resolve(packageRoot, 'dist');
  let root;
  try {
    const rootMetadata = await lstat(configured);
    if (!rootMetadata.isDirectory() || rootMetadata.isSymbolicLink()) {
      throw new Error('asset root is not a directory');
    }
    root = await realpath(configured);
    if (await resolveContainedFile(root, resolve(root, 'index.html')) === null) {
      throw new Error('index is not a file');
    }
  } catch {
    throw new Error(`built Viewer assets are missing from ${configured}`);
  }
  return root;
}

async function resolveAsset(root, requestPath) {
  const relative = requestPath.replace(/^\/+/, '');
  const candidate = resolve(root, relative);
  if (!isInside(root, candidate)) {
    throw Object.assign(new Error('path traversal is forbidden'), { status: 403 });
  }
  const asset = await resolveContainedFile(root, candidate);
  if (asset !== null) return asset;
  if (
    requestPath === '/assets' ||
    requestPath.startsWith('/assets/') ||
    extname(requestPath) !== ''
  ) return null;
  return resolveContainedFile(root, resolve(root, 'index.html'));
}

async function serveRequest(root, request, response) {
  const method = request.method ?? 'GET';
  if (method !== 'GET' && method !== 'HEAD') {
    sendText(response, method, 405, 'Method not allowed', { Allow: 'GET, HEAD' });
    return;
  }
  try {
    const requestPath = decodeRequestPath(request.url ?? '/');
    const asset = await resolveAsset(root, requestPath);
    if (asset === null) {
      sendText(response, method, 404, 'Not found');
      return;
    }
    const metadata = await stat(asset);
    response.writeHead(200, {
      'Content-Type': contentTypes.get(extname(asset).toLowerCase()) ?? 'application/octet-stream',
      'Content-Length': metadata.size,
      'Cache-Control': requestPath.startsWith('/assets/')
        ? 'public, max-age=31536000, immutable'
        : 'no-cache',
      'X-Content-Type-Options': 'nosniff',
    });
    if (method === 'HEAD') response.end();
    else createReadStream(asset).pipe(response);
  } catch (error) {
    const status = Number.isInteger(error?.status) ? error.status : 500;
    sendText(response, method, status, status === 500 ? 'Internal server error' : error.message);
  }
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  if (options.action === 'help') {
    process.stdout.write(help);
    return;
  }
  if (options.action === 'version') {
    process.stdout.write(`${packageMetadata.version}\n`);
    return;
  }
  const root = await prepareAssetRoot();
  const server = http.createServer((request, response) => {
    void serveRequest(root, request, response);
  });
  await new Promise((resolveListen, reject) => {
    server.once('error', reject);
    server.listen({ host: options.host, port: options.port }, resolveListen);
  });
  const address = server.address();
  if (address === null || typeof address === 'string') throw new Error('listener address unavailable');
  const host = address.family === 'IPv6' ? `[${address.address}]` : address.address;
  process.stdout.write(`Teslatlas Viewer listening on http://${host}:${address.port}/\n`);
  let closing = false;
  const close = () => {
    if (closing) return;
    closing = true;
    server.close((error) => {
      if (error) {
        process.stderr.write(`Teslatlas Viewer shutdown failed: ${error.message}\n`);
        process.exitCode = 1;
      }
    });
    server.closeIdleConnections();
  };
  process.once('SIGINT', close);
  process.once('SIGTERM', close);
}

main().catch((error) => {
  const message = error?.code === 'EADDRINUSE'
    ? `address is already in use: ${error.address}:${error.port}`
    : error instanceof Error
      ? error.message
      : String(error);
  process.stderr.write(`teslatlas-viewer: ${message}\n`);
  process.exitCode = 1;
});
