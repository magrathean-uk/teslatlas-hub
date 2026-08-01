---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design resumable atomic synchronization

Blocked by: [Design pairing and device authorization](049-design-pairing-device-auth.md).

## Question

What interruption, retry, corruption, cancellation, low-disk, duplicate,
source-swap, and stale-cursor behavior preserves the prior Teslatlas mirror?

## Starting recommendation

Download into bounded private staging, verify every receipt and binding, then
activate only one complete signed generation.

## Resolution

The old mirror stays live through interruption, retry, corruption,
cancellation, low disk, duplicate work, source swaps, and stale cursors. Only
a complete, verified signed generation atomically replaces it; all other paths
clean their private stage and retain no partial visible state.
