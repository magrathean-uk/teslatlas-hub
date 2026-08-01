---
type: wayfinder:task
status: closed
parent: 000-map
---
# Back up immutable pack set

Blocked by: [Add consistent Hub catalogue backup](115-add-consistent-hub-catalogue-backup.md).

## Question

How can a restored catalogue retain every immutable pack it references?

## Starting recommendation

Build a new Hub-owned backup directory from the consistent catalogue snapshot,
then copy and verify only its canonical referenced pack objects.

## Resolution

The complete backup creates a new Hub-owned directory, snapshots its catalogue,
then reads that immutable copied catalogue to enumerate only referenced packs.
Each pack name is constrained to a digest, copied into the canonical restore
layout, and size-checked. Failure removes only the newly created backup root.

Focused pack-set restore proof passed.
