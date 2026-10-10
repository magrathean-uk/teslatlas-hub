import {
  asSafeRequestId,
  ProtocolValidationError,
  TeslatlasError,
  TransportError,
} from "../core/errors.js";
import { FetchTransport, type FetchImplementation } from "../http/fetch-transport.js";
import { readBoundedText } from "../http/bounded-json.js";
import { requireEmptyResponseBody } from "../http/empty-body.js";
import {
  InvalidStrongEntityTagError,
  isStrongEntityTag,
  type StrongEntityTag,
} from "../http/strong-etag.js";
import { cancelResponseBody, withResponseOwnership } from "../http/response-ownership.js";
import type { operations, paths } from "../generated/hub-protocol.js";
import type {
  CreateHubClientOptions,
  HubCapability,
  HubClaimTransport,
  HubClaim,
  HubClient,
  HubCredential,
  HubDiscovery,
  HubDrive,
  HubDrivesOptions,
  HubDrivesResult,
  HubErrorCode,
  HubInvitation,
  HubRequestOptions,
  HubResponse,
  HubResponseMetadata,
} from "./models.js";
import {
  decodeHubJson,
  validateHubClaim,
  validateHubCurrent,
  validateHubDiscovery,
  validateHubDrives,
  validateHubError,
  validateHubHealth,
  validateHubInvitation,
  validateHubReady,
  validateHubVehicles,
  type HubValidator,
} from "./validate.js";

const discoveryPath = "/.well-known/teslatlas-hub";
const maximumResponseBytes = 1_048_576;
const maximumClaimRequestBytes = 4_096;
const maximumDeviceNameBytes = 128;
const uuidPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/u;
const tokenPattern = /^[0-9a-f]{64}$/u;
const maximumHubEntityTagLength = 65_536;

/**
 * One deadline bounds everything logout does before it clears the local credential for the
 * best-effort revoke: the credential load, a discovery fetch when none is cached, and the request.
 * It stays far below a UI's patience and below the 5 s default per-test timeout of the unit tests.
 */
export const hubLogoutRevokeDeadlineMs = 2_000;
const revokeCapability: HubCapability = "auth.revoke";
const revokePath = "/v1/device/revoke" satisfies keyof paths;
type RevokeStatus = keyof operations["revoke"]["responses"];
// 204: revoked now. 401: the bearer is already unusable everywhere (revoked, expired or unknown),
// which is what the caller wants, so a repeated or late call counts as revoked as well.
const revokedStatuses = [204, 401] as const satisfies readonly RevokeStatus[];
const rotationAtMaximum = "max_lifetime" satisfies NonNullable<HubClaim["reason"]>;

interface HubOperation {
  readonly generation: number;
  readonly signal: AbortSignal;
}

interface LoadedCredential {
  readonly credential: HubCredential | undefined;
  /** The logout barrier epoch this load ran under. */
  readonly epoch: number;
}

interface PendingDiscovery {
  readonly promise: Promise<HubResponse<HubDiscovery>>;
  readonly controller: AbortController;
  waiters: number;
}

export class HubIdentityMismatchError extends TeslatlasError<"hub_identity_mismatch"> {
  constructor() {
    super("Hub identity does not match the expected installation", {
      code: "hub_identity_mismatch",
    });
  }
}

export class VehicleIdentityMismatchError extends TeslatlasError<"vehicle_identity_mismatch"> {
  constructor() {
    super("Hub response vehicle identity does not match the requested vehicle", {
      code: "vehicle_identity_mismatch",
    });
  }
}

export class UnsupportedHubMethodError extends TeslatlasError<"unsupported_method"> {
  readonly capability: HubCapability;

  constructor(capability: HubCapability) {
    super("Hub does not advertise the capability required by this method", {
      code: "unsupported_method",
    });
    this.capability = capability;
  }
}

export class HubHttpError extends TeslatlasError<HubErrorCode | "hub_http_error"> {
  constructor(status: number, code: HubErrorCode | "hub_http_error", message?: string) {
    super(message ?? "Hub request failed", { code, status });
  }
}

export class HubClientDisposedError extends TeslatlasError<"client_disposed"> {
  constructor() {
    super("Hub client has been disposed", { code: "client_disposed" });
  }
}

export class HubTlsPinUnavailableError extends TeslatlasError<"hub_tls_pin_unavailable"> {
  constructor() {
    super("No transport capable of enforcing the invitation TLS leaf pin is available", {
      code: "hub_tls_pin_unavailable",
    });
  }
}

export class HubTlsPinMismatchError extends TeslatlasError<"hub_tls_pin_mismatch"> {
  constructor() {
    super("Hub TLS leaf certificate does not match the invitation pin", {
      code: "hub_tls_pin_mismatch",
    });
  }
}

export function createHubClient(options: CreateHubClientOptions): HubClient {
  return new StaticHubClient(options);
}

