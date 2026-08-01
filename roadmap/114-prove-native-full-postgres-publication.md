---
type: wayfinder:task
status: closed
parent: 000-map
---
# Prove native full PostgreSQL publication

Blocked by: [Clean prepublication candidate failures](113-clean-prepublication-candidate-failures.md).

## Question

Can the complete native source fixture traverse read-only capture, direct
projection, pack verification, signed manifest publication, and Hub catalogue
durability as one operation?

## Starting recommendation

Add an environment-gated local PostgreSQL proof through the public importer
and inspect the stored manifest totals.

## Resolution

The environment-gated native fixture test now performs the public PostgreSQL
import into a fresh Hub store. It checks the six projected facts, stored signed
manifest identity and total, nonempty immutable chunks, and a final Hub repair
integrity result.

Native PostgreSQL publication proof passed.
