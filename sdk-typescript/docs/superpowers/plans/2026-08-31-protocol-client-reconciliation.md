# Teslatlas TypeScript SDK protocol-client reconciliation implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the tested transport foundation into a reproducible, protocol-derived browser and Node.js client for all 20 public Teslatlas operations while selectively retaining the valid invariants from the concurrent documentation-only SDK design.

**Architecture:** Pin and vendor the validated protocol revision, generate private OpenAPI types and standalone runtime validators, then layer a closed `TeslatlasClient` over the existing safe Fetch and SSE cores. Browser and Node.js factories share one implementation; credentials and checkpoints remain caller-owned; generated internals and arbitrary-path transport APIs are removed from package exports.

**Tech Stack:** Node.js 26.7.0, npm 11.19.0, TypeScript 5.9.3, ESM/ES2022, OpenAPI 3.1.1, JSON Schema 2020-12, `openapi-typescript` 7.13.0, Ajv 8.20.0, `ajv-formats` 3.0.1, Vitest 4.1.11, Playwright 1.62.1, Biome 2.5.11.

**Spec:** `docs/superpowers/specs/2026-08-31-protocol-client-reconciliation-design.md`

## Global constraints

- Work on canonical SDK `main`; leave the documentation-only clone untouched.
- Protocol authority is `/Users/bolyki/dev/source/teslatlas-protocol` at exact commit `79ced4c7fdc79520ad31d72a0280bf5f3f19f407`.
- Supported protocol profiles are exactly `1.0.0`, `1.1.0`, and `1.2.0`; the generated contract is profile `1.2.0` with capability gates for minor-version additions.
- Vendored protocol bytes and generated output must be SHA-256 locked and regenerable without network access after `npm ci`.
- JSON field names remain protocol wire names; do not add camelCase payload copies.
- No public arbitrary-path request API, private route, pairing flow, identity-proof policy, credential store, body cache, checkpoint store, TLS policy, viewer code, or Hub implementation.
- No automatic ordinary-request retry or command reissue. Once a command request is handed to Fetch, a missing conforming response is uncertain.
- Public errors retain only safe typed fields; never retain arbitrary causes, raw bodies, authorization values, or credential-bearing URLs.
- The caller scopes each SSE checkpoint store to its authorization principal and stable filters; the SDK neither identifies principals nor retries a terminal cross-principal `event_id_invalid` response.
- No GitHub Actions, Dependabot, hosted CI, release automation, npm publication, or repeated GitHub connections. Push once after all local gates.
- Do not touch `/Users/bolyki/dev/source/teslatlas-service/app`.
- Use TDD: add a focused failing test, observe the intended failure, implement, then rerun focused and full affected suites.

---

## File map

| Unit | Files | Responsibility |
| --- | --- | --- |
| Protocol snapshot | `protocol/lock.json`, `protocol/source/**` | Exact public authority bytes and digests |
| Generation | `scripts/protocol-files.mjs`, `scripts/sync-protocol.mjs`, `scripts/generate-protocol.mjs`, `scripts/check-protocol.mjs` | Allowlisted copy, OpenAPI generation, Ajv standalone validators, lock verification |
| Generated internals | `src/generated/protocol.ts`, `src/generated/validators.ts`, `src/generated/protocol-cases.ts` | Private wire declarations, validator functions, browser-safe case data |
| Protocol facade | `src/protocol/models.ts`, `src/protocol/validate.ts`, `src/protocol/negotiation.ts`, `src/protocol/capabilities.ts` | Stable exported aliases, runtime decoding, version/capability selection |
| Client core | `src/client/types.ts`, `src/client/session.ts`, `src/client/client.ts`, `src/client/operations.ts` | Lifecycle, descriptor, typed methods, safe results |
| HTTP | `src/http/fetch-transport.ts`, `src/http/request-builder.ts`, `src/http/response-decoder.ts` | Internal one-shot requests, fixed templates, safe response/problem decoding |
| Events | `src/events/protocol-subscription.ts`, existing parser/subscription files | Typed events, replay, terminal status, reconnect policy |
| Runtime entry points | `src/index.ts`, `src/browser.ts`, `src/node.ts` | Public types plus browser/Node `createClient` factories |
| Verification | `tests/unit/**`, `tests/conformance/**`, `tests/protocol/**`, `scripts/check-pack.mjs` | Unit, Node/Chromium parity, protocol-case consumption, package boundary |

---

### Task 1: Pin protocol inputs and generate reproducible types and validators

**Files:**

- Modify: `package.json`
- Modify: `package-lock.json`
- Modify: `biome.json`
- Create: `protocol/lock.json`
- Create: `protocol/source/openapi/teslatlas-v1.openapi.json`
- Create: `protocol/source/events/teslatlas-v1.sse.json`
- Create: `protocol/source/schemas/*.schema.json`
- Create: `protocol/source/examples/**/*.json`
- Create: `protocol/source/fixtures/**/*.json`
- Create: `protocol/source/compatibility/**/*.json`
- Create: `protocol/source/conformance/cases/*.json`
- Create: `scripts/protocol-files.mjs`
- Create: `scripts/sync-protocol.mjs`
- Create: `scripts/generate-protocol.mjs`
- Create: `scripts/check-protocol.mjs`
- Create: `src/generated/protocol.ts`
- Create: `src/generated/validators.ts`
- Create: `src/generated/protocol-cases.ts`
- Create: `tests/unit/protocol-lock.test.ts`
- Create: `tests/unit/protocol-validation.test.ts`

**Interfaces:**

- Consumes: exact protocol checkout `79ced4c7fdc79520ad31d72a0280bf5f3f19f407`.
- Produces: `components`, `operations`, and `paths` declarations; named runtime validators; locked browser-safe protocol cases.

- [ ] **Step 1: Add failing lock and validator tests**

Create `tests/unit/protocol-lock.test.ts` with these assertions:

```ts
import { execFile } from "node:child_process";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { describe, expect, it } from "vitest";

const execFileAsync = promisify(execFile);
const root = fileURLToPath(new URL("../..", import.meta.url));

describe("protocol lock", () => {
  it("pins the validated authority revision and all three profiles", async () => {
    const lock = JSON.parse(await readFile(new URL("../../protocol/lock.json", import.meta.url), "utf8"));
    expect(lock).toMatchObject({
      commit: "79ced4c7fdc79520ad31d72a0280bf5f3f19f407",
      currentProfile: "1.2.0",
      supportedProfiles: ["1.0.0", "1.1.0", "1.2.0"],
      generator: { package: "openapi-typescript", version: "7.13.0" },
    });
    expect(Object.keys(lock.files).length).toBeGreaterThan(40);
  });

  it("passes the offline byte and generation check", async () => {
    const result = await execFileAsync(process.execPath, ["scripts/check-protocol.mjs"], {
      cwd: root,
      encoding: "utf8",
    });
    expect(result.stderr).toBe("");
    expect(result.stdout).toContain("Protocol lock verified");
  });
});
```

