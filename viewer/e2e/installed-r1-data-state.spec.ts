import { readFile } from 'node:fs/promises';

import { expect, test, type Page, type Route } from '@playwright/test';

function required(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is required for installed Viewer data-state tests`);
  return value;
}

function textField(record: Record<string, unknown>, snake: string, camel: string): string {
  const value = record[snake] ?? record[camel];
  if (typeof value !== 'string' || value.length === 0) {
    throw new Error(`descriptor field ${snake} is missing`);
  }
  return value;
}

function recordField(record: Record<string, unknown>, snake: string, camel: string): unknown {
  return record[snake] ?? record[camel];
}

function routePath(route: Route): URL {
  return new URL(route.request().url());
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
  if (await tlsInput.count() > 0) await tlsInput.fill(tlsIdentity);
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

test('installed Viewer preserves cache, explicit unsupported state, partial failure, and cancellation', async ({ page }) => {
  test.setTimeout(180_000);
  if (process.env.TESLATLAS_VIEWER_EXTERNAL_SERVER !== '1') {
    throw new Error('installed data-state tests require TESLATLAS_VIEWER_EXTERNAL_SERVER=1');
  }

  const descriptor = JSON.parse(
    await readFile(required('TESLATLAS_HUB_HTTP_CONFIG'), 'utf8'),
  ) as Record<string, unknown>;
  const endpoint = required('TESLATLAS_VIEWER_HUB_ENDPOINT');
  const endpointOrigin = new URL(endpoint).origin;
  const hubId = textField(descriptor, 'hub_id', 'hubId');
  const invitationPath = textField(descriptor, 'invitation_path', 'invitationPath');
  const scenarioPath = textField(descriptor, 'scenario_path', 'scenarioPath');
  const invitation = await readFile(invitationPath, 'utf8');
  const invitationRecord = JSON.parse(invitation) as Record<string, unknown>;
  const tlsIdentity = textField(invitationRecord, 'tls_pin', 'tlsPin');
  const scenario = JSON.parse(await readFile(scenarioPath, 'utf8')) as Record<string, unknown>;
  const vehicleIds = recordField(scenario, 'vehicle_ids', 'vehicleIds');
  if (!Array.isArray(vehicleIds) || typeof vehicleIds[0] !== 'string') {
    throw new Error('scenario must provide vehicle_ids');
  }
  const scenarioVehicles = recordField(scenario, 'vehicles', 'vehicles');
  if (!Array.isArray(scenarioVehicles)) throw new Error('scenario must provide named vehicles');
  const vehicleRecord = (id: string): Record<string, unknown> | undefined => {
    const value = scenarioVehicles.find(
      (candidate) =>
        candidate &&
        typeof candidate === 'object' &&
        recordField(candidate as Record<string, unknown>, 'vehicle_id', 'vehicleId') === id,
    );
    return value && typeof value === 'object' ? value as Record<string, unknown> : undefined;
  };
  const pagingVehicleId = vehicleIds[0];
  const pagingVehicle = vehicleRecord(pagingVehicleId);
  const pagingVehicleName =
    pagingVehicle && recordField(pagingVehicle, 'display_name', 'displayName');
  if (typeof pagingVehicleName !== 'string' || pagingVehicleName.length === 0) {
    throw new Error('scenario must name the paging vehicle');
  }
  const drivePages = recordField(scenario, 'drive_pages_at_limit_25', 'drivePagesAtLimit25') ??
    recordField(scenario, 'drive_pages_at_limit_2', 'drivePagesAtLimit2');
  if (
    !Array.isArray(drivePages) ||
    drivePages.length !== 3 ||
    drivePages.some(
      (values) =>
        !Array.isArray(values) ||
        values.some((value) => !Number.isSafeInteger(value) || value < 1),
    )
  ) {
    throw new Error('scenario must provide three ordered drive pages');
  }
  const expectedDriveIds = drivePages.flat().map(String);
  if (
    expectedDriveIds.length !== 51 ||
    drivePages[0].length !== 25 ||
    drivePages[1].length !== 25 ||
    drivePages[2].length !== 1 ||
    new Set(expectedDriveIds).size !== expectedDriveIds.length
  ) {
    throw new Error('scenario must provide 51 unique drives in 25/25/1 pages');
  }

  const requests: Array<{
    method: string;
    path: string;
    hasIfNoneMatch: boolean;
  }> = [];
  const responses: Array<{ method: string; path: string; status: number }> = [];
  const failedRequests: Array<{ method: string; path: string; errorText: string }> = [];
  page.on('request', (request) => {
    const url = new URL(request.url());
    if (url.origin !== endpointOrigin) return;
    const headers = request.headers();
    requests.push({
      method: request.method(),
      path: `${url.pathname}${url.search}`,
      hasIfNoneMatch: 'if-none-match' in headers,
    });
  });
  page.on('response', (response) => {
    const url = new URL(response.url());
    if (url.origin !== endpointOrigin) return;
    responses.push({
      method: response.request().method(),
      path: `${url.pathname}${url.search}`,
      status: response.status(),
    });
  });
  page.on('requestfailed', (request) => {
    const url = new URL(request.url());
    if (url.origin !== endpointOrigin) return;
    failedRequests.push({
      method: request.method(),
      path: `${url.pathname}${url.search}`,
      errorText: request.failure()?.errorText ?? 'unknown',
    });
  });

  await page.goto('/');
  await page.getByRole('link', { name: 'Connect to my Hub' }).click();
  await expect(page.getByRole('heading', { name: 'Pair with a Hub' })).toBeVisible();
  await page.getByLabel('Hub endpoint').fill(endpoint);
  await page.getByLabel('Expected Hub UUID').fill('ffffffff-ffff-4fff-8fff-ffffffffffff');
  await page.getByRole('button', { name: 'Inspect live Hub' }).click();
  await expect(page.getByRole('alert')).toContainText(/identity|match/i);
  await page.getByRole('button', { name: 'Edit connection' }).first().click();
  await fillConnection(page, endpoint, hubId, tlsIdentity);
  await expect(page.getByRole('heading', { name: `Hub ${hubId}` })).toBeVisible();
  await page.getByLabel('Pairing invitation JSON').fill(invitation);
  await page.getByRole('button', { name: 'Pair live Hub' }).click();
  await expect(page.getByRole('region', { name: 'Hub health' })).toBeVisible();
  await expect(page.getByText(hubId, { exact: true })).toBeVisible();

  const navigation = page.getByRole('navigation', { name: 'Viewer sections' });
  await navigation.getByRole('button', { name: 'Current state' }).click();
  await expectKnownCurrent(page);
  await navigation.getByRole('button', { name: 'Recent sessions' }).click();
  const group = page.locator('.session-group').filter({
    has: page.getByRole('heading', { name: pagingVehicleName, exact: true }),
  });
  await expect(group).toBeVisible();
  await expect(group.locator('.count-chip')).toHaveText('25');
  await group.getByRole('button', { name: /Load more drives/ }).click();
  await expect(group.locator('.count-chip')).toHaveText('50');
  await group.getByRole('button', { name: /Load more drives/ }).click();
  await expect(group.locator('.count-chip')).toHaveText('51');
  await expect(group).toContainText('End of available history');

  const drivePath = `/v1/vehicles/${pagingVehicleId}/drives`;
  const replayRequestStart = requests.length;
  const replayResponseStart = responses.length;
  await page.getByRole('button', { name: 'Refresh Hub data' }).click();
  await expect.poll(
    () => responses.slice(replayResponseStart).filter(({ path }) => path.startsWith(`${drivePath}?`)).length,
  ).toBe(3);
  const replayRequests = requests.slice(replayRequestStart).filter(
    ({ method, path }) => method === 'GET' && path.startsWith(`${drivePath}?`),
  );
  const replayResponses = responses.slice(replayResponseStart).filter(
    ({ method, path }) => method === 'GET' && path.startsWith(`${drivePath}?`),
  );
  expect(replayRequests).toHaveLength(3);
  expect(replayRequests.every(({ hasIfNoneMatch }) => hasIfNoneMatch)).toBe(true);
  expect(replayResponses.map(({ status }) => status)).toEqual([304, 304, 304]);
  await expect(page.getByRole('button', { name: 'Refresh Hub data' })).toBeEnabled();

  const unsupportedRequestStart = requests.length;
  await navigation.getByRole('button', { name: 'Data quality' }).click();
  await expect(page.getByText('Data quality is unsupported by hub-http-v1.')).toBeVisible();
  await navigation.getByRole('button', { name: 'Collector freshness' }).click();
  await expect(page.getByText('Collector cost and backup age are unsupported by hub-http-v1.')).toBeVisible();
  await navigation.getByRole('button', { name: 'Paired devices' }).click();
  await expect(page.getByText('Remote paired-device management is unsupported by hub-http-v1.')).toBeVisible();
  await navigation.getByRole('button', { name: 'Recent sessions' }).click();
  await expect(page.getByText('Charge history unsupported')).toBeVisible();
  expect(
    requests.slice(unsupportedRequestStart).filter(({ path }) =>
      /\/v1\/(charges|quality|collectors|devices)(?:\/|\?|$)/u.test(path),
    ),
  ).toEqual([]);

  const isDriveRoute = (url: URL): boolean =>
    url.origin === endpointOrigin && url.pathname === drivePath;
  let abortedDriveGets = 0;
  const abortDriveReads = async (route: Route): Promise<void> => {
    const url = routePath(route);
    if (route.request().method() === 'GET' && url.pathname === drivePath) {
      abortedDriveGets += 1;
      await route.abort('failed');
      return;
    }
    await route.continue();
  };
  await page.route(isDriveRoute, abortDriveReads);
  await page.getByRole('button', { name: 'Refresh Hub data' }).click();
  await expect(page.getByText('Showing retained drive sessions')).toBeVisible();
  await expect(group.locator('.count-chip')).toHaveText('51');
  expect(abortedDriveGets).toBeGreaterThanOrEqual(1);
  await page.unroute(isDriveRoute, abortDriveReads);
  await page.getByRole('button', { name: 'Refresh Hub data' }).click();
  await expect(page.getByText('Showing retained drive sessions')).toHaveCount(0);
  await expect(group.locator('.count-chip')).toHaveText('51');

  let delayedRequestSeen = false;
  let releaseDelayedRequest: (() => void) | null = null;
  const isCurrentRoute = (url: URL): boolean =>
    url.origin === endpointOrigin && url.pathname.endsWith('/current');
  const delayCurrentRead = async (route: Route): Promise<void> => {
    const url = routePath(route);
    if (
      !delayedRequestSeen &&
      route.request().method() === 'GET' &&
      url.pathname === `/v1/vehicles/${pagingVehicleId}/current`
    ) {
      delayedRequestSeen = true;
      await new Promise<void>((resolve) => {
        releaseDelayedRequest = resolve;
      });
    }
    await route.continue();
  };
  await page.route(isCurrentRoute, delayCurrentRead);
  await page.getByRole('button', { name: 'Refresh Hub data' }).click();
  await expect.poll(() => delayedRequestSeen).toBe(true);
  await page.getByRole('button', { name: 'Clear local session' }).click();
  await expect(page.getByRole('heading', { name: 'Pair with a Hub' })).toBeVisible();
  releaseDelayedRequest?.();
  await expect.poll(
    () => failedRequests.filter(({ path }) => path === `/v1/vehicles/${pagingVehicleId}/current`).length,
  ).toBeGreaterThan(0);
  await page.unroute(isCurrentRoute, delayCurrentRead);
  await expect(page.getByRole('heading', { name: 'Pair with a Hub' })).toBeVisible();
});
