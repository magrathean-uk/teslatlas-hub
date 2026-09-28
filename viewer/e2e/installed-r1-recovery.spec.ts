import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

import { expect, test, type Page } from '@playwright/test';

function required(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is required for installed Viewer recovery`);
  return value;
}

function textField(record: Record<string, unknown>, snake: string, camel: string): string {
  const value = record[snake] ?? record[camel];
  if (typeof value !== 'string' || value.length === 0) {
    throw new Error(`descriptor field ${snake} is missing`);
  }
  return value;
}

async function writeMarker(
  directory: string,
  filename: string,
  type: string,
  fields: Record<string, unknown> = {},
): Promise<void> {
  const marker = {
    schema_version: 1,
    type,
    ...fields,
    created_at: new Date().toISOString(),
  };
  await writeFile(join(directory, filename), `${JSON.stringify(marker)}\n`, {
    encoding: 'utf8',
    flag: 'wx',
    mode: 0o600,
  });
}

async function waitForMarker(
  directory: string,
  filename: string,
  type: string,
): Promise<Record<string, unknown>> {
  const path = join(directory, filename);
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    try {
      const value = JSON.parse(await readFile(path, 'utf8')) as Record<string, unknown>;
      if (value.schema_version !== 1 || value.type !== type) {
        throw new Error(`${filename} has an invalid marker identity`);
      }
      return value;
    } catch (error) {
      if (
        !(
          typeof error === 'object' &&
          error !== null &&
          'code' in error &&
          error.code === 'ENOENT'
        )
      ) {
        throw error;
      }
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`timed out waiting for ${filename}`);
}

async function fillConnection(
  page: Page,
  endpoint: string,
  hubId: string,
  tlsIdentity: string,
): Promise<void> {
  await page.getByLabel('Hub endpoint').fill(endpoint);
  await page.getByLabel('Expected Hub UUID').fill(hubId);
  const tlsInput = page.getByLabel('Invitation TLS identity');
  if (await tlsInput.count() > 0) {
    await tlsInput.fill(tlsIdentity);
  }
  await page.getByRole('button', { name: 'Inspect live Hub' }).click();
}

async function expectKnownCurrent(page: Page): Promise<void> {
  const current = page.getByRole('region', { name: 'Current state' });
  const firstVehicleState = current.locator('.vehicle-state').first();
  await expect(firstVehicleState).toBeVisible();
  await expect(firstVehicleState.getByText('0%', { exact: true })).toBeVisible();
  await expect(firstVehicleState.getByText('160.93 km', { exact: true })).toBeVisible();
  await expect(firstVehicleState.getByText('16,093.44 km', { exact: true })).toBeVisible();
  await expect(firstVehicleState.getByText('Observed', { exact: true })).toBeVisible();
}

test('installed Viewer survives Hub restart, revocation and fresh re-pair', async ({ page }) => {
  test.setTimeout(600_000);
  if (process.env.TESLATLAS_VIEWER_EXTERNAL_SERVER !== '1') {
    throw new Error('installed recovery requires TESLATLAS_VIEWER_EXTERNAL_SERVER=1');
  }

  const descriptor = JSON.parse(
    await readFile(required('TESLATLAS_HUB_HTTP_CONFIG'), 'utf8'),
  ) as Record<string, unknown>;
  const endpoint = required('TESLATLAS_VIEWER_HUB_ENDPOINT');
  const hubId = textField(descriptor, 'hub_id', 'hubId');
  const invitationPath = textField(descriptor, 'invitation_path', 'invitationPath');
  const invitationText = await readFile(invitationPath, 'utf8');
  const invitation = JSON.parse(invitationText) as Record<string, unknown>;
  const initialTlsIdentity = textField(invitation, 'tls_pin', 'tlsPin');
  const controlDirectory = required('TESLATLAS_VIEWER_RECOVERY_CONTROL_DIR');
  const responses: Array<{ path: string; status: number }> = [];
  page.on('response', (response) => {
    if (!response.url().startsWith(endpoint)) return;
    const url = new URL(response.url());
    responses.push({ path: `${url.pathname}${url.search}`, status: response.status() });
  });

  await page.goto('/?mode=live&paired=false');
  await fillConnection(
    page,
    endpoint,
    'ffffffff-ffff-4fff-8fff-ffffffffffff',
    initialTlsIdentity,
  );
  await expect(page.getByRole('alert')).toContainText(/identity|match/i);

  await page.getByRole('button', { name: 'Edit connection' }).first().click();
  await fillConnection(page, endpoint, hubId, initialTlsIdentity);
  await expect(page.getByRole('heading', { name: `Hub ${hubId}` })).toBeVisible();
  await page.getByLabel('Pairing invitation JSON').fill(invitationText);
  const initialPairResponse = page.waitForResponse(
    (response) =>
      response.url().startsWith(endpoint) &&
      response.url().includes('/pairings/') &&
      response.request().method() === 'POST',
  );
  await page.getByRole('button', { name: 'Pair live Hub' }).click();
  expect((await initialPairResponse).status()).toBe(200);
  await expect(page.getByRole('region', { name: 'Hub health' })).toBeVisible();
  await expect(page.getByText(hubId, { exact: true })).toBeVisible();

  const navigation = page.getByRole('navigation', { name: 'Viewer sections' });
  await navigation.getByRole('button', { name: 'Current state' }).click();
  await expectKnownCurrent(page);

  await writeMarker(controlDirectory, 'ready-for-outage.json', 'ready-for-outage');
  const outage = await waitForMarker(
    controlDirectory,
    'outage-complete.json',
    'outage-complete',
  );
  expect(outage.status).toBe('stopped');
  expect(outage.listener_closed).toBe(true);

  await page.getByRole('button', { name: 'Refresh Hub data' }).click();
  await expect(page.getByRole('button', { name: 'Refresh Hub data' })).toBeEnabled();
  await expect(page.getByText('Showing retained current state')).toBeVisible();
  const staleVehicleState = page.getByRole('region', { name: 'Current state' }).locator('.vehicle-state').first();
  await expect(staleVehicleState.getByText('Stale', { exact: true })).toBeVisible();
  await expect(staleVehicleState.getByText('0%', { exact: true })).toBeVisible();
  await navigation.getByRole('button', { name: 'Hub health' }).click();
  await expect(page.getByText('Cannot reach this Hub.')).toBeVisible();
  await expect(page.getByText('Offline', { exact: true })).toBeVisible();

  await writeMarker(controlDirectory, 'ready-for-restore.json', 'ready-for-restore');
  const restore = await waitForMarker(
    controlDirectory,
    'restore-complete.json',
    'restore-complete',
  );
  expect(restore.status).toBe('ready');
  expect(restore.listener_open).toBe(true);

  await page.getByRole('button', { name: 'Refresh Hub data' }).click();
  await expect(page.getByRole('button', { name: 'Refresh Hub data' })).toBeEnabled();
  await expect(page.getByText('Cannot reach this Hub.')).toHaveCount(0);
  await expect(page.getByText('Healthy', { exact: true })).toBeVisible();
  await navigation.getByRole('button', { name: 'Current state' }).click();
  await expect(page.getByText('Showing retained current state')).toHaveCount(0);
  await expectKnownCurrent(page);

  await writeMarker(controlDirectory, 'ready-for-revoke.json', 'ready-for-revoke');
  const revoke = await waitForMarker(
    controlDirectory,
    'revoke-complete.json',
    'revoke-complete',
  );
  expect(revoke.status).toBe('restarted');
  expect(revoke.listener_open).toBe(true);

  const authResponseStart = responses.length;
  await page.getByRole('button', { name: 'Refresh Hub data' }).click();
  await expect(page.getByRole('heading', { name: 'Pair with a Hub' })).toBeVisible();
  await expect(page.getByText('Access ended; pair again to continue.')).toBeVisible();
  expect(
    responses
      .slice(authResponseStart)
      .some(({ path, status }) => path.startsWith('/v1/') && status === 401),
  ).toBe(true);

  await writeMarker(controlDirectory, 'ready-for-repair.json', 'ready-for-repair');
  const freshInvitation = await waitForMarker(
    controlDirectory,
    'repair-ready.json',
    'repair-ready',
  );
  expect(freshInvitation.invitation_sha256).toMatch(/^[a-f0-9]{64}$/u);
  const freshInvitationText = await readFile(invitationPath, 'utf8');
  const freshInvitationRecord = JSON.parse(freshInvitationText) as Record<string, unknown>;
  const freshTlsIdentity = textField(freshInvitationRecord, 'tls_pin', 'tlsPin');
  await fillConnection(page, endpoint, hubId, freshTlsIdentity);
  await expect(page.getByRole('heading', { name: `Hub ${hubId}` })).toBeVisible();
  await page.getByLabel('Pairing invitation JSON').fill(freshInvitationText);
  const repairPairResponse = page.waitForResponse(
    (response) =>
      response.url().startsWith(endpoint) &&
      response.url().includes('/pairings/') &&
      response.request().method() === 'POST',
  );
  await page.getByRole('button', { name: 'Pair live Hub' }).click();
  expect((await repairPairResponse).status()).toBe(200);
  await expect(page.getByRole('region', { name: 'Current state' })).toBeVisible();
  await navigation.getByRole('button', { name: 'Hub health' }).click();
  await expect(page.getByRole('region', { name: 'Hub health' })).toBeVisible();
  await expect(page.getByText(hubId, { exact: true })).toBeVisible();
  await navigation.getByRole('button', { name: 'Current state' }).click();
  await expectKnownCurrent(page);

  await writeMarker(controlDirectory, 'browser-complete.json', 'browser-complete');
  await page.getByRole('button', { name: 'Clear local session' }).click();
  await expect(page.getByRole('heading', { name: 'Pair with a Hub' })).toBeVisible();
});