Create `tests/unit/protocol-validation.test.ts` and import the not-yet-existing named decoders:

```ts
import discovery from "../../protocol/source/examples/discovery.json" with { type: "json" };
import invalidError from "../../protocol/source/examples/invalid/error-success-status.json" with { type: "json" };
import vehicles from "../../protocol/source/examples/vehicles-page.json" with { type: "json" };
import { describe, expect, it } from "vitest";
import { validateDiscovery, validateProblem, validateVehiclePage } from "../../src/generated/validators.js";

describe("generated protocol validators", () => {
  it("accepts canonical discovery and vehicle-page examples", () => {
    expect(validateDiscovery(discovery)).toBe(true);
    expect(validateVehiclePage(vehicles)).toBe(true);
  });

  it("rejects a problem document with a success status", () => {
    expect(validateProblem(invalidError)).toBe(false);
  });
});
```

- [ ] **Step 2: Run the focused tests and confirm RED**

Run:

```bash
npm run test:unit -- --run tests/unit/protocol-lock.test.ts tests/unit/protocol-validation.test.ts
```

Expected: FAIL because `protocol/lock.json`, vendored examples, and generated validator modules do not exist.

- [ ] **Step 3: Install exact generation dependencies and add scripts**

Run:

```bash
npm install --save-dev --save-exact openapi-typescript@7.13.0
npm install --save --save-exact ajv@8.20.0 ajv-formats@3.0.1
```

Pin TypeScript to `5.9.3`. `openapi-typescript@7.13.0` declares
`typescript: ^5.x` and uses the TypeScript factory API; do not retain the
baseline TypeScript 7 pin because generation crashes under it.

Add these package scripts:

```json
{
  "protocol:sync": "node scripts/sync-protocol.mjs",
  "protocol:generate": "node scripts/generate-protocol.mjs",
  "protocol:check": "node scripts/check-protocol.mjs"
}
```

Exclude `src/generated/**` and `protocol/**` from Biome so formatting cannot
alter generator-locked or authority-locked bytes. Prefix generated validator
output with `// @ts-nocheck` and `// @generated`; the checked public wrapper
supplies its narrow typed surface.

In `scripts/protocol-files.mjs`, export immutable allowlists:

```js
export const protocolCommit = "79ced4c7fdc79520ad31d72a0280bf5f3f19f407";
export const currentProfile = "1.2.0";
export const supportedProfiles = ["1.0.0", "1.1.0", "1.2.0"];
export const sourceGlobs = [
  "openapi/teslatlas-v1.openapi.json",
  "events/teslatlas-v1.sse.json",
  "schemas/*.schema.json",
  "examples/**/*.json",
  "fixtures/**/*.json",
  "compatibility/**/*.json",
  "conformance/cases/*.json",
];
```

Implement recursive JSON discovery without shell globs, lexicographic sorting,
SHA-256 using `node:crypto`, and path-containment checks using `resolve()` plus
`relative()`. `sync-protocol.mjs` must:

1. require one absolute checkout argument;
2. run `git -C <checkout> rev-parse HEAD` and compare exact commit;
3. copy only matching regular JSON files into `protocol/source/`;
4. reject symlinks and any path outside the checkout;
5. write a stable-key-ordered `protocol/lock.json` with per-file hashes;
6. invoke `scripts/generate-protocol.mjs`;
7. record hashes for all three generated files.

Before a sync, verify the supplied checkout is already detached or checked out
at the lock target. Never move the authority checkout from the SDK task. If it
has advanced, create a disposable detached worktree at the lock target and pass
that path to `protocol:sync`; remove that temporary worktree only after the
sync and check have succeeded.

- [ ] **Step 4: Generate private OpenAPI declarations, standalone validators, and case data**

`scripts/generate-protocol.mjs` must call the locally installed generator with
offline npm execution:

```js
execFileSync(npmExecutable, [
  "exec", "--offline", "--", "openapi-typescript",
  "protocol/source/openapi/teslatlas-v1.openapi.json",
  "--output", "src/generated/protocol.ts",
], { cwd: repositoryRoot, stdio: "inherit" });
```

Register every canonical schema with `Ajv2020`, apply `ajv-formats`, and emit
ESM standalone validators with these exact exports:

```js
const validatorRefs = {
  validateDiscovery: "urn:teslatlas:protocol:schema:discovery:1.2.0",
  validateProblem: "urn:teslatlas:protocol:schema:error:1.2.0",
  validateVehiclePage: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/vehicle_page",
  validateCurrentState: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/current_state",
  validateDrivePage: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/drive_page",
  validateDrive: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/drive",
  validatePositionPage: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/position_page",
  validateChargePage: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/charge_page",
  validateCharge: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/charge",
  validateChargeSamplePage: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/charge_sample_page",
  validateStatePage: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/state_page",
  validateUpdatePage: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/update_page",
  validateDataQualityPage: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/data_quality_page",
  validateCommandRequest: "urn:teslatlas:protocol:schema:command:1.2.0#/$defs/command_request",
  validateCommandJob: "urn:teslatlas:protocol:schema:command:1.2.0#/$defs/command_job",
  validateMetadataPage: "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/metadata_page",
  validateMetadataCreate: "urn:teslatlas:protocol:schema:metadata:1.2.0#/$defs/metadata_create",
  validateMetadataReplace: "urn:teslatlas:protocol:schema:metadata:1.2.0#/$defs/metadata_replace",
  validateMetadataRecord: "urn:teslatlas:protocol:schema:metadata:1.2.0#/$defs/metadata_record",
  validateMetadataTombstone: "urn:teslatlas:protocol:schema:metadata:1.2.0#/$defs/metadata_tombstone",
  validateEvent: "urn:teslatlas:protocol:schema:event:1.2.0",
};
```

Use Ajv `code: { esm: true, source: true }`. Ajv 8 still emits its three
runtime helper expressions as CommonJS `require(...)` calls; deterministically
replace only those fixed expressions with static ESM imports and fail generation
if any `require(` remains. This keeps validators browser-loadable while the
exact Ajv package and resulting bytes stay locked.

