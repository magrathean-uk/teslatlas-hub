---
type: wayfinder:task
status: closed
parent: 000-map
---
# Clean unpublished pack failures

Blocked by: [Enforce native ten-minute migration timeout](111-enforce-native-ten-minute-migration-timeout.md).

## Question

When direct capture fails after writing verified local fragments but before
publication, can stale candidate packs remain reachable on Hub storage?

## Starting recommendation

Make the candidate pack sink own cleanup until its chunks are explicitly
transferred to the publication path, and prove drop cleanup locally.

## Resolution

The candidate sink now removes Hub-owned pack files on drop unless its chunks
are explicitly transferred into a successful result. Both staged and direct
pack producers perform that transfer only after their full production path
succeeds, so an error during fragment accumulation, projection, or direct
count reconciliation leaves no candidate object behind.

Focused local cleanup proof passed.
