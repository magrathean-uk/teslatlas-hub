---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design side-by-side startup

Blocked by: [Design credential handoff](060-design-credential-handoff.md).

## Question

How does the migration install and start Hub while TeslaMate remains available,
without port, database, credential, or collection ownership conflicts?

## Starting recommendation

Start only Hub, on isolated storage and endpoints. Import first and delay Hub
collection authority until a controlled verification step. TeslaMate services,
containers, databases, configuration, and schedules remain untouched.

## Resolution

Hub starts only on isolated Hub-owned state and endpoint. Import is manual and
read-only; every imported vehicle is migration-only, so Hub collection cannot
start until a later owner-controlled step. TeslaMate stays unchanged and live.
