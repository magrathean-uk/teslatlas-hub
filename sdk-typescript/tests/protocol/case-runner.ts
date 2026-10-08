import { createClientSession } from "../../src/client/session.js";
import { TeslatlasClient } from "../../src/client/client.js";
import { asIdempotencyKey } from "../../src/commands/idempotency.js";
import { ProtocolHttpError, ReplayGapError, TeslatlasError } from "../../src/core/errors.js";
import { asEntityTag, asOpaqueCursor } from "../../src/core/opaque-values.js";
import { validateCommandRequest, validateMetadataReplace } from "../../src/generated/validators.js";
import { asStrongEntityTag } from "../../src/http/strong-etag.js";
import type { FetchImplementation } from "../../src/http/fetch-transport.js";
import type { CommandRequest, MetadataReplace, ProtocolEvent } from "../../src/protocol/models.js";
import type { SupportedProtocolVersion } from "../../src/protocol/negotiation.js";
import { decodeProtocolValue } from "../../src/protocol/validate.js";
import {
  bodyForPath,
  resolveCaseSteps,
  type ProtocolCase,
  type ProtocolProfile,
  type ResolvedCaseStep,
} from "./case-loader.js";

interface JsonRecord {
  readonly [key: string]: unknown;
}

type CasePrincipal = "default" | "principal-a" | "principal-b";

interface ActiveRequest {
  readonly step: ResolvedCaseStep;
  readonly principal: CasePrincipal;
}

interface MutableCheckpoint {
  value: string | undefined;
}

export interface CaseTranscriptEntry {
  readonly method: string;
  readonly path: string;
  readonly query: Readonly<Record<string, string>>;
  readonly principal: CasePrincipal;
  readonly authorization: "present" | "absent";
  readonly headers: Readonly<Record<string, string>>;
}

export interface CaseCheckpointEntry {
  readonly stepId: string;
  readonly principal: CasePrincipal;
  readonly values: readonly (string | undefined)[];
}

export interface LocalErrorEvidence {
  readonly label: string;
  readonly code: string;
}

export interface CaseErrorEvidence {
  readonly stepId: string;
  readonly status: number;
  readonly code: string;
  readonly retryable: boolean;
  readonly retryAfterSeconds?: number;
}

export interface ServerConformanceOnlyEvidence {
  readonly stepId: string;
  readonly reason: string;
}

export interface SdkProtocolCaseResult {
  readonly failures: readonly string[];
  readonly transcript: readonly CaseTranscriptEntry[];
  readonly checkpoints: readonly CaseCheckpointEntry[];
  readonly localErrors: readonly LocalErrorEvidence[];
  readonly errors: readonly CaseErrorEvidence[];
  readonly serverConformanceOnly: readonly ServerConformanceOnlyEvidence[];
}

const safeHeaders = [
  "accept",
  "content-type",
  "idempotency-key",
  "if-match",
  "if-none-match",
  "last-event-id",
  "teslatlas-protocol-version",
] as const;

const harnessStop = new Error("protocol case stream complete");

export async function runSdkProtocolCase(
  profile: ProtocolProfile,
  testCase: ProtocolCase,
): Promise<SdkProtocolCaseResult> {
  const harness = new CaseHarness(profile, resolveCaseSteps(profile, testCase));
  const client = await harness.createClient();
  try {
    switch (testCase.caseId) {
      case "command-idempotency":
        await runCommandCase(harness, client);
        break;
      case "cursor-pagination":
        await runCursorCase(harness, client);
        break;
      case "deprecation-sunset":
        await runDeprecationCase(harness, client);
        break;
      case "discovery-version-negotiation":
        await runDiscoveryCase(harness, client);
        break;
      case "documented-limits":
        await runLimitsCase(harness, client);
        break;
      case "etag-conditional-get":
        await runConditionalCase(harness, client);
        break;
      case "metadata-if-match":
        await runMetadataCase(harness, client);
        break;
      case "problem-details":
        await runProblemCase(harness, client);
        break;
      case "sse-empty-id-reset":
      case "sse-last-event-id":
      case "sse-principal-visibility":
      case "sse-terminal-204":
        await runEventCase(harness, client);
        break;
      default:
        harness.fail(`unhandled case ${testCase.caseId}`);
    }
  } catch (error) {
    harness.fail(`runner threw ${safeErrorName(error)}`);
  }
  return Object.freeze({
    failures: Object.freeze([...harness.failures]),
    transcript: Object.freeze([...harness.transcript]),
    checkpoints: Object.freeze([...harness.checkpoints]),
    localErrors: Object.freeze([...harness.localErrors]),
    errors: Object.freeze([...harness.errors]),
    serverConformanceOnly: Object.freeze([...harness.serverConformanceOnly]),
  });
}

class CaseHarness {
  readonly failures: string[] = [];
  readonly transcript: CaseTranscriptEntry[] = [];
  readonly checkpoints: CaseCheckpointEntry[] = [];
  readonly localErrors: LocalErrorEvidence[] = [];
  readonly errors: CaseErrorEvidence[] = [];
  readonly serverConformanceOnly: ServerConformanceOnlyEvidence[] = [];
  readonly #steps: ReadonlyMap<string, ResolvedCaseStep>;
  readonly #responseHeaders = new Map<string, Readonly<Record<string, string>>>();
  readonly #observations = new Map<string, JsonRecord>();
  #active: ActiveRequest | undefined;
  #bootstrapPrincipal: CasePrincipal = "default";

