import {
  asStrongEntityTag,
  HubHttpError,
  type HubClient,
  type HubCurrent,
  type HubDiscovery,
  type HubDrive,
} from '@teslatlas/sdk';
import { describe, expect, it, vi } from 'vitest';

import {
  SdkDataSource,
  mapHubCurrent,
  mapHubDiscovery,
  mapHubDrive,
  mapHubVehicle,
} from './sdk-data-source';

const HUB_ID = 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa';
const VEHICLE_ID = '11111111-1111-4111-8111-111111111111';
const SECOND_VEHICLE_ID = '22222222-2222-4222-8222-222222222222';

function current(overrides: Partial<HubCurrent> = {}): HubCurrent {
  return {
    activeRouteDestination: null,
    activeRouteEnergyAtArrival: null,
    activeRouteLatitude: null,
    activeRouteLongitude: null,
    activeRouteMilesToArrival: null,
    activeRouteMinutesToArrival: null,
    activeRouteTrafficMinutesDelay: null,
    batteryLevel: null,
    car: null,
    centerDisplayState: null,
    chargeCurrentRequest: null,
    chargeCurrentRequestMax: null,
    chargeEnergyAdded: null,
    chargeLimitSoc: null,
    chargePortDoorOpen: null,
    chargerActualCurrent: null,
    chargerPhases: null,
    chargerPower: null,
    chargerVoltage: null,
    chargingState: null,
    climateKeeperMode: null,
    displayName: null,
    doorsOpen: null,
    downloadPerc: null,
    driverFrontDoorOpen: null,
    driverFrontWindowOpen: null,
    driverRearDoorOpen: null,
    driverRearWindowOpen: null,
    elevation: null,
    estBatteryRangeKm: null,
    exteriorColor: null,
    frunkOpen: null,
    geofence: null,
    heading: null,
    healthy: null,
    idealBatteryRangeKm: null,
    insideTemp: null,
    installPerc: null,
    isClimateOn: null,
    isPreconditioning: null,
    isUserPresent: null,
    latitude: null,
    locked: null,
    longitude: null,
    model: null,
    observedAtMs: null,
    odometer: null,
    outsideTemp: null,
    passengerFrontDoorOpen: null,
    passengerFrontWindowOpen: null,
    passengerRearDoorOpen: null,
    passengerRearWindowOpen: null,
    pluggedIn: null,
    power: null,
    ratedBatteryRangeKm: null,
    scheduledChargingStartTime: null,
    sentryMode: null,
    serviceMode: null,
    shiftState: null,
    since: null,
    speed: null,
    spoilerType: null,
    state: null,
    sunRoofInstalled: null,
    sunRoofPercentOpen: null,
    sunRoofState: null,
    timeToFullCharge: null,
    tpmsPressureFl: null,
    tpmsPressureFr: null,
    tpmsPressureRl: null,
    tpmsPressureRr: null,
    tpmsSoftWarningFl: null,
    tpmsSoftWarningFr: null,
    tpmsSoftWarningRl: null,
    tpmsSoftWarningRr: null,
    trimBadging: null,
    trunkOpen: null,
    updateAvailable: null,
    updateStatus: null,
    updateVersion: null,
    usableBatteryLevel: null,
    vehicleId: VEHICLE_ID,
    version: null,
    wheelType: null,
    windowsOpen: null,
    ...overrides,
  };
}

function drive(overrides: Partial<HubDrive> = {}): HubDrive {
  return {
    ascent: null,
    descent: null,
    distanceKm: null,
    durationMin: null,
    efficiency: null,
    endAddress: null,
    endDateMs: 1_788_565_960_000,
    endGeofence: null,
    endIdealRangeKm: null,
    endLatitude: null,
    endLongitude: null,
    endRatedRangeKm: null,
    endSoc: null,
    id: 101,
    insideTempAvg: null,
    outsideTempAvg: null,
    powerMax: null,
    powerMin: null,
    speedMax: null,
    startAddress: null,
    startDateMs: 1_788_565_900_000,
    startGeofence: null,
    startIdealRangeKm: null,
    startLatitude: null,
    startLongitude: null,
    startRatedRangeKm: null,
    startSoc: null,
    vehicleId: VEHICLE_ID,
    ...overrides,
  };
}