Generate `src/generated/protocol-cases.ts` as a frozen JSON-compatible array
containing compatibility manifests, case documents, and referenced valid
examples. The generated module must contain no absolute path or timestamp.

- [ ] **Step 5: Sync from the exact protocol checkout and verify GREEN**

First verify this checkout remains at the exact pin:

```bash
git -C /Users/bolyki/dev/source/teslatlas-protocol rev-parse HEAD
```

The expected output is `79ced4c7fdc79520ad31d72a0280bf5f3f19f407`. If it
differs, create a disposable detached worktree at that exact commit and use its
path instead. Then run:

```bash
npm run protocol:sync -- /Users/bolyki/dev/source/teslatlas-protocol
npm run protocol:check
npm run test:unit -- --run tests/unit/protocol-lock.test.ts tests/unit/protocol-validation.test.ts
npm run typecheck
```

Expected: lock and generated output checks pass; both focused test files pass;
typecheck passes.

- [ ] **Step 6: Commit the coherent protocol-intake layer**

```bash
git add package.json package-lock.json biome.json protocol scripts src/generated tests/unit/protocol-lock.test.ts tests/unit/protocol-validation.test.ts
git commit -m "feat: pin and generate Teslatlas protocol contracts"
```

---

### Task 2: Add safe protocol models, validation, negotiation, and client session

**Files:**

- Create: `src/protocol/models.ts`
- Create: `src/protocol/validate.ts`
- Create: `src/protocol/negotiation.ts`
- Create: `src/protocol/capabilities.ts`
- Create: `src/client/types.ts`
- Create: `src/client/session.ts`
- Modify: `src/core/errors.ts`
- Modify: `src/core/opaque-values.ts`
- Modify: `src/events/sse-subscription.ts`
- Create: `tests/unit/protocol-models.test.ts`
- Create: `tests/unit/negotiation.test.ts`
- Create: `tests/unit/client-session.test.ts`
- Modify: `tests/unit/errors.test.ts`
- Modify: `tests/unit/sse-subscription.test.ts`

**Interfaces:**

- Consumes: generated protocol declarations/validators and internal `FetchTransport`.
- Produces: exported protocol aliases, safe error taxonomy, negotiated `ClientSession`.

- [ ] **Step 1: Write failing model, negotiation, and session tests**

Treat JSON imports as untrusted runtime data. Validate them before obtaining
the exact generated-derived types; do not widen protocol literal fields merely
to make unchecked JSON imports assignable:

```ts
import type { HubDescriptor, VehiclePage } from "../../src/protocol/models.js";
import discovery from "../../protocol/source/examples/discovery.json" with { type: "json" };
import vehicles from "../../protocol/source/examples/vehicles-page.json" with { type: "json" };
import { decodeProtocolValue } from "../../src/protocol/validate.js";
import { validateDiscovery, validateVehiclePage } from "../../src/generated/validators.js";

const descriptor: HubDescriptor = decodeProtocolValue<HubDescriptor>(discovery, validateDiscovery, "discovery");
const page: VehiclePage = decodeProtocolValue<VehiclePage>(vehicles, validateVehiclePage, "vehicle_page");
expect(descriptor.protocol.current_version).toBe("1.2.0");
expect(page.items.length).toBeGreaterThan(0);
```

In `tests/unit/negotiation.test.ts`, cover exact selection and failure:

```ts
const descriptor = decodeProtocolValue<HubDescriptor>(discovery, validateDiscovery, "discovery");
expect(negotiateProtocolVersion(descriptor, "1.2.0")).toBe("1.2.0");
expect(negotiateProtocolVersion(descriptor, "1.1.0")).toBe("1.1.0");
expect(() => negotiateProtocolVersion(descriptor, "2.0.0")).toThrow(IncompatibleProtocolError);
expect(requireCapability(descriptor, "commands.async").id).toBe("commands.async");
expect(() => requireCapability(descriptor, "missing.capability")).toThrow(MissingCapabilityError);
```

In `tests/unit/client-session.test.ts`, inject Fetch and assert:

```ts
const session = await createClientSession({
  baseUrl: "https://hub.example.invalid",
  authorization: () => "Bearer redacted-test-value",
  fetch: discoveryFixtureFetch,
  requestedProtocolVersion: "1.2.0",
});
expect(session.protocolVersion).toBe("1.2.0");
expect(session.descriptor.hub_id).toBe("hub_demo_uk_0001");
expect(observed[0]).toMatchObject({
  url: "https://hub.example.invalid/.well-known/teslatlas-hub",
  authorization: null,
});
```

Also prove that a non-loopback `http:` bootstrap URL fails before any Fetch,
discovery uses `redirect: "error"`, a schema-valid non-`200` discovery response
does not create a session, and an abort while reading discovery body propagates
the supplied signal reason unchanged.

Add malformed JavaScript checkpoint values to the SSE test using a cast and
assert `InvalidSseCheckpointError`, never raw `TypeError`.

- [ ] **Step 2: Run focused tests and confirm RED**

```bash
npm run test:unit -- --run tests/unit/protocol-models.test.ts tests/unit/negotiation.test.ts tests/unit/client-session.test.ts tests/unit/errors.test.ts tests/unit/sse-subscription.test.ts
```

Expected: FAIL because protocol facade/session modules and new errors do not exist; the malformed checkpoint currently escapes the explicit validation path.

- [ ] **Step 3: Export protocol-derived aliases without duplicating fields**

`src/protocol/models.ts` must derive public aliases from generated declarations
without repeating wire fields. `openapi-typescript` emits JSON Schema `$defs`
as a required type-level property on a schema root even though it is not a
runtime payload member; omit that generator-only property at affected roots:

```ts
import type { components } from "../generated/protocol.js";

export type HubDescriptor = Omit<components["schemas"]["Discovery"], "$defs">;
export type VehiclePage = components["schemas"]["Resources"]["$defs"]["vehicle_page"];
export type CurrentState = components["schemas"]["Resources"]["$defs"]["current_state"];
export type DrivePage = components["schemas"]["Resources"]["$defs"]["drive_page"];
export type Drive = components["schemas"]["Resources"]["$defs"]["drive"];
export type PositionPage = components["schemas"]["Resources"]["$defs"]["position_page"];
export type ChargePage = components["schemas"]["Resources"]["$defs"]["charge_page"];
export type Charge = components["schemas"]["Resources"]["$defs"]["charge"];
export type ChargeSamplePage = components["schemas"]["Resources"]["$defs"]["charge_sample_page"];
export type StatePage = components["schemas"]["Resources"]["$defs"]["state_page"];
export type UpdatePage = components["schemas"]["Resources"]["$defs"]["update_page"];
export type DataQualityPage = components["schemas"]["Resources"]["$defs"]["data_quality_page"];
export type MetadataPage = components["schemas"]["Resources"]["$defs"]["metadata_page"];
export type MetadataCreate = components["schemas"]["Metadata"]["$defs"]["metadata_create"];
export type MetadataReplace = components["schemas"]["Metadata"]["$defs"]["metadata_replace"];
export type MetadataRecord = components["schemas"]["Metadata"]["$defs"]["metadata_record"];
export type MetadataTombstone = components["schemas"]["Metadata"]["$defs"]["metadata_tombstone"];
export type CommandRequest = components["schemas"]["Command"]["$defs"]["command_request"];
export type CommandJob = components["schemas"]["Command"]["$defs"]["command_job"];
export type ProtocolEvent = components["schemas"]["Event"];
export type ProtocolProblem = Omit<components["schemas"]["Problem"], "$defs">;
```

Do not copy or transform payload fields at runtime. If a later exposed alias
contains another generated root `$defs` artifact, apply the same narrow
`Omit<T, "$defs">` helper only to that alias and add a direct canonical-example
assignment test before doing so.

Wrap generated boolean validators in `decodeProtocolValue<T>()`; on failure
throw `ProtocolValidationError` with validator name only. Never expose Ajv
error objects because they can contain payload fragments.

- [ ] **Step 4: Implement safe errors and runtime guards**

Add these public classes with static messages and safe scalar fields:

```ts
export class IncompatibleProtocolError extends TeslatlasError<"incompatible_protocol"> {
  constructor() {
    super("Teslatlas protocol versions are incompatible", {
      code: "incompatible_protocol",
    });
  }
}
export class MissingCapabilityError extends TeslatlasError<"missing_capability"> {
  readonly capability: CapabilityId;

  constructor(capability: CapabilityId) {
    super("Teslatlas capability is unavailable", { code: "missing_capability" });
    this.capability = capability;
  }
}
export class ProtocolValidationError extends TeslatlasError<"protocol_validation"> {
  readonly validator: string;

  constructor(validator: string) {
    super("Teslatlas protocol response is invalid", { code: "protocol_validation" });
    this.validator = validator;
  }
}
export class ProtocolHttpError extends TeslatlasError<ProtocolErrorCode> {
  readonly retryable: boolean;
  readonly retryAfterSeconds?: number;

  constructor(options: ProtocolErrorOptions & {
    readonly retryable: boolean;
    readonly retryAfterSeconds?: number;
  }) {
    super("Teslatlas protocol request failed", {
      code: options.code,
      status: options.status,
      ...(options.requestId === undefined ? {} : { requestId: options.requestId }),
    });
    this.retryable = options.retryable;
    if (options.retryAfterSeconds !== undefined) {
      this.retryAfterSeconds = options.retryAfterSeconds;
    }
  }
}
export class ReplayGapError extends TeslatlasError<"event_replay_expired"> {
  constructor(status: number, requestId?: SafeRequestId) {
    super("Teslatlas event replay point expired", {
      code: "event_replay_expired",
      status,
      ...(requestId === undefined ? {} : { requestId }),
    });
  }
}
export class CommandUncertainError extends TeslatlasError<"command_uncertain"> {
  constructor() {
    super("Teslatlas command submission outcome is uncertain", {
      code: "command_uncertain",
    });
  }
}
```

Use `asSafeRequestId()` for every request ID. Never accept `detail`, response
body, headers, or cause in an error constructor. Update checkpoint validation:

```ts
function validateCheckpoint(value: unknown): asserts value is string | undefined {
  if (value !== undefined && (typeof value !== "string" || containsControlCharacters(value))) {
    throw new InvalidSseCheckpointError();
  }
}
```

- [ ] **Step 5: Implement exact version/capability negotiation and discovery session**

`negotiateProtocolVersion()` parses the requested version, intersects it with
the locked profiles and descriptor `supported_versions`, rejects incompatible
major/minimum-client constraints, then selects the highest candidate that is
both not newer than the request and not older than
`minimum_client_version`. If no candidate meets both bounds, it throws
`IncompatibleProtocolError`.

`createClientSession()` has this exact shape:

```ts
export interface CreateClientOptions {
  readonly baseUrl: string | URL;
  readonly authorization: AuthorizationProvider;
  readonly fetch?: FetchImplementation;
  readonly requestedProtocolVersion?: "1.0.0" | "1.1.0" | "1.2.0";
  readonly signal?: AbortSignal;
}

export interface ClientSession {
  readonly descriptor: HubDescriptor;
  readonly protocolVersion: "1.0.0" | "1.1.0" | "1.2.0";
  readonly discoveryTransport: FetchTransport;
  readonly apiTransport: FetchTransport;
  readonly eventTransport: FetchTransport;
}
```

Before any discovery Fetch, it rejects credential-bearing or non-loopback
`http:` bootstrap URLs. It creates an unauthenticated `discoveryTransport`,
fetches `/.well-known/teslatlas-hub` with Authorization absent and
`redirect: "error"`, requires a `200` response, validates the body and
HTTPS/loopback endpoint URLs, negotiates the profile, then creates authenticated
API/event transports using descriptor endpoints. An abort during body reading
propagates the caller signal reason unchanged. `discoveryTransport` remains
available only to implement the public unauthenticated `discoverHub` operation.
It does not persist or assert trust in `hub_id`.

- [ ] **Step 6: Run focused and foundation regression tests GREEN**

```bash
npm run test:unit -- --run tests/unit/protocol-models.test.ts tests/unit/negotiation.test.ts tests/unit/client-session.test.ts tests/unit/errors.test.ts tests/unit/sse-subscription.test.ts tests/unit/fetch-transport.test.ts
npm run typecheck
```

Expected: all focused tests and existing transport/SSE regressions pass.

- [ ] **Step 7: Commit the safe session layer**

```bash
git add src/protocol src/client/types.ts src/client/session.ts src/core src/events/sse-subscription.ts tests/unit
git commit -m "feat: add validated protocol negotiation and sessions"
```

---

### Task 3: Implement all released read operations, conditional results, and pagination

**Files:**

