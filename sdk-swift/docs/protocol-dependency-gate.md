# Protocol dependency gate

## Current authority

Checked `teslatlas-protocol` `main` at commit `b7b48a86a7705e8ab016f1debd25cecd20ebbb89` after refreshing `origin` on 2026-08-30.

That revision explicitly says it is foundation-only and has no deployed protocol implementation, generated SDK, or compatibility promise. Its architecture identifies intended artifact classes but says the exact v1 resource schemas are not frozen. The tracked tree contains only:

```text
AGENTS.md
LICENSE
README.md
docs/architecture.md
docs/plans/2026-08-30-foundation.md
```

Untracked schema, example, and test work appeared in the local protocol checkout after this SDK batch began. It is not part of the pinned commit, has no released compatibility status, and was not consumed. This SDK activates contract work only from committed, released protocol artifacts.

## Blocked public work

| SDK slice | Required released protocol artifacts that are absent |
| --- | --- |
| Core types and capabilities | Version/capability schema, feature identifiers, negotiation rules, deprecation rules, compatibility fixtures |
| Typed errors | Error envelope schema, stable code registry, HTTP mapping, retryability rules, request-ID header/field |
| Discovery and identity | Well-known schema, Hub identity representation, endpoint fields, trust material, pinning and identity-change rules |
| Pairing and bearer rotation | Invitation/claim schemas, state machine, expiry/cancellation rules, persisted resume fields, token issuance/rotation/revocation rules |
| Paginated queries and ETags | OpenAPI paths and models, opaque-cursor semantics, UTC filter encoding, bounds, ETag and conditional-request rules |
| SSE replay/reconnect | Stream endpoint, event names and payload schemas, ID semantics, replay retention, reset/gap response, reconnect bounds and fixtures |
| Signed manifests | Manifest schema, canonical byte representation, key distribution, signature algorithm, rotation rules and verification vectors |
| Pack downloads | Pack endpoint, byte-range/validator rules, length and integrity fields, transfer bounds, restart and corruption fixtures |
| Conformance | Deterministic redacted fixtures, runner interface, expected outputs and previous-two-minor-version corpus |

The SDK does not infer any of those fields, names, paths, algorithms, limits, or errors from the product brief, proprietary App, or AGPL Hub implementation.

## Safe foundation present

The package currently contains only internal, contract-neutral mechanics:

- incremental standard SSE line framing with configurable internal caps (64 KiB per line and 8 MiB per event by default);
- actor-isolated atomic persistence for caller-defined Codable state;
- strict validation of a resumed HTTP 206 response, byte range, and caller-supplied opaque ETag;
- deterministic XCTest coverage and a build-only standalone example.

These declarations are internal. Importing `TeslatlasHubSDK` does not expose a speculative public Hub API.

## Activation rule

Implement one public slice only after its schema/specification and deterministic fixtures land in a released protocol revision. Pin that revision, write fixture-derived failing tests, implement protocol-derived Swift types and behaviour, then run the language-neutral conformance runner. Do not activate a slice from prose candidates alone.
