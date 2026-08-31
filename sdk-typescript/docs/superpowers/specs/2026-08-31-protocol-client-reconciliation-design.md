# Teslatlas TypeScript SDK protocol-client reconciliation design

## Status and source inputs

This design reconciles two concurrent SDK efforts:

- canonical SDK `main` at
  `2d4518a2f2fc55ec62724b45a586b2db910e993f`, which contains a tested
  browser/Node transport foundation;
- the dirty documentation-only clone at
  `/Users/bolyki/Documents/Codex/2026-08-30/teslatlas-sdk-typescript/outputs/teslatlas-sdk-typescript`
  at `e6715a4c005ceeb483e081efba6ea998ca5fb1ea`, whose useful material is
  architecture and implementation-planning analysis rather than executable
  code.

The protocol authority is now
`/Users/bolyki/dev/source/teslatlas-protocol` at
`79ced4c7fdc79520ad31d72a0280bf5f3f19f407`. Its local `./tools/check` gate
passes 54 artifact tests and 31 conformance profile/case runs across protocol
profiles `1.0.0`, `1.1.0`, and `1.2.0`.

The SDK stays private during reconciliation. This work does not publish a
registry package or claim live-Hub compatibility.

## Merge decision

Canonical SDK `main` remains the code base. The documentation-only clone is
not merged as a Git branch because it has no runtime implementation and is
based on the pre-protocol commit. Its valid invariants are incorporated into
the implementation and current documentation.

The following ideas are retained:

- exact, digest-verified protocol input;
- generated wire types with a hand-written first-class client surface;
- caller-owned authorization and checkpoint persistence;
- no credential, body, or arbitrary-cause leakage through errors;
- one shared request/SSE implementation for browser and Node.js;
- no automatic retry of ordinary requests or command submissions;
- opaque cursors, event IDs, ETags, request IDs, and idempotency keys;
- the same deterministic protocol cases exercised in both runtimes;
- a closed, typed public API rather than a public arbitrary-path escape hatch.

The following documentation-only proposals are rejected because the protocol
does not define them or they conflict with repository policy:

- hosted GitHub CI, release automation, or GitHub-downloaded build inputs;
- a signed-artifact requirement not present in the protocol repository;
- SDK-managed pairing, bearer rotation, Hub identity proofs, certificate
  pinning, or trust-store semantics;
- an invented Node.js/browser support promise beyond the locally tested,
  locked toolchain;
- viewer implementation work;
- automatic retries, automatic command reissue, or automatic replay-gap
  recovery.

## Protocol intake and reproducibility

`protocol/lock.json` records:

- authority repository name and exact Git commit;
- current profile `1.2.0` and supported profiles `1.0.0`, `1.1.0`, `1.2.0`;
- SHA-256 for every vendored input;
- the exact generator package and version;
- the expected SHA-256 for generated output.

The SDK vendors only the public inputs needed to regenerate and test the
client under `protocol/source/`:

- self-contained `openapi/teslatlas-v1.openapi.json`;
- `events/teslatlas-v1.sse.json`;
- canonical JSON Schemas needed for runtime validators;
- compatibility manifest/profiles;
- language-neutral conformance manifest and cases.

Vendoring creates a pinned snapshot, not a competing authority. A local
`scripts/sync-protocol.mjs` command accepts an explicit protocol checkout,
verifies that its HEAD equals the lock target, copies only the allowlisted
files, recalculates digests, regenerates outputs, and fails on any unexpected
diff. Normal package builds use checked-in generated files and make no network
request.

`scripts/check-protocol.mjs` verifies the lock, every vendored byte, generated
output, profile order, and OpenAPI version. Generation is deterministic and
checked locally. Protocol source snapshots, generator tools, tests, and plans
are excluded from the packed npm artifact.

## Package and module boundaries

The package remains ESM-only with root, browser, and Node.js entry points.

The root entry point exports:

- protocol-derived public data types and method option/result types;
- opaque-value constructors;
- safe error classes;
- credential/checkpoint interfaces;
- client interfaces that do not select a runtime transport.

`@teslatlas/sdk/browser` and `@teslatlas/sdk/node` each export an asynchronous
`createClient` factory. Both factories use the same core and differ only in
their named runtime entry point and default `globalThis.fetch`. An injected
Fetch implementation remains available for deterministic tests and
caller-owned Node network policy.

`FetchTransport`, raw request construction, generated OpenAPI `paths`, schema
validators, and raw SSE parser types become internal modules. They remain
directly testable inside the repository but are removed from package exports.
This prevents consumers from institutionalising private or undocumented Hub
routes.

