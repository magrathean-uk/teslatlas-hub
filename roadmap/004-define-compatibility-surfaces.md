---
type: wayfinder:task
status: closed
parent: 000-map
---
# Define compatibility surfaces

Blocked by: [Freeze the current Teslatlas contract](003-freeze-teslatlas-contract.md).

## Question

Which TeslaMate-facing surfaces beyond stored telemetry must Hub preserve,
including MQTT, settings semantics, imports, health behavior, and database reads?

## Starting recommendation

Keep only surfaces with a real consumer, but make each inclusion or exclusion
explicit and testable.

## Resolution

Hub preserves only these compatibility surfaces:

- TeslaMate PostgreSQL is a read-only, one-shot Source Database for selected-car
  history migration. Hub probes the pinned schema, uses fixed read-only
  projections in a repeatable-read snapshot, and never becomes a PostgreSQL
  client after publication.
- TeslaMate collection outcomes and settings semantics are parity inputs, not a
  compatible settings database or API. Later collector tickets must make the
  corresponding Hub-owned policy explicit; TeslaMate settings are never copied
  or changed.
- Hub exposes its own machine-readable `/healthz` and `/readyz`, plus the paired
  Teslatlas sync and pairing endpoints. Health means Hub process health and
  local-database readiness, never TeslaMate health.

The following surfaces have no Hub consumer and are excluded: TeslaMate MQTT
broker connection, topic, retain, QoS, discovery, and Home Assistant behavior;
Phoenix routes and `/api/v1` responses; Grafana; web UI; TeslaMate CSV/import
workflows; and direct client reads of the TeslaMate database after migration.
Teslatlas's separate MyTeslaMate API source remains independent and is not a
Hub compatibility target.

This boundary is testable: Hub source access is confined to the read-only
PostgreSQL adapter, the shipped listener exposes only the Hub routes, and the
Hub dependency/configuration surface contains no MQTT, Phoenix, Grafana, or
TeslaMate API client. Any later integration needs an explicit roadmap decision,
starting with ticket 044 for MQTT.