  constructor(
    readonly profile: ProtocolProfile,
    steps: readonly ResolvedCaseStep[],
  ) {
    this.#steps = new Map(steps.map((step) => [step.stepId, step]));
  }

  async createClient(principal: CasePrincipal = "default"): Promise<TeslatlasClient> {
    return this.#createClient(principal, this.profile);
  }

  async createClientForRequestedVersion(
    principal: CasePrincipal,
    requestedProtocolVersion: string,
  ): Promise<TeslatlasClient> {
    return this.#createClient(principal, requestedProtocolVersion);
  }

  async #createClient(
    principal: CasePrincipal,
    requestedProtocolVersion: string | undefined,
  ): Promise<TeslatlasClient> {
    this.#bootstrapPrincipal = principal;
    try {
      const session = await createClientSession({
        baseUrl: "https://hub.example.invalid",
        authorization: () => authorizationFor(principal),
        ...(requestedProtocolVersion === undefined
          ? {}
          : { requestedProtocolVersion: requestedProtocolVersion as SupportedProtocolVersion }),
        fetch: this.fetch,
      });
      return new TeslatlasClient(session);
    } finally {
      this.#bootstrapPrincipal = "default";
    }
  }

  readonly fetch: FetchImplementation = async (input, init) => {
    const url = new URL(String(input));
    const active = this.#active;
    if (active === undefined && url.pathname === "/.well-known/teslatlas-hub") {
      this.recordBootstrap(init, url);
      return jsonResponse(bodyForPath("examples/discovery.json"), 200, {
        "Content-Type": "application/json",
        ETag: '"protocol-case-discovery"',
      });
    }
    if (active === undefined) {
      this.fail(`unexpected request ${url.pathname}`);
      return new Response(null, { status: 500 });
    }
    this.record(active, init, url);
    this.#active = undefined;
    const response = responseFor(active.step.response);
    this.recordResponse(active.step.stepId, active.step, response);
    return response;
  };

  async call<T>(
    stepId: string,
    invoke: () => Promise<T>,
    principal: CasePrincipal = "default",
  ): Promise<{ value?: T; error?: unknown }> {
    this.activate(stepId, principal);
    try {
      const value = await invoke();
      this.verifyOutcome(stepId, value);
      return { value };
    } catch (error) {
      this.verifyError(stepId, error);
      return { error };
    } finally {
      if (this.#active !== undefined) {
        this.fail(`${stepId}: expected request was not dispatched`);
        this.#active = undefined;
      }
    }
  }

  async local<T>(label: string, expectedCode: string, invoke: () => Promise<T>): Promise<void> {
    const before = this.apiDispatchCount();
    try {
      await invoke();
      this.fail(`${label}: expected local rejection`);
    } catch (error) {
      if (!(error instanceof TeslatlasError)) {
        this.fail(`${label}: unsafe local error ${safeErrorName(error)}`);
      } else {
        this.localErrors.push(Object.freeze({ label, code: error.code }));
        if (error.code !== expectedCode) {
          this.fail(`${label}: expected ${expectedCode}, received ${error.code}`);
        }
      }
    }
    if (this.apiDispatchCount() !== before) {
      this.fail(`${label}: local validation dispatched a request`);
    }
  }

  serverOnly(stepId: string, reason: string): void {
    this.serverConformanceOnly.push(Object.freeze({ stepId, reason }));
  }

  step(stepId: string): ResolvedCaseStep {
    const step = this.#steps.get(stepId);
    if (step === undefined) throw new Error(`missing protocol step ${stepId}`);
    return step;
  }

  fail(message: string): void {
    this.failures.push(message);
  }

  #activeStep(stepId: string): ResolvedCaseStep {
    const step = this.step(stepId);
    if (this.#active !== undefined) throw new Error("protocol case request is already active");
    return step;
  }

  activate(stepId: string, principal: CasePrincipal = "default"): void {
    this.#active = Object.freeze({ step: this.#activeStep(stepId), principal });
  }

  recordBootstrap(init: RequestInit | undefined, url: URL): void {
    const headers = new Headers(init?.headers);
    this.transcript.push(
      Object.freeze({
        method: init?.method ?? "GET",
        path: url.pathname,
        query: Object.freeze({}),
        principal: this.#bootstrapPrincipal,
        authorization: headers.has("authorization") ? "present" : "absent",
        headers: Object.freeze({}),
      }),
    );
    if (headers.has("authorization") || headers.has("teslatlas-protocol-version")) {
      this.fail("bootstrap discovery leaked API authorization or version header");
    }
  }

  record(active: ActiveRequest, init: RequestInit | undefined, url: URL): void {
    const { principal, step } = active;
    const headers = new Headers(init?.headers);
    const observedHeaders: Record<string, string> = Object.create(null) as Record<string, string>;
    for (const name of safeHeaders) {
      const value = headers.get(name);
      if (value !== null) observedHeaders[name] = value;
    }
    const query = queryFromUrl(url);
    this.transcript.push(
      Object.freeze({
        method: init?.method ?? "GET",
        path: url.pathname,
        query: Object.freeze(query),
        principal,
        authorization: headers.has("authorization") ? "present" : "absent",
        headers: Object.freeze(observedHeaders),
      }),
    );

    const request = recordOf(step.request);
    const expectedPath = stringOf(request.path);
    if (url.pathname !== expectedPath) this.fail(`${step.stepId}: path differs`);
    if ((init?.method ?? "GET") !== stringOf(request.method))
      this.fail(`${step.stepId}: method differs`);
    if (expectedPath === "/.well-known/teslatlas-hub") {
      if (headers.has("authorization") || headers.has("teslatlas-protocol-version")) {
        this.fail(`${step.stepId}: discovery headers are unsafe`);
      }
    } else if (!headers.has("authorization")) {
      this.fail(`${step.stepId}: authorization provider was not used`);
    } else if (headers.get("authorization") !== authorizationFor(principal)) {
      this.fail(`${step.stepId}: authorization provider differs`);
    }
    if (!sameJson(query, expectedQuery(recordOf(request.query)))) {
      this.fail(`${step.stepId}: query differs`);
    }
    for (const [name, expected] of Object.entries(recordOf(request.headers))) {
      if (name.toLowerCase().startsWith("teslatlas-conformance-")) {
        if (headers.has(name)) this.fail(`${step.stepId}: emitted conformance-only header`);
        continue;
      }
      if (headers.get(name) !== stringOf(expected))
        this.fail(`${step.stepId}: header ${name} differs`);
    }
    for (const [name] of headers) {
      if (name.toLowerCase().startsWith("teslatlas-conformance-")) {
        this.fail(`${step.stepId}: emitted conformance-only header`);
      }
    }
    if (Object.hasOwn(request, "body") && !Object.hasOwn(request, "body_bytes")) {
      const actualBody = init?.body;
      if (typeof actualBody !== "string" || !sameJson(parseJson(actualBody), request.body)) {
        this.fail(`${step.stepId}: JSON body differs`);
      }
    }
  }

  recordResponse(stepId: string, step: ResolvedCaseStep, response: Response): void {
    const expectedHeaders = recordOf(recordOf(step.response).headers);
    const observed: Record<string, string> = Object.create(null) as Record<string, string>;
    for (const name of Object.keys(expectedHeaders)) {
      const value = response.headers.get(name);
      if (value !== null) observed[name] = value;
    }
    this.#responseHeaders.set(stepId, Object.freeze(observed));
  }

  verifyOutcome(stepId: string, value: unknown): void {
    const step = this.step(stepId);
    const response = recordOf(step.response);
    const expected = recordOf(step.expect);
    const status = numberOf(response.status);
    const expectedStatus = this.verifyDeclaredStatus(stepId, expected, response);
    if (status >= 400) {
      this.fail(`${stepId}: expected HTTP problem but call resolved`);
      return;
    }
    const result = recordOf(value);
    const metadata = recordOf(result.metadata);
    if (numberOf(metadata.status) !== expectedStatus) {
      this.fail(`${stepId}: typed status differs`);
    }
    const headers = this.responseHeaders(stepId);
    this.verifyExpectedHeaders(stepId, expected, headers);
    this.verifyMetadata(stepId, headers, metadata);
    const observation: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
    observation.status = numberOf(metadata.status);
    observation.headers = headers;
    if (status === 304) {
      if (result.kind !== "not-modified") this.fail(`${stepId}: expected not-modified result`);
      this.recordObservation(stepId, expected, observation);
      return;
    }
    const actual = valueOfResult(value);
    if (Object.hasOwn(response, "body")) {
      if (!sameJson(actual, response.body)) this.fail(`${stepId}: typed value differs`);
    }
    observation.body = actual;
    this.recordObservation(stepId, expected, observation);
  }

  verifyError(stepId: string, error: unknown): void {
    const step = this.step(stepId);
    const response = recordOf(step.response);
    const expected = recordOf(step.expect);
    const status = numberOf(response.status);
    if (status < 400) {
      if (error !== harnessStop) this.fail(`${stepId}: unexpected ${safeErrorName(error)}`);
      return;
    }
    this.verifyDeclaredStatus(stepId, expected, response);
    const body = recordOf(response.body);
    const code = stringOf(body.code);
    let typedError: TeslatlasError | undefined;
    if (status === 410 && code === "event_replay_expired") {
      if (!(error instanceof ReplayGapError) || error.status !== status) {
        this.fail(`${stepId}: expected ReplayGapError`);
      } else {
        typedError = error;
      }
    } else if (
      !(error instanceof ProtocolHttpError) ||
      error.status !== status ||
      error.code !== code
    ) {
      this.fail(`${stepId}: expected safe ProtocolHttpError`);
    } else {
      typedError = error;
    }
    const headers = this.responseHeaders(stepId);
    this.verifyExpectedHeaders(stepId, expected, headers);
    const requestId = headerValue(headers, "X-Request-ID");
    if (requestId !== undefined && typedError?.requestId !== requestId) {
      this.fail(`${stepId}: typed request ID differs`);
    }
    if (typedError instanceof ProtocolHttpError) {
      const expectedRetryable = booleanOf(body.retryable);
      const expectedRetryAfterSeconds = expectedRetryable
        ? (retryAfterFromHeaders(headers) ?? optionalNumber(body.retry_after_seconds))
        : undefined;
      if (typedError.retryable !== expectedRetryable) {
        this.fail(`${stepId}: typed retryable differs`);
      }
      if (typedError.retryAfterSeconds !== expectedRetryAfterSeconds) {
        this.fail(`${stepId}: typed retry-after differs`);
      }
      this.errors.push(
        Object.freeze({
          stepId,
          status: typedError.status ?? status,
          code: typedError.code,
          retryable: typedError.retryable,
          ...(typedError.retryAfterSeconds === undefined
            ? {}
            : { retryAfterSeconds: typedError.retryAfterSeconds }),
        }),
      );
    }
    const observation: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
    observation.status = typedError?.status ?? status;
    observation.headers = headers;
    observation.body = Object.freeze({
      status: typedError?.status ?? status,
      code: typedError?.code ?? code,
      ...(typedError?.requestId === undefined ? {} : { request_id: typedError.requestId }),
      ...(typedError instanceof ProtocolHttpError ? { retryable: typedError.retryable } : {}),
      ...(typedError instanceof ProtocolHttpError && typedError.retryAfterSeconds !== undefined
        ? { retry_after_seconds: typedError.retryAfterSeconds }
        : {}),
    });
    this.recordObservation(stepId, expected, observation);
  }

  verifyEventOutcome(
    stepId: string,
    events: readonly JsonRecord[],
    lastEventId: string | undefined,
  ): void {
    const step = this.step(stepId);
    const expected = recordOf(step.expect);
    const response = recordOf(step.response);
    const status = this.verifyDeclaredStatus(stepId, expected, response);
    const headers = this.responseHeaders(stepId);
    this.verifyExpectedHeaders(stepId, expected, headers);
    const observation: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
    observation.status = status;
    observation.headers = headers;
    observation.events = Object.freeze([...events]);
    if (Object.hasOwn(response, "last_event_id_after")) {
      observation.last_event_id_after = lastEventId ?? "";
    }
    this.recordObservation(stepId, expected, observation);
  }

  recordCheckpoint(
    stepId: string,
    principal: CasePrincipal,
    values: readonly (string | undefined)[],
  ): void {
    this.checkpoints.push(Object.freeze({ stepId, principal, values: Object.freeze([...values]) }));
  }

  responseHeaders(stepId: string): Readonly<Record<string, string>> {
    const headers = this.#responseHeaders.get(stepId);
    if (headers !== undefined) return headers;
    this.fail(`${stepId}: response was not observed`);
    return Object.freeze({});
  }

  verifyDeclaredStatus(stepId: string, expected: JsonRecord, response: JsonRecord): number {
    const expectedStatus = numberOf(expected.status);
    if (numberOf(response.status) !== expectedStatus) {
      this.fail(`${stepId}: reference response status differs`);
    }
    return expectedStatus;
  }

  verifyExpectedHeaders(
    stepId: string,
    expected: JsonRecord,
    headers: Readonly<Record<string, string>>,
  ): void {
    const equal = optionalRecord(expected.headers_equal);
    if (equal !== undefined) {
      for (const [name, value] of Object.entries(equal)) {
        if (headerValue(headers, name) !== stringOf(value)) {
          this.fail(`${stepId}: response header ${name} differs`);
        }
      }
    }
    if (Object.hasOwn(expected, "headers_present")) {
      for (const name of arrayOf(expected.headers_present)) {
        const header = stringOf(name);
        if (headerValue(headers, header) === undefined) {
          this.fail(`${stepId}: response header ${header} missing`);
        }
      }
    }
  }

  verifyMetadata(
    stepId: string,
    headers: Readonly<Record<string, string>>,
    metadata: JsonRecord,
  ): void {
    const headerMetadata = [
      ["ETag", "etag", "ETag"],
      ["Location", "location", "Location"],
      ["X-Request-ID", "requestId", "request ID"],
      ["Teslatlas-Protocol-Version", "protocolVersion", "protocol version"],
    ] as const;
    for (const [headerName, metadataName, label] of headerMetadata) {
      const header = headerValue(headers, headerName);
      if (header !== undefined && optionalString(metadata[metadataName]) !== header) {
        this.fail(`${stepId}: typed ${label} differs`);
      }
    }
  }

  recordObservation(stepId: string, expected: JsonRecord, value: Record<string, unknown>): void {
    const observation = Object.freeze(value) as JsonRecord;
    this.verifyAssertions(stepId, expected, observation);
    this.#observations.set(stepId, observation);
  }

  verifyAssertions(stepId: string, expected: JsonRecord, observation: JsonRecord): void {
    if (!Object.hasOwn(expected, "assertions")) return;
    for (const value of arrayOf(expected.assertions)) {
      const assertion = recordOf(value);
      const operation = stringOf(assertion.op);
      const path = stringOf(assertion.path);
      const actual = valueAtPointer(observation, path);
      if (operation === "exists") {
        if (actual === missingValue) this.fail(`${stepId}: assertion exists ${path} differs`);
        continue;
      }
      if (operation === "is_empty") {
        if (!isEmptyValue(actual)) this.fail(`${stepId}: assertion is_empty ${path} differs`);
        continue;
      }
      if (operation === "equals") {
        if (
          !Object.hasOwn(assertion, "value") ||
          actual === missingValue ||
          !sameJson(actual, assertion.value)
        ) {
          this.fail(`${stepId}: assertion equals ${path} differs`);
        }
        continue;
      }
      if (operation === "matches") {
        if (!Object.hasOwn(assertion, "value") || typeof actual !== "string") {
          this.fail(`${stepId}: assertion matches ${path} differs`);
          continue;
        }
        try {
          if (!new RegExp(decodeAuthorityRegex(stringOf(assertion.value)), "u").test(actual)) {
            this.fail(`${stepId}: assertion matches ${path} differs`);
          }
        } catch {
          this.fail(`${stepId}: assertion matches ${path} differs`);
        }
        continue;
      }
      if (operation === "same_as" || operation === "not_same_as") {
        const reference = Object.hasOwn(assertion, "ref")
          ? valueAtReference(this.#observations, stringOf(assertion.ref))
          : missingValue;
        const equal =
          actual !== missingValue && reference !== missingValue && sameJson(actual, reference);
        if ((operation === "same_as" && !equal) || (operation === "not_same_as" && equal)) {
          this.fail(`${stepId}: assertion ${operation} ${path} differs`);
        }
        continue;
      }
      this.fail(`${stepId}: unsupported assertion ${operation}`);
    }
  }

  apiDispatchCount(): number {
    return this.transcript.filter((entry) => entry.path !== "/.well-known/teslatlas-hub").length;
  }
}