- Create: `src/http/request-builder.ts`
- Create: `src/http/response-decoder.ts`
- Create: `src/client/operations.ts`
- Create: `src/client/client.ts`
- Create: `tests/unit/request-builder.test.ts`
- Create: `tests/unit/response-decoder.test.ts`
- Create: `tests/unit/read-operations.test.ts`
- Create: `tests/unit/pagination.test.ts`
- Create: `tests/conformance/typed-client-suite.ts`
- Modify: `tests/conformance/node.test.ts`
- Modify: `tests/conformance/browser.test.ts`

**Interfaces:**

- Consumes: `ClientSession`, generated validators, fixed OpenAPI templates.
- Produces: `TeslatlasClient` with 12 released discovery/query/data read methods.

- [ ] **Step 1: Write failing request/response and operation transcript tests**

Test root-relative template replacement, repeated query arrays, cursor byte
preservation, no undeclared query/header, 304 mapping, problem redaction, and
one Fetch call. Include a table for these exact methods and expected paths:

```ts
const reads = [
  ["discoverHub", "/.well-known/teslatlas-hub"],
  ["listVehicles", "/v1/vehicles"],
  ["getVehicleCurrentState", "/v1/vehicles/vehicle%2Fdemo/current"],
  ["listVehicleDrives", "/v1/vehicles/vehicle%2Fdemo/drives"],
  ["getDrive", "/v1/drives/drive%2Fdemo"],
  ["listDrivePositions", "/v1/drives/drive%2Fdemo/positions"],
  ["listVehicleCharges", "/v1/vehicles/vehicle%2Fdemo/charges"],
  ["getCharge", "/v1/charges/charge%2Fdemo"],
  ["listChargeSamples", "/v1/charges/charge%2Fdemo/samples"],
  ["listVehicleStates", "/v1/vehicles/vehicle%2Fdemo/states"],
  ["listVehicleUpdates", "/v1/vehicles/vehicle%2Fdemo/updates"],
  ["listDataQuality", "/v1/data-quality"],
] as const;
```

The shared typed-client suite must execute `discoverHub`, `listVehicles`,
current state `200/304`, history pagination, and protocol problem decoding
through the Node and Chromium factories.

- [ ] **Step 2: Run focused tests and confirm RED**

```bash
npm run test:unit -- --run tests/unit/request-builder.test.ts tests/unit/response-decoder.test.ts tests/unit/read-operations.test.ts tests/unit/pagination.test.ts
```

Expected: FAIL because the closed request builder, decoder, client, and methods do not exist.

- [ ] **Step 3: Implement fixed request templates and safe query/header construction**

`request-builder.ts` must expose only internal descriptors:

```ts
export type ReadOperationName =
  | "discoverHub" | "listVehicles" | "getVehicleCurrentState" | "listVehicleDrives"
  | "getDrive" | "listDrivePositions" | "listVehicleCharges"
  | "getCharge" | "listChargeSamples" | "listVehicleStates"
  | "listVehicleUpdates" | "listDataQuality" | "listVehicleMetadata"
  | "getMetadata" | "getCommand";

export function interpolatePath(template: string, values: Readonly<Record<string, string>>): string {
  const used = new Set<string>();
  const path = template.replace(/\{([^}]+)\}/gu, (_match, name: string) => {
    const value = values[name];
    if (value === undefined) throw new InvalidRequestPathError();
    used.add(name);
    return encodeURIComponent(value);
  });
  if (Object.keys(values).some((name) => !used.has(name))) throw new InvalidRequestPathError();
  return path;
}
```

Append only declared scalar/array query fields using `URLSearchParams`.
Automatically send the negotiated protocol header on versioned operations;
`discoverHub` remains unauthenticated and unversioned. Send `If-None-Match`
only from a validated `EntityTag`. Reserve Authorization exactly as the
foundation transport already does.

- [ ] **Step 4: Implement validated success/problem decoding**

Define result types:

```ts
export interface ResponseMetadata {
  readonly status: number;
  readonly etag?: EntityTag;
  readonly location?: string;
  readonly requestId?: SafeRequestId;
  readonly protocolVersion?: string;
}
export type ReadResult<T> =
  | { readonly kind: "modified"; readonly value: T; readonly metadata: ResponseMetadata }
  | { readonly kind: "not-modified"; readonly metadata: ResponseMetadata };
export interface WriteResult<T> {
  readonly value: T;
  readonly metadata: ResponseMetadata;
}
```

For JSON success, require `application/json`, parse once, validate with the
operation validator, and discard raw text after parsing. Reject an undeclared
success status. For `304`, require no body and a valid ETag. For any
non-success response, accept only `application/problem+json`, validate
`ProtocolProblem`, cross-check body status against HTTP status, and build
`ProtocolHttpError` from safe fields. This includes documented generic cursor
problems (`cursor_scope_changed`, `cursor_query_mismatch`, `cursor_expired`)
even where an operation response list omits their 403/409/410 statuses.
Malformed error bodies become `ProtocolValidationError`; neither error retains
the body.

- [ ] **Step 5: Implement the typed read methods and page iterator**

Expose these signatures on `TeslatlasClient`:

```ts
discoverHub(options?: ConditionalReadOptions): Promise<ReadResult<HubDescriptor>>;
listVehicles(options?: PageReadOptions): Promise<ReadResult<VehiclePage>>;
getVehicleCurrentState(vehicleId: string, options?: ConditionalReadOptions): Promise<ReadResult<CurrentState>>;
listVehicleDrives(vehicleId: string, options?: HistoryPageOptions): Promise<ReadResult<DrivePage>>;
getDrive(driveId: string, options?: ConditionalReadOptions): Promise<ReadResult<Drive>>;
listDrivePositions(driveId: string, options?: HistoryPageOptions): Promise<ReadResult<PositionPage>>;
listVehicleCharges(vehicleId: string, options?: HistoryPageOptions): Promise<ReadResult<ChargePage>>;
getCharge(chargeId: string, options?: ConditionalReadOptions): Promise<ReadResult<Charge>>;
listChargeSamples(chargeId: string, options?: HistoryPageOptions): Promise<ReadResult<ChargeSamplePage>>;
listVehicleStates(vehicleId: string, options?: HistoryPageOptions): Promise<ReadResult<StatePage>>;
listVehicleUpdates(vehicleId: string, options?: HistoryPageOptions): Promise<ReadResult<UpdatePage>>;
listDataQuality(options?: DataQualityPageOptions): Promise<ReadResult<DataQualityPage>>;
```

`PageReadOptions` contains `cursor?: OpaqueCursor`, `limit?: number`,
`ifNoneMatch?: EntityTag`, `signal?: AbortSignal`. `HistoryPageOptions` adds
`from?: string` and `to?: string`; `DataQualityPageOptions` also adds
`vehicleId?: string`. Validate finite integer limits against descriptor limits
and validate exact-millisecond UTC `from < to` before Fetch.

