---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design operational observability

Blocked by: [Decide MQTT and integration parity](044-decide-mqtt-integration-parity.md).

## Question

Which health, readiness, freshness, lag, queue, disk, database, credential, and
per-vehicle signals must operators and Teslatlas see?

## Starting recommendation

Expose low-cardinality structured metrics and reason-coded health without raw
telemetry or credential leakage.

## Resolution

Health is liveness; readiness is truthful catalogue, schema, identity, and
quarantine state; `doctor` keeps detailed local reasons. Future local metrics
cover freshness, lifecycle, collection, import, storage, credential state, and
reason-coded outcomes with bounded cardinality. Logs and metrics contain no
secret or raw telemetry and never block durable work.
