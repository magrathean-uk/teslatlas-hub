---
type: wayfinder:task
status: closed
parent: 000-map
---
# Stream cars through binary COPY

Blocked by: [Build fixed binary COPY statements](084-build-fixed-binary-copy-statements.md).

## Question

Can the first direct-import projection decode PostgreSQL binary COPY rows with
the same typed output and hard row limit as the previous prepared query path?

## Starting recommendation

Use `BinaryCopyOutStream` with the reviewed car output types, retain each row
only after its binary frame is validated, and preserve the existing selected-car
and missing-car behavior.

## Resolution

Cars now use PostgreSQL binary `COPY TO STDOUT` and the reviewed fifteen-column
type contract. Binary frames stream directly into typed car facts under the
existing hard source-row ceiling; no car history text dump, JSON, page vector,
or source write is used.