async function runCommandCase(harness: CaseHarness, client: TeslatlasClient): Promise<void> {
  for (const stepId of ["create", "replay", "conflict", "nuisance-no-retry"] as const) {
    const step = harness.step(stepId);
    const request = recordOf(step.request);
    await harness.call(stepId, () =>
      client.createCommand(commandBody(request), { idempotencyKey: commandKey(request) }),
    );
  }
  await harness.local("missing-confirmation", "protocol_validation", () => {
    const request = recordOf(harness.step("missing-confirmation").request);
    return client.createCommand(commandBody(request), { idempotencyKey: commandKey(request) });
  });
}

async function runCursorCase(harness: CaseHarness, client: TeslatlasClient): Promise<void> {
  for (const stepId of [
    "initial",
    "next",
    "query-mismatch",
    "expired",
    "scope-changed",
    "invalid",
  ] as const) {
    const request = recordOf(harness.step(stepId).request);
    await harness.call(stepId, () =>
      client.listVehicleDrives("vehicle_demo_alpha", historyOptions(request)),
    );
  }
}

async function runDeprecationCase(harness: CaseHarness, client: TeslatlasClient): Promise<void> {
  await harness.call("discovery", () => client.discoverHub());
  await harness.call("response-headers", () => client.listVehicles());
}

