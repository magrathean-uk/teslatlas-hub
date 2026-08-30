import type {
  DiscoveredHub,
  FixtureScenario,
  HubSnapshot,
} from './types';

export const FIXTURE_NOW = '2026-08-30T10:00:00.000Z';

export const discoveredFixtureHub: DiscoveredHub = {
  id: 'hub-redacted-1',
  displayName: 'Hawthorn Hub',
  endpoint: 'https://hub.fixture.invalid',
  identityFingerprint: 'SHA256:4A:89:71:03:DE:MO',
  protocolVersion: 'foundation-draft',
  status: 'healthy',
};

const completeSnapshot: HubSnapshot = {
  generatedAt: FIXTURE_NOW,
  hub: {
    ...discoveredFixtureHub,
    version: 'fixture-1.1.0',
    checkedAt: '2026-08-30T09:59:40.000Z',
    freshness: 'fresh',
    capabilities: [
      'current state',
      'session history',
      'data quality',
      'paired devices',
    ],
  },
  vehicles: [
    {
      id: 'vehicle-redacted-1',
      displayName: 'Northstar',
      state: 'online',
      visibility: 'full',
      updatedAt: '2026-08-30T09:59:12.000Z',
      freshness: 'fresh',
    },
    {
      id: 'vehicle-redacted-2',
      displayName: 'Juniper',
      state: 'asleep',
      visibility: 'limited',
      updatedAt: '2026-08-30T09:48:00.000Z',
      freshness: 'fresh',
    },
  ],
  currentByVehicle: {
    'vehicle-redacted-1': {
      vehicleId: 'vehicle-redacted-1',
      updatedAt: '2026-08-30T09:59:12.000Z',
      freshness: 'fresh',
      stateOfChargePercent: 68,
      estimatedRangeKm: 242,
      odometerKm: 58_214,
      locationLabel: 'Home',
      locked: true,
      insideTemperatureC: 20.5,
      outsideTemperatureC: 18,
      inferredFields: [],
    },
    'vehicle-redacted-2': {
      vehicleId: 'vehicle-redacted-2',
      updatedAt: '2026-08-30T09:48:00.000Z',
      freshness: 'fresh',
      stateOfChargePercent: 51,
      estimatedRangeKm: null,
      odometerKm: 31_804,
      locationLabel: null,
      locked: true,
      insideTemperatureC: null,
      outsideTemperatureC: 17.5,
      inferredFields: [],
    },
  },
  drives: [
    {
      id: 'drive-redacted-101',
      vehicleId: 'vehicle-redacted-1',
      startedAt: '2026-08-29T17:20:00.000Z',
      endedAt: '2026-08-29T17:52:00.000Z',
      startLabel: 'Town centre',
      endLabel: 'Home',
      distanceKm: 22.4,
      durationMinutes: 32,
      energyUsedKwh: 4.8,
      quality: {
        level: 'complete',
        sources: ['fleet telemetry', 'fleet api'],
        gapCount: 0,
        largestGapSeconds: 0,
        derivedFields: [],
      },
    },
    {
      id: 'drive-redacted-100',
      vehicleId: 'vehicle-redacted-2',
      startedAt: '2026-08-28T08:02:00.000Z',
      endedAt: '2026-08-28T08:39:00.000Z',
      startLabel: null,
      endLabel: 'Work',
      distanceKm: 29.1,
      durationMinutes: 37,
      energyUsedKwh: null,
      quality: {
        level: 'partial',
        sources: ['fleet telemetry'],
        gapCount: 0,
        largestGapSeconds: 0,
        derivedFields: [],
      },
    },
  ],
  charges: [
    {
      id: 'charge-redacted-51',
      vehicleId: 'vehicle-redacted-1',
      startedAt: '2026-08-29T21:10:00.000Z',
      endedAt: '2026-08-29T23:42:00.000Z',
      locationLabel: 'Home',
      energyAddedKwh: 18.6,
      startPercent: 42,
      endPercent: 71,
      quality: {
        level: 'complete',
        sources: ['fleet telemetry'],
        gapCount: 0,
        largestGapSeconds: 0,
        derivedFields: [],
      },
    },
    {
      id: 'charge-redacted-50',
      vehicleId: 'vehicle-redacted-2',
      startedAt: '2026-08-27T18:05:00.000Z',
      endedAt: '2026-08-27T19:17:00.000Z',
      locationLabel: null,
      energyAddedKwh: 11.2,
      startPercent: 30,
      endPercent: null,
      quality: {
        level: 'partial',
        sources: ['fleet api'],
        gapCount: 0,
        largestGapSeconds: 0,
        derivedFields: [],
      },
    },
  ],
  quality: {
    overall: 'complete',
    observedCoveragePercent: 99.8,
    generatedAt: '2026-08-30T09:59:45.000Z',
    gaps: [],
  },
  collectors: [
    {
      id: 'collector-telemetry',
      displayName: 'Fleet Telemetry',
      source: 'fleet telemetry',
      status: 'healthy',
      lastEventAt: '2026-08-30T09:59:12.000Z',
      lagSeconds: 48,
      detail: 'Receiving change-based vehicle signals.',
    },
    {
      id: 'collector-api',
      displayName: 'Fleet API',
      source: 'fleet api',
      status: 'healthy',
      lastEventAt: '2026-08-30T09:58:40.000Z',
      lagSeconds: 80,
      detail: 'Reconciliation queries are available.',
    },
  ],
  devices: [
    {
      id: 'device-viewer',
      displayName: 'Reference viewer',
      kind: 'browser',
      scopes: ['read telemetry', 'manage own pairing'],
      pairedAt: '2026-08-30T09:55:00.000Z',
      lastSeenAt: '2026-08-30T09:59:40.000Z',
      status: 'active',
    },
    {
      id: 'device-home-assistant',
      displayName: 'Home automation',
      kind: 'integration',
      scopes: ['read telemetry'],
      pairedAt: '2026-08-10T12:30:00.000Z',
      lastSeenAt: '2026-08-30T09:58:54.000Z',
      status: 'active',
    },
  ],
};

