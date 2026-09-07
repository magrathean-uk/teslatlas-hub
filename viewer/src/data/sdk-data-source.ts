import {
  asStrongEntityTag,
  createHubClient,
  type CreateHubClientOptions,
  type HubClient,
  type HubCurrent,
  type HubDiscovery,
  type HubDrive,
  type HubInvitation,
} from '@teslatlas/sdk/browser';

import {
  ViewerDataError,
  type CurrentVehicleState,
  type DiscoveredHub,
  type DriveSummary,
  type FixtureScenario,
  type HubSnapshot,
  type LiveConnectionInput,
  type PairedDevice,
  type PairedHub,
  type PairingInput,
  type ResourceState,
  type VehicleSummary,
  type ViewerDataSource,
} from './types';

const DRIVE_PAGE_LIMIT = 2;
const MAX_DRIVE_PAGES = 3;

type HubClientFactory = (options: CreateHubClientOptions) => HubClient;

interface DrivePageCache {
  cursor: string | null;
  etag: string;
  items: DriveSummary[];
  nextCursor: string | null;
}

interface DriveCache {
  pages: DrivePageCache[];
}

const present = (): ResourceState => ({
  availability: 'present',
  retained: false,
  detail: null,
});

const unsupported = (detail: string): ResourceState => ({
  availability: 'unsupported',
  retained: false,
  detail,
});

const unavailable = (error: unknown, retained: boolean): ResourceState => ({
  availability: 'temporarily-unavailable',
  retained,
  detail:
    error instanceof Error && error.message.trim()
      ? error.message
      : 'The resource is temporarily unavailable.',
});

function hasRetainedResource(
  snapshot: HubSnapshot | null,
  resource: keyof HubSnapshot['resources'],
): boolean {
  const prior = snapshot?.resources[resource];
  return prior?.availability === 'present' || prior?.retained === true;
}

function isAuthorizationLoss(error: unknown): boolean {
  return (
    typeof error === 'object' &&
    error !== null &&
    'status' in error &&
    error.status === 401
  );
}

function abortError(): DOMException {
  return new DOMException('The operation was aborted.', 'AbortError');
}

function isoFromMilliseconds(value: number | null): string | null {
  return value === null ? null : new Date(value).toISOString();
}

function normaliseEndpoint(value: string): string {
  const endpoint = new URL(value);
  if (endpoint.protocol !== 'https:') {
    throw new ViewerDataError(
      'HTTPS_REQUIRED',
      'Live Hub endpoints must use HTTPS.',
    );
  }
  endpoint.hash = '';
  endpoint.search = '';
  endpoint.pathname = endpoint.pathname.replace(/\/+$/u, '') || '/';
  return endpoint.href.replace(/\/$/u, '');
}

function stringField(
  value: unknown,
  snakeName: string,
  camelName: string,
): string {
  if (typeof value !== 'object' || value === null) {
    throw new ViewerDataError(
      'INVALID_INVITATION',
      'The pairing invitation is not valid JSON invitation data.',
    );
  }
  const record = value as Record<string, unknown>;
  const field = record[snakeName] ?? record[camelName];
  if (typeof field !== 'string' || field.length === 0) {
    throw new ViewerDataError(
      'INVALID_INVITATION',
      'The pairing invitation is missing required fields.',
    );
  }
  return field;
}

function numberField(
  value: unknown,
  snakeName: string,
  camelName: string,
): number {
  const record = value as Record<string, unknown>;
  const field = record[snakeName] ?? record[camelName];
  if (typeof field !== 'number' || !Number.isSafeInteger(field)) {
    throw new ViewerDataError(
      'INVALID_INVITATION',
      'The pairing invitation is missing required fields.',
    );
  }
  return field;
}

export function parseHubInvitation(value: string): HubInvitation {
  let parsed: unknown;
  try {
    parsed = JSON.parse(value);
  } catch {
    throw new ViewerDataError(
      'INVALID_INVITATION',
      'The pairing invitation is not valid JSON invitation data.',
    );
  }
  return {
    pairingId: stringField(parsed, 'pairing_id', 'pairingId'),
    secret: stringField(parsed, 'secret', 'secret'),
    expiresAtMs: numberField(parsed, 'expires_at_ms', 'expiresAtMs'),
    endpoint: stringField(parsed, 'endpoint', 'endpoint'),
    tlsPin: stringField(parsed, 'tls_pin', 'tlsPin'),
    pairingUri: stringField(parsed, 'pairing_uri', 'pairingUri'),
  };
}

