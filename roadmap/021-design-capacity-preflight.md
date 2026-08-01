---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design capacity preflight

Blocked by: [Set retention and compaction policy](020-set-retention-compaction-policy.md).

## Question

How will Hub estimate database growth, migration workspace, WAL, backups,
packs, safety margin, inode demand, and filesystem limits before writing?

## Starting recommendation

Measure the source, calculate worst-case temporary and durable demand, reserve
a fixed recovery margin, and fail closed before any migration mutation.

## Resolution

Preflight records source counts/bytes, Hub durable footprint, free bytes,
filesystem type, inodes, and reserve. It overflow-checks the sum of current
data, unpublished stage, pack workspace, WAL/checkpoint growth, backup window,
and reserve, then fails closed before Hub writes. Stage creation already
reserves explicit cap plus free-space margin and enforces each page bound.
Defaults are safety caps only; corpus measurement must establish the production
ten-million-row budgets and inode/WAL/backup demand.
