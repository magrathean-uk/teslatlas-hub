---
type: wayfinder:task
status: closed
parent: 000-map
---
# Set the durability policy

Blocked by: [Design the query and index contract](018-design-query-index-contract.md).

## Question

What transaction, WAL, checkpoint, synchronization, atomic rename, and fsync
policy satisfies acknowledged-write durability under crash and power loss?

## Starting recommendation

Prefer stronger durability defaults and relax them only through measured,
explicit profiles that never weaken journaled observation guarantees.

## Resolution

Hub uses WAL/full synchronous immediate transactions for acknowledged journal,
lifecycle, pairing, sequence, and manifest work. A pack is fully written,
verified, file-synced, immutably linked, and directory-synced before its
manifest transaction. Staging is full synchronous and seals only after complete
integrity/accounting checks. A crash may leave an unreferenced verified pack but
never a published manifest without its durable pack. Profiles cannot weaken
journal, lifecycle, manifest, or stage durability; checkpoint changes require
measured crash/recovery evidence.