function discovery(overrides: Partial<HubDiscovery> = {}): HubDiscovery {
  return {
    apiVersions: ['1.0'],
    capabilities: [
      'query.vehicles',
      'query.current',
      'query.drives',
      'sync.packs',
    ],
    hubId: HUB_ID,
    manifestPublicKey: 'b'.repeat(64),
    packFormat: 'sqlite-zstd',
    protocol: 'teslatlas-sync',
    protocolMajor: 1,
    sourceUrl: 'https://example.invalid/source',
    version: '2026.36.2',
    ...overrides,
  };
}

describe('current-Hub display mapping', () => {
  it('keeps Hub UUID, manifest key, and TLS identity separate', () => {
    const mapped = mapHubDiscovery(
      discovery(),
      'https://hub.example.test:8443',
      'sha256:tls-certificate',
    );

    expect(mapped.id).toBe(HUB_ID);
    expect(mapped.manifestKey).toBe('b'.repeat(64));
    expect(mapped.tlsIdentity).toBe('sha256:tls-certificate');
    expect(mapped.protocolVersion).toBe('teslatlas-sync/1');
  });

  it('maps a known vehicle without inventing state, visibility, or update time', () => {
    expect(
      mapHubVehicle({ vehicleId: VEHICLE_ID, displayName: 'Interop vehicle' }),
    ).toEqual({
      id: VEHICLE_ID,
      displayName: 'Interop vehicle',
      state: 'unknown',
      visibility: 'unknown',
      updatedAt: null,
      freshness: 'unknown',
    });
  });

  it.each([
    ['zero', 0, 0],
    ['null', null, null],
  ])('preserves %s battery level', (_name, batteryLevel, expected) => {
    expect(mapHubCurrent(current({ batteryLevel })).stateOfChargePercent).toBe(
      expected,
    );
  });

  it('converts observed milliseconds and keeps estimated, rated, and unknown lock values distinct', () => {
    const mapped = mapHubCurrent(
      current({
        observedAtMs: 1_788_566_400_000,
        estBatteryRangeKm: 160.93,
        ratedBatteryRangeKm: 155.4,
        locked: null,
      }),
    );

    expect(mapped.updatedAt).toBe('2026-09-05T00:00:00.000Z');
    expect(mapped.estimatedRangeKm).toBe(160.93);
    expect(mapped.ratedRangeKm).toBe(155.4);
    expect(mapped.locked).toBeNull();
  });

  it('keeps null drive distance and duration and converts millisecond timestamps', () => {
    expect(mapHubDrive(drive())).toEqual(
      expect.objectContaining({
        id: '101',
        vehicleId: VEHICLE_ID,
        startedAt: '2026-09-04T23:51:40.000Z',
        endedAt: '2026-09-04T23:52:40.000Z',
        distanceKm: null,
        durationMinutes: null,
        quality: null,
      }),
    );
  });
});

