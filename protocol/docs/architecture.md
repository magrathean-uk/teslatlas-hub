# Protocol architecture

## Responsibility

Own versioned, implementation-neutral public contracts. Hub is the authoritative server implementation; this repository is the public compatibility authority.

## Contract layers

| Layer | Artifact | Purpose |
| --- | --- | --- |
| Discovery | `schemas/discovery.schema.json` | Hub identity, versions, command catalogue, capabilities, endpoints, limits |
| Query | `openapi/teslatlas-v1.openapi.json` | Bounded REST resources, cursors, ETags, errors |
| Events | `events/teslatlas-v1.sse.json` | SSE event names, payloads, framing, replay rules |
| Data | `schemas/*.schema.json` | Observation, projection, quality, resource, command, metadata payloads |
| Compatibility | `fixtures/`, `compatibility/`, `conformance/` | Deterministic cross-client behaviour and two-minor-version support |

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

## v1 surface

`/.well-known/teslatlas-hub`, vehicle/current-state, drives/positions,
charges/samples, state/update history, events, data-quality, asynchronous
command jobs, and mutable metadata are specified by OpenAPI and the canonical
schemas. This prose explains boundaries; machine-readable artifacts are the
wire authority.

## Source-of-truth order

1. JSON Schemas define payload validity.
2. OpenAPI defines HTTP operations, parameters, status codes, and media types.
3. The SSE contract defines event framing and replay behaviour.
4. Compatibility profiles and conformance cases define executable behaviour.
5. Prose documents explain intent without overriding machine-readable rules.

OpenAPI embeds generated copies of all standalone schemas so generic OpenAPI
3.1 tooling does not need a custom URN resolver. `tools/build_openapi.py --check`
proves those copies match the canonical schema files.
