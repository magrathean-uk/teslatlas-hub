---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design delta synchronization

Blocked by: [Design full-snapshot synchronization](047-design-full-snapshot-sync.md).

## Question

How do cursors, ordered mutations, tombstones, retention floors, compaction,
deduplication, and snapshot-required recovery work?

## Starting recommendation

Base deltas on a durable source-ordered ledger and never serve a cursor whose
required history has been compacted.

## Resolution

Current phone sync stays full-snapshot only. Future deltas use a durable,
idempotent per-generation mutation ledger and exact signed cursor binding.
Only contiguous verified ranges apply atomically; missing, old, changed, or
quarantined bases require a full snapshot. Compaction waits for a durable floor,
verified snapshot, and retained backup window.