`iteratePages(load, firstOptions)` yields modified pages, forwards
`next_cursor` unchanged, and throws `ProtocolValidationError` on a repeated
cursor. It never merges or retries pages.

- [ ] **Step 6: Run unit and cross-runtime read suites GREEN**

```bash
npm run test:unit -- --run tests/unit/request-builder.test.ts tests/unit/response-decoder.test.ts tests/unit/read-operations.test.ts tests/unit/pagination.test.ts
npm run test:conformance
npm run typecheck
```

Expected: all operation transcript and Node/Chromium shared-client cases pass.

- [ ] **Step 7: Commit the read client**

```bash
git add src/http src/client tests/unit tests/conformance
git commit -m "feat: add typed Teslatlas query client"
```

---

### Task 4: Implement metadata writes and safe asynchronous command jobs

**Files:**

- Modify: `src/client/client.ts`
- Modify: `src/client/operations.ts`
- Modify: `src/http/request-builder.ts`
- Create: `src/commands/idempotency.ts`
- Create: `src/http/strong-etag.ts`
- Create: `tests/unit/metadata-operations.test.ts`
- Modify: `tests/unit/command-safety.test.ts`
- Create: `tests/unit/command-operations.test.ts`
- Modify: `tests/conformance/typed-client-suite.ts`

**Interfaces:**

- Consumes: typed HTTP executor, command/metadata generated types, capability descriptor.
- Produces: all metadata operations and one-shot command submission/status reads.

- [ ] **Step 1: Write failing metadata and command safety tests**

Cover exact method/path/header/body/status for:

```ts
await client.createMetadata("vehicle_demo_alpha", metadataCreate);
await client.getMetadata("metadata_demo_note_0001", { ifNoneMatch: etag });
await client.replaceMetadata("metadata_demo_note_0001", metadataReplace, { ifMatch: strongEtag });
await client.deleteMetadata("metadata_demo_note_0001", { ifMatch: strongEtag });
await client.createCommand(commandRequest, { idempotencyKey, signal });
await client.getCommand("command_demo_0001", { ifNoneMatch: etag });
```

Assertions must prove:

- weak/malformed `If-Match` fails before Fetch;
- malformed/non-UUID idempotency key fails before Fetch;
- metadata write bodies are validated before Fetch;
- command capability absence fails before Authorization lookup;
- command submission makes exactly one Fetch call after `429`, `5xx`, abort,
  or transport failure;
- a failure after dispatch becomes `CommandUncertainError` with no cause;
- validated HTTP problem responses remain `ProtocolHttpError`, not uncertain;
- `202` requires valid job body, ETag, and Location.

- [ ] **Step 2: Run focused tests and confirm RED**

```bash
npm run test:unit -- --run tests/unit/metadata-operations.test.ts tests/unit/command-safety.test.ts tests/unit/command-operations.test.ts
```

Expected: FAIL because strong ETag/idempotency brands and write methods do not exist.

- [ ] **Step 3: Add protocol-valid strong ETag and UUID brands**

```ts
export function asStrongEntityTag(value: string): StrongEntityTag {
  if (!/^"[^"]+"$/u.test(value) || containsControlCharacters(value) || value.length > 512) {
    throw new InvalidStrongEntityTagError();
  }
  return value as StrongEntityTag;
}

export function asIdempotencyKey(value: string): IdempotencyKey {
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/iu.test(value)) {
    throw new InvalidIdempotencyKeyError();
  }
  return value as IdempotencyKey;
}
```

Keep the existing `assertCommandSafety` as a lower-level guard, but make the
typed `createCommand` method itself one-shot so callers cannot opt into retry.

- [ ] **Step 4: Implement metadata methods with exact preconditions**

Expose:

```ts
listVehicleMetadata(vehicleId: string, options?: MetadataPageOptions): Promise<ReadResult<MetadataPage>>;
createMetadata(vehicleId: string, body: MetadataCreate, options?: RequestOptions): Promise<WriteResult<MetadataRecord>>;
getMetadata(metadataId: string, options?: ConditionalReadOptions): Promise<ReadResult<MetadataRecord | MetadataTombstone>>;
replaceMetadata(metadataId: string, body: MetadataReplace, options: IfMatchOptions): Promise<WriteResult<MetadataRecord>>;
deleteMetadata(metadataId: string, options: IfMatchOptions): Promise<WriteResult<MetadataTombstone>>;
```

Validate body before authorization/network. Send `If-Match` unchanged for PUT
and DELETE. Decode only `201` for creation and `200` for get/replace/delete.
Require strong ETags on metadata entity responses.

- [ ] **Step 5: Implement command one-shot/uncertainty semantics**

Expose:

```ts
createCommand(body: CommandRequest, options: CommandCreateOptions): Promise<WriteResult<CommandJob>>;
getCommand(commandId: string, options?: ConditionalReadOptions): Promise<ReadResult<CommandJob>>;
```

Before dispatch: require `commands.async`, validate request body, validate UUID,
and honor an already-aborted signal unchanged. Set `dispatchStarted = true`
immediately before calling Fetch. If Fetch rejects or aborts after that point,
throw a new cause-free `CommandUncertainError`. If a conforming HTTP response
arrives, decode it normally; `429`, `5xx`, and other problems are not retried.

- [ ] **Step 6: Run write/command and shared-runtime suites GREEN**

```bash
npm run test:unit -- --run tests/unit/metadata-operations.test.ts tests/unit/command-safety.test.ts tests/unit/command-operations.test.ts tests/unit/response-decoder.test.ts
npm run test:conformance
npm run typecheck
```

Expected: exact write transcripts pass; no command test observes more than one dispatch.

- [ ] **Step 7: Commit the write surfaces**

```bash
git add src/client src/commands src/http tests/unit tests/conformance
git commit -m "feat: add metadata and command job clients"
```

---

### Task 5: Add typed SSE replay, terminal handling, and protocol-case consumption

**Files:**

- Create: `src/events/protocol-subscription.ts`
- Modify: `src/events/sse-subscription.ts`
- Modify: `src/client/client.ts`
- Create: `tests/unit/protocol-events.test.ts`
- Create: `tests/unit/protocol-sse-reconnect.test.ts`
- Create: `tests/protocol/case-loader.ts`
- Create: `tests/protocol/protocol-cases.test.ts`
- Modify: `tests/conformance/typed-client-suite.ts`
- Modify: `vitest.config.ts`
- Modify: `package.json`

