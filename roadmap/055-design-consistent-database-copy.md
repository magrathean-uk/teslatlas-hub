---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design the consistent database copy

Blocked by: [Design migration preflight](054-design-migration-preflight.md).

## Question

How is a transactionally consistent TeslaMate database copy created, retained,
verified, resumed, and protected quickly while TeslaMate may still be writing?

## Starting recommendation

Use one PostgreSQL read-only repeatable-read snapshot, preferably exported to
parallel typed `COPY ... TO STDOUT (FORMAT BINARY)` lanes. Stream directly into
an unpublished Hub staging database with bounded buffers. Never create a text
dump, JSON copy, raw PostgreSQL data-file copy, source temp table, or source
write. Preserve TeslaMate unchanged throughout migration.

## Resolution

One exported read-only repeatable-read snapshot feeds bounded typed binary COPY
lanes into one unpublished Hub stage. A failed capture is discarded and starts
fresh; only a sealed verified stage may continue Hub-only publication, while
TeslaMate remains unchanged.