async function runDiscoveryCase(harness: CaseHarness, client: TeslatlasClient): Promise<void> {
  await harness.call("discover", () => client.discoverHub());
  harness.serverOnly(
    "omitted-version-selects-minimum",
    "closed SDK requests always send the negotiated protocol profile",
  );
  await harness.call("select-profile", () => client.listVehicles());
  await harness.local("unsupported-major", "incompatible_protocol", () =>
    harness.createClientForRequestedVersion("default", "2.0.0"),
  );
}

async function runLimitsCase(harness: CaseHarness, client: TeslatlasClient): Promise<void> {
  await harness.local("page-size", "invalid_read_options", () =>
    client.listVehicles({ limit: 501 }),
  );
  await harness.local("dense-range", "invalid_read_options", () =>
    client.listDrivePositions("drive_demo_0001", {
      from: "2026-01-01T00:00:00.000Z",
      to: "2026-02-02T00:00:00.000Z",
    }),
  );
  const request = recordOf(harness.step("request-body").request);
  await harness.call("request-body", () =>
    client.createCommand(commandBody(request), {
      idempotencyKey: asIdempotencyKey("22222222-2222-4222-8222-222222222222"),
    }),
  );
  await harness.call("concurrency", () => client.listVehicles());
}

async function runConditionalCase(harness: CaseHarness, client: TeslatlasClient): Promise<void> {
  const initial = await harness.call("initial", () =>
    client.getVehicleCurrentState("vehicle_demo_alpha"),
  );
  const initialValue = initial.value;
  const etag = etagFromResult(initialValue);
  if (etag === undefined) {
    harness.fail("initial: missing typed ETag");
    return;
  }
  await harness.call("not-modified", () =>
    client.getVehicleCurrentState("vehicle_demo_alpha", { ifNoneMatch: asEntityTag(etag) }),
  );
  await harness.call("changed", () => client.getVehicleCurrentState("vehicle_demo_alpha"));
}

