# Protocol-gated Swift SDK foundation design

## Status

Approved by the request to start the public package now while making only safe harness progress when released protocol artifacts are unavailable.

The dependency gate is active at `teslatlas-protocol` commit `b7b48a86a7705e8ab016f1debd25cecd20ebbb89`. That revision contains foundation prose only. It has no discovery schema, OpenAPI, event contract, JSON Schema, deterministic fixtures, conformance runner, pairing or bearer contract, manifest signature format, or immutable-pack transfer contract.

## Scope

Create an importable Swift package and contract-neutral internal seams that can later host generated or hand-authored protocol-derived code. Add deterministic tests for mechanics already fixed by the SDK architecture or open standards:

- bounded incremental Server-Sent Events framing;
- crash-resumable atomic state persistence;
- validation of resumed HTTP byte-range responses;
- an executable example that states the active protocol gate.

Do not add public discovery, identity, capability, pairing, bearer, query, event-payload, manifest, pack, or error models. Those names, fields, codes, algorithms, bounds, and compatibility rules must come from released `teslatlas-protocol` artifacts.

## Package shape

The package exports one library product, `TeslatlasHubSDK`, and one executable example, `TeslatlasHubSDKExample`. The library intentionally exposes no protocol API while the gate is active. Internal files stay small and independent:

- `ServerSentEventDecoder.swift` owns incremental UTF-8 SSE framing.
- `AtomicJSONStateStore.swift` owns actor-isolated, atomic Codable persistence.
- `RangeResumeValidator.swift` owns HTTP 206 and `Content-Range` validation for a caller-supplied offset and optional opaque validator.

No package dependency is required. Foundation supplies file and byte handling. FoundationNetworking is conditionally imported for non-Apple SwiftPM validation.

## Behaviour

### SSE framing

The decoder accepts arbitrarily split byte chunks, normalises CRLF/CR/LF line endings, ignores comment lines, joins repeated `data` fields with a newline, parses non-negative `retry` milliseconds, preserves opaque event IDs, and emits only when a blank line terminates an event. Internal configurable caps default to 64 KiB per line and 8 MiB of data per event; overflow throws and resets parser state. It does not interpret Teslatlas event names or payloads.

### Persistent state

The store accepts any internal `Codable & Sendable` state, writes encoded bytes atomically, reloads after a new process/store instance, and removes state idempotently. It does not define pairing or download state schemas and does not choose credential storage.

### Range validation

The validator accepts only `206 Partial Content` for non-zero resume offsets, requires a syntactically valid `Content-Range` whose start equals the requested offset, rejects impossible totals, and optionally requires an opaque ETag to remain unchanged. It does not choose manifest fields, digest algorithms, signature algorithms, endpoint paths, retry limits, or download bounds.

## Error boundary

Internal mechanic failures use internal Swift errors so tests can distinguish malformed SSE text, persistence failures, and invalid range responses. No public `TeslatlasError` taxonomy or server-code mapping is created before the protocol error schema and code registry exist.

## Testing

Tests use literal byte fixtures and temporary directories. Each behaviour is introduced red-first, then implemented minimally. No network, Tesla account, Hub database, VIN, secret, proprietary source, or AGPL implementation is used.

The full validation set is:

```sh
swift test
swift run TeslatlasHubSDKExample
DEVELOPER_DIR=/Applications/Xcode-beta.app/Contents/Developer xcodebuild \
  -scheme TeslatlasHubSDK \
  -destination 'generic/platform=iOS' \
  build
```

## Contract activation gate

Public SDK work resumes only after the protocol repository releases all artifacts needed by that slice. At minimum:

1. discovery schema with Hub identity, endpoint, trust and capability semantics;
2. pairing and bearer lifecycle schemas plus crash-resume and rotation fixtures;
3. OpenAPI with resource models, opaque cursor rules, ETag semantics, limits and stable error schema/code registry;
4. event contract with SSE names, payload schemas, replay retention/reset rules and deterministic replay fixtures;
5. signed-manifest schema with canonical bytes, key distribution, signature algorithm and verification vectors;
6. pack endpoint/range contract with validators, bounds, integrity rules and interrupted-download fixtures;
7. language-neutral conformance runner and compatibility fixtures for the supported protocol versions.

Until then, the standalone example is a build and packaging example, not a functional Hub client.
