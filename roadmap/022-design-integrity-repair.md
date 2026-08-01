---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design integrity checking and repair

Blocked by: [Design capacity preflight](021-design-capacity-preflight.md).

## Question

Which fast, scheduled, deep, and offline checks detect corruption or semantic
drift, and which repairs are safe without source recontact?

## Starting recommendation

Separate detection, quarantine, reconstruction, and destructive repair; require
a machine-readable report and backup before any irreversible action.

## Resolution

Readiness and doctor run fast catalogue checks; sealed stages/packs run full
integrity/accounting checks before publication. Semantic failures quarantine
state while preserving immutable facts. Safe repair only reports/detects,
preserves quarantine, and removes verified unreferenced packs. Journal rebuild
does not contact Tesla. Catalogue, referenced-pack, or sealed-stage corruption
forces not-ready and offline backup restore/rebuild; no automatic destructive
repair. Reports are machine-readable and backup precedes irreversible work.
