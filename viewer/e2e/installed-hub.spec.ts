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
  const scenarioVehicles = scenario.vehicles;
  if (!Array.isArray(scenarioVehicles)) {
    throw new Error('scenario must provide named vehicles');
  }
  const pagingVehicle = scenarioVehicles.find(
    (vehicle) =>
      vehicle &&
      typeof vehicle === 'object' &&
      (vehicle.vehicle_id ?? vehicle.vehicleId) === vehicleIds[0],
  );
  const pagingVehicleName =
    pagingVehicle && typeof pagingVehicle === 'object'
      ? pagingVehicle.display_name ?? pagingVehicle.displayName
      : undefined;
  if (typeof pagingVehicleName !== 'string' || pagingVehicleName.length === 0) {
    throw new Error('scenario must name the paging vehicle');
  }
  const emptyVehicle =
    typeof vehicleIds[1] === 'string'
      ? scenarioVehicles.find(
          (vehicle) =>
            vehicle &&
            typeof vehicle === 'object' &&
            (vehicle.vehicle_id ?? vehicle.vehicleId) === vehicleIds[1],
        )
      : undefined;
  const emptyVehicleName =
    emptyVehicle && typeof emptyVehicle === 'object'
      ? emptyVehicle.display_name ?? emptyVehicle.displayName
      : undefined;
  const drivePages = scenario.drive_pages_at_limit_25 ?? scenario.drive_pages_at_limit_2;
  if (
    !Array.isArray(drivePages) ||
    drivePages.length === 0 ||
    drivePages.some(
      (page) =>
        !Array.isArray(page) ||
        page.some((id) => !Number.isSafeInteger(id) || id < 1),
    )
  ) {
    throw new Error('scenario must provide ordered drive pages');
  }
  const expectedDriveIds = drivePages.flat().map(String);
  if (new Set(expectedDriveIds).size !== expectedDriveIds.length) {
    throw new Error('scenario drive pages must not repeat drive IDs');
  }
  const expectedDriveRequestCount = Math.ceil(expectedDriveIds.length / 25);
  const pagingVehicleId =
    process.env.TESLATLAS_VIEWER_PAGING_VEHICLE_ID ?? vehicleIds[0];
  const emptyVehicleId =
    process.env.TESLATLAS_VIEWER_EMPTY_VEHICLE_ID ??
    (typeof vehicleIds[1] === 'string' ? vehicleIds[1] : undefined);

  const driveRequests: string[] = [];
  const driveResponses: Array<{ status: number; ids: string[] }> = [];
  const driveResponseReads: Promise<void>[] = [];
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
      const record = { status: response.status(), ids: [] as string[] };
      driveResponses.push(record);
      if (record.status === 200) {
        driveResponseReads.push(
          (async () => {
            const payload = (await response.json()) as Record<string, unknown>;
            if (!Array.isArray(payload.items)) {
              throw new Error('drive response must provide items');
            }
            record.ids = payload.items.map((item) => {
              if (!item || typeof item !== 'object' || !('id' in item)) {
                throw new Error('drive response item must provide an ID');
              }
              return String(item.id);
            });
          })(),
        );
      }
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

  const navigation = page.getByRole('navigation', { name: 'Viewer sections' });
  await navigation.getByRole('button', { name: 'Current state' }).click();
  const currentState = page.getByRole('region', { name: 'Current state' });
  const firstVehicleState = currentState.locator('.vehicle-state').first();
  await expect(firstVehicleState).toBeVisible();
  await expect(firstVehicleState.getByText('0%', { exact: true })).toBeVisible();
  await expect(firstVehicleState.getByText('160.93 km', { exact: true })).toBeVisible();
  await expect(firstVehicleState.getByText('16,093.44 km', { exact: true })).toBeVisible();
  await expect(firstVehicleState.getByText('Observed', { exact: true })).toBeVisible();

  await navigation.getByRole('button', { name: 'Recent sessions' }).click();
  const group = page.locator('.session-group').filter({
    has: page.getByRole('heading', { name: pagingVehicleName, exact: true }),
  });
  await expect(group).toBeVisible();
  await expect(group.locator('.count-chip')).toHaveText(
    String(Math.min(25, expectedDriveIds.length)),
  );

  let loadMoreCount = 0;
  while (await group.getByRole('button', { name: /Load more drives/ }).count() > 0) {
    loadMoreCount += 1;
    await group.getByRole('button', { name: /Load more drives/ }).click();
    await expect(group.locator('.count-chip')).toHaveText(
      String(Math.min(25 * (loadMoreCount + 1), expectedDriveIds.length)),
    );
    if (loadMoreCount >= expectedDriveRequestCount) {
      throw new Error('installed Viewer did not reach terminal history');
    }
  }
  expect(loadMoreCount).toBe(Math.max(0, expectedDriveRequestCount - 1));
  await expect(group).toContainText('End of available history');
  await Promise.all(driveResponseReads);
  expect(driveRequests.filter((query) => query.includes('limit=25'))).toHaveLength(expectedDriveRequestCount);
  expect(driveResponses.filter(({ status }) => status === 200 || status === 304).length).toBeGreaterThanOrEqual(expectedDriveRequestCount);
  expect(driveResponses.filter(({ status }) => status === 200).flatMap(({ ids }) => ids)).toEqual(expectedDriveIds);

  if (emptyVehicleId !== undefined && typeof emptyVehicleName === 'string') {
    const emptyGroup = page.locator('.session-group').filter({
      has: page.getByRole('heading', { name: emptyVehicleName, exact: true }),
    });
    if (await emptyGroup.count() > 0) {
      await expect(emptyGroup).toContainText(/No recent drives|End of available history/);
    }
  }

  await page.getByRole('button', { name: 'Refresh Hub data' }).click();
  await expect(page.getByRole('button', { name: 'Refresh Hub data' })).toBeEnabled();
  await page.getByRole('button', { name: 'Clear local session' }).click();
  await expect(page.getByRole('heading', { name: 'Pair with a Hub' })).toBeVisible();
});
