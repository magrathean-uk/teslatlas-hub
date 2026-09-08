import { describe, expect, it, vi } from 'vitest';

import { createDataSource } from './create-data-source';
import { createFixtureSnapshot } from './fixture-data';
import { FixtureDataSource } from './fixture-data-source';
import { SdkDataSource } from './sdk-data-source';
import type { DriveSummary } from './types';

function drive(vehicleId: string, id: string): DriveSummary {
  return {
    id,
    vehicleId,
    startedAt: '2026-08-30T09:00:00.000Z',
    endedAt: '2026-08-30T09:15:00.000Z',
    startLabel: 'Start',
    endLabel: 'End',
    distanceKm: 10,
    durationMinutes: 15,
    energyUsedKwh: 2,
    quality: null,
  };
}

describe('FixtureDataSource', () => {
  it('returns the fixed complete fixture without sharing mutable values', async () => {
    const source = new FixtureDataSource();

    const first = await source.readSnapshot('complete');
    first.vehicles[0].displayName = 'changed by caller';
    const second = await source.readSnapshot('complete');

    expect(second.generatedAt).toBe('2026-08-30T10:00:00.000Z');
    expect(second.hub.displayName).toBe('Hawthorn Hub');
    expect(second.vehicles.map((vehicle) => vehicle.displayName)).toEqual([
      'Northstar',
      'Juniper',
    ]);
  });

  it('returns genuinely empty collections in the empty fixture', async () => {
    const snapshot = await new FixtureDataSource().readSnapshot('empty');

    expect(snapshot.vehicles).toEqual([]);
    expect(snapshot.drives).toEqual([]);
    expect(snapshot.charges).toEqual([]);
    expect(snapshot.devices).toEqual([]);
  });

  it('marks old Hub and vehicle data as stale', async () => {
    const snapshot = await new FixtureDataSource().readSnapshot('stale');

    expect(snapshot.hub.freshness).toBe('stale');
    expect(snapshot.currentByVehicle['vehicle-redacted-1']?.freshness).toBe(
      'stale',
    );
  });

  it('identifies derived current-state fields in the inferred fixture', async () => {
    const snapshot = await new FixtureDataSource().readSnapshot('inferred');

    expect(
      snapshot.currentByVehicle['vehicle-redacted-1']?.inferredFields,
    ).toEqual(['estimatedRangeKm']);
  });

  it('exposes unresolved gaps and degraded collectors', async () => {
    const snapshot = await new FixtureDataSource().readSnapshot('degraded');

    expect(snapshot.quality?.overall).toBe('degraded');
    expect(snapshot.quality?.gaps).toHaveLength(2);
    expect(snapshot.collectors.map((collector) => collector.status)).toContain(
      'degraded',
    );
  });

  it('keeps offline distinct from degraded', async () => {
    const snapshot = await new FixtureDataSource().readSnapshot('offline');

    expect(snapshot.hub.status).toBe('offline');
    expect(
      snapshot.vehicles.every(
        (vehicle) =>
          vehicle.state === 'offline' && vehicle.freshness === 'stale',
      ),
    ).toBe(true);
    expect(
      Object.values(snapshot.currentByVehicle).every(
        (current) => current === null || current.freshness === 'stale',
      ),
    ).toBe(true);
    expect(snapshot.collectors.every((collector) => collector.status === 'offline')).toBe(
      true,
    );
  });

  it('throws a typed local error for the error fixture', async () => {
    const promise = new FixtureDataSource().readSnapshot('error');

    await expect(promise).rejects.toMatchObject({
      code: 'FIXTURE_FAILURE',
      message: 'The fixture Hub could not be read.',
    });
  });

  it('holds the loading fixture until the caller aborts it', async () => {
    const controller = new AbortController();
    const promise = new FixtureDataSource().readSnapshot(
      'loading',
      controller.signal,
    );

    controller.abort();

    await expect(promise).rejects.toMatchObject({ name: 'AbortError' });
  });

  it('discovers and pairs the redacted fixture Hub', async () => {
    const source = new FixtureDataSource();
    const hubs = await source.discover();

    expect(hubs).toEqual([
      expect.objectContaining({
        id: 'hub-redacted-1',
        displayName: 'Hawthorn Hub',
        endpoint: 'https://hub.fixture.invalid',
      }),
    ]);

    await expect(
      source.pair({
        hubId: 'hub-redacted-1',
        invitationCode: '482731',
        deviceName: 'Reference viewer',
      }),
    ).resolves.toMatchObject({
      deviceId: 'device-viewer',
      hubId: 'hub-redacted-1',
      pairedAt: '2026-08-30T09:55:00.000Z',
    });
  });

  it('rejects an invalid fixture invitation without creating a device', async () => {
    await expect(
      new FixtureDataSource().pair({
        hubId: 'hub-redacted-1',
        invitationCode: '000000',
        deviceName: 'Reference viewer',
      }),
    ).rejects.toMatchObject({ code: 'INVALID_INVITATION' });
  });

  it('removes one paired device without mutating other snapshots', async () => {
    const source = new FixtureDataSource();

    const updated = await source.removePairedDevice('device-home-assistant');
    const nextRead = await source.readSnapshot('complete');

    expect(updated.map((device) => device.id)).toEqual(['device-viewer']);
    expect(nextRead.devices.map((device) => device.id)).toEqual([
      'device-viewer',
      'device-home-assistant',
    ]);
  });

  it('pages an optional dataset at 25 rows and keeps vehicle groups independent', async () => {
    const firstVehicle = 'vehicle-redacted-1';
    const secondVehicle = 'vehicle-redacted-2';
    const drives = [
      ...Array.from({ length: 51 }, (_, index) =>
        drive(firstVehicle, `drive-${index + 1}`),
      ),
      drive(secondVehicle, 'drive-1'),
    ];
    const source = new FixtureDataSource({ drives });

    const first = await source.readSnapshot('complete');
    expect(first.drives.filter((item) => item.vehicleId === firstVehicle)).toHaveLength(25);
    expect(first.drivePaging[firstVehicle]).toMatchObject({
      hasMore: true,
      loadedCount: 25,
    });
    expect(first.drivePaging[secondVehicle]).toMatchObject({
      hasMore: false,
      loadedCount: 1,
    });

    const second = await source.loadMoreDrives(firstVehicle);
    expect(second.drives.filter((item) => item.vehicleId === firstVehicle)).toHaveLength(50);
    expect(second.drivePaging[firstVehicle]).toMatchObject({
      hasMore: true,
      loadedCount: 50,
    });

    const third = await source.loadMoreDrives(firstVehicle);
    expect(third.drives.filter((item) => item.vehicleId === firstVehicle)).toHaveLength(51);
    expect(third.drivePaging[firstVehicle]).toMatchObject({
      hasMore: false,
      loadedCount: 51,
    });
    expect(
      third.drives.filter((item) => item.vehicleId === secondVehicle),
    ).toEqual([drive(secondVehicle, 'drive-1')]);
  });

  it('preserves an empty page that still has continuation', async () => {
    const vehicleId = 'vehicle-redacted-1';
    const source = new FixtureDataSource({
      drivePages: {
        [vehicleId]: [[], [drive(vehicleId, 'after-empty')]],
      },
    });

    const first = await source.readSnapshot('complete');
    expect(first.drives.filter((item) => item.vehicleId === vehicleId)).toEqual([]);
    expect(first.drivePaging[vehicleId]).toMatchObject({
      hasMore: true,
      loadedCount: 0,
    });

    const second = await source.loadMoreDrives(vehicleId);
    expect(second.drives.filter((item) => item.vehicleId === vehicleId)).toHaveLength(1);
    expect(second.drivePaging[vehicleId]).toMatchObject({
      hasMore: false,
      loadedCount: 1,
    });
  });

  it('resets fixture continuation after a fresh read and logout', async () => {
    const vehicleId = 'vehicle-redacted-1';
    const source = new FixtureDataSource({
      drives: Array.from({ length: 26 }, (_, index) =>
        drive(vehicleId, `drive-${index}`),
      ),
    });

    await source.readSnapshot('complete');
    await source.loadMoreDrives(vehicleId);
    const refreshed = await source.readSnapshot('complete');
    expect(refreshed.drivePaging[vehicleId]).toMatchObject({
      hasMore: true,
      loadedCount: 25,
    });

    await source.logout();
    await expect(source.loadMoreDrives(vehicleId)).rejects.toMatchObject({
      code: 'FIXTURE_NOT_PAIRED',
    });
  });
});

describe('createDataSource', () => {
  it('selects the packaged SDK data source for live mode without fetching during construction', () => {
    const fetchSpy = vi.fn();
    vi.stubGlobal('fetch', fetchSpy);
    const source = createDataSource('live');

    expect(source).toBeInstanceOf(SdkDataSource);
    expect(fetchSpy).not.toHaveBeenCalled();

    vi.unstubAllGlobals();
  });
});
