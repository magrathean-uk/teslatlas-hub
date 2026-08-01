---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design comprehensive source validation

Blocked by: [Design migration transformation](057-design-migration-transform.md).

## Question

Which structural, relational, temporal, numeric, spatial, lifecycle, duplicate,
orphan, aggregate, and corruption checks must run against the TeslaMate copy?

## Starting recommendation

Run fast gates before copying, deep read-only validation on the preserved copy,
and classify every anomaly as fatal, repairable, or accepted with evidence.
Push set-based validation and aggregate checks into read-only PostgreSQL queries
instead of parsing every row twice.

## Resolution

Fast source gates, typed capture checks, and deep sealed-stage validation prove
structure, relations, values, lifecycle, counts, hashes, and aggregates.
Every anomaly is fatal, Hub-repairable, or accepted with durable evidence; only
the latter can reach a reduced projection.
