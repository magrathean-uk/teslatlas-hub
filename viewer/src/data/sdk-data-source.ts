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
  type DrivePagingState,
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

const DRIVE_PAGE_LIMIT = 25;

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

interface DriveReadResult {
  items: DriveSummary[];
  cache: DriveCache;
  paging: DrivePagingState;
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

function errorCode(error: unknown): string | null {
  if (typeof error !== 'object' || error === null || !('code' in error)) {
    return null;
  }
  return typeof error.code === 'string' ? error.code : null;
}

function isHistoryContinuationReset(error: unknown): boolean {
  if (
    typeof error === 'object' &&
    error !== null &&
    'status' in error &&
    (error.status === 403 || error.status === 404)
  ) {
    return true;
  }
  return new Set([
    'HISTORY_CHANGED',
    'INVALID_ETAG_REPLAY',
    'invalid_cursor',
    'cursor_expired',
    'cursor_query_mismatch',
    'cursor_scope_changed',
    'vehicle_not_found',
    'unsupported_method',
    'missing_capability',
  ]).has(errorCode(error) ?? '');
}

function driveKey(drive: DriveSummary): string {
  return `${drive.vehicleId}:${drive.id}`;
}

function replaceVehicleDrives(
  drives: DriveSummary[],
  vehicleId: string,
  replacement: DriveSummary[],
): DriveSummary[] {
  const firstIndex = drives.findIndex((drive) => drive.vehicleId === vehicleId);
  if (firstIndex < 0) return [...drives, ...replacement];
  const result: DriveSummary[] = [];
  let inserted = false;
  drives.forEach((drive, index) => {
    if (drive.vehicleId === vehicleId) {
      if (!inserted && index === firstIndex) {
        result.push(...replacement);
        inserted = true;
      }
      return;
    }
    result.push(drive);
  });
  return result;
}

function abortError(): DOMException {
  return new DOMException('The operation was aborted.', 'AbortError');
}

function isoFromMilliseconds(value: number | null): string | null {
  return value === null ? null : new Date(value).toISOString();
}

function dedupeDrives(items: DriveSummary[]): DriveSummary[] {
  const seen = new Set<string>();
  return items.filter((item) => {
    const key = `${item.vehicleId}:${item.id}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function normaliseEndpoint(value: string): string {
  const endpoint = new URL(value.trim());
  if (endpoint.protocol !== 'https:') {
    throw new ViewerDataError(
      'HTTPS_REQUIRED',
      'Live Hub endpoints must use HTTPS.',
    );
  }
  if (
    endpoint.username.length > 0 ||
    endpoint.password.length > 0 ||
    endpoint.pathname !== '/' ||
    endpoint.search.length > 0 ||
    endpoint.hash.length > 0
  ) {
    throw new ViewerDataError(
      'INVALID_ENDPOINT',
      'Live Hub endpoints must be a root HTTPS origin without a path, query, or fragment.',
    );
  }
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
  private acceptedTlsIdentity: string | null = null;
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
    this.acceptedTlsIdentity = null;
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
          this.acceptedTlsIdentity ?? connection.tlsIdentity,
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
      this.acceptedTlsIdentity = invitation.tlsPin;
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
      const drivePaging: Record<string, DrivePagingState> = {};
      const driveCaches = new Map<string, DriveCache>();
      let drivesState: ResourceState;
      if (!(this.discoveryValue.capabilities as readonly string[]).includes('query.drives')) {
        drivesState = unsupported('This Hub does not advertise drive queries.');
        vehicles.forEach((vehicle) => {
          drivePaging[vehicle.id] = {
            resource: drivesState,
            hasMore: null,
            loadedCount: 0,
          };
        });
      } else if (vehiclesResult.status === 'rejected' && vehicles.length === 0) {
        drivesState = unavailable(
          vehiclesResult.reason,
          hasRetainedResource(previous, 'drives'),
        );
        drives = previous?.drives ?? [];
        Object.assign(drivePaging, previous?.drivePaging ?? {});
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
          if (result.status === 'fulfilled') {
            const value = result.value;
            driveCaches.set(vehicles[index].id, value.cache);
            drivePaging[vehicles[index].id] = value.paging;
            return value.items;
          }
          return (previous?.drives ?? []).filter(
            (drive) => drive.vehicleId === vehicles[index].id,
          );
        });
        driveResults.forEach((result, index) => {
          if (result.status === 'rejected') {
            const prior = previous?.drivePaging[vehicles[index].id];
            drivePaging[vehicles[index].id] = {
              resource: unavailable(
                result.reason,
                prior?.loadedCount !== undefined && prior.loadedCount > 0,
              ),
              hasMore: null,
              loadedCount: prior?.loadedCount ?? 0,
            };
          }
        });
        const previousDriveKeys = new Set(
          (previous?.drives ?? []).map((drive) => driveKey(drive)),
        );
        drivesState =
          failures.length === 0
            ? present()
            : unavailable(
                failures[0].reason,
                hasRetainedResource(previous, 'drives') &&
                  drives.some((drive) => previousDriveKeys.has(driveKey(drive))),
              );
      }

      const connection = this.requireConnection();
      const discoveryMapped = mapHubDiscovery(
        this.discoveryValue,
        connection.endpoint,
        this.acceptedTlsIdentity ?? connection.tlsIdentity,
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
        drivePaging,
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
      const nextDriveCache = new Map(this.driveCache);
      if (vehiclesResult.status === 'fulfilled') {
        const currentVehicleIds = new Set(vehicles.map((vehicle) => vehicle.id));
        for (const vehicleId of nextDriveCache.keys()) {
          if (!currentVehicleIds.has(vehicleId)) nextDriveCache.delete(vehicleId);
        }
      }
      driveCaches.forEach((cache, vehicleId) => nextDriveCache.set(vehicleId, cache));
      this.assertCurrent(generation, controller.signal);
      this.driveCache.clear();
      nextDriveCache.forEach((cache, vehicleId) => this.driveCache.set(vehicleId, cache));
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

  async loadMoreDrives(
    vehicleId: string,
    signal?: AbortSignal,
  ): Promise<HubSnapshot> {
    const existing = this.previousSnapshot;
    if (existing === null) {
      throw new ViewerDataError(
        'LIVE_CONNECTION_REQUIRED',
        'Read the Hub before loading more drive history.',
      );
    }
    const paging = existing.drivePaging[vehicleId];
    if (paging === undefined || paging.resource.availability === 'unsupported') {
      return existing;
    }
    if (paging.hasMore === false || paging.hasMore === null) {
      return existing;
    }
    const cached = this.driveCache.get(vehicleId);
    const lastPage = cached?.pages[cached.pages.length - 1];
    if (cached === undefined || lastPage === undefined || lastPage.nextCursor === null) {
      return existing;
    }

    const { client, generation, controller, release } = this.beginRead(signal);
    try {
      const response = await client.drives(vehicleId, {
        cursor: lastPage.nextCursor,
        limit: DRIVE_PAGE_LIMIT,
        signal: controller.signal,
      });
      this.assertCurrent(generation, controller.signal);
      if (response.kind === 'notModified') {
        throw new ViewerDataError(
          'INVALID_ETAG_REPLAY',
          'The Hub returned not modified for an uncached history page.',
        );
      }
      const nextCursor = response.value.nextCursor;
      const priorCursors = new Set(
        cached.pages.flatMap((page) => [page.cursor, page.nextCursor]),
      );
      if (nextCursor !== null && priorCursors.has(nextCursor)) {
        throw new ViewerDataError(
          'HISTORY_CHANGED',
          'History changed; refresh to continue.',
        );
      }
      const page: DrivePageCache = {
        cursor: lastPage.nextCursor,
        etag: response.metadata.etag,
        items: response.value.items.map(mapHubDrive),
        nextCursor,
      };
      const nextCache: DriveCache = { pages: [...cached.pages, page] };
      const allItems = dedupeDrives(nextCache.pages.flatMap(({ items }) => items));
      const nextPaging: DrivePagingState = {
        resource: present(),
        hasMore: nextCursor !== null,
        loadedCount: allItems.length,
      };
      this.assertCurrent(generation, controller.signal);
      this.driveCache.set(vehicleId, nextCache);
      const snapshot: HubSnapshot = {
        ...existing,
        generatedAt: new Date().toISOString(),
        drives: replaceVehicleDrives(existing.drives, vehicleId, allItems),
        drivePaging: { ...existing.drivePaging, [vehicleId]: nextPaging },
      };
      this.previousSnapshot = snapshot;
      return snapshot;
    } catch (error) {
      if (isAuthorizationLoss(error)) {
        await this.logout();
        throw new ViewerDataError(
          'AUTH_LOST',
          'Hub authorization was lost. Pair this viewer again.',
        );
      }
      if (isHistoryContinuationReset(error)) {
        this.assertCurrent(generation, controller.signal);
        const blocked = this.blockDriveContinuation(existing, vehicleId, error);
        this.previousSnapshot = blocked;
        return blocked;
      }
      throw error;
    } finally {
      release();
    }
  }

  async logout(): Promise<void> {
    const client = this.client;
    this.invalidate();
    this.previousSnapshot = null;
    this.discoveryValue = null;
    this.acceptedTlsIdentity = null;
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
  ): Promise<DriveReadResult> {
    const cached = this.driveCache.get(vehicleId);
    const pages: DrivePageCache[] = [];
    let items: DriveSummary[] = [];
    let cursor: string | null = null;

    const pageCount = Math.max(cached?.pages.length ?? 0, 1);
    for (let page = 0; page < pageCount; page += 1) {
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
        items = dedupeDrives([...items, ...matchingCache.items]);
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
        items = dedupeDrives([...items, ...pageItems]);
        cursor = pageCache.nextCursor;
      }
      if (cursor === null) break;
      const knownCursors = new Set(
        pages
          .slice(0, -1)
          .flatMap((entry) => [entry.cursor, entry.nextCursor]),
      );
      if (knownCursors.has(cursor)) {
        throw new ViewerDataError(
          'HISTORY_CHANGED',
          'History changed; refresh to continue.',
        );
      }
    }
    const cache: DriveCache = { pages };
    return {
      items,
      cache,
      paging: {
        resource: present(),
        hasMore: cursor !== null,
        loadedCount: items.length,
      },
    };
  }

  private blockDriveContinuation(
    snapshot: HubSnapshot,
    vehicleId: string,
    error: unknown,
  ): HubSnapshot {
    const paging = snapshot.drivePaging[vehicleId];
    if (paging === undefined) return snapshot;
    return {
      ...snapshot,
      generatedAt: new Date().toISOString(),
      drivePaging: {
        ...snapshot.drivePaging,
        [vehicleId]: {
          ...paging,
          resource: unavailable(error, paging.loadedCount > 0),
          hasMore: null,
        },
      },
    };
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
      const clientGeneration = this.generation;
      this.client = this.clientFactory({
        endpoint: connection.endpoint,
        expectedHubId: connection.expectedHubId,
        credentials: {
          load: () =>
            this.generation === clientGeneration ? this.credential : undefined,
          save: (credential) => {
            if (this.generation === clientGeneration) this.credential = credential;
          },
          clear: () => {
            if (this.generation === clientGeneration) this.credential = undefined;
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
