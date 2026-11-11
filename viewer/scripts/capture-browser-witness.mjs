import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { chromium } from '@playwright/test';

function required(name) {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is required`);
  return value;
}

function certificateDigest(bytes) {
  const text = bytes.toString('ascii');
  const match = text.match(
    /-----BEGIN CERTIFICATE-----([A-Za-z0-9+/=\s]+)-----END CERTIFICATE-----/u,
  );
  return createHash('sha256')
    .update(
      match === null
        ? bytes
        : Buffer.from(match[1].replaceAll(/\s/gu, ''), 'base64'),
    )
    .digest('hex');
}

async function browserArguments(url) {
  const browser = await chromium.connectOverCDP(url);
  const session = await browser.newBrowserCDPSession();
  return (await session.send('Browser.getBrowserCommandLine')).arguments;
}

const [trustedArguments, untrustedArguments] = await Promise.all([
  browserArguments(required('TESLATLAS_BROWSER_CDP_URL')),
  browserArguments(required('TESLATLAS_BROWSER_UNTRUSTED_CDP_URL')),
]);
const certificateSha256 = certificateDigest(
  await readFile(required('TESLATLAS_HUB_CERTIFICATE')),
);
const witness = {
  schemaVersion: 1,
  certificateSha256,
  trusted: {
    arguments: trustedArguments,
    nssCaExportPath: required('TESLATLAS_BROWSER_NSS_CA_EXPORT'),
    nssDatabase: required('TESLATLAS_BROWSER_TRUSTED_NSS_DATABASE'),
  },
  untrusted: {
    arguments: untrustedArguments,
    nssDatabase: required('TESLATLAS_BROWSER_UNTRUSTED_NSS_DATABASE'),
  },
};
await writeFile(
  required('TESLATLAS_BROWSER_LAUNCH_WITNESS'),
  `${JSON.stringify(witness, null, 2)}\n`,
  { mode: 0o600, flag: 'wx' },
);
process.stdout.write(`Captured browser witness for ${certificateSha256}\n`);
process.exit(0);