class StaticHubClient implements HubClient {
  readonly #endpoint: URL;
  readonly #expectedHubId: string;
  readonly #credentials: CreateHubClientOptions["credentials"];
  readonly #claimTransport: HubClaimTransport | undefined;
  readonly #transportOptions: {
    readonly baseUrl: URL;
    readonly fetch?: FetchImplementation;
  };
  readonly #unauthenticatedTransport: FetchTransport;
  readonly #ownerSignal: AbortSignal | undefined;
  readonly #disposeAbort = new AbortController();
  #sessionAbort = new AbortController();
  #discovery: HubDiscovery | undefined;
  #discoveryPending: PendingDiscovery | undefined;
  #disposed = false;
  #sessionGeneration = 0;
  #credentialMutation: Promise<void> = Promise.resolve();
  // Set while a logout has not finished its queued clear; credential loads wait for it.
  #barrier: Promise<void> | undefined;
  #barrierEpoch = 0;
  readonly #cursorBindings = new Map<string, string>();

  constructor(options: CreateHubClientOptions) {
    this.#endpoint = parseEndpoint(options.endpoint);
    if (!uuidPattern.test(options.expectedHubId))
      throw new ProtocolValidationError("expectedHubId");
    this.#expectedHubId = options.expectedHubId;
    this.#credentials = options.credentials;
    this.#claimTransport = options.claimTransport;
    this.#ownerSignal = options.signal;
    this.#transportOptions = {
      baseUrl: this.#endpoint,
      ...(options.fetch === undefined ? {} : { fetch: options.fetch as FetchImplementation }),
    };
    this.#unauthenticatedTransport = new FetchTransport(this.#transportOptions);
  }

  async discover(options: HubRequestOptions = {}): Promise<HubResponse<HubDiscovery>> {
    return this.#discover(this.#beginOperation(options.signal));
  }

  async #discover(operation: HubOperation): Promise<HubResponse<HubDiscovery>> {
    this.#assertOperation(operation);
    let pending = this.#discoveryPending;
    if (pending === undefined) {
      const controller = new AbortController();
      const sharedOperation = this.#beginOperation(controller.signal);
      const promise = this.#read(
        this.#unauthenticatedTransport,
        discoveryPath,
        validateHubDiscovery,
        "HubDiscovery",
        sharedOperation,
        [200],
      )
        .then((result: HubResponse<HubDiscovery>) => {
          this.#assertOperation(sharedOperation);
          if (result.value.hubId !== this.#expectedHubId) throw new HubIdentityMismatchError();
          this.#discovery = result.value;
          this.#cursorBindings.clear();
          return result;
        })
        .catch((error: unknown) => {
          if (
            this.#discoveryPending === pending &&
            sharedOperation.generation === this.#sessionGeneration &&
            !this.#disposed
          ) {
            this.#discovery = undefined;
            this.#cursorBindings.clear();
          }
          throw error;
        })
        .finally(() => {
          if (this.#discoveryPending === pending) this.#discoveryPending = undefined;
        });
      pending = { promise, controller, waiters: 0 };
      this.#discoveryPending = pending;
    }
    return this.#waitForDiscovery(pending, operation);
  }

  #waitForDiscovery(
    pending: PendingDiscovery,
    operation: HubOperation,
  ): Promise<HubResponse<HubDiscovery>> {
    pending.waiters += 1;
    return new Promise((resolve, reject) => {
      let settled = false;
      const finish = (settle: () => void) => {
        if (settled) return;
        settled = true;
        operation.signal.removeEventListener("abort", onAbort);
        pending.waiters -= 1;
        if (pending.waiters === 0 && this.#discoveryPending === pending) {
          this.#discoveryPending = undefined;
          pending.controller.abort(
            new DOMException("Discovery has no active callers", "AbortError"),
          );
        }
        settle();
      };
      const onAbort = () =>
        finish(() => reject(operation.signal.reason ?? new DOMException("Aborted", "AbortError")));
      operation.signal.addEventListener("abort", onAbort, { once: true });
      pending.promise.then(
        (result) => {
          if (settled) return;
          try {
            this.#assertOperation(operation);
            finish(() => resolve(result));
          } catch (error) {
            finish(() => reject(error));
          }
        },
        (error: unknown) => finish(() => reject(error)),
      );
      if (operation.signal.aborted) onAbort();
    });
  }

  async health(options: HubRequestOptions = {}) {
    const operation = this.#beginOperation(options.signal);
    return this.#read(
      this.#unauthenticatedTransport,
      "/healthz",
      validateHubHealth,
      "HubHealth",
      operation,
      [200],
    );
  }

  async readiness(options: HubRequestOptions = {}) {
    const operation = this.#beginOperation(options.signal);
    return this.#read(
      this.#unauthenticatedTransport,
      "/readyz",
      validateHubReady,
      "HubReadiness",
      operation,
      [200, 503],
    );
  }

  async claimPairing(
    invitation: HubInvitation,
    deviceName: string,
    options: HubRequestOptions = {},
  ): Promise<HubResponse<HubClaim>> {
    const operation = this.#beginOperation(options.signal);
    validateInvitation(invitation, this.#endpoint);
    if (!isValidDeviceName(deviceName)) {
      throw new ProtocolValidationError("HubClaimRequest.deviceName");
    }
    const body = JSON.stringify({ secret: invitation.secret, device_name: deviceName });
    // Backstop: the validated secret and 128-byte device name keep the body far below this bound.
    if (new TextEncoder().encode(body).byteLength > maximumClaimRequestBytes) {
      throw new ProtocolValidationError("HubClaimRequest.size");
    }
    const claimTransport = this.#claimTransport;
    if (claimTransport === undefined) throw new HubTlsPinUnavailableError();
    await this.#ensureDiscovery(operation);
    this.#assertOperation(operation);
    let response: Response;
    try {
      response = await claimTransport({
        url: new URL(
          `/v1/pairings/${encodeURIComponent(invitation.pairingId)}/claim`,
          this.#endpoint,
        ),
        body,
        tlsPin: invitation.tlsPin,
        signal: operation.signal,
      });
    } catch (error) {
      if (error instanceof TeslatlasError) throw error;
      if (operation.signal.aborted) throw operation.signal.reason ?? error;
      if (error instanceof DOMException && error.name === "AbortError") throw error;
      throw new TransportError();
    }
    const result = await this.#decodeClaimResponse(response, operation, "claim");
    this.#assertOperation(operation);
    requireUnexpiredClaim(result.value);
    const credential = asCredential(result.value);
    await this.#saveCredential(credential, operation);
    this.#assertOperation(operation);
    return result;
  }

  async rotateDevice(options: HubRequestOptions = {}): Promise<HubResponse<HubClaim>> {
    const operation = this.#beginOperation(options.signal);
    await this.#ensureDiscovery(operation);
    const loaded = await this.#loadCredential(operation);
    this.#assertOperation(operation);
    const previous = loaded.credential;
    if (previous === undefined) throw new HubHttpError(401, "hub_http_error");
    validateCredential(previous);
    const result = await this.#writeClaim(
      this.#makeAuthenticatedTransport(operation, previous),
      "/v1/device/rotate",
      undefined,
      operation,
      "rotate",
    );
    this.#assertOperation(operation);
    requireUnexpiredClaim(result.value);
    if (result.value.deviceId !== previous.deviceId) {
      throw new ProtocolValidationError("HubClaim.deviceId");
    }
    if (result.value.reason === rotationAtMaximum) {
      // The device is at its absolute lifetime: the Hub issued nothing and answered with the
      // bearer we presented. Keep that bearer and do not retry; the caller branches on `reason`
      // and has until `expiresAtMs` to re-pair. The stored expiry follows the Hub's, so it is
      // written when the answer states a different one (the bearer and the device are unchanged).
      // The write is a compare-and-set against the credential this rotation presented: a pairing
      // claimed meanwhile has replaced it, and its credential must stay.
      if (result.value.accessToken !== previous.accessToken) {
        throw new ProtocolValidationError("HubClaim.accessToken");
      }
      if (result.value.expiresAtMs !== previous.expiresAtMs) {
        await this.#saveCredential(asCredential(result.value), operation, loaded.epoch, previous);
        this.#assertOperation(operation);
      }
      return result;
    }
    const credential = asCredential(result.value);
    await this.#saveCredential(credential, operation, loaded.epoch);
    this.#assertOperation(operation);
    return result;
  }

  async vehicles(options: HubRequestOptions = {}) {
    const operation = this.#beginOperation(options.signal);
    await this.#requireCapability("query.vehicles", operation);
    return this.#read(
      this.#makeAuthenticatedTransport(operation),
      "/v1/vehicles",
      validateHubVehicles,
      "HubVehicles",
      operation,
      [200],
    );
  }

  async current(vehicleId: string, options: HubRequestOptions = {}) {
    validateUuid(vehicleId, "vehicleId");
    const operation = this.#beginOperation(options.signal);
    await this.#requireCapability("query.current", operation);
    const result = await this.#read(
      this.#makeAuthenticatedTransport(operation),
      `/v1/vehicles/${encodeURIComponent(vehicleId)}/current`,
      validateHubCurrent,
      "HubCurrent",
      operation,
      [200],
    );
    if (result.value.vehicleId !== vehicleId) throw new VehicleIdentityMismatchError();
    return result;
  }

  async drives(vehicleId: string, options: HubDrivesOptions = {}): Promise<HubDrivesResult> {
    const { fromMs, toMs, limit, cursor, ifNoneMatch, signal } = options;
    validateUuid(vehicleId, "vehicleId");
    if (ifNoneMatch !== undefined) asHubEntityTag(ifNoneMatch, true);
    const operation = this.#beginOperation(signal);
    await this.#requireCapability("query.drives", operation);
    this.#assertOperation(operation);
    const binding = cursorBinding(vehicleId, fromMs, toMs);
    if (
      cursor !== undefined &&
      this.#cursorBindings.has(cursor) &&
      this.#cursorBindings.get(cursor) !== binding
    ) {
      throw new ProtocolValidationError("HubDrives.cursorBinding");
    }
    const query = new URLSearchParams();
    appendInteger(query, "from_ms", fromMs);
    appendInteger(query, "to_ms", toMs);
    appendInteger(query, "limit", limit);
    if (cursor !== undefined) query.set("cursor", cursor);
    const response = await this.#makeAuthenticatedTransport(operation).request(
      `/v1/vehicles/${encodeURIComponent(vehicleId)}/drives${query.size === 0 ? "" : `?${query}`}`,
      {
        redirect: "error",
        signal: operation.signal,
        onDispatch: () => this.#assertOperation(operation),
        ...(ifNoneMatch === undefined ? {} : { headers: { "If-None-Match": ifNoneMatch } }),
      },
    );
    return withResponseOwnership(response, async (): Promise<HubDrivesResult> => {
      this.#assertOperation(operation);
      if (response.status !== 200 && response.status !== 304) {
        return this.#throwHubError(response, "drives", operation);
      }
      requireNoStore(response);
      const metadata = responseMetadata(response, true);
      if (response.status === 304) {
        if (ifNoneMatch === undefined || metadata.etag !== ifNoneMatch) {
          throw new ProtocolValidationError("HubDrives.304.condition");
        }
        await requireEmptyResponseBody(response, operation.signal, "HubDrives.304");
        this.#assertOperation(operation);
        return { kind: "notModified", metadata };
      }
      const value = await decodeResponse(
        response,
        validateHubDrives,
        "HubDrives",
        operation.signal,
      );
      this.#assertOperation(operation);
      if (value.items.some((drive: HubDrive) => drive.vehicleId !== vehicleId)) {
        throw new VehicleIdentityMismatchError();
      }
      if (value.items.length > (limit ?? 100)) {
        throw new ProtocolValidationError("HubDrives.limit");
      }
      if (
        value.items.some(
          (drive: HubDrive) =>
            drive.startDateMs < (fromMs ?? 0) ||
            drive.startDateMs >= (toMs ?? Number.MAX_SAFE_INTEGER),
        )
      ) {
        throw new ProtocolValidationError("HubDrives.timeRange");
      }
      if (
        value.items.some((drive: HubDrive, index: number) => {
          const earlier = value.items[index - 1];
          return (
            earlier !== undefined &&
            (earlier.startDateMs < drive.startDateMs ||
              (earlier.startDateMs === drive.startDateMs && earlier.id <= drive.id))
          );
        })
      ) {
        throw new ProtocolValidationError("HubDrives.order");
      }
      if (value.nextCursor !== null) this.#cursorBindings.set(value.nextCursor, binding);
      return { kind: "page", value, metadata };
    });
  }

  async logout(): Promise<void> {
    this.#requireActive();
    const cachedDiscovery = this.#discovery;
    // The barrier goes up before the generation moves: from here on a bearer operation waits for
    // the clear instead of loading the credential that is about to go.
    const barrier = this.#raiseBarrier();
    try {
      // The queue entry is reserved in the same synchronous step as the barrier, before any abort
      // listener can run: a listener that logs out again or saves a credential re-entrantly queues
      // behind this clear, so the barriers and the clears keep the same order and an inner logout
      // cannot lower the barrier of an outer clear that has not finished.
      const cleared = this.#enqueueCredentialMutation(async () => {
        await this.#revokeBestEffort(cachedDiscovery);
        await this.#credentials.clear();
      });
      this.#sessionAbort.abort(new DOMException("Hub client logged out", "AbortError"));
      this.#sessionGeneration += 1;
      this.#sessionAbort = new AbortController();
      this.#cursorBindings.clear();
      this.#discovery = undefined;
      this.#discoveryPending = undefined;
      await cleared;
    } finally {
      barrier.lower();
    }
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#sessionGeneration += 1;
    this.#sessionAbort.abort(new DOMException("Hub client disposed", "AbortError"));
    this.#disposeAbort.abort(new DOMException("Hub client disposed", "AbortError"));
    this.#cursorBindings.clear();
    this.#discovery = undefined;
    this.#discoveryPending = undefined;
  }

  async #ensureDiscovery(operation: HubOperation): Promise<HubDiscovery> {
    this.#assertOperation(operation);
    return this.#discovery ?? (await this.#discover(operation)).value;
  }

  #raiseBarrier(): { lower(): void } {
    let settle!: () => void;
    const barrier = new Promise<void>((resolve) => {
      settle = resolve;
    });
    this.#barrier = barrier;
    this.#barrierEpoch += 1;
    return {
      lower: () => {
        settle();
        if (this.#barrier === barrier) this.#barrier = undefined;
      },
    };
  }

  async #loadCredential(operation: HubOperation): Promise<LoadedCredential> {
    this.#assertOperation(operation);
    const barrier = this.#barrier;
    if (barrier !== undefined) {
      // A logout is clearing the store: load only what is there once it has finished.
      await raceAbort(barrier, operation.signal);
      this.#assertOperation(operation);
    }
    const epoch = this.#barrierEpoch;
    try {
      const credential = await this.#credentials.load();
      this.#assertOperation(operation);
      return {
        credential: credential === undefined ? undefined : snapshotCredential(credential),
        epoch,
      };
    } catch (error) {
      if (this.#isExpired(operation) || operation.signal.aborted) this.#assertOperation(operation);
      throw error;
    }
  }

  /**
   * Save `credential` in the credential-mutation queue. With `onlyIfStoredIs`, the save is a
   * compare-and-set: the store is read inside the queue entry, after every earlier mutation, and
   * nothing is written unless it still holds the same bearer and device id as `onlyIfStoredIs`.
   * A store that holds anything else (another pairing, or nothing) keeps what it has.
   */
  async #saveCredential(
    credential: HubCredential,
    operation: HubOperation,
    loadEpoch?: number,
    onlyIfStoredIs?: HubCredential,
  ): Promise<void> {
    try {
      await this.#enqueueCredentialMutation(async () => {
        this.#assertOperation(operation);
        if (loadEpoch !== undefined && loadEpoch !== this.#barrierEpoch) {
          // The credential this one derives from was loaded before a logout.
          throw new DOMException("Hub client credential predates a logout", "AbortError");
        }
        if (onlyIfStoredIs !== undefined) {
          // Straight from the store: this entry runs inside the queue, and no logout is pending
          // (the epoch above is current), so there is no barrier to wait for.
          const current = await this.#credentials.load();
          this.#assertOperation(operation);
          if (
            current === undefined ||
            current.accessToken !== onlyIfStoredIs.accessToken ||
            current.deviceId !== onlyIfStoredIs.deviceId
          ) {
            return;
          }
        }
        await this.#credentials.save(credential);
      });
      this.#assertOperation(operation);
    } catch (error) {
      if (this.#isExpired(operation) || operation.signal.aborted) this.#assertOperation(operation);
      throw error;
    }
  }

  #enqueueCredentialMutation(mutation: () => Promise<void> | void): Promise<void> {
    const operation = this.#credentialMutation.catch(() => undefined).then(mutation);
    this.#credentialMutation = operation.catch(() => undefined);
    return operation;
  }

  /**
   * Best-effort device self-revoke for logout, run inside the credential-mutation entry that
   * clears the store. It never throws and never retries: whatever happens, the caller clears the
   * local credential next. The bearer is sent only to a Hub that advertises `auth.revoke`, by the
   * discovery cached when logout started or, when none was cached, by one fresh discovery whose
   * identity matches the expected Hub.
   */
  async #revokeBestEffort(cached: HubDiscovery | undefined): Promise<boolean> {
    if (cached !== undefined && !advertisesRevoke(cached)) return false;
    const deadline = new AbortController();
    const timer = setTimeout(() => {
      deadline.abort(new DOMException("Hub revoke deadline reached", "TimeoutError"));
    }, hubLogoutRevokeDeadlineMs);
    // Not the session signal: logout has just aborted it. Dispose still stops the revoke.
    const signal = AbortSignal.any([deadline.signal, this.#disposeAbort.signal]);
    try {
      return await raceAbort(this.#revoke(cached, signal), signal);
    } catch {
      // Best effort: a refused, failed or abandoned revoke never blocks the local clear.
      return false;
    } finally {
      clearTimeout(timer);
      // An attempt that lost the race must not dispatch anything later.
      deadline.abort(new DOMException("Hub revoke attempt is over", "AbortError"));
    }
  }

  async #revoke(cached: HubDiscovery | undefined, signal: AbortSignal): Promise<boolean> {
    signal.throwIfAborted();
    // Straight from the store: this load must not wait for the barrier logout itself installed.
    const stored = await raceAbort((async () => this.#credentials.load())(), signal);
    if (stored === undefined) return false;
    const credential = snapshotCredential(stored);
    validateCredential(credential);
    signal.throwIfAborted();
    const discovery = cached ?? (await this.#discoverForRevoke(signal));
    if (discovery === undefined || !advertisesRevoke(discovery)) return false;
    signal.throwIfAborted();
    const transport = new FetchTransport({
      ...this.#transportOptions,
      authorization: () => `Bearer ${credential.accessToken}`,
    });
    const response = await transport.request(revokePath, {
      method: "POST",
      redirect: "error",
      signal,
    });
    return withResponseOwnership(response, async () => {
      // The route takes no body and answers with none; check that for the 204 and drop the rest.
      if (response.status === 204) await requireEmptyResponseBody(response, signal, "HubRevoke");
      else await cancelResponseBody(response);
      return (revokedStatuses as readonly number[]).includes(response.status);
    });
  }

  async #discoverForRevoke(signal: AbortSignal): Promise<HubDiscovery | undefined> {
    const response = await this.#unauthenticatedTransport.request(discoveryPath, {
      redirect: "error",
      signal,
    });
    return withResponseOwnership(response, async () => {
      if (response.status !== 200) {
        await cancelResponseBody(response);
        return undefined;
      }
      const discovery = await decodeResponse(
        response,
        validateHubDiscovery,
        "HubDiscovery",
        signal,
      );
      // Never hand a bearer to an endpoint that is not the Hub this client was built for.
      return discovery.hubId === this.#expectedHubId ? discovery : undefined;
    });
  }

  async #requireCapability(capability: HubCapability, operation: HubOperation): Promise<void> {
    this.#assertOperation(operation);
    const discovery = await this.#ensureDiscovery(operation);
    this.#assertOperation(operation);
    if (!(discovery.capabilities as readonly string[]).includes(capability)) {
      throw new UnsupportedHubMethodError(capability);
    }
  }

  #makeAuthenticatedTransport(operation: HubOperation, snapshot?: HubCredential): FetchTransport {
    return new FetchTransport({
      ...this.#transportOptions,
      authorization: async () => {
        const credential = snapshot ?? (await this.#loadCredential(operation)).credential;
        if (credential === undefined) return undefined;
        validateCredential(credential);
        return `Bearer ${credential.accessToken}`;
      },
    });
  }

  async #read<T>(
    transport: FetchTransport,
    path: string,
    validator: HubValidator<T>,
    validatorName: string,
    operation: HubOperation,
    successStatuses: readonly number[],
  ): Promise<HubResponse<T>> {
    this.#assertOperation(operation);
    const response = await transport.request(path, {
      redirect: "error",
      signal: operation.signal,
      onDispatch: () => this.#assertOperation(operation),
    });
    return withResponseOwnership(response, async () => {
      this.#assertOperation(operation);
      if (!successStatuses.includes(response.status)) {
        return this.#throwHubError(
          response,
          path === discoveryPath ? "discovery" : "other",
          operation,
        );
      }
      const value = await decodeResponse(response, validator, validatorName, operation.signal);
      this.#assertOperation(operation);
      return {
        value,
        metadata: responseMetadata(response, false),
      };
    });
  }

  async #writeClaim(
    transport: FetchTransport,
    path: string,
    body: string | undefined,
    operation: HubOperation,
    route: "claim" | "rotate",
  ): Promise<HubResponse<HubClaim>> {
    this.#assertOperation(operation);
    const response = await transport.request(path, {
      method: "POST",
      redirect: "error",
      signal: operation.signal,
      onDispatch: () => this.#assertOperation(operation),
      ...(body === undefined ? {} : { body, headers: { "Content-Type": "application/json" } }),
    });
    return this.#decodeClaimResponse(response, operation, route);
  }

  async #decodeClaimResponse(
    response: Response,
    operation: HubOperation,
    route: "claim" | "rotate",
  ): Promise<HubResponse<HubClaim>> {
    return withResponseOwnership(response, async () => {
      this.#assertOperation(operation);
      if (response.status !== 200) {
        if (route === "claim" && isClaimExtractorStatus(response.status)) {
          return this.#throwClaimExtractorError(response, operation);
        }
        return this.#throwHubError(response, "other", operation);
      }
      const value = await decodeResponse(response, validateHubClaim, "HubClaim", operation.signal);
      this.#assertOperation(operation);
      return {
        value,
        metadata: responseMetadata(response, false),
      };
    });
  }

  async #throwClaimExtractorError(response: Response, operation: HubOperation): Promise<never> {
    const body = await readBody(response, operation.signal, "HubError");
    this.#assertOperation(operation);
    requireTextContentType(response, "HubError");
    if (body.length === 0) throw new ProtocolValidationError("HubError.body");
    throw new HubHttpError(response.status, "hub_http_error");
  }

  async #throwHubError(
    response: Response,
    route: "discovery" | "drives" | "other",
    operation: HubOperation,
  ): Promise<never> {
    const body = await readBody(response, operation.signal, "HubError");
    this.#assertOperation(operation);
    if (body.length === 0) throw new HubHttpError(response.status, "hub_http_error");
    requireJsonContentType(response, "HubError");
    const envelope = decodeHubJson(body, validateHubError, "HubError");
    const code = envelope.error.code;
    if (!isHubErrorCode(code)) throw new ProtocolValidationError("HubError.code");
    const expectedStatus = hubErrorStatuses[code];
    if (
      expectedStatus !== response.status ||
      (route === "discovery" && code !== "service_unavailable") ||
      (route === "drives" && response.status === 503 && code !== "service_unavailable") ||
      route === "other"
    ) {
      throw new ProtocolValidationError("HubError.status");
    }
    throw new HubHttpError(response.status, code, envelope.error.message);
  }

  #beginOperation(signal: AbortSignal | undefined): HubOperation {
    this.#requireActive();
    const operation = {
      generation: this.#sessionGeneration,
      signal: this.#signal(signal),
    };
    this.#assertOperation(operation);
    return operation;
  }

  #assertOperation(operation: HubOperation): void {
    if (this.#isExpired(operation)) {
      throw new DOMException("Hub client operation belongs to an expired session", "AbortError");
    }
    if (operation.signal.aborted) {
      throw operation.signal.reason ?? new DOMException("Aborted", "AbortError");
    }
  }

  #isExpired(operation: HubOperation): boolean {
    return operation.generation !== this.#sessionGeneration || this.#disposed;
  }

  #signal(signal: AbortSignal | undefined): AbortSignal {
    this.#requireActive();
    return AbortSignal.any(
      [this.#ownerSignal, this.#sessionAbort.signal, signal].filter(
        (candidate): candidate is AbortSignal => candidate !== undefined,
      ),
    );
  }

  #requireActive(): void {
    if (this.#disposed) throw new HubClientDisposedError();
  }
}