async function runMetadataCase(harness: CaseHarness, client: TeslatlasClient): Promise<void> {
  const read = await harness.call("read", () => client.getMetadata("metadata_demo_note_0001"));
  const readTag = etagFromResult(read.value);
  if (readTag === undefined) {
    harness.fail("read: missing typed ETag");
    return;
  }
  const updateRequest = recordOf(harness.step("update").request);
  const update = await harness.call("update", () =>
    client.replaceMetadata("metadata_demo_note_0001", metadataBody(updateRequest), {
      ifMatch: asStrongEntityTag(readTag),
    }),
  );
  await harness.call("stale", () =>
    client.replaceMetadata(
      "metadata_demo_note_0001",
      metadataBody(recordOf(harness.step("stale").request)),
      { ifMatch: asStrongEntityTag(readTag) },
    ),
  );
  const replaceWithoutIfMatch = client.replaceMetadata.bind(client) as unknown as (
    metadataId: string,
    body: MetadataReplace,
    options: unknown,
  ) => Promise<unknown>;
  await harness.local("missing-precondition", "invalid_strong_entity_tag", () =>
    replaceWithoutIfMatch(
      "metadata_demo_note_0001",
      metadataBody(recordOf(harness.step("missing-precondition").request)),
      {},
    ),
  );
  const updateTag = etagFromResult(update.value);
  if (updateTag === undefined) {
    harness.fail("update: missing typed ETag");
    return;
  }
  await harness.call("delete", () =>
    client.deleteMetadata("metadata_demo_note_0001", { ifMatch: asStrongEntityTag(updateTag) }),
  );
  await harness.call("get-tombstone", () => client.getMetadata("metadata_demo_note_0001"));
  await harness.call("list-excludes-tombstone", () =>
    client.listVehicleMetadata("vehicle_demo_alpha", { kind: "note" }),
  );
  await runEventStep(harness, client, "event-tombstone");
}

