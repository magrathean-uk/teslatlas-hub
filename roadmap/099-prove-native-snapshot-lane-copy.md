---
type: wayfinder:task
status: closed
parent: 000-map
---
# Prove native snapshot lane copy

Blocked by: [Prove native source write refusal](098-prove-native-source-write-refusal.md).

## Question

Can a second native read-only transaction attach to an exported PostgreSQL
snapshot and return the typed binary selected-car row?

## Starting recommendation

Export the owner snapshot, attach a second transaction using the validated
snapshot statement before its copy query, and require the same typed result.

## Resolution

Native proof passed: PostgreSQL accepted the exported owner snapshot in a
second read-only lane, and that lane returned the same typed binary car fact.
