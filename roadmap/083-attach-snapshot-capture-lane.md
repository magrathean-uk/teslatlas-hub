---
type: wayfinder:task
status: closed
parent: 000-map
---
# Attach a snapshot capture lane

Blocked by: [Export a source snapshot lease](082-export-source-snapshot-lease.md).

## Question

How does a capture connection attach to the exported source view without
falling back to an unsafe or inconsistent transaction?

## Starting recommendation

Open another read-only repeatable-read PostgreSQL transaction, set the
validated exported snapshot before any source query, then repeat source schema
validation. Keep the owner lease until this lane ends.

## Resolution

Direct capture now attaches a separate read-only lane to the validated exported
snapshot before any source query. The schema is checked again inside that lane,
and both lane and owner transaction finish in order, so lane startup failure or
lease loss ends the attempt rather than mixing source views.
