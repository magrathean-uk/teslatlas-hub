---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design background refresh

Blocked by: [Design resumable atomic synchronization](050-design-resumable-atomic-sync.md).

## Question

How do refresh scheduling, freshness limits, network and power constraints,
backoff, cache eviction, and foreground recovery behave?

## Starting recommendation

Use cursor-aware bounded refresh with explicit freshness state and make every
background failure recoverable by the next foreground run.

## Resolution

Background work is best-effort cache maintenance with bounded retry and no
freshness claim. It uses the same atomic mirror replacement as foreground;
failure, expiry, network loss, or low storage leaves the old mirror active and
the next foreground run recovers from the signed manifest.
