---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design the query and index contract

Blocked by: [Design database invariants](017-design-database-invariants.md).

## Question

Which collector, repair, migration, Teslatlas sync, and operator queries define
the required index and query-plan workload?

## Starting recommendation

Create representative query-plan budgets before adding indexes, including very
large position histories and latest-state lookups.

## Resolution

Online work is limited to indexed identity, token, manifest, pack, lifecycle,
and bounded journal-page queries. Repair/audit scans are explicitly offline.
The server exposes no arbitrary historic query API. Existing indexes bind each
online predicate/order and staging keyset scan. Every future index requires its
named workload, selectivity and write-cost rationale, plus representative-corpus
query-plan proof that online paths avoid scans/temp sorts and migration remains
keyset-bounded. Performance budgets await the later corpus gate.
