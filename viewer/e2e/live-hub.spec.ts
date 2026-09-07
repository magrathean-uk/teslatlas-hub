import { createHash } from 'node:crypto';
import { spawn, type ChildProcess } from 'node:child_process';
import { readFile, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { chromium, expect, test, type Page, type Route } from '@playwright/test';
import { createHubClient, type HubCredential } from '@teslatlas/sdk/node';

import { hashBundle, verifyBrowserTrust } from './live-evidence';

function required(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is required for live Hub acceptance`);
  return value;
}

function field(record: Record<string, unknown>, snake: string, camel: string): string {
  const value = record[snake] ?? record[camel];
  if (typeof value !== 'string' || value.length === 0) {
    throw new Error(`invitation field ${snake} is missing`);
  }
  return value;
}

async function fillConnection(
  page: Page,
  endpoint: string,
  hubId: string,
  tlsIdentity: string,
) {
  await page.getByLabel('Hub endpoint').fill(endpoint);
  await page.getByLabel('Expected Hub UUID').fill(hubId);
  await page.getByLabel('Invitation TLS identity').fill(tlsIdentity);
  await page.getByRole('button', { name: 'Inspect live Hub' }).click();
}

test('built Viewer pairs and reads the real current-Hub SDK path', async () => {
  test.setTimeout(120_000);
  const scenario = process.env.TESLATLAS_VIEWER_LIVE_SCENARIO ?? 'logout';
  if (scenario !== 'logout' && scenario !== 'auth-loss') {
    throw new Error('TESLATLAS_VIEWER_LIVE_SCENARIO must be logout or auth-loss');
  }
  const descriptor = JSON.parse(
    await readFile(required('TESLATLAS_HUB_HTTP_CONFIG'), 'utf8'),
  );
  const fixtureScenario = JSON.parse(
    await readFile(descriptor.scenario_path as string, 'utf8'),
  ) as { vehicle_ids?: unknown };
  if (
    !Array.isArray(fixtureScenario.vehicle_ids) ||
    typeof fixtureScenario.vehicle_ids[1] !== 'string'
  ) {
    throw new Error('fixture scenario does not identify its empty second vehicle');
  }
  const emptyVehicleId = fixtureScenario.vehicle_ids[1];
  const invitationPath = descriptor.invitation_path as string;
  const invitationText = await readFile(invitationPath, 'utf8');
  const invitation = JSON.parse(invitationText) as Record<string, unknown>;
  const endpoint = required('TESLATLAS_VIEWER_HUB_ENDPOINT');
  const pageOrigin = required('TESLATLAS_VIEWER_PAGE_ORIGIN');
  const tlsIdentity = field(invitation, 'tls_pin', 'tlsPin');
  const hubId = descriptor.hub_id as string;
  let replacementHubForward: ChildProcess | null = null;

  const trustedBrowser = await chromium.connectOverCDP(
    required('TESLATLAS_BROWSER_CDP_URL'),
  );
  const untrustedBrowser = await chromium.connectOverCDP(
    required('TESLATLAS_BROWSER_UNTRUSTED_CDP_URL'),
  );
  try {
    const trustedCdp = await trustedBrowser.newBrowserCDPSession();
    const untrustedCdp = await untrustedBrowser.newBrowserCDPSession();
    const trustedArguments = (
      await trustedCdp.send('Browser.getBrowserCommandLine')
    ).arguments as string[];
    const untrustedArguments = (
      await untrustedCdp.send('Browser.getBrowserCommandLine')
    ).arguments as string[];
    const trust = await verifyBrowserTrust({
      witnessPath: required('TESLATLAS_BROWSER_LAUNCH_WITNESS'),
      certificatePath: descriptor.certificate_path,
      trustedArguments,
      untrustedArguments,
    });

    const untrustedContext =
      untrustedBrowser.contexts()[0] ?? (await untrustedBrowser.newContext());
    const untrustedPage = await untrustedContext.newPage();
    let untrustedTlsError = '';
    try {
      await untrustedPage.goto(`${endpoint}/healthz`, { timeout: 10_000 });
    } catch (error) {
      untrustedTlsError = String(error);
    }
    expect(untrustedTlsError).toContain('ERR_CERT_AUTHORITY_INVALID');
    await untrustedPage.close();

    const context =
      trustedBrowser.contexts()[0] ?? (await trustedBrowser.newContext());
    const wrongHubPage = await context.newPage();
    const wrongHubRequests: string[] = [];
    wrongHubPage.on('request', (request) => {
      if (request.url().startsWith(endpoint)) wrongHubRequests.push(request.url());
    });
    await wrongHubPage.goto(`${pageOrigin}/?mode=live&paired=false`);
    await fillConnection(
      wrongHubPage,
      endpoint,
      'ffffffff-ffff-4fff-8fff-ffffffffffff',
      tlsIdentity,
    );
    await expect(wrongHubPage.getByRole('alert')).toContainText(
      /identity|match/i,
    );
    expect(wrongHubRequests.some((url) => url.includes('/.well-known/'))).toBe(
      true,
    );
    await wrongHubPage.close();

    const page = await context.newPage();
    const networkCdp = await context.newCDPSession(page);
    const requests: Array<{
      method: string;
      path: string;
      hasAuthorization: boolean;
      hasIfNoneMatch: boolean;
    }> = [];
    const responses: Array<{ path: string; status: number }> = [];
    const drivePageEvidence: Array<{
      path: string;
      vehicleId: string;
      itemIds: string[];
      nextCursor: string | null;
    }> = [];
    const driveEvidenceReads: Promise<void>[] = [];
    networkCdp.on('Network.requestWillBeSent', (event) => {
      if (!event.request.url.startsWith(endpoint)) return;
      const url = new URL(event.request.url);
      const headers = Object.fromEntries(
        Object.entries(event.request.headers).map(([name, value]) => [
          name.toLowerCase(),
          value,
        ]),
      );
      requests.push({
        method: event.request.method,
        path: `${url.pathname}${url.search}`,
        hasAuthorization: 'authorization' in headers,
        hasIfNoneMatch: 'if-none-match' in headers,
      });
    });
    await networkCdp.send('Network.enable');
    page.on('response', (response) => {
      if (!response.url().startsWith(endpoint)) return;
      const url = new URL(response.url());
      responses.push({ path: `${url.pathname}${url.search}`, status: response.status() });
      const vehicleId = url.pathname.match(
        /^\/v1\/vehicles\/([^/]+)\/drives$/u,
      )?.[1];
      if (
        vehicleId !== undefined &&
        response.request().method() === 'GET' &&
        response.status() === 200
      ) {
        driveEvidenceReads.push(
          response.json().then((wire: Record<string, unknown>) => {
            const items = wire.items;
            if (!Array.isArray(items)) throw new Error('drive page items missing');
            const nextCursor =
              'next_cursor' in wire ? wire.next_cursor : wire.nextCursor;
            if (nextCursor !== null && typeof nextCursor !== 'string') {
              throw new Error('drive page next cursor missing');
            }
            drivePageEvidence.push({
              path: `${url.pathname}${url.search}`,
              vehicleId,
              itemIds: items.map((item) =>
                String((item as Record<string, unknown>).id),
              ),
              nextCursor,
            });
          }),
        );
      }
    });

    await page.goto(`${pageOrigin}/?mode=live&paired=false`);
    await fillConnection(page, endpoint, hubId, tlsIdentity);
    await expect(page.getByRole('heading', { name: `Hub ${hubId}` })).toBeVisible();
    await page.getByLabel('Pairing invitation JSON').fill(invitationText);
    let initialMixedCurrentAborts = 0;
    let initialMixedDriveAborts = 0;
    const isInitialEmptyVehicleRoute = (url: URL) =>
      url.origin === new URL(endpoint).origin &&
      (url.pathname === `/v1/vehicles/${emptyVehicleId}/current` ||
        url.pathname === `/v1/vehicles/${emptyVehicleId}/drives`);
    const abortInitialEmptyVehicleReads = async (route: Route) => {
      if (route.request().method() !== 'GET') {
        await route.continue();
        return;
      }
      if (new URL(route.request().url()).pathname.endsWith('/current')) {
        initialMixedCurrentAborts += 1;
      } else {
        initialMixedDriveAborts += 1;
      }
      await route.abort('failed');
    };
    await page.route(isInitialEmptyVehicleRoute, abortInitialEmptyVehicleReads);
    const initialClaimResponse = page.waitForResponse(
      (response) =>
        response.url().startsWith(endpoint) &&
        response.url().includes('/pairings/') &&
        response.request().method() === 'POST',
    );
    await page.getByRole('button', { name: 'Pair live Hub' }).click();
    const initialClaim = await initialClaimResponse;
    expect(initialClaim.status()).toBe(200);
    const initialCredentialWire = (await initialClaim.json()) as Record<
      string,
      unknown
    >;
    await expect(page.getByRole('region', { name: 'Hub health' })).toBeVisible();
    await expect(page.getByText(hubId, { exact: true })).toBeVisible();

    const navigation = page.getByRole('navigation', { name: 'Viewer sections' });
    await navigation.getByRole('button', { name: 'Vehicles' }).click();
    await expect(page.getByRole('heading', { name: 'Interop – Árvíztűrő 🚗' })).toBeVisible();
    await expect(page.getByRole('heading', { name: 'Interop empty' })).toBeVisible();

    await navigation.getByRole('button', { name: 'Current state' }).click();
    await expect(
      page.getByText('Some current state is temporarily unavailable'),
    ).toBeVisible();
    await expect(page.getByText('0%', { exact: true })).toBeVisible();
    await expect(page.getByText('160.93 km', { exact: true })).toBeVisible();
    await expect(page.getByText('Not reported').first()).toBeVisible();
    await expect(
      page.getByRole('heading', { name: 'Interop empty' }),
    ).toHaveCount(0);

    await navigation.getByRole('button', { name: 'Recent sessions' }).click();
    const drivesSection = page.locator('section.session-section').first();
    await expect(
      drivesSection.getByText('Some drive sessions are temporarily unavailable'),
    ).toBeVisible();
    await expect(drivesSection.locator('.count-chip')).toHaveText('5');
    await expect(drivesSection.getByText('Not reported').first()).toBeVisible();
    expect(initialMixedCurrentAborts).toBe(1);
    expect(initialMixedDriveAborts).toBe(1);
    await page.unroute(
      isInitialEmptyVehicleRoute,
      abortInitialEmptyVehicleReads,
    );
    const initialMixedRecoveryResponseStart = responses.length;
    await page.getByRole('button', { name: 'Refresh Hub data' }).click();
    await expect(
      drivesSection.getByText('Some drive sessions are temporarily unavailable'),
    ).toHaveCount(0);
    await expect(drivesSection.locator('.count-chip')).toHaveText('5');
    await navigation.getByRole('button', { name: 'Current state' }).click();
    await expect(
      page.getByText('Some current state is temporarily unavailable'),
    ).toHaveCount(0);
    await expect(
      page.getByRole('heading', { name: 'Interop empty' }),
    ).toBeVisible();
    expect(
      responses.slice(initialMixedRecoveryResponseStart).some(
        (response) =>
          response.path === `/v1/vehicles/${emptyVehicleId}/current` &&
          response.status === 200,
      ),
    ).toBe(true);
    expect(
      responses.slice(initialMixedRecoveryResponseStart).some(
        (response) =>
          response.path.startsWith(
            `/v1/vehicles/${emptyVehicleId}/drives?`,
          ) && response.status === 200,
      ),
    ).toBe(true);
    await navigation.getByRole('button', { name: 'Recent sessions' }).click();
    await Promise.all(driveEvidenceReads);
    const fiveDriveVehicleId = drivePageEvidence.find(
      (pageEvidence) => pageEvidence.itemIds.includes('105'),
    )?.vehicleId;
    if (fiveDriveVehicleId === undefined) {
      throw new Error('five-drive seed vehicle was not observed');
    }
    const initialFiveDrivePages = drivePageEvidence.filter(
      (pageEvidence) => pageEvidence.vehicleId === fiveDriveVehicleId,
    );
    expect(initialFiveDrivePages.map(({ itemIds }) => itemIds)).toEqual([
      ['105', '104'],
      ['103', '102'],
      ['101'],
    ]);
    expect(initialFiveDrivePages.at(-1)?.nextCursor).toBeNull();

    const requestCountBeforeRefresh = requests.length;
    const responseCountBeforeRefresh = responses.length;
    const etagReplayResponse = page.waitForResponse(
      (response) =>
        response.url().startsWith(endpoint) &&
        response.url().includes('/drives?') &&
        response.status() === 304,
    );
    await page.getByRole('button', { name: 'Refresh Hub data' }).click();
    await etagReplayResponse;
    await expect(page.getByRole('button', { name: 'Refresh Hub data' })).toBeEnabled();
    expect(
      requests.some(
        (request) => request.path.includes('/drives?') && request.hasIfNoneMatch,
      ),
    ).toBe(true);
    expect(
      responses
        .slice(responseCountBeforeRefresh)
        .some((response) => response.path.includes('/drives?') && response.status === 304),
    ).toBe(true);

    const replayedFiveDriveRequests = () =>
      requests.slice(requestCountBeforeRefresh).filter(
        (request) =>
          request.method === 'GET' &&
          request.path.startsWith(
            `/v1/vehicles/${fiveDriveVehicleId}/drives?`,
          ),
      );
    await expect.poll(() => replayedFiveDriveRequests().length).toBe(3);
    expect(replayedFiveDriveRequests().every(({ hasIfNoneMatch }) => hasIfNoneMatch)).toBe(
      true,
    );
    const replayedFiveDriveResponses = () =>
      responses.slice(responseCountBeforeRefresh).filter(
        (response) =>
          response.path.startsWith(
            `/v1/vehicles/${fiveDriveVehicleId}/drives?`,
          ) && (response.status === 200 || response.status === 304),
      );
    await expect.poll(() => replayedFiveDriveResponses().length).toBe(3);
    expect(replayedFiveDriveResponses()[0].status).toBe(304);
    const replayedFiveDriveRequestEvidence = replayedFiveDriveRequests();
    const replayedFiveDriveResponseEvidence = replayedFiveDriveResponses();

    const selectiveRequestStart = requests.length;
    const selectiveResponseStart = responses.length;
    let selectivelyAbortedDriveGets = 0;
    const isDriveRoute = (url: URL) =>
      url.origin === new URL(endpoint).origin &&
      /^\/v1\/vehicles\/[^/]+\/drives$/u.test(url.pathname);
    const abortDriveReads = async (route: Route) => {
      if (route.request().method() === 'GET') {
        selectivelyAbortedDriveGets += 1;
        await route.abort('failed');
      } else {
        await route.continue();
      }
    };
    await page.route(isDriveRoute, abortDriveReads);
    await page.getByRole('button', { name: 'Refresh Hub data' }).click();
    await expect(page.getByText('Showing retained drive sessions')).toBeVisible();
    await expect(drivesSection.locator('.count-chip')).toHaveText('5');
    expect(selectivelyAbortedDriveGets).toBeGreaterThanOrEqual(1);
    expect(
      responses
        .slice(selectiveResponseStart)
        .some((response) => response.path === '/healthz' && response.status === 200),
    ).toBe(true);
    expect(
      responses.slice(selectiveResponseStart).some(
        (response) =>
          response.path.startsWith(
            `/v1/vehicles/${fiveDriveVehicleId}/current`,
          ) && response.status === 200,
      ),
    ).toBe(true);
    expect(
      responses
        .slice(selectiveResponseStart)
        .some(
          (response) =>
            response.path.includes('/drives?') &&
            (response.status === 200 || response.status === 304),
        ),
    ).toBe(false);

    await navigation.getByRole('button', { name: 'Hub health' }).click();
    await expect(page.getByText('Healthy', { exact: true })).toBeVisible();
    await navigation.getByRole('button', { name: 'Recent sessions' }).click();
    await page.unroute(isDriveRoute, abortDriveReads);
    const recoveryResponseStart = responses.length;
    await page.getByRole('button', { name: 'Refresh Hub data' }).click();
    await expect(page.getByText('Showing retained drive sessions')).toHaveCount(0);
    await expect(drivesSection.locator('.count-chip')).toHaveText('5');
    expect(
      responses.slice(recoveryResponseStart).some(
        (response) =>
          response.path.startsWith(
            `/v1/vehicles/${fiveDriveVehicleId}/drives?`,
          ) && response.status === 304,
      ),
    ).toBe(true);
    const selectiveRequestCount = requests.length - selectiveRequestStart;

    await navigation.getByRole('button', { name: 'Hub health' }).click();
    const outageRequestStart = requests.length;
    const outageResponseStart = responses.length;
    const hubForwardPid = Number(required('TESLATLAS_HUB_FORWARD_PID'));
    if (!Number.isSafeInteger(hubForwardPid) || hubForwardPid <= 1) {
      throw new Error('TESLATLAS_HUB_FORWARD_PID is invalid');
    }
    process.kill(hubForwardPid, 'SIGTERM');
    await expect
      .poll(
        () =>
          page.evaluate(async (url) => {
            try {
              await fetch(`${url}/healthz`);
              return 'reachable';
            } catch {
              return 'unavailable';
            }
          }, endpoint),
        { timeout: 15_000 },
      )
      .toBe('unavailable');
    await page.getByRole('button', { name: 'Refresh Hub data' }).click();
    await expect(page.getByText('Cannot reach this Hub.')).toBeVisible();
    expect(requests.length).toBeGreaterThan(outageRequestStart);
    const outageRequestCount = requests.length - outageRequestStart;

    const forwardPort = required('TESLATLAS_HUB_FORWARD_PORT');
    replacementHubForward = spawn(
      'ssh',
      [
        '-F',
        required('TESLATLAS_SSH_CONFIG'),
        '-S',
        'none',
        '-o',
        'ExitOnForwardFailure=yes',
        '-N',
        '-R',
        `localhost:${forwardPort}:localhost:${forwardPort}`,
        required('TESLATLAS_SSH_ALIAS'),
      ],
      { stdio: 'ignore' },
    );
    await expect
      .poll(
        async () => {
          if (replacementHubForward?.exitCode !== null) return 'exited';
          return page.evaluate(async (url) => {
            try {
              const response = await fetch(`${url}/healthz`);
              return response.ok ? 'ready' : 'not-ready';
            } catch {
              return 'unavailable';
            }
          }, endpoint);
        },
        { timeout: 20_000 },
      )
      .toBe('ready');
    await page.getByRole('button', { name: 'Refresh Hub data' }).click();
    await expect(page.getByRole('button', { name: 'Refresh Hub data' })).toBeEnabled();
    await expect(page.getByText('Cannot reach this Hub.')).toHaveCount(0);
    expect(
      responses
        .slice(outageResponseStart)
        .some((response) => response.path === '/healthz' && response.status === 200),
    ).toBe(true);

    await navigation.getByRole('button', { name: 'Data quality' }).click();
    await expect(page.getByText('Data quality is unsupported by hub-http-v1.')).toBeVisible();
    await navigation.getByRole('button', { name: 'Collector freshness' }).click();
    await expect(
      page.getByText('Collector cost and backup age are unsupported by hub-http-v1.'),
    ).toBeVisible();
    await navigation.getByRole('button', { name: 'Paired devices' }).click();
    await expect(
      page.getByText('Remote paired-device management is unsupported by hub-http-v1.'),
    ).toBeVisible();
    expect(
      requests.filter((request) =>
        /\/(charges|quality|collectors|devices)(?:\/|\?|$)/u.test(request.path),
      ),
    ).toEqual([]);

    let localLogoutRequestCount: number | null = null;
    let authenticated401Count = 0;
    if (scenario === 'logout') {
      const beforeLogout = requests.length;
      await page.getByRole('button', { name: 'Clear local session' }).click();
      await expect(page.getByRole('heading', { name: 'Pair with a Hub' })).toBeVisible();
      localLogoutRequestCount = requests.length - beforeLogout;
      expect(localLogoutRequestCount).toBe(0);

      await fillConnection(page, endpoint, hubId, tlsIdentity);
      await page.getByLabel('Pairing invitation JSON').fill(invitationText);
      await page.getByRole('button', { name: 'Pair live Hub' }).click();
      await expect(page.getByRole('alert')).toBeVisible();
      expect(
        responses.some(
          (response) =>
            response.path.includes('/pairings/') && response.status === 401,
        ),
      ).toBe(true);
    } else {
      const credential: HubCredential = {
        accessToken: field(initialCredentialWire, 'access_token', 'accessToken'),
        deviceId: field(initialCredentialWire, 'device_id', 'deviceId'),
        expiresAtMs: Number(
          initialCredentialWire.expires_at_ms ?? initialCredentialWire.expiresAtMs,
        ),
      };
      let externalCredential: HubCredential | undefined = credential;
      const rotator = createHubClient({
        endpoint,
        expectedHubId: hubId,
        credentials: {
          load: () => externalCredential,
          save: (value) => {
            externalCredential = value;
          },
          clear: () => {
            externalCredential = undefined;
          },
        },
      });
      await rotator.discover();
      await rotator.rotateDevice();
      rotator.dispose();
      const responseStart = responses.length;
      await page.getByRole('button', { name: 'Refresh Hub data' }).click();
      await expect(page.getByRole('heading', { name: 'Pair with a Hub' })).toBeVisible();
      authenticated401Count = responses
        .slice(responseStart)
        .filter((response) => response.status === 401).length;
      expect(authenticated401Count).toBeGreaterThanOrEqual(1);
    }

    const drivePageCounts = Object.fromEntries(
      [...new Set(
        requests
          .map((request) => request.path.match(/^\/v1\/vehicles\/([^/]+)\/drives\?/u)?.[1])
          .filter((vehicleId): vehicleId is string => vehicleId !== undefined),
      )].map((vehicleId) => [
        vehicleId,
        requests.filter(
          (request) => request.method === 'GET' &&
            request.path.startsWith(`/v1/vehicles/${vehicleId}/drives?`) &&
            !request.hasIfNoneMatch,
        ).length,
      ]),
    );
    expect(Math.max(...Object.values(drivePageCounts))).toBe(3);

    const viewerBundle = await hashBundle(resolve('dist'));
    const servedViewerFiles = await page.evaluate(
      async ({ origin, paths }) =>
        Promise.all(
          paths.map(async (path) => {
            const response = await fetch(new URL(path, `${origin}/`));
            if (!response.ok) {
              throw new Error(`Viewer bundle fetch failed for ${path}`);
            }
            const digest = await crypto.subtle.digest(
              'SHA-256',
              await response.arrayBuffer(),
            );
            return {
              path,
              sha256: [...new Uint8Array(digest)]
                .map((value) => value.toString(16).padStart(2, '0'))
                .join(''),
            };
          }),
        ),
      { origin: pageOrigin, paths: viewerBundle.files.map(({ path }) => path) },
    );
    servedViewerFiles.sort((left, right) => left.path.localeCompare(right.path));
    expect(servedViewerFiles).toEqual(viewerBundle.files);
    const sdkMetadata = JSON.parse(
      await readFile(resolve('artifacts/teslatlas-sdk.json'), 'utf8'),
    );
    const receipt = {
      schemaVersion: 1,
      browser: await trustedBrowser.version(),
      pageOrigin,
      endpoint,
      hubId,
      profile: sdkMetadata.profile,
      sdk: {
        packageVersion: sdkMetadata.package_version,
        tarballSha256: sdkMetadata.tarball_sha256,
        installedContentManifestSha256:
          sdkMetadata.installed_content_manifest_sha256,
        installedMemberCount: sdkMetadata.installed_member_count,
      },
      viewerBundle,
      servedViewerBundle: {
        fileCount: servedViewerFiles.length,
        matchesBuiltBundle: true,
        files: servedViewerFiles,
      },
      normalCertificateValidation: true,
      certificateSha256: trust.certificateSha256,
      trustedNssDatabase: trust.trustedNssDatabase,
      untrustedControlError: 'ERR_CERT_AUTHORITY_INVALID',
      cases: {
        wrongHub: 'passed',
        pairing: 'passed',
        vehicles: 2,
        drivePageCounts,
        drivePagination: {
          vehicleId: fiveDriveVehicleId,
          pages: initialFiveDrivePages,
          flattenedIds: initialFiveDrivePages.flatMap(({ itemIds }) => itemIds),
          terminalCursorObserved: initialFiveDrivePages.at(-1)?.nextCursor === null,
          perPageConditionalRequests: replayedFiveDriveRequestEvidence.length,
          perPageStatuses: replayedFiveDriveResponseEvidence.map(({ status }) => status),
        },
        etagReplay: 'passed',
        initialMixedVehicleFailure: {
          vehicleId: emptyVehicleId,
          injection:
            'Playwright aborted only initial GET current and drive requests for the empty second fixture vehicle; no response was fulfilled',
          currentGetAborts: initialMixedCurrentAborts,
          driveGetAborts: initialMixedDriveAborts,
          successfulVehicleCurrentVisible: true,
          successfulVehicleDriveRows: 5,
          unavailableNoticesVisible: true,
          recovery: 'passed',
        },
        selectiveDriveFailure: {
          injection: 'Playwright aborted GET requests only for /v1/vehicles/{id}/drives; no response was fulfilled',
          requestCount: selectiveRequestCount,
          abortedGetCount: selectivelyAbortedDriveGets,
          healthControl200: true,
          currentControl200: true,
          retainedRows: 5,
          recovery: 'passed',
        },
        outageRequests: outageRequestCount,
        reconnectAfterOutage: 'passed',
        authLoss401Count: authenticated401Count,
        reusedInvitation401: scenario === 'logout' ? 'passed' : null,
        unsupportedRoutesRequested: 0,
        localLogoutRequestCount,
      },
      requests,
      responses,
    };
    const receiptText = `${JSON.stringify(receipt, null, 2)}\n`;
    const receiptPath = required('TESLATLAS_VIEWER_HUB_RECEIPT');
    await writeFile(receiptPath, receiptText, { mode: 0o600, flag: 'wx' });
    process.stdout.write(
      `Viewer live receipt SHA-256 ${createHash('sha256').update(receiptText).digest('hex')}\n`,
    );
    await page.close();
  } finally {
    if (replacementHubForward?.exitCode === null) {
      replacementHubForward.kill('SIGTERM');
    }
    await trustedBrowser.close();
    await untrustedBrowser.close();
  }
});
