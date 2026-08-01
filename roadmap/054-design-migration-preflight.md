---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design migration preflight

Blocked by: [Design migration discovery](053-design-migration-discovery.md).

## Question

Which source health, backup, permissions, versions, clocks, network, disk,
memory, ports, service, estimated transfer time, and rollback checks must pass
before migration?

## Starting recommendation

Generate a signed or checksummed preflight report and make every failed gate
non-mutating and reason-coded. Estimate row counts and bytes using read-only
catalog and aggregate queries, then reject insufficient disk or a projected
runtime beyond the migration ceiling before copying.

## Resolution

Preflight is a reason-coded, signed/checksummed read-only gate. It proves
source compatibility, selected-car scope, target capacity, rollback readiness,
and measured runtime prediction before any unpublished Hub migration stage is
created; all failures stop without source mutation.
