---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design schema evolution

Blocked by: [Freeze derived calculations](015-freeze-derived-calculations.md).

## Question

How will Hub apply, resume, verify, and roll back database schema changes across
long-lived installations?

## Starting recommendation

Use monotonic transactional migrations, preflight every destructive rewrite,
and retain a tested restore path for irreversible upgrades.

## Resolution

Hub uses ordered forward-only SQLite migrations, each in one immediate
transaction. Failure rolls back the step and readiness stays false. Destructive
or large rewrites require integrity/free-space preflight, verified backup,
resumable copy, and operator restore; rollback is restoration with the matching
binary, never a down migration. Catalogue, pack schema, and wire protocol have
separate versions. Unknown versions fail closed; compatible changes need safe
minor evolution, while incompatible changes require major version, client
capability, and an explicit transition path.
