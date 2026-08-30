# TypeScript SDK transport foundation implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a locked, browser-safe and Node.js-safe transport foundation without inventing protocol resources that the authority has not published.

**Architecture:** A shared standards-only core owns protocol-rule primitives, fetch transport, and SSE behavior. Thin browser and Node.js entry points inject their native `fetch`; a future generated layer will provide routes, schemas, auth details, and event payload decoders after the protocol dependency gate opens.

**Tech stack:** Node.js 26.7.0, npm 11.19.0, TypeScript 7.0.2, Biome 2.5.11, Vitest 4.1.11, `@vitest/browser-playwright` 4.1.11, Playwright 1.62.1, ESM, Web Fetch/Streams APIs.

**Spec:** `docs/superpowers/specs/2026-08-30-sdk-foundation-design.md`

## Global constraints

- Contract authority is `/Users/bolyki/dev/source/teslatlas-protocol` at `b7b48a86a7705e8ab016f1debd25cecd20ebbb89`.
- Do not create route methods, resource payloads, event names, capability names, error-code values, credential formats, or command states absent from released protocol artifacts.
- Keep credential and SSE checkpoint persistence caller-owned.
- Ordinary HTTP requests make one fetch attempt; command submission retry mode is always `never`.
- Browser and Node.js adapters execute the same shared conformance fixtures.
- Package stays private; no registry publication or release automation.
- Add no GitHub Actions, hosted CI, Dependabot, or GitHub release files.
- Do not read implementation contracts from Hub Rust or private Teslatlas Swift.
- Do not touch `/Users/bolyki/dev/source/teslatlas-service/app`.
- Use one coherent final commit and push after all local gates; do not make per-task commits.

---

### Task 1: Locked package and test harness

**Files:**

- Create: `.gitignore`
- Create: `.node-version`
- Create: `.npmrc`
- Create: `package.json`
- Create: `package-lock.json`
- Create: `biome.json`
- Create: `tsconfig.json`
- Create: `tsconfig.build.json`
- Create: `vitest.config.ts`
- Create: `src/index.ts`
- Create: `src/browser.ts`
- Create: `src/node.ts`
- Create: `tests/unit/package-surface.test.ts`
- Create: `tests/conformance/browser.test.ts`
- Create: `tests/conformance/node.test.ts`

**Interfaces:**

- Produces: npm scripts `format`, `format:check`, `lint`, `typecheck`, `build`, `test:unit`, `test:browser`, `test:conformance`, `test`, `example:node`, `pack:check`, and `verify`.
- Produces: ESM exports `.`, `./browser`, and `./node` from `dist/`.

- [ ] **Step 1: Create the exact package manifest and configs**

Use a private package manifest with exact dev dependency versions:

```json
{
  "name": "@teslatlas/sdk",
  "version": "0.0.0-development",
  "private": true,
  "type": "module",
  "packageManager": "npm@11.19.0",
  "devDependencies": {
    "@biomejs/biome": "2.5.11",
    "@vitest/browser-playwright": "4.1.11",
    "playwright": "1.62.1",
    "typescript": "7.0.2",
    "vitest": "4.1.11"
  }
}
```

Pin `26.7.0` in `.node-version`, set `engine-strict=true` and
`save-exact=true` in `.npmrc`, target standards-only ESM in TypeScript, and
configure separate `unit`, `node-conformance`, and `browser-conformance`
Vitest projects. The browser project uses Playwright Chromium headlessly.

- [ ] **Step 2: Install once and commit the lockfile**

Run:

```bash
npm install
npx playwright install chromium
```

Expected: exact packages recorded in `package-lock.json`; Chromium installed in
the Playwright cache.

- [ ] **Step 3: Write package-surface tests before exports exist**

```typescript
import { describe, expect, it } from "vitest";

describe("package surface", () => {
  it("keeps the development package non-publishable", async () => {
    const manifest = await import("../../package.json", { with: { type: "json" } });
    expect(manifest.default.private).toBe(true);
  });
});
```

Create browser and Node.js conformance test files that import their respective
entry points and call a shared suite function which does not exist yet.

- [ ] **Step 4: Run the tests and verify RED**

Run:

```bash
npm run test:unit
npm run test:conformance
```

Expected: package-surface test passes; conformance tests fail because shared
suite and adapter creators are missing.

- [ ] **Step 5: Add minimal entry-point stubs and verify configuration**

Export no route or payload types. Create the three entry points so TypeScript
can resolve them, then run:

```bash
npm run format:check
npm run lint
npm run typecheck
```

Expected: configuration succeeds; conformance remains red until Task 3.

### Task 2: Contract-rule primitives and safety errors

**Files:**

- Create: `src/core/version.ts`
- Create: `src/core/capabilities.ts`
- Create: `src/core/opaque-values.ts`
- Create: `src/core/errors.ts`
- Create: `src/auth/credential-store.ts`
- Create: `src/http/conditional.ts`
- Create: `src/commands/safety.ts`
- Modify: `src/index.ts`
- Create: `tests/unit/version.test.ts`
- Create: `tests/unit/capabilities.test.ts`
- Create: `tests/unit/opaque-values.test.ts`
- Create: `tests/unit/errors.test.ts`
- Create: `tests/unit/conditional.test.ts`
- Create: `tests/unit/command-safety.test.ts`

**Interfaces:**

- Produces: `parseProtocolVersion(value: string): ProtocolVersion`.
- Produces: `checkProtocolVersion(version, window): ProtocolCompatibility`.
- Produces: `createCapabilitySet(values)` and `requireCapabilities(available, required)`.
- Produces: `asOpaqueCursor(value)` and `asEntityTag(value)` branded strings.
- Produces: `TeslatlasError`, `ProtocolError`, and `TransportError`.
- Produces: `CredentialStore<T>` and `AuthorizationProvider` interfaces.
- Produces: `appendOpaqueQueryValue(url, name, value)`, `applyIfNoneMatch(headers, etag)`, and `readEntityTag(headers)`.
- Produces: `assertCommandSafety({ idempotencyKey, retry: "never" })`.

- [ ] **Step 1: Write semantic-version tests and verify RED**

Use hand-derived literals:

```typescript
expect(parseProtocolVersion("1.4.2")).toEqual({ major: 1, minor: 4, patch: 2 });
expect(() => parseProtocolVersion("01.4.2")).toThrow(InvalidProtocolVersionError);
expect(checkProtocolVersion(parseProtocolVersion("1.4.0"), {
  major: 1,
  minimumMinor: 3,
  maximumMinor: 5,
})).toEqual({ compatible: true });
```

Run `npx vitest run tests/unit/version.test.ts` and confirm missing imports fail.

- [ ] **Step 2: Implement version parsing and caller-supplied compatibility**

Use a strict `major.minor.patch` parser with optional SemVer prerelease/build
suffixes. Never hard-code Teslatlas's compatibility window. Return typed reasons
`unsupported-major`, `below-minimum-minor`, and `above-maximum-minor`.

Run `npx vitest run tests/unit/version.test.ts`; expected PASS.

- [ ] **Step 3: Write capability and opaque-value tests and verify RED**

Tests must prove deduplication, deterministic missing-capability order, rejection
of empty values, cursor preservation through `URLSearchParams`, ETag preservation,
and CR/LF rejection before header insertion.

Run:

```bash
npx vitest run tests/unit/capabilities.test.ts tests/unit/opaque-values.test.ts
```

Expected: FAIL because functions are missing.

- [ ] **Step 4: Implement capabilities and opaque values**

Treat capability names and cursor contents as opaque strings. Sort only the
reported missing-capability list. Do not decode, normalize, trim, or case-fold a
valid cursor or ETag.

Run the two focused test files; expected PASS.

- [ ] **Step 5: Write error/auth/conditional/safety tests and verify RED**

Cover these behaviors:

```typescript
expect(new ProtocolError({ code: asProtocolErrorCode("rate_limited"), status: 429,
  requestId: asSafeRequestId("safe-request-id") }).requestId).toBe("safe-request-id");
expect(error).not.toHaveProperty("cause");
expect(headers.get("if-none-match")).toBe('W/"revision-7"');
expect(() => assertCommandSafety({ idempotencyKey: "", retry: "never" })).toThrow();
expect(() => assertCommandSafety({ idempotencyKey: "job-1", retry: "automatic" as never })).toThrow();
```

Run the four focused files and confirm missing behavior fails.

- [ ] **Step 6: Implement minimal safe types and helpers**

`ProtocolError` accepts only code, status, and explicitly safe request ID. It
does not accept arbitrary response details. `CredentialStore<T>` remains a pure
caller interface with `load`, `save`, and `clear`; no implementation is shipped.

Run all Task 2 tests; expected PASS.

### Task 3: Shared fetch transport and runtime adapters

