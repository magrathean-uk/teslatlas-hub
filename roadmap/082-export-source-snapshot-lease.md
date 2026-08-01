---
type: wayfinder:task
status: closed
parent: 000-map
---
# Export a source snapshot lease

Blocked by: [Audit implementation completion](081-audit-implementation-completion.md).

## Question

How can Hub retain one source-consistent, read-only PostgreSQL view while
future bounded capture lanes attach without receiving unsafe SQL text?

## Starting recommendation

After source schema validation, export PostgreSQL's repeatable-read snapshot
identifier from the owner transaction. Keep that transaction alive in a
Hub-owned lease and validate the identifier before a later lane may consume it.

## Resolution

Hub now exports a strictly validated PostgreSQL snapshot identifier only after
its read-only schema-checked owner transaction begins. Direct capture holds the
lease through completion and always ends it afterward, so a later parallel lane
cannot consume an untrusted identifier or silently continue after owner release.