## Generated and public types

An exact `openapi-typescript` version generates private wire declarations in
`src/generated/protocol.ts`. Generated code is checked in and never edited by
hand.

The hand-written public surface refers to generated operation and schema types
instead of repeating resource fields. Public resource aliases cover the
released vehicle, current state, drive, position, charge, sample, state,
software-update, data-quality, metadata, event, problem, discovery, and command
job shapes. Where the generator represents a JSON Schema root `$defs` block as
a required type property, the facade omits that generator-only metadata from
the affected public alias; it never remaps runtime payload fields.

Wire names remain protocol names. The SDK does not add a second camelCase data
model because that would create another mapping authority and increase
cross-language drift. Method and option names use normal TypeScript casing;
JSON payload members remain exactly as defined by the protocol.

Runtime response validators are generated from the vendored schemas into
browser-safe standalone code. A success body is returned only after validation.
Malformed JSON, a schema mismatch, a missing required response header, or an
operation/status combination absent from OpenAPI becomes a safe
`ProtocolValidationError` without exposing the raw body.

Fixture and JSON-module imports are treated as untrusted values too. Tests
obtain exact public protocol types only by passing them through the same named
validator/decoder path; the facade does not weaken protocol literal fields to
accommodate TypeScript's widened JSON-import inference.

## Client lifecycle and capability negotiation

`createClient` accepts only an HTTPS bootstrap URL or loopback HTTP URL without
embedded credentials. It performs public discovery without authorization and
with redirects rejected, requires a 200 response, validates the descriptor,
and negotiates the highest mutually supported profile from the locked set that
is not older than the descriptor's minimum client version. The created client
exposes the validated descriptor and selected protocol version.

The SDK does not persist or establish trust in `hub_id`; the protocol defines
the identifier but no identity-proof or trust-store lifecycle. Callers may
persist and compare it according to their product policy.

Every versioned request sends `Teslatlas-Protocol-Version`. Optional operation
groups are checked against the descriptor's advertised capabilities before
request creation. The SDK never derives an endpoint from a version string and
never falls back to a route missing from OpenAPI.

The authorization provider is called immediately before each authenticated
request. The returned complete Authorization value is placed only in the
request headers, is never persisted, and is not retained in public errors.
Discovery is unauthenticated.

## Public operation surface

The client exposes one named method per released OpenAPI `operationId`:

- `discoverHub`;
- `listVehicles`;
- `getVehicleCurrentState`;
- `listVehicleDrives` and `getDrive`;
- `listDrivePositions`;
- `listVehicleCharges` and `getCharge`;
- `listChargeSamples`;
- `listVehicleStates`;
- `listVehicleUpdates`;
- `streamEvents`;
- `listDataQuality`;
- `createCommand` and `getCommand`;
- `listVehicleMetadata`, `createMetadata`, `getMetadata`, `replaceMetadata`,
  and `deleteMetadata`.

Each method accepts a typed object containing only the path, query, header, and
body inputs declared for that operation plus a caller abort signal. The client
constructs paths from fixed generated templates and percent-encodes individual
path/query values. No public method accepts an arbitrary URL or path.

Successful JSON operations return a structured result containing the validated
value and safe response metadata: status, ETag when present, Location when
present, selected protocol version, and request ID when present. Conditional
GETs use a discriminated `modified` or `not-modified` result; `304` never
creates a fake empty value.

Pagination returns protocol pages unchanged. Optional async page iteration
forwards each opaque cursor without decoding it and stops on an absent or
repeated cursor, abort, or typed failure. It never merges pages or retries.

Metadata replacement and deletion require a validated strong ETag and send it
as `If-Match`. Missing or weak values fail before dispatch. Creation,
replacement, and deletion follow only the exact OpenAPI response contracts.

## Safe errors

Public failures contain stable SDK categories and only safe fields:

- invalid local input;
- incompatible protocol or missing capability;
- transport failure;
- HTTP problem response;
- protocol validation failure;
- replay gap or terminal stream response;
- checkpoint storage failure;
- command uncertainty.

A validated RFC 9457 response contributes only status, stable protocol code,
safe static SDK message, validated request ID, `retryable`, and parsed
`Retry-After` where declared. Human `detail`, arbitrary extension members, raw
headers, raw response bodies, authorization values, URLs containing query
credentials, and arbitrary underlying causes are discarded.

An abort observed before network dispatch propagates unchanged so standard
Web-platform cancellation remains detectable. Ordinary read aborts also
propagate unchanged. Once `createCommand` has handed its request to Fetch, an
abort or transport failure without a conforming response is classified as
uncertain and never retried; the underlying abort value is not retained.

