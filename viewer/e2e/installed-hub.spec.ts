import { readFile } from 'node:fs/promises';

import { expect, test } from '@playwright/test';

function required(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is required for installed Hub smoke`);
  return value;
}

function textField(record: Record<string, unknown>, snake: string, camel: string): string {
  const value = record[snake] ?? record[camel];
  if (typeof value !== 'string' || value.length === 0) {
    throw new Error(`descriptor field ${snake} is missing`);
  }
  return value;
}

test('installed Viewer pairs, reads and pages a disposable current Hub', async ({ page }) => {
  test.setTimeout(120_000);
  if (process.env.TESLATLAS_VIEWER_EXTERNAL_SERVER !== '1') {
    throw new Error('installed Hub smoke requires TESLATLAS_VIEWER_EXTERNAL_SERVER=1');
  }

  const descriptorPath = required('TESLATLAS_HUB_HTTP_CONFIG');
  const descriptor = JSON.parse(await readFile(descriptorPath, 'utf8')) as Record<string, unknown>;
  const endpoint = required('TESLATLAS_VIEWER_HUB_ENDPOINT');
  const hubId = textField(descriptor, 'hub_id', 'hubId');
  const invitationPath = textField(descriptor, 'invitation_path', 'invitationPath');
  const invitation = await readFile(invitationPath, 'utf8');
  const scenarioPath = textField(descriptor, 'scenario_path', 'scenarioPath');
  const scenario = JSON.parse(await readFile(scenarioPath, 'utf8')) as Record<string, unknown>;
  const vehicleIds = scenario.vehicle_ids ?? scenario.vehicleIds;
  if (!Array.isArray(vehicleIds) || typeof vehicleIds[0] !== 'string') {
    throw new Error('scenario must provide vehicle_ids');
  }
  const pagingVehicleId =
    process.env.TESLATLAS_VIEWER_PAGING_VEHICLE_ID ?? vehicleIds[0];
  const emptyVehicleId =
    process.env.TESLATLAS_VIEWER_EMPTY_VEHICLE_ID ??
    (typeof vehicleIds[1] === 'string' ? vehicleIds[1] : undefined);

  const driveRequests: string[] = [];
  const driveResponses: number[] = [];
  page.on('request', (request) => {
    const url = new URL(request.url());
    if (
      url.origin === new URL(endpoint).origin &&
      url.pathname === `/v1/vehicles/${pagingVehicleId}/drives`
    ) {
      driveRequests.push(url.search);
    }
  });
  page.on('response', (response) => {
    const url = new URL(response.url());
    if (
      url.origin === new URL(endpoint).origin &&
      url.pathname === `/v1/vehicles/${pagingVehicleId}/drives`
    ) {
      driveResponses.push(response.status());
    }
  });

  await page.goto('/');
  await page.getByRole('link', { name: 'Connect to my Hub' }).click();
  await expect(page.getByRole('heading', { name: 'Pair with a Hub' })).toBeVisible();

  await page.getByLabel('Hub endpoint').fill(endpoint);
  await page.getByLabel('Expected Hub UUID').fill('ffffffff-ffff-4fff-8fff-ffffffffffff');
  await page.getByRole('button', { name: 'Inspect live Hub' }).click();
  await expect(page.getByRole('alert')).toContainText(/identity|match/i);

  await page.getByRole('button', { name: 'Edit connection' }).first().click();
  await page.getByLabel('Hub endpoint').fill(endpoint);
  await page.getByLabel('Expected Hub UUID').fill(hubId);
  const tlsInput = page.getByLabel('Invitation TLS identity');
  if (await tlsInput.count() > 0 && process.env.TESLATLAS_VIEWER_HUB_TLS_IDENTITY) {
    await tlsInput.fill(process.env.TESLATLAS_VIEWER_HUB_TLS_IDENTITY);
  }
  await page.getByRole('button', { name: 'Inspect live Hub' }).click();
  await expect(page.getByRole('heading', { name: `Hub ${hubId}` })).toBeVisible();
  await page.getByLabel('Pairing invitation JSON').fill(invitation);
  await page.getByRole('button', { name: 'Pair live Hub' }).click();
  await expect(page.getByRole('region', { name: 'Hub health' })).toBeVisible();
  await expect(page.getByText(hubId, { exact: true })).toBeVisible();

  await page.getByRole('button', { name: 'Recent sessions' }).click();
  const pagingGroup = page.locator('.session-group').filter({ hasText: pagingVehicleId });
  const fallbackGroup = page.locator('.session-group').filter({ has: page.getByRole('button', { name: /Load more drives/ }) }).first();
  const group = (await pagingGroup.count()) > 0 ? pagingGroup : fallbackGroup;
  await expect(group).toBeVisible();
  await expect(group.locator('.count-chip')).toHaveText('25');

  let loadMoreCount = 0;
  while (await group.getByRole('button', { name: /Load more drives/ }).count() > 0) {
    loadMoreCount += 1;
    await group.getByRole('button', { name: /Load more drives/ }).click();
    await expect(group.locator('.count-chip')).toHaveText(String(Math.min(25 * (loadMoreCount + 1), 51)));
    if (loadMoreCount > 2) throw new Error('installed Viewer did not reach terminal history');
  }
  expect(loadMoreCount).toBe(2);
  await expect(group).toContainText('End of available history');
  expect(driveRequests.filter((query) => query.includes('limit=25'))).toHaveLength(3);
  expect(driveResponses.filter((status) => status === 200 || status === 304).length).toBeGreaterThanOrEqual(3);

  if (emptyVehicleId !== undefined) {
    const emptyGroup = page.locator('.session-group').filter({ hasText: emptyVehicleId });
    if (await emptyGroup.count() > 0) {
      await expect(emptyGroup).toContainText(/No recent drives|End of available history/);
    }
  }

  await page.getByRole('button', { name: 'Refresh Hub data' }).click();
  await expect(page.getByRole('button', { name: 'Refresh Hub data' })).toBeEnabled();
  await page.getByRole('button', { name: 'Clear local session' }).click();
  await expect(page.getByRole('heading', { name: 'Pair with a Hub' })).toBeVisible();
});