function advertisesRevoke(discovery: HubDiscovery): boolean {
  return discovery.capabilities.includes(revokeCapability);
}

function snapshotCredential(credential: HubCredential): HubCredential {
  return {
    accessToken: credential.accessToken,
    deviceId: credential.deviceId,
    expiresAtMs: credential.expiresAtMs,
  };
}

/** Settle with `promise`, or reject with the signal's reason as soon as the signal aborts. */
async function raceAbort<T>(promise: PromiseLike<T>, signal: AbortSignal): Promise<T> {
  if (signal.aborted) {
    // Nobody will await `promise`; observe it so that a later rejection is not unhandled.
    void Promise.resolve(promise).catch(() => undefined);
    throw signal.reason ?? new DOMException("Aborted", "AbortError");
  }
  let onAbort: (() => void) | undefined;
  const aborted = new Promise<never>((_resolve, reject) => {
    onAbort = () => reject(signal.reason ?? new DOMException("Aborted", "AbortError"));
    signal.addEventListener("abort", onAbort, { once: true });
  });
  try {
    // Promise.race also observes a rejection of `promise` after the abort won.
    return await Promise.race([promise, aborted]);
  } finally {
    if (onAbort !== undefined) signal.removeEventListener("abort", onAbort);
  }
}

function parseEndpoint(value: string | URL): URL {
  let url: URL;
  try {
    url = new URL(value instanceof URL ? value.href : value);
  } catch {
    throw new ProtocolValidationError("endpoint");
  }
  if (
    (url.protocol !== "https:" && !(url.protocol === "http:" && isLoopback(url.hostname))) ||
    url.username.length > 0 ||
    url.password.length > 0 ||
    url.pathname !== "/" ||
    url.search.length > 0 ||
    url.hash.length > 0
  ) {
    throw new ProtocolValidationError("endpoint");
  }
  return url;
}

