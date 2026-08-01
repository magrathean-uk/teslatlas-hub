---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design full-snapshot synchronization

Blocked by: [Freeze the complete Teslatlas data contract](046-freeze-teslatlas-data-contract.md).

## Question

How are complete vehicle snapshots built, signed, chunked, resumed, verified,
retained, and atomically made visible?

## Starting recommendation

Preserve immutable content-addressed packs and signed manifests, with bounded
construction and prior-generation survival at every failure point.

## Resolution

Full snapshots use one consistent source boundary, reserved non-reused sequence,
bounded verified parent-complete chunks, immutable SHA-256 paths, and an exact
signed manifest. Client range resume is ETag-bound; only all verified chunks
activate atomically. Failure leaves both prior Hub publication and phone mirror
alive, while incomplete source capture is discarded rather than mixed.
