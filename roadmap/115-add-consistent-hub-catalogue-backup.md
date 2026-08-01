---
type: wayfinder:task
status: closed
parent: 000-map
---
# Add consistent Hub catalogue backup

Blocked by: [Prove native full PostgreSQL publication](114-prove-native-full-postgres-publication.md).

## Question

How can a live WAL-backed Hub catalogue be copied to a restore candidate
without relying on an unsafe file copy?

## Starting recommendation

Use SQLite's online backup API to create a new Hub-owned destination database,
then open it through the normal store integrity path.

## Resolution

Hub now uses SQLite's online backup API for a new, Hub-owned catalogue file.
It refuses the live database and existing destinations, removes a partial
destination on backup failure, and leaves immutable pack backup as its own
explicit later phase. A restored backup opens through normal migrations,
passes integrity, and preserves the installation identity.

Focused backup/restore proof passed.
