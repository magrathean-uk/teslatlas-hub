# Task 4 report — metadata and command jobs

## Commit

`feat: add metadata and command job clients` (this commit), based on
`d9a762e`.

## Implemented

- Added the six remaining named metadata/command methods on
  `TeslatlasClient`: `listVehicleMetadata`, `createMetadata`, `getMetadata`,
  `replaceMetadata`, `deleteMetadata`, `createCommand`, and `getCommand`.
- Added fixed write templates, generated body validation, capability-first
  preflight, strong metadata ETags, UUID idempotency keys, safe root-relative
  Locations, and exact success-status decoding.
- Command dispatch is one-shot. A transport/abort/nonconforming receipt after
  dispatch becomes a new cause-free `CommandUncertainError`; a conforming
  problem remains `ProtocolHttpError` and is never retried.
- Kept response ETag grammar unbounded while retaining the 512-character
  request `If-Match` bound. No authority, lock, or generated file changed.

## TDD evidence

Focused RED cases were run before their minimal fixes:

- absent Task 4 brands/methods;
- 514-character quoted strong ETags on metadata `200` and `304` rejected by
  request-only length validation;
- abort while reading a command receipt leaked its raw abort reason;
- a 129-character metadata `kind` was rejected despite the locked OpenAPI
  declaring only `type: string`;
- malformed/missing command receipt body, ETag, or Location became
  `ProtocolValidationError` after dispatch;
- forged `Symbol` idempotency input escaped as raw `TypeError`.

Focused GREEN:

```text
npm run test:unit -- --run tests/unit/metadata-operations.test.ts tests/unit/command-operations.test.ts tests/unit/command-safety.test.ts tests/unit/response-decoder.test.ts
npm run typecheck
```

Result: 4 files, 35 tests passed; typecheck passed.

## Browser cache diagnosis

The first browser conformance run failed before tests with stale Vite optimized
dependency CJS interop for `ajv/dist/runtime/equal.js` after the direct Ajv
import-shape change. Clearing Vitest's cache, rebuilding, and rerunning the
browser/package gates passed without changing the direct Ajv design:

```text
npx vitest --clearCache
npm run build
npm run test:browser
npm run pack:check
```

Result: Chromium 12/12 passed; package contents: 122 files.

## Final verification

```text
npx vitest --clearCache
npm run test:browser
npm run protocol:check
npm run verify
npm run typecheck
git diff --check
```

Result: protocol lock passed; browser 12/12 passed; format, lint, typecheck,
build, examples, and package check passed; unit 22 files/157 tests passed;
Node/Chromium conformance 2 files/23 tests passed; whitespace check passed.

## P2 hardening follow-up — read ETags and safe write boundaries

Authority evidence: the pinned protocol's `docs/http.md` states that every JSON
`GET` response exposes `ETag` and accepts `If-None-Match`. The OpenAPI `200`
response components for the JSON read operations also require that header.

### RED tests

Before implementation, focused decoder and client tests proved that a JSON
`200` without `ETag` was accepted by the generic read decoder, including all
Task 3 reads and the Task 4 metadata-list / command-status reads. The same RED
run proved that omitted or `null` JavaScript options for metadata writes and
command creation threw raw property-access `TypeError`s, and that schema-valid
free-form metadata values were serialized lossily or leaked raw stringify
errors.

```text
npm run test:unit -- --run tests/unit/response-decoder.test.ts tests/unit/read-operations.test.ts
# 2 expected failures: missing 200 ETag accepted

npm run test:unit -- --run tests/unit/response-decoder.test.ts tests/unit/read-operations.test.ts tests/unit/metadata-operations.test.ts tests/unit/command-operations.test.ts
# 7 expected failures: ETags, raw TypeErrors, and lossy JSON values
```

### Fix

`decodeReadResponse` now requires an ordinary protocol entity tag for every
JSON `200` read; metadata still layers its strong-tag requirement for both
`200` and `304`. Runtime option access for `replaceMetadata`, `deleteMetadata`,
and `createCommand` is null-safe before branded validation, while TypeScript
signatures remain required. The shared JSON request-body builder now preflights
only lossless JSON values before authorization or Fetch: finite numbers,
complete arrays, plain objects, string keys, data properties, and cycle-free
structure. Invalid values consistently raise cause-free
`InvalidRequestBodyError` (`invalid_request_body`).

The tests cover valid/absent ordinary and strong ETags; omitted/null options;
and nested `NaN`, infinities, undefined/function/symbol values and keys,
BigInt, and cycles, asserting zero authorization/Fetch calls and stable SDK
errors.

### Verification

```text
npm run test:unit -- --run tests/unit/response-decoder.test.ts tests/unit/read-operations.test.ts tests/unit/metadata-operations.test.ts tests/unit/command-operations.test.ts
# 4 files / 49 tests passed

npx vitest --clearCache
npm run test:browser
# Chromium 12/12 passed

npm run protocol:check
npm run verify
npm run typecheck
git diff --check
# lock passed; unit 22 files / 164 tests; conformance 2 files / 23 tests
```

The first clean-cache browser run correctly exposed old conformance fixtures
that returned JSON GET responses without the now-required ETag. Updating those
fixtures to valid ETag-bearing protocol responses made the fresh browser gate
pass; no browser runtime workaround or validator design change was needed.
