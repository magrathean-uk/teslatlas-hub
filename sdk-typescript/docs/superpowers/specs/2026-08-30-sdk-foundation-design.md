# TypeScript SDK transport foundation design

## Status and authority

This design implements the contract-neutral work that is possible at SDK
HEAD `e6715a4` while treating
`/Users/bolyki/dev/source/teslatlas-protocol` at
`b7b48a86a7705e8ab016f1debd25cecd20ebbb89` as the public contract authority.

The protocol repository currently defines architectural rules, but it does not
publish the OpenAPI, JSON Schema, event-stream schema, authentication contract,
error catalogue, command-job schema, compatibility fixtures, or conformance
runner required for generated resource clients. This SDK must not turn the
product brief's candidate routes or payload examples into an accidental public
contract.

## Chosen approach

Build a small, strict, browser-safe transport foundation now, then attach a
generated protocol layer when released artifacts exist.

Two alternatives were rejected:

1. Generating a client now is impossible because there is no OpenAPI input.
2. Hand-writing resource methods from the product brief would freeze candidate
   routes, fields, errors, credentials, and command states without protocol
   authority.

The selected approach produces useful, independently testable mechanics without
claiming that the public v1 API exists.

## Deliverable

The repository will contain:

- an npm package named `@teslatlas/sdk`, marked private so this development
  snapshot cannot be published accidentally;
- exact Node.js, npm, TypeScript, formatter/linter, Vitest, and Playwright
  versions in source control;
- ESM output and explicit root, browser, and Node.js exports;
- protocol-rule primitives for semantic versions, capability sets, opaque
  cursors, entity tags, stable error codes, and safe command submission policy;
- caller-owned generic credential and SSE checkpoint interfaces, with no
  default persistence;
- one fetch-based transport core shared by browser and Node.js adapters;
- standard HTTP conditional-request helpers and opaque query-value handling;
- an incremental SSE parser plus reconnect loop with `Last-Event-ID`, bounded
  caller-selected retries, server retry hints, abort support, and at-least-once
  checkpoint timing;
- deterministic unit and cross-runtime conformance suites;
- task-oriented API documentation and runnable local examples;
- an explicit dependency gate listing everything that remains blocked on the
  protocol repository.

No dependency on Hub, viewer, or private product source is allowed.

## Public package boundaries

### Root export

The root export contains contract-neutral types and behavior:

- semantic version parsing and caller-supplied compatibility-window checks;
- opaque capability identifiers and missing-capability checks;
- opaque cursor and ETag values that are preserved, never decoded;
- typed SDK errors that retain status, stable code, and an explicitly supplied
  safe request ID, but never arbitrary server diagnostics;
- generic caller-owned credential storage interfaces;
- ETag and cursor request helpers;
- raw SSE parsing and subscription primitives;
- command submission safety validation requiring a non-empty idempotency key
  and forbidding automatic retry.

Capability names, error-code values, resource models, event names, credential
formats, and command states are intentionally absent.

### Browser export

`@teslatlas/sdk/browser` creates the browser adapter from `globalThis.fetch` or
an injected fetch implementation. It stores no credential, event ID, or other
state unless the caller supplies a store or hook.

### Node.js export

`@teslatlas/sdk/node` creates the Node.js adapter from `globalThis.fetch` or an
injected fetch implementation. It uses the same request and response behavior
as the browser adapter and introduces no Node-only wire semantics.

## Transport behavior

The transport accepts a base URL and relative protocol paths supplied by the
future generated layer. It:

- allows only `http:` and `https:` base URLs;
- rejects embedded user information and protocol-relative or absolute request
  targets;
- merges ordinary headers while reserving `Authorization` for the caller's
  authorization provider;
- adds `If-None-Match` and `Last-Event-ID` only when explicitly requested;
- makes exactly one fetch attempt per ordinary request;
- returns native `Response` objects so schema decoding stays in the future
  generated layer;
- normalizes missing-fetch and network failures without including headers,
  credentials, or response bodies in error messages.

The low-level adapter contains no route names. Documentation makes clear that it
is infrastructure for released protocol clients, not permission to depend on
Hub-private routes.

## SSE behavior

The parser follows the standard event-stream field rules for `data`, `event`,
`id`, and integer `retry` values. It supports UTF-8 chunks split at arbitrary
byte boundaries, LF/CRLF/CR line endings, comments, multiline data, empty event
IDs, and ignored unknown fields. An `id` containing NUL is ignored.

The subscription loop:

1. loads the caller-owned checkpoint once;
2. sends it as `Last-Event-ID` when non-empty;
3. parses and yields raw events;
4. commits the yielded event ID only when the consumer requests the next event;
5. reconnects only when the injected policy returns a delay;
6. uses a valid server `retry` field as the next delay hint;
7. stops immediately on abort or when the policy returns no delay.

Committing after the yield means a consumer that stops before advancing can see
the event again after restart, avoiding a silent skip. Event payload decoding is
blocked until the protocol event schema exists.

## Command safety

The protocol authority says command jobs are asynchronous, scoped,
idempotency-keyed, and state-verified. It does not define their route, header,
payload, states, or error responses.

The SDK therefore implements only a guard that validates a caller-supplied
idempotency key and requires retry mode `never`. It does not submit or poll a
command. A command client becomes implementable only after the protocol
repository publishes its command-job contract and conformance fixtures.

## Errors and diagnostics

Public errors have stable SDK-owned categories for invalid input, compatibility,
missing capability, transport failure, HTTP/event-stream failure, and command
safety. A protocol error can carry a protocol-owned opaque code only when a
released decoder supplies it.

The SDK never copies arbitrary JSON error details into an exception. Request IDs
are accepted only through an explicit decoder/header mapping supplied by the
future protocol layer. Authorization values are never exposed in messages.

## Testing

Tests use fixed literals, injected fetch functions, injected sleeps, and local
`ReadableStream` instances. No live Hub, Tesla account, credential, clock, or
network service is required.

The gates are:

- formatting check;
- lint;
- strict TypeScript typecheck;
- package build;
- Node.js unit tests;
- real Chromium browser tests;
- one shared conformance suite executed through both adapters;
- runnable Node.js example smoke test;
- packed-package inspection.

The conformance label applies only to the SDK transport foundation. It must not
be presented as Teslatlas protocol conformance while upstream fixtures are
absent.

## Protocol dependency gate

Generated/resource-level SDK work remains blocked until the protocol authority
publishes all of the following with versioned paths and compatibility rules:

- discovery schema for `/.well-known/teslatlas-hub`;
- OpenAPI document for approved query and command resources;
- JSON Schemas for discovery, resources, errors, events, and commands;
- event-stream contract covering names, payloads, IDs, replay window, and
  reconnect terminal conditions;
- authentication and pairing contract, including credential scheme and scopes;
- stable error-code catalogue and safe request-ID location;
- pagination request/response envelope and cursor parameter names;
- ETag semantics for each resource and mutation preconditions;
- command-job submission, idempotency, state machine, polling, expiry,
  cancellation, and verification rules;
- deterministic redacted fixtures and the language-neutral conformance runner;
- browser and Node.js runtime support policy;
- two-minor-version compatibility and deprecation policy.

Until that gate opens, the package remains private and examples use deterministic
local responses rather than candidate Hub routes.

## Non-goals

- viewer UI or integration;
- Hub implementation or private-route discovery;
- credential persistence policy;
- server credentials or provider tokens;
- command UI or executable command client;
- GitHub Actions, hosted CI, Dependabot, releases, or registry publication;
- touching the Teslatlas app checkout.
