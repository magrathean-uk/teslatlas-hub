import {
  asSafeRequestId,
  ProtocolValidationError,
  TeslatlasError,
  TransportError,
} from "../core/errors.js";
import { FetchTransport, type FetchImplementation } from "../http/fetch-transport.js";
import { requireEmptyResponseBody } from "../http/empty-body.js";
import { asStrongEntityTag, isStrongEntityTag } from "../http/strong-etag.js";
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
const uuidPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/u;
const tokenPattern = /^[0-9a-f]{64}$/u;

interface HubOperation {
  readonly generation: number;
  readonly signal: AbortSignal;
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
  #sessionAbort = new AbortController();
  #discovery: HubDiscovery | undefined;
  #discoveryPending: Promise<HubResponse<HubDiscovery>> | undefined;
  #disposed = false;
  #sessionGeneration = 0;
  #credentialMutation: Promise<void> = Promise.resolve();
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
    if (this.#discoveryPending !== undefined) return this.#discoveryPending;
    const pending = this.#read(
      this.#unauthenticatedTransport,
      discoveryPath,
      validateHubDiscovery,
      "HubDiscovery",
      operation,
      [200],
    ).then((result: HubResponse<HubDiscovery>) => {
      this.#assertOperation(operation);
      if (result.value.hubId !== this.#expectedHubId) throw new HubIdentityMismatchError();
      this.#discovery = result.value;
      this.#cursorBindings.clear();
      return result;
    });
    this.#discoveryPending = pending;
    try {
      return await pending;
    } catch (error) {
      if (operation.generation === this.#sessionGeneration && !this.#disposed) {
        this.#discovery = undefined;
        this.#cursorBindings.clear();
      }
      throw error;
    } finally {
      if (this.#discoveryPending === pending) this.#discoveryPending = undefined;
    }
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
    if (deviceName.length === 0 || deviceName.length > 65_536) {
      throw new ProtocolValidationError("HubClaimRequest.deviceName");
    }
    const body = JSON.stringify({ secret: invitation.secret, device_name: deviceName });
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
    const previous = await this.#loadCredential(operation);
    this.#assertOperation(operation);
    if (previous === undefined) throw new HubHttpError(401, "hub_http_error");
    validateCredential(previous);
    const result = await this.#writeClaim(
      this.#makeAuthenticatedTransport(operation),
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
    const credential = asCredential(result.value);
    await this.#saveCredential(credential, operation);
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
    validateUuid(vehicleId, "vehicleId");
    if (options.ifNoneMatch !== undefined) asStrongEntityTag(options.ifNoneMatch);
    const operation = this.#beginOperation(options.signal);
    await this.#requireCapability("query.drives", operation);
    this.#assertOperation(operation);
    const binding = cursorBinding(vehicleId, options.fromMs, options.toMs);
    if (
      options.cursor !== undefined &&
      this.#cursorBindings.has(options.cursor) &&
      this.#cursorBindings.get(options.cursor) !== binding
    ) {
      throw new ProtocolValidationError("HubDrives.cursorBinding");
    }
    const query = new URLSearchParams();
    appendInteger(query, "from_ms", options.fromMs);
    appendInteger(query, "to_ms", options.toMs);
    appendInteger(query, "limit", options.limit);
    if (options.cursor !== undefined) query.set("cursor", options.cursor);
    const response = await this.#makeAuthenticatedTransport(operation).request(
      `/v1/vehicles/${encodeURIComponent(vehicleId)}/drives${query.size === 0 ? "" : `?${query}`}`,
      {
        redirect: "error",
        signal: operation.signal,
        ...(options.ifNoneMatch === undefined
          ? {}
          : { headers: { "If-None-Match": options.ifNoneMatch } }),
      },
    );
    this.#assertOperation(operation);
    if (response.status !== 200 && response.status !== 304) {
      return this.#throwHubError(response, "drives", operation);
    }
    requireNoStore(response);
    const metadata = responseMetadata(response, true);
    if (response.status === 304) {
      await requireEmptyResponseBody(response, operation.signal, "HubDrives.304");
      this.#assertOperation(operation);
      return { kind: "notModified", metadata };
    }
    const value = await decodeResponse(response, validateHubDrives, "HubDrives", operation.signal);
    this.#assertOperation(operation);
    if (value.items.some((drive: HubDrive) => drive.vehicleId !== vehicleId)) {
      throw new VehicleIdentityMismatchError();
    }
    if (value.nextCursor !== null) this.#cursorBindings.set(value.nextCursor, binding);
    return { kind: "page", value, metadata };
  }