async function runProblemCase(harness: CaseHarness, client: TeslatlasClient): Promise<void> {
  await harness.local("invalid-range", "invalid_read_options", () =>
    client.listVehicleDrives("vehicle_demo_alpha", {
      from: "2026-09-01T00:00:00.000Z",
      to: "2026-08-01T00:00:00.000Z",
    }),
  );
}

async function runEventCase(harness: CaseHarness, client: TeslatlasClient): Promise<void> {
  try {
    harness.step("principal-a-resource");
  } catch {
    await runOrdinaryEventCase(harness, client);
    return;
  }

  const principalA = await harness.createClient("principal-a");
  const principalB = await harness.createClient("principal-b");
  const checkpointA: MutableCheckpoint = { value: undefined };
  const checkpointB: MutableCheckpoint = { value: undefined };
  await harness.call(
    "principal-a-resource",
    () => principalA.getVehicleCurrentState("vehicle_demo_alpha"),
    "principal-a",
  );
  await runEventStep(harness, principalA, "principal-a-live", "principal-a", checkpointA);
  await harness.call(
    "principal-b-resource",
    () => principalB.getVehicleCurrentState("vehicle_demo_alpha"),
    "principal-b",
  );
  await runEventStep(harness, principalB, "principal-b-live", "principal-b", checkpointB);
  await runEventStep(harness, principalB, "cross-principal-replay", "principal-b", checkpointA);
}

async function runOrdinaryEventCase(harness: CaseHarness, client: TeslatlasClient): Promise<void> {
  const all = ["reset", "initial", "resume", "expired", "invalid", "terminal"] as const;
  for (const stepId of all) {
    let step: ResolvedCaseStep;
    try {
      step = harness.step(stepId);
    } catch {
      continue;
    }
    const request = recordOf(step.request);
    if (stringOf(request.path) === "/v1/events") {
      await runEventStep(harness, client, stepId);
    } else {
      await harness.call(stepId, () => client.getVehicleCurrentState("vehicle_demo_alpha"));
    }
  }
}

async function runEventStep(
  harness: CaseHarness,
  client: TeslatlasClient,
  stepId: string,
  principal: CasePrincipal = "default",
  checkpointState?: MutableCheckpoint,
): Promise<void> {
  const step = harness.step(stepId);
  const request = recordOf(step.request);
  const response = recordOf(step.response);
  const checkpointValues: Array<string | undefined> = [];
  const headers = recordOf(request.headers);
  const seeded = optionalString(headers["Last-Event-ID"]);
  const checkpoint = {
    load: () => checkpointState?.value ?? seeded,
    save: (value: string | undefined) => {
      checkpointValues.push(value);
      if (checkpointState !== undefined) checkpointState.value = value;
    },
  };
  const options = {
    ...eventOptions(request),
    checkpoint,
    sleep: async () => {
      throw harnessStop;
    },
  };
  const status = numberOf(response.status);
  const expectedEvents = arrayOf(response.events);
  const observedEvents: JsonRecord[] = [];
  harness.activate(stepId, principal);
  const iterator = client.streamEvents(options)[Symbol.asyncIterator]();
  try {
    if (status >= 400) {
      await iterator.next();
      harness.fail(`${stepId}: expected terminal event error`);
    } else if (status === 204) {
      const result = await iterator.next();
      if (!result.done) harness.fail(`${stepId}: terminal stream yielded data`);
      harness.verifyEventOutcome(stepId, observedEvents, checkpointValues.at(-1));
    } else {
      for (const expected of expectedEvents) {
        const result = await iterator.next();
        if (result.done || !sameJson(result.value, recordOf(expected).data)) {
          harness.fail(`${stepId}: event differs`);
        } else {
          const event = recordOf(result.value);
          observedEvents.push(
            Object.freeze({
              id: event.event_id,
              event: event.event_type,
              data: result.value,
            }),
          );
        }
      }
      await requireSseReconnect(harness, stepId, iterator);
      harness.verifyEventOutcome(stepId, observedEvents, checkpointValues.at(-1));
    }
  } catch (error) {
    harness.verifyError(stepId, error);
  } finally {
    // The active response is consumed by the injected Fetch adapter; reconnect is stopped by sleep.
  }
  const rawCheckpoint = optionalString(response.last_event_id_after);
  const expectedCheckpoint = rawCheckpoint === "" ? undefined : rawCheckpoint;
  if (Object.hasOwn(response, "last_event_id_after")) {
    const expectedValues =
      expectedEvents.length === 0
        ? [expectedCheckpoint]
        : expectedEvents.map((event) => normalizeExpectedCheckpoint(recordOf(event).id));
    if (!sameJson(checkpointValues, expectedValues)) {
      harness.fail(`${stepId}: checkpoint order differs`);
    }
  }
  harness.recordCheckpoint(stepId, principal, checkpointValues);
}

