---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design vehicle metadata and telemetry completeness

Blocked by: [Design software-update lifecycle parity](035-design-software-update-lifecycle.md).

## Question

Which vehicle configuration, temperatures, ranges, tire pressures, climate,
doors, charging, and firmware fields belong to durable history?

## Starting recommendation

Inventory every TeslaMate-persisted field, retain Teslatlas-useful additions,
and version optional fields to survive upstream schema drift.

## Resolution

Durable observation maps keep safe source metadata and telemetry, including
unknown optional fields. Current typed history is limited to mirror-required
identity, firmware, position, climate, drive, range, and charge fields;
optional door, tyre, and future fields stay versioned journal data until a
consumer needs a typed extension. Missing remains distinct from false or zero.
