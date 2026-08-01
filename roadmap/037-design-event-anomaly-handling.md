---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design event anomaly handling

Blocked by: [Design vehicle metadata and telemetry completeness](036-design-vehicle-metadata-telemetry.md).

## Question

How are duplicate, delayed, out-of-order, missing, contradictory, future-dated,
and malformed observations handled without silent history corruption?

## Starting recommendation

Journal them with classification, quarantine unsafe projection effects, and
make deterministic replay produce the same accepted history and warnings.

## Resolution

Exact retries deduplicate; same-time differing payloads remain conflicting
facts. Projection order is durable observation order, while source-time
disorder, missing data, contradiction, and malformed payloads are classified.
Unsafe projection quarantines only the affected cursor and keeps replayable
facts, completed history, gaps, and warnings intact.
