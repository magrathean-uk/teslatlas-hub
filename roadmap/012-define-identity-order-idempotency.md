---
type: wayfinder:task
status: closed
parent: 000-map
---
# Define identity, ordering, and idempotency

Blocked by: [Define the observation journal](011-define-observation-journal.md).

## Question

How are accounts, vehicles, observations, sessions, rows, retries, and source
replays identified and ordered across restarts?

## Starting recommendation

Use stable source identities, monotonic per-vehicle sequence metadata, and
content-aware idempotency keys with explicit gap detection.

## Resolution

Source identity is the stable non-secret `(kind, key)` pair and vehicle identity
is `(source_id, source_vehicle_key)`; TeslaMate derives its Hub UUID from that
identity. Journal retry identity is source, vehicle, observed time, and
canonical payload hash. Replay order is observed time then durable observation
ID; lifecycle cursor advancement makes replay and restart idempotent. A durable
per-vehicle reservation allocates each full-snapshot sequence before building.
Unpublished failed work may leave a harmless full-snapshot gap; future deltas
must reject gaps or overlaps against their explicit base sequence.