function makeClient() {
  const firstEtag = asStrongEntityTag('"page-one"');
  let secondEtag = asStrongEntityTag('"page-two"');
  let secondItems = [drive({ id: 103 }), drive({ id: 102 })];
  const drives = vi.fn<HubClient['drives']>(async (_vehicleId, options) => {
    if (options?.cursor === 'next-page') {
      if (options.ifNoneMatch === secondEtag) {
        return {
          kind: 'notModified',
          metadata: { status: 304, etag: secondEtag },
        };
      }
      return {
        kind: 'page',
        value: { items: secondItems, nextCursor: 'third-page' },
        metadata: { status: 200, etag: secondEtag },
      };
    }
    if (options?.cursor === 'third-page') {
      const thirdEtag = asStrongEntityTag('"page-three"');
      if (options.ifNoneMatch === thirdEtag) {
        return {
          kind: 'notModified',
          metadata: { status: 304, etag: thirdEtag },
        };
      }
      return {
        kind: 'page',
        value: { items: [drive({ id: 101 })], nextCursor: null },
        metadata: { status: 200, etag: thirdEtag },
      };
    }
    if (options?.ifNoneMatch === firstEtag) {
      return { kind: 'notModified', metadata: { status: 304, etag: firstEtag } };
    }
    return {
      kind: 'page',
      value: {
        items: [drive({ id: 105 }), drive({ id: 104 })],
        nextCursor: 'next-page',
      },
      metadata: { status: 200, etag: firstEtag },
    };
  });
  const client: HubClient = {
    discover: vi.fn(async () => ({
      value: discovery(),
      metadata: { status: 200 },
    })),
    health: vi.fn<HubClient['health']>(async () => ({
      value: { status: 'ok', version: '2026.36.2' },
      metadata: { status: 200 },
    })),
    readiness: vi.fn<HubClient['readiness']>(async () => ({
      value: { status: 'ready' },
      metadata: { status: 200 },
    })),
    claimPairing: vi.fn(async () => ({
      value: {
        accessToken: 'test-token',
        deviceId: 'cccccccc-cccc-4ccc-8ccc-cccccccccccc',
        expiresAtMs: 1_788_600_000_000,
      },
      metadata: { status: 200 },
    })),
    rotateDevice: vi.fn(),
    vehicles: vi.fn(async () => ({
      value: {
        vehicles: [{ vehicleId: VEHICLE_ID, displayName: 'Interop vehicle' }],
      },
      metadata: { status: 200 },
    })),
    current: vi.fn(async () => ({
      value: current({ batteryLevel: 0, observedAtMs: 1_788_566_400_000 }),
      metadata: { status: 200 },
    })),
    drives,
    logout: vi.fn(async () => undefined),
    dispose: vi.fn(),
  };
  return {
    client,
    drives,
    updateSecondPage: () => {
      secondEtag = asStrongEntityTag('"page-two-updated"');
      secondItems = [drive({ id: 103, distanceKm: 44.2 }), drive({ id: 102 })];
    },
  };
}