Opaque-value constructors reject only protocol-required and local injection
hazards. Runtime inputs from JavaScript callers receive explicit type checks so
malformed checkpoint or header values cannot escape as raw `TypeError`.

## SSE events and replay

The existing incremental parser remains the framing core. The typed event
layer adds protocol behavior:

- response status `204` is terminal and never reconnects;
- `400 event_id_invalid` and `410 event_replay_expired` are typed terminal
  failures;
- valid server retry values are capped at 30,000 milliseconds;
- the default reconnect delay is the protocol-defined 3,000 milliseconds;
- unknown event names are ignored before JSON decoding, as required by the
  event contract;
- known event names require matching SSE ID/type and validated JSON payload;
- an empty `id:` clears the caller checkpoint;
- replay resumes strictly after the last accepted ID.

The caller owns checkpoint persistence. A yielded event ID is saved only when
the consumer advances after applying that event. Returning early leaves it
uncommitted so replay remains at-least-once. Checkpoint load/save failures stop
the stream with a safe storage error.

The SDK does not identify authorization principals. Callers must scope each
checkpoint store to its authorization principal and stable event filters;
reusing one across principals or filters can correctly yield the protocol's
terminal `event_id_invalid` response and is never retried by the SDK.

Reconnect occurs only after accidental EOF or a retryable transport failure.
Authentication, capability, validation, terminal status, and replay-gap
failures do not reconnect. Replay-gap recovery requires explicit caller query
resynchronization and checkpoint replacement.

## Command-job safety

`createCommand` requires a caller-supplied protocol-valid UUID idempotency key
and performs one POST. It never generates, transforms, stores, or reuses the
key. It returns only a validated `202` job receipt.

If dispatch began but no conforming response arrived, the method throws
`CommandUncertainError`. It does not retry after timeout, abort, transport
failure, `429`, `5xx`, credential refresh, reconnect, or process restart.

`getCommand` provides pollable status and conditional ETag support. The SDK
reports protocol job states exactly as defined. It does not claim state
verification beyond the server's validated job representation and does not
invent polling intervals, cancellation, or lookup-by-idempotency-key APIs.

Command availability is gated by the advertised `commands.async` capability.
Required scopes and command parameter/expected-state schemas are exposed from
discovery. Server authorization remains authoritative because the SDK cannot
inspect an opaque bearer credential's actual scopes.

## Deterministic testing and conformance

The existing foundation tests remain as regression coverage. New gates add:

- byte-for-byte protocol lock and generation checks;
- runtime validator positive/negative tests from protocol examples;
- exact request transcript tests for all 20 OpenAPI operations;
- error-redaction tests with credential-bearing inputs and bodies;
- version/capability tests across profiles `1.0.0`, `1.1.0`, and `1.2.0`;
- conditional GET, cursor, metadata `If-Match`, command uncertainty, and SSE
  terminal/replay tests;
- the same typed-client suite through Node.js and real Chromium;
- an SDK consumer harness that loads the language-neutral protocol cases,
  drives their reference responses through the matching typed SDK methods, and
  compares safe normalized client results. This is reported separately from
  the protocol repository's server/reference-adapter self-test.

No test uses a live Hub, Tesla account, credential, VIN, identifying location,
wall clock, hosted CI, or registry publication. Fetch, sleeps, and response
streams are deterministic and injected.

`npm run verify` performs format, lint, typecheck, generation check, build,
unit tests, browser/Node conformance, examples, protocol case checks, and
packed-content inspection. The packed package contains only runtime output,
public documentation, licence, README, and package metadata.

## Documentation merge

`README.md`, `docs/architecture.md`, `docs/api.md`, and the dependency-gate
document are updated to describe the implemented protocol client rather than
the superseded missing-contract state.

The detailed documentation-only plan is retained as design input, not copied
verbatim. Unsupported pairing, identity-proof, certificate, CI, support-range,
and viewer claims are omitted. The original dirty clone is left untouched.

## Non-goals

- Hub implementation, private-route discovery, or proprietary source use;
- viewer UI or viewer repository changes;
- credential, Hub identity, body-cache, or checkpoint storage implementation;
- credential provisioning, pairing, rotation, or revocation flows;
- TLS pinning, custom CA, proxy, or mTLS policy;
- command UI, automatic command retry, cancellation, or invented job APIs;
- WebSocket support;
- GitHub Actions, Dependabot, release automation, npm publication, or live-Hub
  readiness claims;
- changes to the Teslatlas app checkout.