function isLoopback(hostname: string): boolean {
  return (
    hostname === "localhost" || hostname === "[::1]" || /^127(?:\.[0-9]{1,3}){3}$/u.test(hostname)
  );
}

function validateUuid(value: string, name: string): void {
  if (!uuidPattern.test(value)) throw new ProtocolValidationError(name);
}

function validateCredential(value: HubCredential): void {
  if (
    !tokenPattern.test(value.accessToken) ||
    !uuidPattern.test(value.deviceId) ||
    !Number.isSafeInteger(value.expiresAtMs)
  ) {
    throw new ProtocolValidationError("HubCredential");
  }
  if (value.expiresAtMs <= Date.now()) throw new ProtocolValidationError("HubCredential.expiry");
}

function validateInvitation(invitation: HubInvitation, endpoint: URL): void {
  const wire = {
    pairingId: invitation.pairingId,
    secret: invitation.secret,
    expiresAtMs: invitation.expiresAtMs,
    endpoint: invitation.endpoint,
    tlsPin: invitation.tlsPin,
    pairingUri: invitation.pairingUri,
  };
  if (!validateHubInvitation(wire)) throw new ProtocolValidationError("HubInvitation");
  if (!Number.isSafeInteger(invitation.expiresAtMs) || invitation.expiresAtMs <= Date.now()) {
    throw new ProtocolValidationError("HubInvitation.expiry");
  }
  const invitationEndpoint = parseEndpoint(invitation.endpoint);
  if (invitationEndpoint.origin !== endpoint.origin) {
    throw new ProtocolValidationError("HubInvitation.endpoint");
  }
  let pairingUri: URL;
  try {
    pairingUri = new URL(invitation.pairingUri);
  } catch {
    throw new ProtocolValidationError("HubInvitation.pairingUri");
  }
  const expected = new Map([
    ["endpoint", invitation.endpoint],
    ["pairing_id", invitation.pairingId],
    ["secret", invitation.secret],
    ["tls_pin", invitation.tlsPin],
  ]);
  if (
    pairingUri.protocol !== "teslatlas-hub:" ||
    pairingUri.hostname !== "pair" ||
    pairingUri.searchParams.size !== expected.size ||
    [...expected].some(([key, value]) => pairingUri.searchParams.get(key) !== value)
  ) {
    throw new ProtocolValidationError("HubInvitation.pairingUri");
  }
}

