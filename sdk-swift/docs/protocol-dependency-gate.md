# Protocol activation record

## Current authority

This batch consumes `teslatlas-protocol` commit
`79ced4c7fdc79520ad31d72a0280bf5f3f19f407`, profile `1.2.0`, with compatibility
profiles `1.0.0`, `1.1.0`, and `1.2.0`.

The authority now contains committed OpenAPI, JSON Schemas, SSE contract,
examples, deterministic fixtures, compatibility profiles, and the
language-neutral conformance runner. Copies of the released examples used by
XCTest are pinned under `Tests/TeslatlasHubSDKTests/Fixtures` with the exact
authority SHA.

## Activated slices

| Slice | Authority | SDK behavior |
| --- | --- | --- |
| Discovery | discovery schema and versioning rules | strict versions, capabilities, limits, HTTPS, forbidden private fields |
| Endpoint trust | stable Hub identity plus advertised endpoints | fixed trusted-origin set and fail-closed identity refresh |
| Errors | RFC 9457 schema and stable codes | typed problem details preserving request IDs and retryability |
| Queries | OpenAPI resources, cursors, limits, ETags | typed read routes, opaque cursors/ETags, bounded history and response bytes |
| Events | SSE contract and event envelope | request construction, bounded framing, retry cap, identity semantics |
| Commands | command schema and catalogue | separate product, confirmation gate, UUID idempotency, one network attempt |

## Still gated or incomplete

| Slice | Reason |
| --- | --- |
| Pairing and bearer rotation | Deployment contract is explicitly outside the released query protocol |
| Certificate pin material | No public key/certificate pin distribution contract exists |
| Signed manifests and packs | No manifest schema, signature vectors, pack endpoint, or integrity contract exists |
| Mutable metadata | Protocol exists; SDK API and conditional-write tests are not implemented in this batch |
| Managed SSE reconnect | Request and decoder exist; terminal/replay/reconnect orchestration remains caller-owned |
| Full conformance | No independently implemented SDK JSONL adapter runs all 31 profile/case combinations yet |

## Proof boundary

Passing XCTest proves the Swift behavior and decoding of the pinned released
examples. Running `teslatlas-protocol` validation proves the authority corpus
is internally valid. Neither result alone proves end-to-end Hub interoperability
or full SDK conformance.

Remaining external gates are live Hub testing, Xcode/iOS build validation,
device Keychain/local-network behavior, forced termination around checkpoints,
and the protocol JSONL adapter.