export function mapHubDiscovery(
  value: HubDiscovery,
  endpoint: string,
  tlsIdentity: string | null,
): DiscoveredHub {
  return {
    id: value.hubId,
    displayName: `Hub ${value.hubId}`,
    endpoint,
    manifestKey: value.manifestPublicKey ?? null,
    tlsIdentity,
    protocolVersion: `${value.protocol}/${value.protocolMajor}`,
    status: 'healthy',
  };
}

export function mapHubVehicle(value: {
  readonly vehicleId: string;
  readonly displayName: string | null;
}): VehicleSummary {
  return {
    id: value.vehicleId,
    displayName: value.displayName ?? `Vehicle ${value.vehicleId}`,
    state: 'unknown',
    visibility: 'unknown',
    updatedAt: null,
    freshness: 'unknown',
  };
}

export function mapHubCurrent(value: HubCurrent): CurrentVehicleState {
  return {
    vehicleId: value.vehicleId,
    updatedAt: isoFromMilliseconds(value.observedAtMs),
    freshness: 'unknown',
    stateOfChargePercent: value.batteryLevel,
    estimatedRangeKm: value.estBatteryRangeKm,
    ratedRangeKm: value.ratedBatteryRangeKm,
    odometerKm: value.odometer,
    locationLabel: value.geofence,
    locked: value.locked,
    insideTemperatureC: value.insideTemp,
    outsideTemperatureC: value.outsideTemp,
    inferredFields: [],
  };
}

export function mapHubDrive(value: HubDrive): DriveSummary {
  return {
    id: String(value.id),
    vehicleId: value.vehicleId,
    startedAt: new Date(value.startDateMs).toISOString(),
    endedAt: new Date(value.endDateMs).toISOString(),
    startLabel: value.startGeofence ?? value.startAddress,
    endLabel: value.endGeofence ?? value.endAddress,
    distanceKm: value.distanceKm,
    durationMinutes: value.durationMin,
    energyUsedKwh: null,
    quality: null,
  };
}

export class SdkDataSource implements ViewerDataSource {
  private readonly clientFactory: HubClientFactory;
  private client: HubClient | null = null;
  private connection: LiveConnectionInput | null = null;
  private credential:
    | { accessToken: string; deviceId: string; expiresAtMs: number }
    | undefined;
  private discoveryValue: HubDiscovery | null = null;
  private previousSnapshot: HubSnapshot | null = null;
  private readonly driveCache = new Map<string, DriveCache>();
  private generation = 0;
  private readonly activeReads = new Set<AbortController>();

  constructor(clientFactory: HubClientFactory = createHubClient) {
    this.clientFactory = clientFactory;
  }

  configure(input: LiveConnectionInput): void {
    const next: LiveConnectionInput = {
      endpoint: normaliseEndpoint(input.endpoint),
      expectedHubId: input.expectedHubId,
      tlsIdentity: input.tlsIdentity,
    };
    if (
      this.connection?.endpoint === next.endpoint &&
      this.connection.expectedHubId === next.expectedHubId &&
      this.connection.tlsIdentity === next.tlsIdentity
    ) {
      return;
    }
    this.invalidate();
    this.client?.dispose();
    this.client = null;
    this.connection = next;
    this.credential = undefined;
    this.discoveryValue = null;
    this.previousSnapshot = null;
    this.driveCache.clear();
  }

  async discover(signal?: AbortSignal): Promise<DiscoveredHub[]> {
    const { client, generation, controller, release } = this.beginRead(signal);
    try {
      const response = await client.discover({ signal: controller.signal });
      this.assertCurrent(generation, controller.signal);
      this.discoveryValue = response.value;
      const connection = this.requireConnection();
      return [
        mapHubDiscovery(
          response.value,
          connection.endpoint,
          connection.tlsIdentity,
        ),
      ];
    } finally {
      release();
    }
  }