function requireUnexpiredClaim(claim: HubClaim): void {
  if (!Number.isSafeInteger(claim.expiresAtMs) || claim.expiresAtMs <= Date.now()) {
    throw new ProtocolValidationError("HubClaim.expiry");
  }
}

function asCredential(claim: HubClaim): HubCredential {
  return {
    accessToken: claim.accessToken,
    deviceId: claim.deviceId,
    expiresAtMs: claim.expiresAtMs,
  };
}

async function decodeResponse<T>(
  response: Response,
  validator: HubValidator<T>,
  validatorName: string,
  signal: AbortSignal | undefined,
): Promise<T> {
  requireJsonContentType(response, validatorName);
  return decodeHubJson(
    await readBody(response, signal, validatorName),
    validator,
    validatorName,
  ) as T;
}

async function readBody(
  response: Response,
  signal: AbortSignal | undefined,
  validatorName: string,
): Promise<string> {
  return readBoundedText(response, signal, validatorName, maximumResponseBytes);
}

function requireJsonContentType(response: Response, validatorName: string): void {
  const mediaType = response.headers.get("Content-Type")?.split(";", 1)[0]?.trim().toLowerCase();
  if (mediaType !== "application/json") {
    throw new ProtocolValidationError(`${validatorName}.contentType`);
  }
}