**Interfaces:**

- Consumes: validated event schema, SSE contract, typed client session, generated case data.
- Produces: typed `streamEvents()` and deterministic protocol-case evidence.

- [ ] **Step 1: Write failing typed event and terminal/replay tests**

Cover these exact behaviors:

```ts
const events = client.streamEvents({
  vehicleId: "vehicle_demo_alpha",
  eventTypes: ["vehicle.current.changed"],
  checkpoint,
  signal,
});
```

- request path is `/v1/events?vehicle_id=vehicle_demo_alpha&event_type=vehicle.current.changed`;
- Authorization and negotiated version are present;
- known event yields a validated `ProtocolEvent` with equal SSE ID/event type;
- unknown SSE event name is ignored before `JSON.parse`;
- malformed known JSON, schema mismatch, ID mismatch, and type mismatch terminate;
- empty `id:` saves `undefined` immediately;
- yielded ID saves only when the consumer requests the next event;
- `204` returns cleanly and performs no reconnect;
- `400 event_id_invalid` and `410 event_replay_expired` terminate with typed errors;
- accidental EOF reconnects after 3,000 ms by default;
- valid server retry is used once and capped at 30,000 ms;
- abort performs no later Fetch;
- checkpoint load/save rejects non-string runtime values safely.
- principal A/B use separate caller-owned Authorization providers and checkpoint
  stores; a principal-A replay ID supplied under principal B becomes a terminal
  typed `ProtocolHttpError` with code `event_id_invalid`, without reconnect.

- [ ] **Step 2: Run event tests and confirm RED**

```bash
npm run test:unit -- --run tests/unit/protocol-events.test.ts tests/unit/protocol-sse-reconnect.test.ts tests/unit/sse-parser.test.ts tests/unit/sse-subscription.test.ts
```

Expected: FAIL because typed event decoding and protocol terminal/default reconnect behavior do not exist.

- [ ] **Step 3: Implement typed protocol subscription over the incremental parser**

`streamEvents` accepts:

```ts
export interface StreamEventsOptions {
  readonly vehicleId?: string;
  readonly eventTypes?: readonly ProtocolEvent["event_type"][];
  readonly checkpoint?: SseCheckpointStore;
  readonly signal?: AbortSignal;
  readonly sleep?: SseSleep;
}
```

Refactor the low-level subscription to accept an internal response classifier
that can return `continue`, `terminal`, or a typed error. The protocol wrapper
uses the safe problem decoder for `400/410`, returns on `204`, and rejects all
other non-`200` responses. Do not place protocol routes or codes back into the
generic parser.

Before parsing data, compare the raw SSE event name with the generated event
catalogue. Ignore unknown names without decoding. For known names, parse JSON,
run `validateEvent`, then require `event.event_id === lastEventId` and
`event.event_type === sseEventName`.

- [ ] **Step 4: Build a protocol-case loader and deterministic SDK consumer checks**

`case-loader.ts` consumes the generated case module, expands `${profile}` and
prior-step references using an immutable per-case context, loads body-file data
already embedded by generation, and returns ordered case steps.

`protocol-cases.test.ts` must run every case admitted by each profile and map
the relevant reference response through the typed SDK surface:

```ts
for (const profile of ["1.0.0", "1.1.0", "1.2.0"] as const) {
  for (const testCase of casesForProfile(profile)) {
    it(`${profile} ${testCase.case_id}`, async () => {
      const result = await runSdkProtocolCase(profile, testCase);
      expect(result.failures).toEqual([]);
    });
  }
}
```

The runner must verify outgoing method/path/query/safe headers, typed value or
error, status, ETag/Location/request ID, event/checkpoint order, and dispatch
count. It must redact Authorization from recorded transcripts. Report these as
SDK protocol-case checks, not as Hub/server conformance.

For `sse-principal-visibility`, model principal A and B with separate injected
Authorization providers and separate checkpoint stores. Do not add a public
principal field or conformance-only header API; the typed result must show the
reference `404`/empty-event/terminal `event_id_invalid` outcomes.

- [ ] **Step 5: Add protocol-case script and run all event/case suites GREEN**

Add:

```json
{
  "test:protocol": "vitest run --project protocol-cases",
  "test": "npm run test:unit && npm run test:conformance && npm run test:protocol",
  "verify": "npm run format:check && npm run lint && npm run typecheck && npm run protocol:check && npm run build && npm run test && npm run example:node && npm run pack:check"
}
```

Configure a Node `protocol-cases` Vitest project for `tests/protocol/**/*.test.ts`.
`verify` must execute `protocol:check` and the protocol-case project exactly
once through its `test` chain; no final gate may bypass either one.
Then run:

```bash
npm run test:unit -- --run tests/unit/protocol-events.test.ts tests/unit/protocol-sse-reconnect.test.ts tests/unit/sse-parser.test.ts tests/unit/sse-subscription.test.ts
npm run test:protocol
npm run test:conformance
npm run typecheck
```

Expected: all protocol profiles/cases, Node/Chromium parity, parser, and replay tests pass.

- [ ] **Step 6: Commit events and protocol-case evidence**

```bash
git add src/events src/client/client.ts tests/protocol tests/unit tests/conformance vitest.config.ts package.json package-lock.json
git commit -m "feat: add typed events and protocol case checks"
```

---

### Task 6: Close public exports, merge documentation, verify package, and push main

**Files:**

- Modify: `src/index.ts`
- Modify: `src/browser.ts`
- Modify: `src/node.ts`
- Modify: `tests/unit/package-surface.test.ts`
- Modify: `tests/unit/example-smoke.test.ts`
- Create: `tests/unit/public-api.test.ts`
- Modify: `examples/node.mjs`
- Modify: `examples/browser/app.js`
- Modify: `README.md`
- Modify: `docs/architecture.md`
- Modify: `docs/api.md`
- Modify: `docs/protocol-dependency-gate.md`
- Modify: `docs/plans/2026-08-30-foundation.md`
- Create: `docs/compatibility.md`
- Modify: `scripts/check-pack.mjs`
- Modify: `package.json`

**Interfaces:**

- Consumes: completed typed client, protocol lock, runtime factories.
- Produces: closed root/browser/node API, runnable examples, accurate docs, clean packed package and pushed `main`.

- [ ] **Step 1: Write failing public API and packed-boundary tests**

Assert runtime imports expose:

```ts
expect(root).toHaveProperty("TeslatlasError");
expect(root).toHaveProperty("asOpaqueCursor");
expect(browser).toHaveProperty("createClient");
expect(node).toHaveProperty("createClient");
```

