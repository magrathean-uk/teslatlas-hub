# Fake Tesla and fault injection v1

Routine proof uses a deterministic, local simulator only. It has one virtual
monotonic clock; scenarios advance that clock explicitly, so tests do not sleep
or depend on wall time, a live car, TeslaMate deployment, or external network.
A fixed-order scheduler delivers discovery, vehicle data, timer, storage, and
restart events. Every run records scenario digest, seed, adapter/profile
identity, virtual-time trace, expected outcome, and audit hashes.

## Scenario contract

Versioned canonical fixtures describe vehicles, discovery-state transitions,
timestamped vehicle-data payloads, and an ordered request/response script.
The fake HTTP source accepts only expected `GET /api/1/products` and permitted
online-only `GET /api/1/vehicles/{id}/vehicle_data` requests. It captures method,
path class, virtual time, request order, and redacted authorization-presence;
any wake, command, unexpected route, redirect following, or out-of-order
request fails the scenario. Fixture payloads are hash-addressed and reject
credential-shaped fields before recording.

Scripts can return valid/malformed/truncated/oversized envelopes, empty or
ambiguous products, offline/asleep/suspended/unknown states, changed/regressed
timestamps, duplicate payloads, 401/403/404/408/429 with valid or invalid
`Retry-After`, 5xx, redirects, TLS/certificate failures, DNS/connect failure,
slow headers/body, connection reset, partial body, and source recovery. Network
delivery is bounded by virtual-time delay, byte/chunk, bandwidth, and backlog
controls. No fault injects a write into any external source.

The companion migration source fixture provides a pinned schema/migration
fingerprint and deterministic typed rows or binary-COPY frames for selected-car
tables. It scripts read-only snapshot/export failure, page boundaries, malformed
types, row/byte overflow, duplicate IDs, relationship gaps, cancellation, and
reconnect. It asserts the expected read-only/repeatable-read/UTC contract and
never starts, configures, or writes PostgreSQL or TeslaMate.

## Fault points and proof

Hub exposes named test-only fault points immediately before and after durable
observation/lifecycle transactions, stage page commit/seal, pack write/fsync/
rename, manifest catalogue commit, cursor write, pairing claim, checkpoint, and
recovery cleanup. The harness can return an error, stall virtual time, exhaust
a bounded resource, corrupt a private Hub copy, or terminate the Hub child at
one named point. Restart tests reopen only that private data directory and
assert truthful readiness within the locked objective, durable-ack retention,
idempotent replay, manifest/pack consistency, and zero duplicate projected
facts. Corruption fixtures never target a user database or pack.

Scenario families cover all availability/activity transitions, interval and
session edges, token/scope/rate/circuit recovery, source and storage pressure,
request retries, projection/quarantine, pack/sync/pairing failures, migration
capture/publication/reconciliation, and operator-plan refusal. Differential
fixtures later feed identical normalized source events to pinned TeslaMate and
Hub. A scenario passes only when its full request audit, durable state,
projection, readiness, and expected fault/recovery result agree; output is a
redacted migration/fault report, never an unchecked log.