describe('SdkDataSource', () => {
  it('marks initial vehicle and dependent resource reads unavailable instead of empty', async () => {
    const { client } = makeClient();
    vi.mocked(client.vehicles).mockRejectedValueOnce(
      new Error('vehicle route unavailable'),
    );
    const source = new SdkDataSource(() => client);
    source.configure({
      endpoint: 'https://hub.example.test:8443',
      expectedHubId: HUB_ID,
      tlsIdentity: 'sha256:tls-certificate',
    });
    await source.discover();

    const snapshot = await source.readSnapshot('complete');

    expect(snapshot.vehicles).toEqual([]);
    expect(snapshot.resources.vehicles).toMatchObject({
      availability: 'temporarily-unavailable',
      retained: false,
    });
    expect(snapshot.resources.current).toMatchObject({
      availability: 'temporarily-unavailable',
      retained: false,
    });
    expect(snapshot.resources.drives).toMatchObject({
      availability: 'temporarily-unavailable',
      retained: false,
    });
    expect(client.current).not.toHaveBeenCalled();
    expect(client.drives).not.toHaveBeenCalled();
  });

  it('loads three bounded drive pages through the terminal cursor and revalidates each page', async () => {
    const { client, drives } = makeClient();
    const source = new SdkDataSource(() => client);
    source.configure({
      endpoint: 'https://hub.example.test:8443',
      expectedHubId: HUB_ID,
      tlsIdentity: 'sha256:tls-certificate',
    });
    await source.discover();
    await source.pair({
      hubId: HUB_ID,
      deviceName: 'Viewer test',
      invitationCode: JSON.stringify({
        pairing_id: 'dddddddd-dddd-4ddd-8ddd-dddddddddddd',
        secret: 'test-secret',
        expires_at_ms: 1_788_600_000_000,
        endpoint: 'https://hub.example.test:8443',
        tls_pin: 'sha256:tls-certificate',
        pairing_uri: 'teslatlas://pair/test',
      }),
    });

    const first = await source.readSnapshot('complete');
    const second = await source.readSnapshot('complete');

    expect(first.drives.map(({ id }) => id)).toEqual(['105', '104', '103', '102', '101']);
    expect(first.resources.drives).toMatchObject({
      availability: 'present',
      retained: false,
    });
    expect(second.drives.map(({ id }) => id)).toEqual(['105', '104', '103', '102', '101']);
    expect(drives).toHaveBeenNthCalledWith(
      1,
      VEHICLE_ID,
      expect.objectContaining({ limit: 2 }),
    );
    expect(drives).toHaveBeenNthCalledWith(
      2,
      VEHICLE_ID,
      expect.objectContaining({ cursor: 'next-page', limit: 2 }),
    );
    expect(drives).toHaveBeenNthCalledWith(
      3,
      VEHICLE_ID,
      expect.objectContaining({ cursor: 'third-page', limit: 2 }),
    );
    expect(drives).toHaveBeenNthCalledWith(
      4,
      VEHICLE_ID,
      expect.objectContaining({ ifNoneMatch: '"page-one"', limit: 2 }),
    );
    expect(drives).toHaveBeenNthCalledWith(
      5,
      VEHICLE_ID,
      expect.objectContaining({
        cursor: 'next-page',
        ifNoneMatch: '"page-two"',
        limit: 2,
      }),
    );
    expect(drives).toHaveBeenNthCalledWith(
      6,
      VEHICLE_ID,
      expect.objectContaining({
        cursor: 'third-page',
        ifNoneMatch: '"page-three"',
        limit: 2,
      }),
    );
  });

  it('does not let an unchanged first page hide a changed second page', async () => {
    const { client, drives, updateSecondPage } = makeClient();
    const source = new SdkDataSource(() => client);
    source.configure({
      endpoint: 'https://hub.example.test:8443',
      expectedHubId: HUB_ID,
      tlsIdentity: 'sha256:tls-certificate',
    });
    await source.discover();
    const first = await source.readSnapshot('complete');
    updateSecondPage();

    const second = await source.readSnapshot('complete');

    expect(first.drives.find(({ id }) => id === '103')?.distanceKm).toBeNull();
    expect(second.drives.find(({ id }) => id === '103')?.distanceKm).toBe(44.2);
    expect(drives).toHaveBeenNthCalledWith(
      4,
      VEHICLE_ID,
      expect.objectContaining({ ifNoneMatch: '"page-one"' }),
    );
    expect(drives).toHaveBeenNthCalledWith(
      5,
      VEHICLE_ID,
      expect.objectContaining({
        cursor: 'next-page',
        ifNoneMatch: '"page-two"',
      }),
    );
  });

  it('retains the last confirmed readiness and reason when only readiness fails', async () => {
    const { client } = makeClient();
    vi.mocked(client.readiness).mockResolvedValueOnce({
      value: { status: 'not_ready', reason: 'collector_stale' },
      metadata: { status: 503 },
    });
    const source = new SdkDataSource(() => client);
    source.configure({
      endpoint: 'https://hub.example.test:8443',
      expectedHubId: HUB_ID,
      tlsIdentity: 'sha256:tls-certificate',
    });
    await source.discover();
    await source.readSnapshot('complete');
    vi.mocked(client.readiness).mockRejectedValueOnce(
      new Error('readiness route unavailable'),
    );

    const retained = await source.readSnapshot('complete');

    expect(retained.hub.status).toBe('healthy');
    expect(retained.hub.readiness).toBe('not-ready');
    expect(retained.hub.readinessReason).toBe('collector_stale');
    expect(retained.resources.readiness).toMatchObject({
      availability: 'temporarily-unavailable',
      retained: true,
    });
  });

  it('retains only a temporarily failed resource as stale while successful resources refresh', async () => {
    const { client } = makeClient();
    const source = new SdkDataSource(() => client);
    source.configure({
      endpoint: 'https://hub.example.test:8443',
      expectedHubId: HUB_ID,
      tlsIdentity: 'sha256:tls-certificate',
    });
    await source.discover();
    await source.readSnapshot('complete');
    vi.mocked(client.current).mockRejectedValueOnce(new Error('route unavailable'));
    vi.mocked(client.health).mockResolvedValueOnce({
      value: { status: 'ok', version: '2026.36.3' },
      metadata: { status: 200 },
    });

    const retained = await source.readSnapshot('complete');

    expect(retained.hub.version).toBe('2026.36.3');
    expect(retained.resources.health).toMatchObject({
      availability: 'present',
      retained: false,
    });
    expect(retained.resources.current).toMatchObject({
      availability: 'temporarily-unavailable',
      retained: true,
    });
    expect(retained.currentByVehicle[VEHICLE_ID]?.freshness).toBe('stale');
  });

  it('marks only the failed vehicle current value stale', async () => {
    const { client } = makeClient();
    vi.mocked(client.discover).mockResolvedValue({
      value: discovery({ capabilities: ['query.vehicles', 'query.current'] }),
      metadata: { status: 200 },
    });
    vi.mocked(client.vehicles).mockResolvedValue({
      value: {
        vehicles: [
          { vehicleId: VEHICLE_ID, displayName: 'First vehicle' },
          { vehicleId: SECOND_VEHICLE_ID, displayName: 'Second vehicle' },
        ],
      },
      metadata: { status: 200 },
    });
    vi.mocked(client.current).mockImplementation(async (vehicleId) => ({
      value: current({ vehicleId }),
      metadata: { status: 200 },
    }));
    const source = new SdkDataSource(() => client);
    source.configure({
      endpoint: 'https://hub.example.test:8443',
      expectedHubId: HUB_ID,
      tlsIdentity: 'sha256:tls-certificate',
    });
    await source.discover();
    await source.readSnapshot('complete');
    vi.mocked(client.current).mockImplementation(async (vehicleId) => {
      if (vehicleId === SECOND_VEHICLE_ID) {
        throw new Error('second vehicle unavailable');
      }
      return {
        value: current({ vehicleId, batteryLevel: 1 }),
        metadata: { status: 200 },
      };
    });

    const retained = await source.readSnapshot('complete');

    expect(retained.currentByVehicle[VEHICLE_ID]).toMatchObject({
      freshness: 'unknown',
      stateOfChargePercent: 1,
    });
    expect(retained.currentByVehicle[SECOND_VEHICLE_ID]?.freshness).toBe(
      'stale',
    );
  });

  it('preserves initial mixed current and drive successes and recovers failed vehicles', async () => {
    const { client, drives } = makeClient();
    vi.mocked(client.vehicles).mockResolvedValue({
      value: {
        vehicles: [
          { vehicleId: VEHICLE_ID, displayName: 'First vehicle' },
          { vehicleId: SECOND_VEHICLE_ID, displayName: 'Second vehicle' },
        ],
      },
      metadata: { status: 200 },
    });
    let failSecondCurrent = true;
    vi.mocked(client.current).mockImplementation(async (vehicleId) => {
      if (vehicleId === SECOND_VEHICLE_ID && failSecondCurrent) {
        throw new Error('second current route unavailable');
      }
      return {
        value: current({ vehicleId, batteryLevel: vehicleId === VEHICLE_ID ? 68 : 51 }),
        metadata: { status: 200 },
      };
    });
    const originalDrives = drives.getMockImplementation();
    if (originalDrives === undefined) throw new Error('drive mock missing');
    let failSecondDrives = true;
    drives.mockImplementation(async (vehicleId, options) => {
      if (vehicleId === SECOND_VEHICLE_ID) {
        if (failSecondDrives) {
          throw new Error('second drive route unavailable');
        }
        return {
          kind: 'page',
          value: {
            items: [drive({ id: 201, vehicleId: SECOND_VEHICLE_ID })],
            nextCursor: null,
          },
          metadata: { status: 200, etag: asStrongEntityTag('"second-vehicle"') },
        };
      }
      return originalDrives(vehicleId, options);
    });
    const source = new SdkDataSource(() => client);
    source.configure({
      endpoint: 'https://hub.example.test:8443',
      expectedHubId: HUB_ID,
      tlsIdentity: 'sha256:tls-certificate',
    });
    await source.discover();

    const mixed = await source.readSnapshot('complete');

    expect(mixed.currentByVehicle[VEHICLE_ID]?.stateOfChargePercent).toBe(68);
    expect(mixed.currentByVehicle[SECOND_VEHICLE_ID]).toBeNull();
    expect(mixed.resources.current).toMatchObject({
      availability: 'temporarily-unavailable',
      retained: false,
    });
    expect(mixed.drives.map(({ id }) => id)).toEqual([
      '105',
      '104',
      '103',
      '102',
      '101',
    ]);
    expect(mixed.resources.drives).toMatchObject({
      availability: 'temporarily-unavailable',
      retained: false,
    });

    failSecondCurrent = false;
    failSecondDrives = false;
    const recovered = await source.readSnapshot('complete');

    expect(recovered.currentByVehicle[SECOND_VEHICLE_ID]?.stateOfChargePercent).toBe(51);
    expect(recovered.drives.some(({ id }) => id === '201')).toBe(true);
    expect(recovered.resources.current.availability).toBe('present');
    expect(recovered.resources.drives.availability).toBe('present');
  });

  it('marks unsupported rich-profile resources without calling a private route', async () => {
    const { client } = makeClient();
    const source = new SdkDataSource(() => client);
    source.configure({
      endpoint: 'https://hub.example.test:8443',
      expectedHubId: HUB_ID,
      tlsIdentity: 'sha256:tls-certificate',
    });
    await source.discover();

    const snapshot = await source.readSnapshot('complete');

    expect(snapshot.resources.charges.availability).toBe('unsupported');
    expect(snapshot.resources.quality.availability).toBe('unsupported');
    expect(snapshot.resources.collectors.availability).toBe('unsupported');
    expect(snapshot.resources.devices.availability).toBe('unsupported');
    expect(snapshot.charges).toEqual([]);
    expect(snapshot.quality).toBeNull();
    expect(snapshot.collectors).toEqual([]);
    expect(snapshot.devices).toEqual([]);
    expect(Object.keys(client).sort()).toEqual([
      'claimPairing',
      'current',
      'discover',
      'dispose',
      'drives',
      'health',
      'logout',
      'readiness',
      'rotateDevice',
      'vehicles',
    ]);
  });

  it('clears identity-bound state when an authenticated route reports authorization loss', async () => {
    const { client } = makeClient();
    vi.mocked(client.vehicles).mockRejectedValueOnce(
      new HubHttpError(401, 'hub_http_error'),
    );
    const source = new SdkDataSource(() => client);
    source.configure({
      endpoint: 'https://hub.example.test:8443',
      expectedHubId: HUB_ID,
      tlsIdentity: 'sha256:tls-certificate',
    });
    await source.discover();

    await expect(source.readSnapshot('complete')).rejects.toMatchObject({
      code: 'AUTH_LOST',
    });
    expect(client.logout).toHaveBeenCalledOnce();
  });

  it('invalidates a pending read on logout so it cannot repopulate the session', async () => {
    const { client } = makeClient();
    let resolveCurrent:
      | ((value: Awaited<ReturnType<HubClient['current']>>) => void)
      | undefined;
    vi.mocked(client.current).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveCurrent = resolve;
        }),
    );
    const source = new SdkDataSource(() => client);
    source.configure({
      endpoint: 'https://hub.example.test:8443',
      expectedHubId: HUB_ID,
      tlsIdentity: 'sha256:tls-certificate',
    });
    await source.discover();
    const pending = source.readSnapshot('complete');
    while (resolveCurrent === undefined) await Promise.resolve();
    await source.logout();
    resolveCurrent({ value: current({ batteryLevel: 99 }), metadata: { status: 200 } });

    await expect(pending).rejects.toMatchObject({ name: 'AbortError' });
    expect(client.logout).toHaveBeenCalledOnce();
  });
});