  async pair(input: PairingInput, signal?: AbortSignal): Promise<PairedHub> {
    const connection = this.requireConnection();
    if (input.hubId !== connection.expectedHubId) {
      throw new ViewerDataError(
        'HUB_IDENTITY_MISMATCH',
        'The selected Hub identity does not match the configured Hub.',
      );
    }
    if (!input.deviceName.trim()) {
      throw new ViewerDataError(
        'DEVICE_NAME_REQUIRED',
        'Enter a name for this viewer.',
      );
    }
    const invitation = parseHubInvitation(input.invitationCode);
    if (normaliseEndpoint(invitation.endpoint) !== connection.endpoint) {
      throw new ViewerDataError(
        'INVITATION_ENDPOINT_MISMATCH',
        'The invitation belongs to a different Hub endpoint.',
      );
    }
    if (
      connection.tlsIdentity !== null &&
      invitation.tlsPin !== connection.tlsIdentity
    ) {
      throw new ViewerDataError(
        'INVITATION_TLS_MISMATCH',
        'The invitation TLS identity does not match the configured identity.',
      );
    }
    const { client, generation, controller, release } = this.beginRead(signal);
    try {
      if (this.discoveryValue === null) {
        const discovered = await client.discover({ signal: controller.signal });
        this.assertCurrent(generation, controller.signal);
        this.discoveryValue = discovered.value;
      }
      const claimed = await client.claimPairing(invitation, input.deviceName, {
        signal: controller.signal,
      });
      this.assertCurrent(generation, controller.signal);
      const discovered = this.discoveryValue;
      return {
        hubId: connection.expectedHubId,
        deviceId: claimed.value.deviceId,
        pairedAt: new Date().toISOString(),
        manifestKey: discovered?.manifestPublicKey ?? null,
        tlsIdentity: invitation.tlsPin,
      };
    } finally {
      release();
    }
  }

