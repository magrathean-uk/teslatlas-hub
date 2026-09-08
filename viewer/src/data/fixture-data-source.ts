import {
  createFixtureSnapshot,
  discoveredFixtureHub,
} from './fixture-data';
import {
  ViewerDataError,
  type DiscoveredHub,
  type DriveSummary,
  type FixtureScenario,
  type HubSnapshot,
  type PairedDevice,
  type PairedHub,
  type PairingInput,
  type VehicleSummary,
  type ViewerDataSource,
} from './types';

/** A page is deliberately allowed to be empty so fixtures can exercise a
 * continuation whose next page contains the first visible record. */
export type FixtureDrivePage = readonly DriveSummary[];

export interface FixtureDataSourceOptions {
  /** A flat drive set is split into pages using `pageSize` (25 by default). */
  drives?: readonly DriveSummary[];
  /** Explicit pages preserve empty pages and per-vehicle continuation. */
  drivePages?: Readonly<Record<string, readonly FixtureDrivePage[]>>;
  pageSize?: number;
}

export type FixtureDataset =
  | FixtureDataSourceOptions
  | readonly DriveSummary[]
  | Readonly<Record<string, readonly DriveSummary[] | readonly FixtureDrivePage[]>>;

type NormalisedDataset = ReadonlyMap<string, readonly FixtureDrivePage[]>;

function throwIfAborted(signal?: AbortSignal): void {
  if (signal?.aborted) {
    throw new DOMException('The operation was aborted.', 'AbortError');
  }
}

function clone<T>(value: T): T {
  return structuredClone(value);
}

function driveKey(drive: DriveSummary): string {
  return `${drive.vehicleId}:${drive.id}`;
}

