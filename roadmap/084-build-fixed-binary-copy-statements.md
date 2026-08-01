---
type: wayfinder:task
status: closed
parent: 000-map
---
# Build fixed binary COPY statements

Blocked by: [Attach a snapshot capture lane](083-attach-snapshot-capture-lane.md).

## Question

How can each reviewed TeslaMate projection enter PostgreSQL binary `COPY TO
STDOUT` without parameters, user-controlled SQL, text dumps, or a limited page?

## Starting recommendation

Derive every statement only from the fixed reviewed projection table and the
validated smallint car id. Replace the existing keyset cursor with its first
valid value and use `LIMIT ALL`; wrap the query in `COPY (...) TO STDOUT` with
binary format.

## Resolution

Every reviewed source projection now has one fixed `COPY (query) TO STDOUT`
binary statement. It contains no source credentials, caller SQL, parameters,
pagination cap, text dump, file path, or server-side command; only the
already-validated selected-car smallint is rendered into the fixed query.