function requireTextContentType(response: Response, validatorName: string): void {
  const mediaType = response.headers.get("Content-Type")?.split(";", 1)[0]?.trim().toLowerCase();
  if (mediaType !== "text/plain") {
    throw new ProtocolValidationError(`${validatorName}.contentType`);
  }
}

function isClaimExtractorStatus(status: number): boolean {
  return status === 400 || status === 415 || status === 422;
}

function responseMetadata(
  response: Response,
  requireEtag: true,
): HubResponseMetadata & { etag: StrongEntityTag };
function responseMetadata(response: Response, requireEtag: false): HubResponseMetadata;
function responseMetadata(response: Response, requireEtag: boolean): HubResponseMetadata {
  const etag = response.headers.get("ETag") ?? undefined;
  if (requireEtag && (etag === undefined || !isStrongEntityTag(etag))) {
    throw new ProtocolValidationError("HubResponse.etag");
  }
  const requestIdHeader = response.headers.get("X-Request-ID");
  const requestId = requestIdHeader === null ? undefined : asSafeRequestId(requestIdHeader);
  return {
    status: response.status,
    ...(etag === undefined ? {} : { etag: requireEtag ? asHubEntityTag(etag) : etag }),
    ...(requestId === undefined ? {} : { requestId }),
  };
}

