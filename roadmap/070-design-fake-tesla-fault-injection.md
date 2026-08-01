---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design fake Tesla and fault injection

Blocked by: [Design resource-pressure controls](069-design-resource-pressure-controls.md).

## Question

What deterministic source simulator and fault controls can reproduce every API,
state-machine, timing, crash, corruption, and network scenario?

## Starting recommendation

Use a scriptable virtual clock and source server with replay fixtures, precise
failure points, and no dependency on a live car for routine proof.

## Resolution

Use a versioned virtual-clock local simulator with strict request auditing,
typed migration fixtures, bounded network faults, and named private Hub crash/
corruption points. Each scenario produces a redacted trace and passes only when
durability, projection, readiness, and no-wake invariants match its expected
result.