  async logout(): Promise<void> {
    this.#requireActive();
    this.#sessionAbort.abort(new DOMException("Hub client logged out", "AbortError"));
    this.#sessionGeneration += 1;
    this.#sessionAbort = new AbortController();
    this.#cursorBindings.clear();
    this.#discovery = undefined;
    this.#discoveryPending = undefined;
    await this.#enqueueCredentialMutation(() => this.#credentials.clear());
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#sessionGeneration += 1;
    this.#sessionAbort.abort(new DOMException("Hub client disposed", "AbortError"));
    this.#cursorBindings.clear();
    this.#discovery = undefined;
    this.#discoveryPending = undefined;
  }

  async #ensureDiscovery(operation: HubOperation): Promise<HubDiscovery> {
    this.#assertOperation(operation);
    return this.#discovery ?? (await this.#discover(operation)).value;
  }

  async #loadCredential(operation: HubOperation): Promise<HubCredential | undefined> {
    this.#assertOperation(operation);
    try {
      const credential = await this.#credentials.load();
      this.#assertOperation(operation);
      return credential;
    } catch (error) {
      if (this.#isExpired(operation) || operation.signal.aborted) this.#assertOperation(operation);
      throw error;
    }
  }

  async #saveCredential(credential: HubCredential, operation: HubOperation): Promise<void> {
    try {
      await this.#enqueueCredentialMutation(async () => {
        this.#assertOperation(operation);
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

  async #requireCapability(capability: HubCapability, operation: HubOperation): Promise<void> {
    this.#assertOperation(operation);
    const discovery = await this.#ensureDiscovery(operation);
    this.#assertOperation(operation);
    if (!(discovery.capabilities as readonly string[]).includes(capability)) {
      throw new UnsupportedHubMethodError(capability);
    }
  }

  #makeAuthenticatedTransport(operation: HubOperation): FetchTransport {
    return new FetchTransport({
      ...this.#transportOptions,
      authorization: async () => {
        const credential = await this.#loadCredential(operation);
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
    });
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
      ...(body === undefined ? {} : { body, headers: { "Content-Type": "application/json" } }),
    });
    return this.#decodeClaimResponse(response, operation, route);
  }

  async #decodeClaimResponse(
    response: Response,
    operation: HubOperation,
    route: "claim" | "rotate",
  ): Promise<HubResponse<HubClaim>> {
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
    const code: HubErrorCode = envelope.error.code;
    const expectedStatus = statusForHubError(code);
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
  if (response.body === null) {
    if (signal?.aborted === true) {
      throw signal.reason ?? new DOMException("Aborted", "AbortError");
    }
    return "";
  }
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let totalBytes = 0;
  let abortListener: (() => void) | undefined;
  const aborted =
    signal === undefined
      ? undefined
      : new Promise<never>((_resolve, reject) => {
          abortListener = () => reject(signal.reason ?? new DOMException("Aborted", "AbortError"));
          signal.addEventListener("abort", abortListener, { once: true });
        });
  try {
    if (signal?.aborted === true) throw signal.reason ?? new DOMException("Aborted", "AbortError");
    while (true) {
      const result = await (aborted === undefined
        ? reader.read()
        : Promise.race([reader.read(), aborted]));
      if (result.done) break;
      totalBytes += result.value.byteLength;
      if (totalBytes > maximumResponseBytes) {
        throw new ProtocolValidationError(`${validatorName}.size`);
      }
      chunks.push(result.value);
    }
  } catch (error) {
    if (signal?.aborted === true) throw signal.reason ?? error;
    if (error instanceof ProtocolValidationError) throw error;
    throw new ProtocolValidationError(validatorName);
  } finally {
    if (abortListener !== undefined) signal?.removeEventListener("abort", abortListener);
    void reader.cancel().catch(() => undefined);
  }
  const body = new Uint8Array(totalBytes);
  let offset = 0;
  for (const chunk of chunks) {
    body.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return new TextDecoder().decode(body);
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
): HubResponseMetadata & { etag: ReturnType<typeof asStrongEntityTag> };
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
    ...(etag === undefined ? {} : { etag: requireEtag ? asStrongEntityTag(etag) : etag }),
    ...(requestId === undefined ? {} : { requestId }),
  };
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

function statusForHubError(code: HubErrorCode): number {
  switch (code) {
    case "invalid_query":
    case "invalid_time_range":
    case "invalid_limit":
    case "invalid_cursor":
      return 400;
    case "vehicle_not_found":
      return 404;
    case "service_unavailable":
      return 503;
    default:
      throw new ProtocolValidationError("HubError.code");
  }
}
