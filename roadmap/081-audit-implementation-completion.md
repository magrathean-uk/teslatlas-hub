---
type: wayfinder:task
status: closed
parent: 000-map
---
# Audit implementation completion

Blocked by: [Define final parity signoff](080-define-final-signoff.md).

## Question

Does the current executable Hub and its current evidence prove the production
parity destination, and which smallest implementation behavior must follow?

## Starting recommendation

Compare the executable commands and runtime paths against every completion
matrix row. Keep the goal active unless direct, current evidence closes every
row. Start the first missing prerequisite before attempting any release claim.

## Resolution

The goal is not complete. Current unit/contract checks cover local components,
but there is no representative ten-million-row run, exported-snapshot parallel
binary COPY path, differential reference harness, fault/recovery suite,
backup/restore rehearsal, owner-authorized one-minute probe, native macOS and
Debian amd64/arm64 proof, or operator cutover rehearsal. The next narrow
implementation prerequisite is an exported read-only PostgreSQL snapshot lease
for bounded parallel capture lanes; it must retain the source transaction until
all lanes finish and fail closed on lease loss.
