---
type: wayfinder:task
status: closed
parent: 000-map
---
# Define Tesla credential modes

Blocked by: [Design startup reconciliation](024-design-startup-reconciliation.md).

## Question

Which legacy owner-token and Fleet credential modes are supported, how are they
selected, and can both coexist during migration?

## Starting recommendation

Support an explicit legacy compatibility mode and a first-class Fleet mode,
with one active collection authority per vehicle.

## Resolution

Hub selects data-only, explicit owner-token compatibility, TeslaMate read-only
migration, or future Fleet by source configuration. Owner-token and Fleet never
fall back to one another. They may coexist for different vehicles during
migration, but exactly one collection authority may be active for each source
vehicle; handoff stops the old authority before starting the new one.

The owner token remains a systemd credential used only for explicit no-wake
compatibility reads. The TeslaMate PostgreSQL password remains a separate,
manual read-only migration credential. Fleet is deliberately unimplemented and
will use a separately authorized application credential set. Cursor keys and
paired-device bearers are Hub credentials, not Tesla credential modes.
