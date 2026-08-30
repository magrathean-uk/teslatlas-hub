# Protocol foundation plan

## Goal

Publish a testable v1 public protocol that an unaffiliated developer can use without inspecting product source.

## Dependencies

- Hub defines authoritative storage, pairing, and service constraints.
- Swift and TypeScript SDKs consume only released protocol artifacts.
- Viewer and Home Assistant prove independent client interoperability.
- Edge uses the ingestion and delivery contracts approved here.

## Delivery sequence

1. Record public identity, capability-negotiation, versioning, and deprecation rules.
2. Specify canonical observation, projection, data-quality, command-job, and mutable-metadata semantics.
3. Write OpenAPI, JSON Schema, and event-stream contracts for the approved v1 surface.
4. Add redacted deterministic fixtures covering normal, duplicate, delayed, missing, and reordered observations.
5. Add a language-neutral conformance runner and require Hub plus both SDKs to pass it.
6. Freeze a v1.0 compatibility baseline and test the previous two minor versions before release.

## Acceptance

- Independent client implementation succeeds from public documentation, schemas, fixtures, and SDKs alone.
- Cursors, ETags, error codes, SSE replay, capability negotiation, and deprecation rules have executable conformance coverage.
- Fixtures never require a Tesla account, Hub database, secret, VIN, or proprietary product source.

## Out of scope

Hub implementation, generated SDK source, a web dashboard, or command UI.
