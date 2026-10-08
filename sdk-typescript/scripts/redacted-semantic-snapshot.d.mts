interface SemanticSnapshotClient {
  vehicles(): Promise<unknown>;
  current(vehicleId: string): Promise<unknown>;
  drives(
    vehicleId: string,
    options: { fromMs: number; toMs: number; limit: number; cursor?: string },
  ): Promise<unknown>;
}

export interface SemanticSnapshotProfile {
  readonly schemaVersion: 1;
  readonly primaryVehicleId: string;
  readonly vehicleCount: number;
  readonly history: { readonly fromMs: number; readonly toMs: number };
  readonly boundaryMode: "derived-latest-drive";
}

interface SemanticSourceVehicle {
  readonly vehicleId: string;
}

export const SEMANTIC_SNAPSHOT_CANONICALIZATION: "sorted-json-v1";
export const SEMANTIC_SNAPSHOT_PROFILE: "hub-http-v1@1.0.0";
export function canonicalizeSortedJson(value: unknown): string;
export function hashSortedJson(value: unknown): Promise<string>;
export function createReadOnlyCredentialStore<T>(credential: T): {
  readonly load: () => T;
  readonly save: (credential: unknown) => never;
  readonly clear: () => never;
};
export function assertNoForbiddenSemanticData(value: unknown): void;
export function assertRedactedSemanticSnapshot(value: unknown): void;
export function selectSemanticSnapshotPrimaryVehicle(
  vehicles: readonly SemanticSourceVehicle[],
  semanticProfile?: SemanticSnapshotProfile,
): string;
export function collectRedactedSemanticSnapshot(
  client: SemanticSnapshotClient,
  semanticProfile?: SemanticSnapshotProfile,
): Promise<unknown>;