function uniqueDrives(drives: readonly DriveSummary[]): DriveSummary[] {
  const seen = new Set<string>();
  return drives.filter((drive) => {
    const key = driveKey(drive);
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function chunk<T>(items: readonly T[], pageSize: number): T[][] {
  const pages: T[][] = [];
  for (let offset = 0; offset < items.length; offset += pageSize) {
    pages.push([...items.slice(offset, offset + pageSize)]);
  }
  return pages.length > 0 ? pages : [[]];
}

function isDrive(value: unknown): value is DriveSummary {
  return (
    typeof value === 'object' &&
    value !== null &&
    'vehicleId' in value &&
    'id' in value
  );
}

function normalisePages(
  entries: Readonly<Record<string, readonly FixtureDrivePage[]>>,
): Map<string, readonly FixtureDrivePage[]> {
  return new Map(
    Object.entries(entries).map(([vehicleId, pages]) => [
      vehicleId,
      pages.map((page) => [...page]),
    ]),
  );
}

function normaliseDataset(
  dataset: FixtureDataset | undefined,
): NormalisedDataset | null {
  if (dataset === undefined) return null;

  if (Array.isArray(dataset)) {
    const pageSize = 25;
    const grouped = new Map<string, DriveSummary[]>();
    dataset.forEach((drive) => {
      const list = grouped.get(drive.vehicleId) ?? [];
      list.push(drive);
      grouped.set(drive.vehicleId, list);
    });
    return new Map(
      [...grouped.entries()].map(([vehicleId, drives]) => [
        vehicleId,
        chunk(drives, pageSize),
      ]),
    );
  }

  const record = dataset as Record<string, unknown>;
  const configuredPageSize = record.pageSize;
  const pageSize =
    typeof configuredPageSize === 'number' &&
    Number.isInteger(configuredPageSize) &&
    configuredPageSize > 0
      ? configuredPageSize
      : 25;

  if (record.drivePages !== undefined) {
    return normalisePages(
      record.drivePages as Readonly<Record<string, readonly FixtureDrivePage[]>>,
    );
  }

  if (Array.isArray(record.drives) && record.drives.every(isDrive)) {
    const grouped = new Map<string, DriveSummary[]>();
    (record.drives as readonly DriveSummary[]).forEach((drive) => {
      const list = grouped.get(drive.vehicleId) ?? [];
      list.push(drive);
      grouped.set(drive.vehicleId, list);
    });
    return new Map(
      [...grouped.entries()].map(([vehicleId, drives]) => [
        vehicleId,
        chunk(drives, pageSize),
      ]),
    );
  }

  // Accept a compact `{ vehicleId: [drives] }` shape as a convenience for
  // tests. A nested array means explicit pages and is kept verbatim.
  const byVehicle = new Map<string, readonly FixtureDrivePage[]>();
  Object.entries(record).forEach(([vehicleId, value]) => {
    if (!Array.isArray(value) || vehicleId === 'pageSize') return;
    if (value.length === 0 || value.every(isDrive)) {
      byVehicle.set(vehicleId, [
        ...chunk(value as readonly DriveSummary[], pageSize),
      ]);
      return;
    }
    if (
      value.every(
        (page) => Array.isArray(page) && page.every(isDrive),
      )
    ) {
      byVehicle.set(
        vehicleId,
        (value as readonly FixtureDrivePage[]).map((page) => [...page]),
      );
    }
  });
  return byVehicle;
}

function syntheticVehicle(vehicleId: string): VehicleSummary {
  return {
    id: vehicleId,
    displayName: `Vehicle ${vehicleId}`,
    state: 'unknown',
    visibility: 'unknown',
    updatedAt: null,
    freshness: 'unknown',
  };
}

export class FixtureDataSource implements ViewerDataSource {
  private readonly dataset: NormalisedDataset | null;
  private snapshot: HubSnapshot | null = null;
  private readonly pageIndexes = new Map<string, number>();

  constructor(dataset?: FixtureDataset) {
    this.dataset = normaliseDataset(dataset);
  }

  async discover(signal?: AbortSignal): Promise<DiscoveredHub[]> {
    throwIfAborted(signal);
    return clone([discoveredFixtureHub]);
  }

  async pair(input: PairingInput, signal?: AbortSignal): Promise<PairedHub> {
    throwIfAborted(signal);

    if (
      input.hubId !== discoveredFixtureHub.id ||
      input.invitationCode !== '482731'
    ) {
      throw new ViewerDataError(
        'INVALID_INVITATION',
        'That fixture invitation is not valid.',
      );
    }

    if (!input.deviceName.trim()) {
      throw new ViewerDataError(
        'DEVICE_NAME_REQUIRED',
        'Enter a name for this viewer.',
      );
    }

    return {
      hubId: discoveredFixtureHub.id,
      deviceId: 'device-viewer',
      pairedAt: '2026-08-30T09:55:00.000Z',
      manifestKey: discoveredFixtureHub.manifestKey,
      tlsIdentity: discoveredFixtureHub.tlsIdentity,
    };
  }

  async readSnapshot(
    scenario: FixtureScenario,
    signal?: AbortSignal,
  ): Promise<HubSnapshot> {
    throwIfAborted(signal);
    this.pageIndexes.clear();

    if (scenario === 'error') {
      this.snapshot = null;
      throw new ViewerDataError(
        'FIXTURE_FAILURE',
        'The fixture Hub could not be read.',
      );
    }

    if (scenario === 'loading') {
      this.snapshot = null;
      return new Promise<HubSnapshot>((_resolve, reject) => {
        signal?.addEventListener(
          'abort',
          () => reject(new DOMException('The operation was aborted.', 'AbortError')),
          { once: true },
        );
      });
    }

    let snapshot = createFixtureSnapshot(scenario);
    if (this.dataset !== null && scenario !== 'empty') {
      snapshot = this.applyFirstPages(snapshot);
    }
    this.snapshot = clone(snapshot);
    return clone(this.snapshot);
  }

  async loadMoreDrives(
    vehicleId: string,
    signal?: AbortSignal,
  ): Promise<HubSnapshot> {
    throwIfAborted(signal);
    if (this.snapshot === null) {
      throw new ViewerDataError(
        'FIXTURE_NOT_PAIRED',
        'Pair the fixture viewer before loading drive history.',
      );
    }
    if (this.dataset === null) return clone(this.snapshot);

    const pages = this.dataset.get(vehicleId);
    const currentIndex = this.pageIndexes.get(vehicleId) ?? 0;
    if (pages === undefined || currentIndex >= pages.length - 1) {
      return clone(this.snapshot);
    }

    const nextIndex = currentIndex + 1;
    const nextPage = pages[nextIndex] ?? [];
    const nextSnapshot = clone(this.snapshot);
    const prior = nextSnapshot.drives.filter(
      (drive) => drive.vehicleId === vehicleId,
    );
    const otherDrives = nextSnapshot.drives.filter(
      (drive) => drive.vehicleId !== vehicleId,
    );
    const vehicleDrives = uniqueDrives([...prior, ...nextPage]);
    nextSnapshot.drives = [...otherDrives, ...vehicleDrives];
    nextSnapshot.drivePaging = {
      ...nextSnapshot.drivePaging,
      [vehicleId]: {
        resource: nextSnapshot.resources.drives,
        hasMore: nextIndex < pages.length - 1,
        loadedCount: vehicleDrives.length,
      },
    };
    this.pageIndexes.set(vehicleId, nextIndex);
    this.snapshot = nextSnapshot;
    return clone(nextSnapshot);
  }

  async removePairedDevice(
    deviceId: string,
    signal?: AbortSignal,
  ): Promise<PairedDevice[]> {
    throwIfAborted(signal);
    const snapshot = createFixtureSnapshot('complete');
    snapshot.devices = snapshot.devices.filter((device) => device.id !== deviceId);
    return snapshot.devices;
  }

  async logout(): Promise<void> {
    this.snapshot = null;
    this.pageIndexes.clear();
  }

  private applyFirstPages(snapshot: HubSnapshot): HubSnapshot {
    const datasetVehicleIds = [...this.dataset!.keys()];
    const knownIds = new Set(snapshot.vehicles.map((vehicle) => vehicle.id));
    for (const vehicleId of datasetVehicleIds) {
      if (!knownIds.has(vehicleId)) {
        snapshot.vehicles.push(syntheticVehicle(vehicleId));
        snapshot.currentByVehicle[vehicleId] = null;
      }
    }

    const drives: DriveSummary[] = [];
    const drivePaging = Object.fromEntries(
      snapshot.vehicles.map((vehicle) => {
        const pages = this.dataset!.get(vehicle.id) ?? [[]];
        const firstPage = pages[0] ?? [];
        const items = uniqueDrives(firstPage);
        drives.push(...items);
        this.pageIndexes.set(vehicle.id, 0);
        return [
          vehicle.id,
          {
            resource: snapshot.resources.drives,
            hasMore: pages.length > 1,
            loadedCount: items.length,
          },
        ];
      }),
    );
    snapshot.drives = drives;
    snapshot.drivePaging = drivePaging;
    return snapshot;
  }
}
