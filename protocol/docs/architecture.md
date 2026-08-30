# Protocol architecture

## Responsibility

Own versioned, implementation-neutral public contracts. Hub is the authoritative server implementation; this repository is the public compatibility authority.

## Contract layers

| Layer | Artifact | Purpose |
| --- | --- | --- |
| Discovery | well-known schema | Hub identity, versions, capabilities, endpoints |
| Query | OpenAPI | Bounded REST resources, cursors, ETags, errors |
| Events | AsyncAPI or equivalent | SSE event names, payloads, replay rules |
| Data | JSON Schema | Observation, projection, quality, command, metadata payloads |
| Compatibility | Fixtures and conformance suite | Cross-client behaviour and two-minor-version support |

## Public rules

- Semantic protocol versions and capability negotiation.
- Opaque cursors only; no offset pagination.
- UTC time bounds, stable machine-readable errors, ETags, and conditional requests.
- SSE supports Last-Event-ID. WebSocket needs a separately approved use case.
- Provider telemetry is immutable; projections are reproducible; user metadata is versioned and audited.
- Command jobs are asynchronous, scoped, idempotency-keyed, and state-verified.
- No secret, VIN, raw provider token, or precise-location requirement belongs in a public fixture.

## Boundaries

The protocol is permissively licensed. It contains schemas, fixtures, specifications, and independently authored examples only. It does not contain AGPL Hub implementation code or proprietary Teslatlas product code.

## Initial v1 surface

`/.well-known/teslatlas-hub`, vehicle/current-state, drives/positions, charges/samples, state/update history, events, data-quality, and command-job resources are the initial contract candidates. Exact resource schemas are not frozen by this document.
