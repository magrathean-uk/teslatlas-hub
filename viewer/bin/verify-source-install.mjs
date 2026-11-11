#!/usr/bin/env node

import { access } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { resolve } from 'node:path';

const root = resolve(import.meta.dirname, '..');
const verifier = resolve(root, 'scripts/verify-sdk-artifact.mjs');
const metadata = resolve(root, 'artifacts/teslatlas-sdk.json');

async function exists(path) {
  try {
    await access(path);
    return true;
  } catch {
    return false;
  }
}

const sourceFiles = await Promise.all([exists(verifier), exists(metadata)]);
if (sourceFiles.some(Boolean) && !sourceFiles.every(Boolean)) {
  throw new Error('source SDK verification inputs are incomplete');
}
if (sourceFiles.every(Boolean)) {
  const result = await new Promise((resolveRun, reject) => {
    const child = spawn(process.execPath, [verifier], { stdio: 'inherit' });
    child.once('error', reject);
    child.once('exit', (code, signal) => resolveRun({ code, signal }));
  });
  if (result.code !== 0) {
    throw new Error(`source SDK verification failed (${result.code ?? result.signal})`);
  }
}
