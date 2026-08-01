---
type: wayfinder:task
status: closed
parent: 000-map
---
# Rehearse native migration restore

Blocked by: [Back up immutable pack set](116-back-up-immutable-pack-set.md).

## Question

Can a native PostgreSQL migration be restored as a complete Hub catalogue and
pack set in a fresh local Hub root?

## Starting recommendation

Extend the existing native public-import proof with an immediate full backup,
fresh-root open, manifest, pack, and integrity checks.

## Resolution

The native full-import proof now immediately backs up the fresh Hub state,
opens the backup as a new Hub root, checks integrity, compares the restored
manifest exactly, and confirms every referenced immutable pack exists.

Native migration-and-restore rehearsal passed.