function asHubEntityTag(value: string, request = false): StrongEntityTag {
  if (
    typeof value !== "string" ||
    value.length > maximumHubEntityTagLength ||
    !isStrongEntityTag(value)
  ) {
    if (request) throw new InvalidStrongEntityTagError();
    throw new ProtocolValidationError("HubResponse.etag");
  }
  return value as StrongEntityTag;
}

function requireNoStore(response: Response): void {
  if (response.headers.get("Cache-Control") !== "no-store") {
    throw new ProtocolValidationError("HubDrives.cacheControl");
  }
}

function appendInteger(query: URLSearchParams, name: string, value: number | undefined): void {
  if (value === undefined) return;
  if (!Number.isSafeInteger(value)) throw new ProtocolValidationError(`HubDrives.${name}`);
  query.set(name, String(value));
}

function cursorBinding(
  vehicleId: string,
  fromMs: number | undefined,
  toMs: number | undefined,
): string {
  return `${vehicleId}\u0000${fromMs ?? 0}\u0000${toMs ?? 9_007_199_254_740_991}`;
}

const hubErrorStatuses = {
  invalid_query: 400,
  invalid_time_range: 400,
  invalid_limit: 400,
  invalid_cursor: 400,
  vehicle_not_found: 404,
  drive_not_found: 404,
  charge_not_found: 404,
  service_unavailable: 503,
} as const satisfies Record<HubErrorCode, number>;

// hub-http-v1@1.2.0 lets any ^[a-z_]{1,64}$ token through as an error code, but this client only
// maps the codes it knows to a status; anything else stays a validation failure.
function isHubErrorCode(code: string): code is HubErrorCode {
  return Object.hasOwn(hubErrorStatuses, code);
}

// hub-http-v1@1.2.0: 1 to 128 UTF-8 bytes and no C0 or C1 control character. The Hub refuses
// any other name with 422 before it consults the invitation, so refuse it before sending.
function isValidDeviceName(deviceName: string): boolean {
  return (
    deviceName.length > 0 &&
    // Every UTF-16 code unit encodes to at least one byte, so a longer string is already over.
    deviceName.length <= maximumDeviceNameBytes &&
    !/\p{Cc}/u.test(deviceName) &&
    new TextEncoder().encode(deviceName).byteLength <= maximumDeviceNameBytes
  );
}
