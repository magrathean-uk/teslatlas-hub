---
type: wayfinder:task
status: closed
parent: 000-map
---
# Prove native source write refusal

Blocked by: [Prove native binary car copy](097-prove-native-binary-car-copy.md).

## Question

Does the native binary-copy source session retain PostgreSQL's read-only
boundary after it has returned telemetry?

## Starting recommendation

In the disposable native source test, attempt one insert after binary COPY and
require PostgreSQL to reject it before rollback.

## Resolution

Native proof passed: PostgreSQL returned the typed binary car row and rejected
the attempted source insert in the same read-only snapshot transaction.
