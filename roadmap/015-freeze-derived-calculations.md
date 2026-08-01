---
type: wayfinder:task
status: closed
parent: 000-map
---
# Freeze derived calculations

Blocked by: [Freeze the core data model](014-freeze-core-data-model.md).

## Question

Which distances, energy values, efficiencies, elevations, ranges, durations,
costs, and start/end facts must match TeslaMate formulas exactly?

## Starting recommendation

Treat each formula and rounding rule as a versioned compatibility contract with
golden source rows and expected results.

## Resolution

TeslaMate migration preserves completed source aggregates as authoritative.
Only missing charge aggregates may fall back to ordered cumulative sample delta;
negative or non-finite values stay absent. Current owner-token lifecycle math
is explicitly provisional and not a TeslaMate formula claim. Exact drive,
charging, terrain, efficiency, and cost formulas require their later dedicated
versioned golden-fixture tickets; no display rounding is permitted in the
canonical path.