**Files:**

- Create: `src/http/fetch-transport.ts`
- Modify: `src/browser.ts`
- Modify: `src/node.ts`
- Create: `tests/conformance/shared-transport-suite.ts`
- Modify: `tests/conformance/browser.test.ts`
- Modify: `tests/conformance/node.test.ts`
- Create: `tests/unit/fetch-transport.test.ts`

**Interfaces:**

- Consumes: `AuthorizationProvider`, `EntityTag`, `applyIfNoneMatch`, and `TransportError` from Task 2.
- Produces: `FetchTransport.request(path, init): Promise<Response>`.
- Produces: `createBrowserTransport(options): FetchTransport`.
- Produces: `createNodeTransport(options): FetchTransport`.

- [ ] **Step 1: Write transport security tests and verify RED**

Tests use an injected fetch and prove:

- only `http:` and `https:` base URLs are accepted;
- base URL user information is rejected;
- `https://other.example/path` and `//other.example/path` request targets are rejected;
- an ordinary relative path remains on the configured origin;
- a request-supplied `Authorization` header is rejected;
- the authorization provider is called per request and its value is not retained;
- a fetch failure yields a generic `TransportError` message without header values;
- one call is made for an HTTP 503 and one call for a thrown network error.

Run `npx vitest run tests/unit/fetch-transport.test.ts`; expected FAIL.

- [ ] **Step 2: Implement the minimal shared transport**

Use standard `URL`, `Headers`, `RequestInit`, `Response`, and `fetch` types only.
Do not parse JSON or throw on HTTP status; the generated protocol layer will own
response decoding. Do not implement retries.

Run the focused unit test; expected PASS.

- [ ] **Step 3: Define the cross-runtime suite and verify RED**

The shared suite takes one factory:

```typescript
export function defineTransportConformanceSuite(
  runtimeName: string,
  createTransport: (options: FetchTransportOptions) => FetchTransport,
): void;
```

It checks exact URL resolution, query preservation, ETag request headers, `304`
responses, caller authorization, abort propagation, and no automatic retry using
the same literal fixtures in both runtimes.

Run `npm run test:conformance`; expected failures reveal any adapter mismatch.

- [ ] **Step 4: Implement thin browser and Node.js factories**

Each factory delegates to the same `FetchTransport` and defaults to
`globalThis.fetch`. Neither factory stores credentials or changes wire behavior.

Run `npm run test:conformance`; expected Node.js and Chromium PASS with the same
suite.

### Task 4: Incremental SSE parser and replay/reconnect

**Files:**

- Create: `src/events/sse-parser.ts`
- Create: `src/events/sse-subscription.ts`
- Modify: `src/index.ts`
- Create: `tests/unit/sse-parser.test.ts`
- Create: `tests/unit/sse-subscription.test.ts`
- Modify: `tests/conformance/shared-transport-suite.ts`

**Interfaces:**

- Consumes: `FetchTransport` and `TransportError` from Task 3.
- Produces: `parseSseStream(stream, options): AsyncIterable<SseEvent>`.
- Produces: `subscribeToSse(options): AsyncIterable<SseEvent>`.
- Produces: `SseCheckpointStore` and `SseReconnectPolicy` caller interfaces.

- [ ] **Step 1: Write parser tests and verify RED**

Use literal byte chunks that split UTF-8 and CRLF boundaries. Cover comments,
multiline data, default `message`, custom event name, persistent and empty IDs,
ignored NUL IDs, integer retry hints, ignored invalid retry values, BOM, unknown
fields, and no dispatch for an unterminated event at EOF.

Run `npx vitest run tests/unit/sse-parser.test.ts`; expected FAIL.

- [ ] **Step 2: Implement the incremental parser**

Decode with one streaming `TextDecoder`, normalize all three line endings
without losing a split CRLF, and update the parser's event ID buffer exactly as
fields arrive. Yield only dispatched data events and expose valid retry hints to
the reconnect layer.

Run the parser tests; expected PASS.

- [ ] **Step 3: Write subscription tests and verify RED**

Use queued fake `Response` objects and injected zero-time sleeps. Prove:

- `Accept: text/event-stream` is sent;
- a stored non-empty ID becomes `Last-Event-ID`;
- the checkpoint is not saved before the consumer advances past the yielded event;
- advancing saves the event ID before the next network read;
- reconnect uses the last committed ID;
- server retry hints override the next policy hint within configured bounds;
- abort stops without another fetch;
- policy refusal stops after EOF or error;
- non-event-stream content type becomes a typed connection error;
- an HTTP error is not decoded as an event stream.

