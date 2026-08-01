---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design TeslaMate version compatibility

Blocked by: [Design the consistent database copy](055-design-consistent-database-copy.md).

## Question

Which TeslaMate schema versions, custom migrations, extensions, partial
upgrades, and unknown columns can the migration accept?

## Starting recommendation

Match known migration high-water marks and structural probes, reject ambiguous
schemas, and keep adapters versioned rather than guessing from column presence.

## Resolution

Compatibility requires a reviewed migration-set digest, high-water interval,
and fixed structural/type probe. Custom, partial, newer, or ambiguous schemas
fail closed; future TeslaMate support ships as a separate tested adapter.