function cloneComplete(): HubSnapshot {
  return structuredClone(completeSnapshot);
}

export function createFixtureSnapshot(
  scenario: Exclude<FixtureScenario, 'error' | 'loading'>,
): HubSnapshot {
  const snapshot = cloneComplete();

  if (scenario === 'empty') {
    snapshot.vehicles = [];
    snapshot.currentByVehicle = {};
    snapshot.drives = [];
    snapshot.charges = [];
    snapshot.devices = [];
    snapshot.quality = {
      overall: 'unknown',
      observedCoveragePercent: null,
      generatedAt: FIXTURE_NOW,
      gaps: [],
    };
  }

  if (scenario === 'stale') {
    snapshot.hub.freshness = 'stale';
    snapshot.hub.checkedAt = '2026-08-30T06:00:00.000Z';
    snapshot.vehicles.forEach((vehicle) => {
      vehicle.freshness = 'stale';
      vehicle.updatedAt = '2026-08-30T06:00:00.000Z';
    });
    Object.values(snapshot.currentByVehicle).forEach((current) => {
      if (current) {
        current.freshness = 'stale';
        current.updatedAt = '2026-08-30T06:00:00.000Z';
      }
    });
    snapshot.collectors.forEach((collector) => {
      collector.status = 'degraded';
      collector.lastEventAt = '2026-08-30T06:00:00.000Z';
      collector.lagSeconds = 14_400;
      collector.detail = 'No recent events; last known data remains visible.';
    });
  }

  if (scenario === 'inferred') {
    const current = snapshot.currentByVehicle['vehicle-redacted-1'];
    if (current) {
      current.estimatedRangeKm = 238;
      current.inferredFields = ['estimatedRangeKm'];
    }
    snapshot.drives[0].quality.level = 'partial';
    snapshot.drives[0].quality.derivedFields = ['energyUsedKwh'];
  }

  if (scenario === 'degraded') {
    snapshot.hub.status = 'degraded';
    snapshot.quality.overall = 'degraded';
    snapshot.quality.observedCoveragePercent = 91.4;
    snapshot.quality.gaps = [
      {
        id: 'gap-redacted-1',
        vehicleId: 'vehicle-redacted-1',
        startedAt: '2026-08-29T17:31:00.000Z',
        endedAt: '2026-08-29T17:34:12.000Z',
        durationSeconds: 192,
        reason: 'Telemetry delivery paused.',
        status: 'open',
      },
      {
        id: 'gap-redacted-2',
        vehicleId: 'vehicle-redacted-2',
        startedAt: '2026-08-28T08:18:00.000Z',
        endedAt: '2026-08-28T08:19:25.000Z',
        durationSeconds: 85,
        reason: 'API reconciliation unavailable.',
        status: 'open',
      },
    ];
    snapshot.collectors[0].status = 'degraded';
    snapshot.collectors[0].detail = 'Events are arriving with intermittent gaps.';
    snapshot.drives[0].quality = {
      ...snapshot.drives[0].quality,
      level: 'degraded',
      gapCount: 1,
      largestGapSeconds: 192,
    };
  }

  if (scenario === 'offline') {
    snapshot.hub.status = 'offline';
    snapshot.hub.freshness = 'stale';
    snapshot.vehicles.forEach((vehicle) => {
      vehicle.state = 'offline';
      vehicle.freshness = 'stale';
    });
    Object.values(snapshot.currentByVehicle).forEach((current) => {
      if (current) {
        current.freshness = 'stale';
      }
    });
    snapshot.collectors.forEach((collector) => {
      collector.status = 'offline';
      collector.detail = 'Collector is offline; showing last known data.';
    });
  }

  return snapshot;
}
