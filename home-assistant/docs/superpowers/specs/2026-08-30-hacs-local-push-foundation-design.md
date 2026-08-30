# HACS local-push foundation design

## Status and scope

This design implements the Home Assistant side of the approved foundation while the public Teslatlas protocol remains unfrozen. It must not invent HTTP paths, request bodies, response schemas, Zeroconf TXT keys, QR formats, or event names.

The deliverable is a runnable, locally tested custom-integration foundation with a typed client boundary. Fixture clients exercise Home Assistant behaviour without claiming that the fixtures are released protocol contracts. The production client fails closed with a clear protocol-readiness error until released protocol artifacts exist.

Commands remain absent. No code may read Hub SQLite, accept Tesla credentials, poll Tesla, call private APIs, or import code from Hub or the proprietary app.

## Architecture

The integration domain is `teslatlas_hub`, displayed as “Teslatlas Hub”, and classified as a local-push hub integration.

`client.py` owns the public-client protocol boundary. It exposes typed operations for probing Hub identity, claiming a pairing secret, loading one initial snapshot, and consuming an event stream with `Last-Event-ID` continuity. No concrete route is encoded before the protocol repository publishes its OpenAPI and event contracts.

`coordinator.py` owns the Home Assistant runtime. It performs one initial snapshot during setup, then consumes push events. Connection loss marks entities unavailable and starts bounded exponential reconnect. Authentication loss stops reconnect and starts Home Assistant reauthentication. It never schedules periodic updates.

`config_flow.py` supports manual setup, Zeroconf discovery, pairing-secret claim, reauthentication, and endpoint reconfiguration. Discovery uses only the published service type `_teslatlas-hub._tcp.local.` plus the host and port supplied by Home Assistant; it assumes no TXT fields. Stable Hub identity comes from the client probe. Pairing secrets are exchanged for a device bearer and are never persisted.

Entities are read-only sensors grouped into one Hub device and one device per vehicle. Stable public Hub and vehicle identifiers form unique IDs. Missing individual fields produce unknown entity state; a lost Hub connection makes the related entities unavailable.

Diagnostics return configuration and runtime summaries only. Device bearers, pairing material, endpoints, vehicle identifiers, names, coordinates, event identifiers, and raw payloads are omitted or redacted.

## Data flow

1. A manual or Zeroconf flow constructs an endpoint and probes the Hub through the client boundary.
2. The user supplies a pairing secret. The client returns stable Hub identity plus a scoped device bearer.
3. Home Assistant stores endpoint fields, Hub identity, and bearer in the config entry.
4. Setup validates identity and authentication, loads one current snapshot, creates devices/entities, and starts the event task.
5. Each accepted event replaces or merges an immutable snapshot and notifies entities.
6. Connection failure marks the coordinator failed, preserves the replay cursor in memory, and reconnects with capped exponential delay.
7. Authentication failure starts reauthentication. Endpoint changes use reconfigure and must resolve to the same Hub identity.

## Initial entities

Hub sensors: collector health, estimated Fleet API cost, and backup age.

Vehicle sensors: state of charge, charging state, charging power, charge limit, estimated range, odometer, activity state, inside temperature, outside temperature, access state, software version, software update state, telemetry age, and data quality.

The internal fixture model uses these fields solely to test Home Assistant mapping. The concrete protocol adapter must later translate released public schemas into this model.

## Packaging and contributor workflow

The repository contains one `custom_components/teslatlas_hub` integration, root `hacs.json`, a pinned local development environment, redacted deterministic fixtures, and local pytest/Ruff commands. It contains no hosted CI, Dependabot, release workflow, or HACS publication action.

Fixture tests require no Hub, Tesla account, Tesla credential, network access, or private source. A future live test is permitted only against the released public protocol.

## Explicit release gates

Runtime transport remains disabled until `teslatlas-protocol` publishes all of:

- well-known discovery schema and Hub identity rules;
- Zeroconf TXT contract and TLS/endpoint rules;
- pairing/claim and bearer lifecycle contract;
- current-state query schemas and stable errors;
- SSE event names, payloads, replay, and version negotiation;
- redacted compatibility fixtures and conformance coverage.

HACS publication remains disabled. It additionally needs an approved/licensed brand asset, the required repository topics, public releases, and the validation process required at publication time; GitHub-hosted automation is outside this repository's approved scope.
