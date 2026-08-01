---
type: wayfinder:task
status: closed
parent: 000-map
---
# Restore validated corpus schema

Blocked by: [Prove three native COPY lanes](103-prove-three-native-copy-lanes.md).

## Question

How can every repeatable native corpus begin with the exact source relations
and types accepted by the direct reader?

## Starting recommendation

Store a deterministic PostgreSQL schema fixture derived from the reviewed Hub
projection contract, including the Ecto migration marker and enum type.

## Resolution

`fixtures/teslamate-corpus/v1/current-schema.sql` is a deterministic,
Hub-owned PostgreSQL schema fixture for all nine reviewed source tables, their
accepted PostgreSQL types, the `states_status` enum, and the pinned migration
marker. It deliberately has no foreign keys or TeslaMate runtime behaviour, so
synthetic bad-relationship cases remain constructible.

Native PostgreSQL restore confirmed the pinned migration marker and all 125
reviewed projection columns.
