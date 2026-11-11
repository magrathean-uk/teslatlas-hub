import { createHash } from 'node:crypto';
import { readdir, readFile, stat } from 'node:fs/promises';
import { join, relative } from 'node:path';

export function assertSafeBrowserArguments(arguments_: string[]): void {
  const forbidden = [
    '--ignore-certificate-errors',
    '--ignore-certificate-errors-spki-list',
    '--allow-insecure-localhost',
    '--test-type',
  ];
  if (
    arguments_.some((value) =>
      forbidden.some((flag) => value === flag || value.startsWith(`${flag}=`)),
    )
  ) {
    throw new Error('browser certificate bypass flag is forbidden');
  }
}

function digest(bytes: Uint8Array): string {
  return createHash('sha256').update(bytes).digest('hex');
}

function certificateDigest(bytes: Buffer): string {
  const text = bytes.toString('ascii');
  const match = text.match(
    /-----BEGIN CERTIFICATE-----([A-Za-z0-9+/=\s]+)-----END CERTIFICATE-----/u,
  );
  return digest(
    match === null
      ? bytes
      : Buffer.from(match[1].replaceAll(/\s/gu, ''), 'base64'),
  );
}

export async function verifyBrowserTrust(options: {
  witnessPath: string;
  certificatePath: string;
  trustedArguments: string[];
  untrustedArguments: string[];
}) {
  const witnessMetadata = await stat(options.witnessPath);
  if (!witnessMetadata.isFile() || (witnessMetadata.mode & 0o077) !== 0) {
    throw new Error('browser trust witness must be owner-only');
  }
  assertSafeBrowserArguments(options.trustedArguments);
  assertSafeBrowserArguments(options.untrustedArguments);
  const witness = JSON.parse(await readFile(options.witnessPath, 'utf8'));
  if (
    witness.schemaVersion !== 1 ||
    JSON.stringify(witness.trusted?.arguments) !==
      JSON.stringify(options.trustedArguments) ||
    JSON.stringify(witness.untrusted?.arguments) !==
      JSON.stringify(options.untrustedArguments)
  ) {
    throw new Error('browser launch witness does not match live CDP arguments');
  }
  const certificateSha256 = certificateDigest(
    await readFile(options.certificatePath),
  );
  const exportedCaSha256 = certificateDigest(
    await readFile(witness.trusted.nssCaExportPath),
  );
  if (
    witness.certificateSha256 !== certificateSha256 ||
    exportedCaSha256 !== certificateSha256
  ) {
    throw new Error('browser NSS trust witness does not match fixture CA');
  }
  return {
    certificateSha256,
    trustedNssDatabase: witness.trusted.nssDatabase as string,
  };
}

export async function hashBundle(root: string) {
  const files: Array<{ path: string; sha256: string }> = [];
  async function visit(directory: string): Promise<void> {
    for (const entry of (await readdir(directory, { withFileTypes: true })).sort(
      (left, right) => left.name.localeCompare(right.name),
    )) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) await visit(path);
      else if (entry.isFile()) {
        files.push({ path: relative(root, path), sha256: digest(await readFile(path)) });
      } else {
        throw new Error('Viewer bundle contains a non-regular entry');
      }
    }
  }
  await visit(root);
  files.sort((left, right) => left.path.localeCompare(right.path));
  return {
    fileCount: files.length,
    manifestSha256: digest(Buffer.from(`${JSON.stringify(files)}\n`, 'utf8')),
    files,
  };
}
