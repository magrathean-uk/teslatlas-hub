# Swift SDK foundation plan

## Goal

Deliver a public Swift package that proves Teslatlas has no private Hub transport privilege.

## Dependencies

- Released v1 protocol schemas, fixtures, and compatibility rules.
- Hub test endpoint or deterministic fixture server.
- App integration supplies crash-resume, history-import, and multi-vehicle acceptance tests.

## Delivery sequence

1. Define package/module names, minimum Swift and Apple-platform support policy, semantic versioning, and public error rules.
2. Implement protocol-derived value types and capability negotiation without product-local types.
3. Implement pairing, bearer rotation, discovery, endpoint roaming, query cursors, SSE, and signed-pack transport.
4. Build deterministic fixture tests for cancellation, restart, TLS identity change, partial download, duplicate prevention, and forced termination.
5. Replace practical app transport paths with this SDK behind a thin product-specific wrapper.
6. Publish API documentation, migration notes, and protocol-version compatibility evidence.

## Acceptance

- A standalone Swift sample can pair, discover, retrieve current state and history, consume events, and resume a pack download.
- App acceptance covers five-year import memory bounds, incremental sync after termination, and vehicle/data isolation.
- No SDK source imports AGPL Hub implementation or proprietary app code.

## Out of scope

SwiftUI screens, Rust FFI, local analytics, and vehicle command UI.
