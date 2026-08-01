---
type: wayfinder:task
status: closed
parent: 000-map
---
# Set retention and compaction policy

Blocked by: [Set the durability policy](019-set-durability-policy.md).

## Question

What may be compacted or deleted, when is a full rebuild still possible, and
how are sync cursors protected from retention gaps?

## Starting recommendation

Retain all canonical vehicle history by default; compact only replay-safe
journals and force snapshot recovery before crossing a cursor floor.

## Resolution

Canonical identity, observations, history, manifests, referenced packs, and
sealed migration evidence retain by default. No automatic journal/history
deletion, pack pruning, cursor-floor advance, or VACUUM occurs. Repair may
remove only verified unreferenced packs and temporary unpublished files. Full
snapshots currently have no retention-sensitive client cursor. Before delta or
operator retention, Hub must publish replacement snapshot recovery, record a
durable floor, retain newer data plus backup window, and reject older cursors
explicitly for snapshot recovery.
