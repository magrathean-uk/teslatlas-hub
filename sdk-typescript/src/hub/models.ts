import type { MaybePromise } from "../auth/credential-store.js";
import type { SafeRequestId } from "../core/errors.js";
import type { components } from "../generated/hub-protocol.js";
import type { StrongEntityTag } from "../http/strong-etag.js";

type SnakeToCamel<T extends string> = T extends `${infer Head}_${infer Tail}`
  ? `${Head}${Capitalize<SnakeToCamel<Tail>>}`
  : T;

export type CamelizeKeys<T> = T extends readonly (infer Item)[]
  ? readonly CamelizeKeys<Item>[]
  : T extends object
    ? {
        readonly [Key in keyof T as Key extends string ? SnakeToCamel<Key> : Key]: CamelizeKeys<
          T[Key]
        >;
      }
    : T;

type HubSchemas = components["schemas"];

export type HubDiscovery = CamelizeKeys<HubSchemas["discovery.schema"]>;
export type HubHealth = CamelizeKeys<HubSchemas["health"]>;
export type HubReadiness = CamelizeKeys<HubSchemas["ready"]>;
export type HubVehicles = CamelizeKeys<HubSchemas["vehicles"]>;
export type HubCurrent = CamelizeKeys<HubSchemas["current"]>;
export type HubDrives = CamelizeKeys<HubSchemas["drives"]>;
export type HubDrive = HubDrives["items"][number];
export type HubClaim = CamelizeKeys<HubSchemas["claim"]>;
export type HubErrorEnvelope = CamelizeKeys<HubSchemas["errors.schema"]>;
export type HubCapability = HubDiscovery["capabilities"][number];
export type HubErrorCode =
  | "invalid_query"
  | "invalid_time_range"
  | "invalid_limit"
  | "invalid_cursor"
  | "vehicle_not_found"
  | "service_unavailable";

export interface HubInvitation {
  readonly pairingId: string;
  readonly secret: string;
  readonly expiresAtMs: number;
  readonly endpoint: string;
  readonly tlsPin: string;
  readonly pairingUri: string;
}

export interface HubCredential {
  readonly accessToken: string;
  readonly deviceId: string;
  readonly expiresAtMs: number;
}

export interface HubCredentialStore {
  load(): MaybePromise<HubCredential | undefined>;
  save(credential: HubCredential): MaybePromise<void>;
  clear(): MaybePromise<void>;
}

export interface HubResponseMetadata {
  readonly status: number;
  readonly etag?: string;
  readonly requestId?: SafeRequestId;
}

export interface HubDriveResponseMetadata extends Omit<HubResponseMetadata, "etag"> {
  readonly etag: StrongEntityTag;
}

export interface HubResponse<T> {
  readonly value: T;
  readonly metadata: HubResponseMetadata;
}

export type HubDrivesResult =
  | {
      readonly kind: "page";
      readonly value: HubDrives;
      readonly metadata: HubDriveResponseMetadata;
    }
  | {
      readonly kind: "notModified";
      readonly metadata: HubDriveResponseMetadata;
    };

export interface HubDrivesOptions {
  readonly fromMs?: number;
  readonly toMs?: number;
  readonly limit?: number;
  readonly cursor?: string;
  readonly ifNoneMatch?: StrongEntityTag;
  readonly signal?: AbortSignal;
}

export interface HubRequestOptions {
  readonly signal?: AbortSignal;
}

/**
 * A claim request whose transport must validate normal TLS trust and the expected connected-leaf
 * DER SHA-256 before it sends any request bytes.
 */
export interface HubClaimTransportRequest {
  readonly url: URL;
  readonly body: string;
  readonly tlsPin: string;
  readonly signal: AbortSignal;
}

/**
 * A caller-owned claim transport. Implementations must validate CA trust, hostname, and `tlsPin`
 * on the same TLS connection before transmitting `body`.
 */
export type HubClaimTransport = (request: HubClaimTransportRequest) => Promise<Response>;

export interface CreateHubClientOptions {
  readonly endpoint: string | URL;
  readonly expectedHubId: string;
  readonly credentials: HubCredentialStore;
  readonly fetch?: (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;
  readonly claimTransport?: HubClaimTransport;
  readonly signal?: AbortSignal;
}

export interface HubClient {
  discover(options?: HubRequestOptions): Promise<HubResponse<HubDiscovery>>;
  health(options?: HubRequestOptions): Promise<HubResponse<HubHealth>>;
  readiness(options?: HubRequestOptions): Promise<HubResponse<HubReadiness>>;
  claimPairing(
    invitation: HubInvitation,
    deviceName: string,
    options?: HubRequestOptions,
  ): Promise<HubResponse<HubClaim>>;
  rotateDevice(options?: HubRequestOptions): Promise<HubResponse<HubClaim>>;
  vehicles(options?: HubRequestOptions): Promise<HubResponse<HubVehicles>>;
  current(vehicleId: string, options?: HubRequestOptions): Promise<HubResponse<HubCurrent>>;
  drives(vehicleId: string, options?: HubDrivesOptions): Promise<HubDrivesResult>;
  /**
   * Abort the current session and clear credentials after SDK-owned mutations already dispatched.
   * Credential mutations started after logout are ordered after that clear.
   */
  logout(): Promise<void>;
  /**
   * Abort the current session while retaining credentials. Caller persistence already dispatched
   * may finish, but the interrupted operation cannot report success.
   */
  dispose(): void;
}
