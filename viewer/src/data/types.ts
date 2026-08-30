export type FixtureScenario =
  | 'complete'
  | 'empty'
  | 'stale'
  | 'inferred'
  | 'degraded'
  | 'offline'
  | 'error'
  | 'loading';

export type AppMode = 'fixture' | 'live';
export type Freshness = 'fresh' | 'stale' | 'unknown';
export type HubStatus = 'healthy' | 'degraded' | 'offline';
export type QualityLevel = 'complete' | 'partial' | 'degraded' | 'unknown';
export type CollectorStatus = 'healthy' | 'degraded' | 'offline';

export class ViewerDataError extends Error {
  readonly code: string;

  constructor(code: string, message: string) {
    super(message);
    this.name = 'ViewerDataError';
    this.code = code;
  }
}

export interface DiscoveredHub {
  id: string;
  displayName: string;
  endpoint: string;
  identityFingerprint: string;
  protocolVersion: string;
  status: HubStatus;
}

export interface PairingInput {
  hubId: string;
  invitationCode: string;
  deviceName: string;
}

export interface PairedHub {
  hubId: string;
  deviceId: string;
  pairedAt: string;
  identityFingerprint: string;
}

export interface HubSummary extends DiscoveredHub {
  version: string;
  checkedAt: string;
  freshness: Freshness;
  capabilities: string[];
}

export interface VehicleSummary {
  id: string;
  displayName: string;
  state: 'online' | 'asleep' | 'driving' | 'charging' | 'offline';
  visibility: 'full' | 'limited';
  updatedAt: string;
  freshness: Freshness;
}

export interface CurrentVehicleState {
  vehicleId: string;
  updatedAt: string;
  freshness: Freshness;
  stateOfChargePercent: number | null;
  estimatedRangeKm: number | null;
  odometerKm: number | null;
  locationLabel: string | null;
  locked: boolean | null;
  insideTemperatureC: number | null;
  outsideTemperatureC: number | null;
  inferredFields: string[];
}

export interface SessionQuality {
  level: QualityLevel;
  sources: string[];
  gapCount: number;
  largestGapSeconds: number;
  derivedFields: string[];
}

export interface DriveSummary {
  id: string;
  vehicleId: string;
  startedAt: string;
  endedAt: string;
  startLabel: string | null;
  endLabel: string | null;
  distanceKm: number;
  durationMinutes: number;
  energyUsedKwh: number | null;
  quality: SessionQuality;
}

export interface ChargeSummary {
  id: string;
  vehicleId: string;
  startedAt: string;
  endedAt: string;
  locationLabel: string | null;
  energyAddedKwh: number | null;
  startPercent: number | null;
  endPercent: number | null;
  quality: SessionQuality;
}

export interface DataGap {
  id: string;
  vehicleId: string;
  startedAt: string;
  endedAt: string;
  durationSeconds: number;
  reason: string;
  status: 'open' | 'resolved';
}

export interface DataQualitySummary {
  overall: QualityLevel;
  observedCoveragePercent: number | null;
  generatedAt: string;
  gaps: DataGap[];
}

export interface CollectorSummary {
  id: string;
  displayName: string;
  source: string;
  status: CollectorStatus;
  lastEventAt: string | null;
  lagSeconds: number | null;
  detail: string;
}

export interface PairedDevice {
  id: string;
  displayName: string;
  kind: string;
  scopes: string[];
  pairedAt: string;
  lastSeenAt: string | null;
  status: 'active' | 'inactive';
}

export interface HubSnapshot {
  generatedAt: string;
  hub: HubSummary;
  vehicles: VehicleSummary[];
  currentByVehicle: Record<string, CurrentVehicleState | null>;
  drives: DriveSummary[];
  charges: ChargeSummary[];
  quality: DataQualitySummary;
  collectors: CollectorSummary[];
  devices: PairedDevice[];
}

export interface ViewerDataSource {
  discover(signal?: AbortSignal): Promise<DiscoveredHub[]>;
  pair(input: PairingInput, signal?: AbortSignal): Promise<PairedHub>;
  readSnapshot(
    scenario: FixtureScenario,
    signal?: AbortSignal,
  ): Promise<HubSnapshot>;
  removePairedDevice(
    deviceId: string,
    signal?: AbortSignal,
  ): Promise<PairedDevice[]>;
}