Run `npx vitest run tests/unit/sse-subscription.test.ts`; expected FAIL.

- [ ] **Step 4: Implement subscription and bounded reconnect**

Load the checkpoint once. Use an injected policy for every reconnect and an
injected abort-aware sleep. Commit after each `yield`, not before it. Never retry
when the policy returns `undefined`.

Run the two SSE unit files; expected PASS.

- [ ] **Step 5: Add SSE cases to both runtime conformance projects**

Add the same single-event, multiline, resume-header, and no-blind-retry cases to
the shared suite. Run `npm run test:conformance`; expected Node.js and Chromium
PASS.

### Task 5: Documentation, examples, package proof, and bulk integration

**Files:**

- Modify: `README.md`
- Modify: `docs/architecture.md`
- Create: `docs/api.md`
- Create: `docs/protocol-dependency-gate.md`
- Create: `examples/node.mjs`
- Create: `examples/browser/index.html`
- Create: `examples/browser/app.js`
- Create: `examples/browser/serve.mjs`
- Create: `scripts/check-pack.mjs`
- Create: `tests/unit/example-smoke.test.ts`

**Interfaces:**

- Consumes: all exported Task 2-4 APIs.
- Produces: runnable Node.js and browser examples using deterministic local fetch responses.
- Produces: a pack inspection that rejects source, tests, private files, and absent declaration/JS exports.

- [ ] **Step 1: Write example smoke and pack checks before examples exist**

The Node.js smoke test runs `node examples/node.mjs` after build and expects a
fixed successful output. `scripts/check-pack.mjs` invokes `npm pack --json`,
inspects the tarball file list, and requires `README.md`, `LICENSE`, root/browser/
Node declarations and JS while rejecting `src/`, `tests/`, and local protocol
paths.

Run:

```bash
npm run build
npx vitest run tests/unit/example-smoke.test.ts
npm run pack:check
```

Expected: FAIL because example and pack checker are absent.

- [ ] **Step 2: Add deterministic runnable examples**

The Node.js example imports built package exports, creates a Node transport with
an injected local fetch, performs one conditional GET, and prints a fixed line.
The browser example does the same with the browser export and can be served by
`node examples/browser/serve.mjs` without external packages.

Run the example smoke test; expected PASS.

- [ ] **Step 3: Write task-oriented documentation**

README order: status warning, install-for-development, quick start, supported
surface, verification, documentation links. `docs/api.md` documents every public
export with executable snippets and error behavior. `docs/protocol-dependency-gate.md`
lists each missing authority artifact and exactly which SDK surface it blocks.

Do not describe candidate routes as shipped APIs or call the foundation tests
protocol conformance.

- [ ] **Step 4: Implement and run package inspection**

Use `npm pack --json --dry-run` so no tarball remains. Inspect exact package
paths from JSON. Run `npm run pack:check`; expected PASS.

- [ ] **Step 5: Self-review design and plan**

Run:

```bash
rg -n 'TB[D]|TO[D]O|implement la[t]er|similar t[o]' docs/superpowers
git diff --check
```

Expected: no placeholders or whitespace errors.

- [ ] **Step 6: Run every fresh gate**

Run:

```bash
npm run format:check
npm run lint
npm run typecheck
npm run build
npm run test:unit
npm run test:browser
npm run test:conformance
npm run example:node
npm run pack:check
npm run verify
git diff --check
```

Expected: all available SDK-foundation gates pass. Protocol-generated resource,
event-payload, auth, error-catalogue, and command-job conformance remains blocked
and is reported separately.

- [ ] **Step 7: Request independent read-only review**

Give the reviewer the design, this plan, protocol authority SHA, and working-tree
diff. Require severity-ranked findings with exact file and line evidence and an
explicit check for invented protocol surface, credential leakage, blind retries,
SSE replay loss, and browser/Node semantic mismatch.

- [ ] **Step 8: Fix validated findings with failing tests first**

For every behavioral finding, add a focused regression test, run it to observe
the expected failure, implement the minimal fix, and rerun the focused and full
gates.

- [ ] **Step 9: Commit and push once**

After re-running Step 6 on the final tree:

```bash
git add --all
git commit -m "feat: build TypeScript SDK transport foundation"
git push origin main
```

Expected: one coherent commit on `main`, pushed once after local validation.