  async readSnapshot(
    _scenario: FixtureScenario,
    signal?: AbortSignal,
  ): Promise<HubSnapshot> {
    const { client, generation, controller, release } = this.beginRead(signal);
    try {
      if (this.discoveryValue === null) {
        const discovered = await client.discover({ signal: controller.signal });
        this.assertCurrent(generation, controller.signal);
        this.discoveryValue = discovered.value;
      }

      const request = { signal: controller.signal };
      const [healthResult, readinessResult, vehiclesResult] =
        await Promise.allSettled([
          client.health(request),
          client.readiness(request),
          client.vehicles(request),
        ]);
      this.assertCurrent(generation, controller.signal);

      if (
        [healthResult, readinessResult, vehiclesResult].some(
          (result) =>
            result.status === 'rejected' && isAuthorizationLoss(result.reason),
        )
      ) {
        await this.logout();
        throw new ViewerDataError(
          'AUTH_LOST',
          'Hub authorization was lost. Pair this viewer again.',
        );
      }

      const previous = this.previousSnapshot;
      const healthState =
        healthResult.status === 'fulfilled'
          ? present()
          : unavailable(
              healthResult.reason,
              hasRetainedResource(previous, 'health'),
            );
      const readinessState =
        readinessResult.status === 'fulfilled'
          ? present()
          : unavailable(
              readinessResult.reason,
              hasRetainedResource(previous, 'readiness'),
            );
      const vehiclesState =
        vehiclesResult.status === 'fulfilled'
          ? present()
          : unavailable(
              vehiclesResult.reason,
              hasRetainedResource(previous, 'vehicles'),
            );

      const vehicles =
        vehiclesResult.status === 'fulfilled'
          ? vehiclesResult.value.value.vehicles.map(mapHubVehicle)
          : (previous?.vehicles ?? []).map((vehicle) => ({
              ...vehicle,
              freshness: 'stale' as const,
            }));

      const currentByVehicle: Record<string, CurrentVehicleState | null> = {};
      const currentFailures: unknown[] = [];
      const currentReads = await Promise.allSettled(
        vehicles.map(async (vehicle) => {
          const response = await client.current(vehicle.id, request);
          return [vehicle.id, mapHubCurrent(response.value)] as const;
        }),
      );
      this.assertCurrent(generation, controller.signal);
      if (
        currentReads.some(
          (result) =>
            result.status === 'rejected' && isAuthorizationLoss(result.reason),
        )
      ) {
        await this.logout();
        throw new ViewerDataError(
          'AUTH_LOST',
          'Hub authorization was lost. Pair this viewer again.',
        );
      }
      currentReads.forEach((result, index) => {
        const vehicleId = vehicles[index].id;
        if (result.status === 'fulfilled') {
          currentByVehicle[result.value[0]] = result.value[1];
        } else {
          currentFailures.push(result.reason);
          const retained = previous?.currentByVehicle[vehicleId];
          currentByVehicle[vehicleId] =
            retained === undefined || retained === null
              ? null
              : { ...retained, freshness: 'stale' };
        }
      });
      const currentState =
        vehiclesResult.status === 'rejected' && vehicles.length === 0
          ? unavailable(
              vehiclesResult.reason,
              hasRetainedResource(previous, 'current'),
            )
          : currentFailures.length === 0
          ? present()
          : unavailable(
              currentFailures[0],
              hasRetainedResource(previous, 'current') &&
                Object.values(currentByVehicle).some((value) => value !== null),
            );

      let drives: DriveSummary[] = [];
      let drivesState: ResourceState;
      if (!(this.discoveryValue.capabilities as readonly string[]).includes('query.drives')) {
        drivesState = unsupported('This Hub does not advertise drive queries.');
      } else if (vehiclesResult.status === 'rejected' && vehicles.length === 0) {
        drivesState = unavailable(
          vehiclesResult.reason,
          hasRetainedResource(previous, 'drives'),
        );
      } else {
        const driveResults = await Promise.allSettled(
          vehicles.map((vehicle) =>
            this.readDrivePages(client, vehicle.id, controller.signal),
          ),
        );
        this.assertCurrent(generation, controller.signal);
        if (
          driveResults.some(
            (result) =>
              result.status === 'rejected' && isAuthorizationLoss(result.reason),
          )
        ) {
          await this.logout();
          throw new ViewerDataError(
            'AUTH_LOST',
            'Hub authorization was lost. Pair this viewer again.',
          );
        }
        const failures = driveResults.filter(
          (result): result is PromiseRejectedResult => result.status === 'rejected',
        );
        drives = driveResults.flatMap((result, index) => {
          if (result.status === 'fulfilled') return result.value;
          return (previous?.drives ?? []).filter(
            (drive) => drive.vehicleId === vehicles[index].id,
          );
        });
        drivesState =
          failures.length === 0
            ? present()
            : unavailable(
                failures[0].reason,
                hasRetainedResource(previous, 'drives') &&
                  drives.some((drive) =>
                    (previous?.drives ?? []).some(
                      (oldDrive) => oldDrive.id === drive.id,
                    ),
                  ),
              );
      }

      const connection = this.requireConnection();
      const discoveryMapped = mapHubDiscovery(
        this.discoveryValue,
        connection.endpoint,
        connection.tlsIdentity,
      );
      const health =
        healthResult.status === 'fulfilled' ? healthResult.value.value : null;
      const readiness =
        readinessResult.status === 'fulfilled'
          ? readinessResult.value.value
          : null;
      const healthRetained = healthResult.status === 'rejected';
      const retainedReadiness =
        readinessState.retained && previous !== null
          ? previous.hub.readiness
          : 'unknown';
      const retainedReadinessReason =
        readinessState.retained && previous !== null
          ? previous.hub.readinessReason
          : null;
      const snapshot: HubSnapshot = {
        generatedAt: new Date().toISOString(),
        hub: {
          ...discoveryMapped,
          status:
            health === null
              ? 'offline'
              : readiness?.status === 'not_ready'
                ? 'degraded'
                : 'healthy',
          version: health?.version ?? previous?.hub.version ?? this.discoveryValue.version,
          checkedAt: new Date().toISOString(),
          freshness: healthRetained ? 'stale' : 'unknown',
          capabilities: [...this.discoveryValue.capabilities],
          readiness:
            readiness?.status === 'ready'
              ? 'ready'
              : readiness?.status === 'not_ready'
                ? 'not-ready'
                : retainedReadiness,
          readinessReason:
            readiness?.status === 'not_ready'
              ? readiness.reason
              : readiness?.status === 'ready'
                ? null
                : retainedReadinessReason,
        },
        vehicles,
        currentByVehicle,
        drives,
        charges: [],
        quality: null,
        collectors: [],
        devices: [],
        resources: {
          health: healthState,
          readiness: readinessState,
          vehicles: vehiclesState,
          current: currentState,
          drives: drivesState,
          charges: unsupported('Charge sessions are unsupported by hub-http-v1.'),
          quality: unsupported('Data quality is unsupported by hub-http-v1.'),
          collectors: unsupported(
            'Collector cost and backup age are unsupported by hub-http-v1.',
          ),
          devices: unsupported(
            'Remote paired-device management is unsupported by hub-http-v1.',
          ),
        },
      };
      this.assertCurrent(generation, controller.signal);
      this.previousSnapshot = snapshot;
      return snapshot;
    } finally {
      release();
    }
  }