async function requireSseReconnect(
  harness: CaseHarness,
  stepId: string,
  iterator: AsyncIterator<ProtocolEvent>,
): Promise<void> {
  try {
    const result = await iterator.next();
    if (result.done) {
      harness.fail(`${stepId}: 200 stream ended without reconnect`);
    } else {
      harness.fail(`${stepId}: stream yielded an extra event`);
    }
  } catch (error) {
    if (error !== harnessStop) throw error;
  }
}

function commandBody(request: JsonRecord): CommandRequest {
  return decodeProtocolValue<CommandRequest>(
    request.body,
    validateCommandRequest,
    "validateCommandRequest",
  );
}

function commandKey(request: JsonRecord) {
  return asIdempotencyKey(stringOf(recordOf(request.headers)["Idempotency-Key"]));
}

function metadataBody(request: JsonRecord): MetadataReplace {
  return decodeProtocolValue<MetadataReplace>(
    request.body,
    validateMetadataReplace,
    "validateMetadataReplace",
  );
}

function historyOptions(request: JsonRecord) {
  const query = recordOf(request.query);
  return {
    ...(query.cursor === undefined ? {} : { cursor: asOpaqueCursor(stringOf(query.cursor)) }),
    ...(query.limit === undefined ? {} : { limit: numberOf(query.limit) }),
    ...(query.from === undefined ? {} : { from: stringOf(query.from) }),
    ...(query.to === undefined ? {} : { to: stringOf(query.to) }),
  };
}

function eventOptions(request: JsonRecord) {
  const query = recordOf(request.query);
  const eventTypes = query.event_type;
  const values = Array.isArray(eventTypes)
    ? eventTypes.map((value) => stringOf(value))
    : eventTypes === undefined
      ? undefined
      : stringOf(eventTypes).split(",");
  return {
    ...(query.vehicle_id === undefined ? {} : { vehicleId: stringOf(query.vehicle_id) }),
    ...(values === undefined ? {} : { eventTypes: values as ProtocolEvent["event_type"][] }),
  };
}

function responseFor(response: JsonRecord): Response {
  const status = numberOf(response.status);
  const headers = headersFrom(recordOf(response.headers));
  if (status === 204 || status === 304) return new Response(null, { status, headers });
  if (Object.hasOwn(response, "events") || headers.get("Content-Type") === "text/event-stream") {
    return new Response(eventWire(response), { status, headers });
  }
  if (Object.hasOwn(response, "body")) return jsonResponse(response.body, status, headers);
  return new Response(null, { status, headers });
}

function eventWire(response: JsonRecord): string {
  const events = arrayOf(response.events);
  if (events.length === 0) {
    return response.last_event_id_after === "" ? "id:\n\n" : "";
  }
  return events
    .map((entry) => {
      const event = recordOf(entry);
      return `id: ${stringOf(event.id)}\nevent: ${stringOf(event.event)}\ndata: ${JSON.stringify(event.data)}\n\n`;
    })
    .join("");
}

function jsonResponse(body: unknown, status: number, headers: HeadersInit): Response {
  return Response.json(body, { status, headers });
}

function headersFrom(value: JsonRecord): Headers {
  const headers = new Headers();
  for (const [name, header] of Object.entries(value)) headers.set(name, stringOf(header));
  return headers;
}

function queryFromUrl(url: URL): Record<string, string> {
  const output: Record<string, string> = Object.create(null) as Record<string, string>;
  for (const [name, value] of url.searchParams) {
    if (Object.hasOwn(output, name)) output[name] = `${output[name]},${value}`;
    else output[name] = value;
  }
  return output;
}

function expectedQuery(value: JsonRecord): Record<string, string> {
  const output: Record<string, string> = Object.create(null) as Record<string, string>;
  for (const [name, entry] of Object.entries(value)) {
    output[name] = Array.isArray(entry)
      ? entry.map((item) => stringOf(item)).join(",")
      : String(entry);
  }
  return output;
}

function valueOfResult(value: unknown): unknown {
  const record = recordOf(value);
  if (record.kind === "modified") return record.value;
  if (Object.hasOwn(record, "value")) return record.value;
  return value;
}

function etagFromResult(value: unknown): string | undefined {
  const record = recordOf(value);
  const metadata = recordOf(record.metadata);
  return optionalString(metadata.etag);
}

function parseJson(value: string): unknown {
  try {
    return JSON.parse(value) as unknown;
  } catch {
    return undefined;
  }
}

