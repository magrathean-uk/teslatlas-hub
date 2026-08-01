---
type: wayfinder:task
status: closed
parent: 000-map
---
# Prove native binary car copy

Blocked by: [Cache direct relation positions](096-cache-direct-relation-positions.md).

## Question

Does the typed binary car stream work against a real PostgreSQL server rather
than only local type-layout tests?

## Starting recommendation

Use a disposable Hub-owned local PostgreSQL instance and an opt-in test that
creates only the reviewed car projection. Verify a read-only snapshot returns
typed selected-car data and excludes another car.

## Resolution

Native local PostgreSQL proof passed: a read-only repeatable-read transaction
streamed the selected car through binary COPY, retained one typed row, and
excluded the other-car row. The server was disposable and Hub-owned.