  async removePairedDevice(
    _deviceId: string,
    _signal?: AbortSignal,
  ): Promise<PairedDevice[]> {
    throw new ViewerDataError(
      'UNSUPPORTED_RESOURCE',
      'Remote paired-device management is unsupported by hub-http-v1.',
    );
  }

  async logout(): Promise<void> {
    const client = this.client;
    this.invalidate();
    this.previousSnapshot = null;
    this.discoveryValue = null;
    this.driveCache.clear();
    this.credential = undefined;
    if (client !== null) {
      await client.logout();
      client.dispose();
      if (this.client === client) this.client = null;
    }
  }

  private async readDrivePages(
    client: HubClient,
    vehicleId: string,
    signal: AbortSignal,
  ): Promise<DriveSummary[]> {
    const cached = this.driveCache.get(vehicleId);
    const pages: DrivePageCache[] = [];
    const items: DriveSummary[] = [];
    let cursor: string | null = null;

    for (let page = 0; page < MAX_DRIVE_PAGES; page += 1) {
      const cachedPage = cached?.pages[page];
      const matchingCache: DrivePageCache | undefined =
        cachedPage !== undefined && cachedPage.cursor === cursor
          ? cachedPage
          : undefined;
      const response = await client.drives(vehicleId, {
        ...(cursor === null ? {} : { cursor }),
        limit: DRIVE_PAGE_LIMIT,
        signal,
        ...(matchingCache === undefined
          ? {}
          : { ifNoneMatch: asStrongEntityTag(matchingCache.etag) }),
      });
      if (response.kind === 'notModified') {
        if (matchingCache === undefined) {
          throw new ViewerDataError(
            'INVALID_ETAG_REPLAY',
            'The Hub returned not modified without matching retained drive data.',
          );
        }
        pages.push(matchingCache);
        items.push(...matchingCache.items);
        cursor = matchingCache.nextCursor;
      } else {
        const pageItems = response.value.items.map(mapHubDrive);
        const pageCache: DrivePageCache = {
          cursor,
          etag: response.metadata.etag,
          items: pageItems,
          nextCursor: response.value.nextCursor,
        };
        pages.push(pageCache);
        items.push(...pageItems);
        cursor = pageCache.nextCursor;
      }
      if (cursor === null) break;
    }
    this.driveCache.set(vehicleId, { pages });
    return items;
  }

  private beginRead(signal?: AbortSignal): {
    client: HubClient;
    generation: number;
    controller: AbortController;
    release: () => void;
  } {
    const controller = new AbortController();
    const abort = () => controller.abort();
    if (signal?.aborted) controller.abort();
    else signal?.addEventListener('abort', abort, { once: true });
    this.activeReads.add(controller);
    const client = this.requireClient();
    const generation = this.generation;
    return {
      client,
      generation,
      controller,
      release: () => {
        signal?.removeEventListener('abort', abort);
        this.activeReads.delete(controller);
      },
    };
  }

  private requireConnection(): LiveConnectionInput {
    if (this.connection === null) {
      throw new ViewerDataError(
        'LIVE_CONNECTION_REQUIRED',
        'Enter the Hub endpoint and expected Hub UUID first.',
      );
    }
    return this.connection;
  }

  private requireClient(): HubClient {
    const connection = this.requireConnection();
    if (this.client === null) {
      this.client = this.clientFactory({
        endpoint: connection.endpoint,
        expectedHubId: connection.expectedHubId,
        credentials: {
          load: () => this.credential,
          save: (credential) => {
            this.credential = credential;
          },
          clear: () => {
            this.credential = undefined;
          },
        },
      });
    }
    return this.client;
  }

  private assertCurrent(generation: number, signal: AbortSignal): void {
    if (signal.aborted || generation !== this.generation) throw abortError();
  }

  private invalidate(): void {
    this.generation += 1;
    this.activeReads.forEach((controller) => controller.abort());
    this.activeReads.clear();
  }
}