Assert these are absent from every package entry:

```ts
for (const api of [root, browser, node]) {
  expect(api).not.toHaveProperty("FetchTransport");
  expect(api).not.toHaveProperty("parseSseStream");
  expect(api).not.toHaveProperty("subscribeToSse");
}
```

Extend `check-pack.mjs` to reject `protocol/`, `scripts/`, `src/`, `tests/`,
`docs/superpowers/`, `.github/`, source maps if not intentionally published,
and generated implementation inputs. Require all runtime JS/declarations and
public docs, including `docs/compatibility.md`. Add that compatibility document
to `package.json.files` explicitly.

- [ ] **Step 2: Run public/package tests and confirm RED**

```bash
npm run test:unit -- --run tests/unit/public-api.test.ts tests/unit/package-surface.test.ts tests/unit/example-smoke.test.ts
npm run pack:check
```

Expected: FAIL while low-level transport/SSE exports remain public and examples still use the foundation API.

- [ ] **Step 3: Publish only the closed root/browser/node surfaces**

Root exports protocol model/result/options types, safe errors, opaque value
constructors, and caller-owned authorization/checkpoint interfaces. It exports
the `TeslatlasClient` type but no factory or low-level request/parser class.

Both runtime entry points export all root types plus:

```ts
export async function createClient(options: CreateClientOptions): Promise<TeslatlasClient> {
  const session = await createClientSession(options);
  return new TeslatlasClient(session);
}
```

Update package self-import tests to import `.`, `./browser`, and `./node` from
built `dist`, instantiate both factories with deterministic Fetch, and assert
all generated declaration targets resolve.

- [ ] **Step 4: Replace examples with typed protocol-fixture flows**

Node example: create client from injected discovery/vehicle fixture responses,
call `listVehicles()`, and print:

```text
Teslatlas SDK Node client: 1 vehicle, protocol 1.2.0
```

Browser example: run the same discovery/list flow through
`@teslatlas/sdk/browser` and render:

```text
Teslatlas SDK browser client: 1 vehicle, protocol 1.2.0
```

The examples contain no live endpoint, credential, private route, VIN, or identifying location.

- [ ] **Step 5: Merge the valid documentation-only design and remove stale gate claims**

Update docs to state:

- canonical code was retained; the separate clone supplied design analysis only;
- protocol commit/profile/digests and local regeneration commands;
- exact 20-operation method list and result/error semantics;
- caller-owned credentials/checkpoints and absence of pairing/identity/TLS policy;
- opaque cursor/ETag/event/idempotency behavior;
- typed SSE replay and terminal behavior;
- one-shot command uncertainty;
- compatibility profiles and distinction between SDK case evidence and server conformance;
- package remains private and no live-Hub or registry-readiness claim exists.

Rewrite `docs/protocol-dependency-gate.md` as the historical gate plus current
open status at `79ced4c`; list only genuinely remaining live-Hub/publication
gates. Replace the stale short foundation plan with a pointer to this executed
plan and preserve historical context without copying unsupported local claims.

- [ ] **Step 6: Run full verification and inspect the exact staged change**

Run from a clean dependency install:

```bash
npm ci
npm run format
npm run format:check
npm run lint
npm run typecheck
npm run protocol:check
npm run build
npm run test:unit
npm run test:browser
npm run test:conformance
npm run test:protocol
npm run example:node
npm run pack:check
npm run verify
git diff --check
```

Expected: every command exits zero; all unit/browser/conformance/protocol tests
report zero failures; generated output is unchanged; package contents contain
no source/protocol/test/automation files.

Review:

```bash
git status --short
git diff --stat 2d4518a2f2fc55ec62724b45a586b2db910e993f..HEAD
git diff --check
rg --files -g '.github/**'
```

Expected: only intended SDK files are modified; no `.github` files exist; the
documentation-only clone and Teslatlas app checkout remain untouched.

- [ ] **Step 7: Obtain independent security/contract/runtime reviews and fix findings**

Dispatch read-only reviewers for:

1. protocol/OpenAPI/schema fidelity and capability/version behavior;
2. credential/error/URL/header/command/SSE security boundaries;
3. Node/Chromium/runtime/package/conformance evidence.

For each actionable finding, add a failing regression test, observe RED, apply
the smallest fix, and rerun affected plus full gates. A null review readback is
an orchestration failure and must be retried, not treated as approval.

- [ ] **Step 8: Commit documentation/export reconciliation and push once**

```bash
git add -A
git diff --cached --check
git commit -m "docs: finalize reconciled TypeScript SDK"
git push origin main
git status -sb
git rev-parse HEAD
git rev-parse origin/main
```

Expected: push succeeds once; local HEAD equals `origin/main`; worktree is clean.

---

## Plan self-review

| Spec requirement | Implemented by |
| --- | --- |
| Canonical-main plus documentation-only merge decision | Tasks 1 and 6 |
| Exact protocol commit, vendored bytes, digests, offline generation | Task 1 |
| Generated private wire types and runtime validators | Tasks 1 and 2 |
| Protocol aliases without camelCase payload copies | Task 2 |
| Discovery, negotiation, capabilities, caller-owned authorization | Task 2 |
| Closed first-class methods for all 20 OpenAPI operations | Tasks 2 through 5 |
| Conditional results, cursors, pagination, metadata If-Match | Tasks 3 and 4 |
| Safe RFC 9457 errors and command uncertainty | Tasks 2 through 4 |
| Typed SSE, checkpoint timing, replay, terminal statuses | Task 5 |
| Profiles 1.0.0/1.1.0/1.2.0 and protocol-case evidence | Tasks 1 and 5 |
| Browser/Node identical semantics | Tasks 3 through 6 |
| Caller-owned storage and rejected unsupported local claims | Tasks 2, 5, and 6 |
| Internal raw transport/generated paths | Task 6 |
| No viewer, Hub, app, GitHub automation, publication, or live claims | Global constraints and Task 6 |

Type consistency check:

- `CreateClientOptions` and `ClientSession` are introduced in Task 2 and used unchanged through Tasks 3–6.
- `ReadResult<T>`, `WriteResult<T>`, and `ResponseMetadata` are introduced in Task 3 and used by metadata, commands, examples, and exports.
- `StrongEntityTag` and `IdempotencyKey` are introduced in Task 4 before any write method consumes them.
- `ProtocolEvent` and `SseCheckpointStore` are defined before `streamEvents()` is added in Task 5.
- Public factory name is consistently `createClient` in both runtime subpaths; no root factory is exported.
