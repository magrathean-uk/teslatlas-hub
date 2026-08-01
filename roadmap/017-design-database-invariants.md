---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design database invariants

Blocked by: [Design schema evolution](016-design-schema-evolution.md).

## Question

Which foreign keys, uniqueness rules, intervals, value ranges, lifecycle
constraints, and cross-table facts must the database enforce?

## Starting recommendation

Push durable invariants into schema constraints where possible and duplicate
critical checks in validation tooling with named failure codes.

## Resolution

Schema enforcement covers foreign ownership, uniqueness, IDs, generations,
timestamps, bounded payloads, append-only observations, pairing/token rules,
and snapshot sequence reservation. Projection commits couple entities and
lifecycle cursor atomically. Pack validation enforces parent presence, selected
car ownership, unique IDs, intervals, finite numeric values, SOC, and WGS84
coordinates before integrity verification and publication. Database and
projection checks use named failures; integrity and later reconciliation are
readiness gates, not advisory reports.
