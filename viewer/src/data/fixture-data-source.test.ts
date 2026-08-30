import { describe, expect, it, vi } from 'vitest';

import { createDataSource } from './create-data-source';
import { FixtureDataSource } from './fixture-data-source';
import { ViewerDataError } from './types';

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

    expect(snapshot.quality.overall).toBe('degraded');
    expect(snapshot.quality.gaps).toHaveLength(2);
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
});

describe('createDataSource', () => {
  it('keeps live mode unavailable until a released SDK exists and never fetches', async () => {
    const fetchSpy = vi.fn();
    vi.stubGlobal('fetch', fetchSpy);
    const source = createDataSource('live');

    await expect(source.discover()).rejects.toEqual(
      new ViewerDataError(
        'SDK_NOT_RELEASED',
        'Live Hub access needs a released Teslatlas TypeScript SDK.',
      ),
    );
    expect(fetchSpy).not.toHaveBeenCalled();

    vi.unstubAllGlobals();
  });
});
