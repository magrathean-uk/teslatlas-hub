---
type: wayfinder:task
status: closed
parent: 000-map
---
# Build the performance baseline

Blocked by: [Design the migration audit report](066-design-migration-audit-report.md).

## Question

Which CPU, memory, disk, WAL, query, migration, collection, sync, startup, and
recovery measurements establish a trustworthy baseline?

## Starting recommendation

Measure representative small, large, and pathological histories on each target
host class before introducing adaptation. Include the existing roughly
10-million-row database, target under 10 minutes, and treat 30 minutes as the
hard migration ceiling on a supported baseline host.

## Resolution

The versioned baseline protocol measures phase-by-phase host, resource, copy,
migration, collection, sync, startup, and recovery evidence across all
supported host classes and corpus shapes. Only fully reconciled runs count;
all qualifying representative runs must finish under ten minutes, while thirty
minutes is an immediate redesign failure.
