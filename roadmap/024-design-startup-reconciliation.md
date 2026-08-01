---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design startup reconciliation

Blocked by: [Design backup and restore](023-design-backup-restore.md).

## Question

How does Hub recover interrupted collection, projection, migration, pack
publication, schema migration, and credential rotation before becoming ready?

## Starting recommendation

Use explicit durable operation states and make readiness depend on deterministic
reconciliation or safe quarantine.

## Resolution

Startup applies transactional schema work, checks catalogue/operation state,
then resumes only from durable journal cursor. Unpublished packs are cleanup;
manifest requires verified pack; open stages remain unpublished until explicit
resume/rebuild; sealed stages retain evidence. Readiness fails for corruption,
unsupported schema, listener credential failure, or lifecycle quarantine and
never clears quarantine or contacts Tesla. Future durable operation records
must cover capture, projection, publication, backup, and credential rotation,
ending in deterministic resume or machine-readable safe quarantine.
