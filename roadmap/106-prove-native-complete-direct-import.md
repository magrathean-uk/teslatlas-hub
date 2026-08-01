---
type: wayfinder:task
status: closed
parent: 000-map
---
# Prove native complete direct import

Blocked by: [Seed complete corpus case](105-seed-complete-corpus-case.md).

## Question

Does the direct PostgreSQL binary-COPY path accept the complete selected-car
fixture and emit every expected projection kind?

## Starting recommendation

Add an environment-gated native test that reads the restored fixture through
the public direct path and checks its projection report.

## Resolution

The environment-gated native test restores the Hub-owned complete fixture,
opens the normal read-only owner and capture lanes, runs direct binary COPY,
and checks one completed drive, two attached positions, one skipped unattached
position, one charge, and one charge sample.

Native PostgreSQL proof passed with three concurrent metadata lanes.