function recordOf(value: unknown): JsonRecord {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("expected generated JSON record");
  }
  return value as JsonRecord;
}

function arrayOf(value: unknown): readonly unknown[] {
  return Array.isArray(value) ? value : [];
}

function stringOf(value: unknown): string {
  if (typeof value !== "string") throw new Error("expected generated JSON string");
  return value;
}

function optionalString(value: unknown): string | undefined {
  return value === undefined ? undefined : stringOf(value);
}

function optionalNumber(value: unknown): number | undefined {
  return value === undefined ? undefined : numberOf(value);
}

function optionalRecord(value: unknown): JsonRecord | undefined {
  return value === undefined ? undefined : recordOf(value);
}

function numberOf(value: unknown): number {
  if (typeof value !== "number") throw new Error("expected generated JSON number");
  return value;
}

function booleanOf(value: unknown): boolean {
  if (typeof value !== "boolean") throw new Error("expected generated JSON boolean");
  return value;
}

function authorizationFor(principal: CasePrincipal): string {
  return `Bearer protocol-case-${principal}`;
}

function headerValue(headers: Readonly<Record<string, string>>, name: string): string | undefined {
  const expected = name.toLowerCase();
  for (const [candidate, value] of Object.entries(headers)) {
    if (candidate.toLowerCase() === expected) return value;
  }
  return undefined;
}

function retryAfterFromHeaders(headers: Readonly<Record<string, string>>): number | undefined {
  const value = headerValue(headers, "Retry-After");
  if (value === undefined) return undefined;
  if (!/^(0|[1-9][0-9]*)$/u.test(value)) return undefined;
  const seconds = Number(value);
  return Number.isSafeInteger(seconds) && seconds <= 86_400 ? seconds : undefined;
}

function normalizeExpectedCheckpoint(value: unknown): string | undefined {
  const eventId = stringOf(value);
  return eventId.length === 0 ? undefined : eventId;
}

function decodeAuthorityRegex(value: string): string {
  return value.replaceAll('\\"', '"');
}

const missingValue = Symbol("missing protocol assertion value");

function valueAtPointer(value: unknown, path: string): unknown | typeof missingValue {
  if (path === "") return value;
  if (!path.startsWith("/")) return missingValue;
  let current: unknown = value;
  for (const encodedPart of path.slice(1).split("/")) {
    const part = decodeJsonPointerPart(encodedPart);
    if (part === undefined) return missingValue;
    if (Array.isArray(current)) {
      if (!/^(0|[1-9][0-9]*)$/u.test(part)) return missingValue;
      const item = current[Number(part)];
      if (item === undefined && Number(part) >= current.length) return missingValue;
      current = item;
      continue;
    }
    if (current === null || typeof current !== "object" || !Object.hasOwn(current, part)) {
      return missingValue;
    }
    current = (current as Record<string, unknown>)[part];
  }
  return current;
}

function valueAtReference(
  observations: ReadonlyMap<string, JsonRecord>,
  reference: string,
): unknown | typeof missingValue {
  const [stepId, ...parts] = reference.split(".");
  if (stepId === undefined || stepId.length === 0) return missingValue;
  let current: unknown = observations.get(stepId);
  if (current === undefined) return missingValue;
  for (const part of parts) {
    if (Array.isArray(current)) {
      if (!/^(0|[1-9][0-9]*)$/u.test(part)) return missingValue;
      const item = current[Number(part)];
      if (item === undefined && Number(part) >= current.length) return missingValue;
      current = item;
      continue;
    }
    if (current === null || typeof current !== "object" || !Object.hasOwn(current, part)) {
      return missingValue;
    }
    current = (current as Record<string, unknown>)[part];
  }
  return current;
}

function decodeJsonPointerPart(value: string): string | undefined {
  let result = "";
  for (let index = 0; index < value.length; index += 1) {
    const character = value[index];
    if (character !== "~") {
      result += character;
      continue;
    }
    const escaped = value[index + 1];
    if (escaped === "0") result += "~";
    else if (escaped === "1") result += "/";
    else return undefined;
    index += 1;
  }
  return result;
}

function isEmptyValue(value: unknown): boolean {
  if (value === missingValue) return false;
  if (typeof value === "string" || Array.isArray(value)) return value.length === 0;
  return value !== null && typeof value === "object" && Object.keys(value).length === 0;
}

function sameJson(left: unknown, right: unknown): boolean {
  if (left === right) return true;
  if (typeof left !== typeof right || left === null || right === null) return false;
  if (Array.isArray(left) || Array.isArray(right)) {
    if (!Array.isArray(left) || !Array.isArray(right) || left.length !== right.length) return false;
    return left.every((value, index) => sameJson(value, right[index]));
  }
  if (typeof left !== "object" || typeof right !== "object") return false;
  const leftRecord = recordOf(left);
  const rightRecord = recordOf(right);
  const leftKeys = Object.keys(leftRecord).sort();
  const rightKeys = Object.keys(rightRecord).sort();
  if (leftKeys.length !== rightKeys.length) return false;
  return leftKeys.every(
    (key, index) => key === rightKeys[index] && sameJson(leftRecord[key], rightRecord[key]),
  );
}

function safeErrorName(error: unknown): string {
  return error instanceof Error ? error.name : typeof error;
}
