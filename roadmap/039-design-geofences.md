---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design geofence parity

Blocked by: [Design address and geocoding parity](038-design-address-geocoding.md).

## Question

How are overlapping geofences, priorities, sleep rules, charging costs, edits,
and historical reassignment represented?

## Starting recommendation

Preserve deterministic spatial matching and version geofence-derived facts so
operator edits cannot silently rewrite history.

## Resolution

Hub imports TeslaMate fence attachments unchanged. Optional local circular
fences choose the nearest containing centre, then stable ID. Fence edits append
versions; completed history stays bound to its match input unless an explicit,
audited derived rebuild publishes a later revision. Fence tariffs wait for cost
parity and cannot alter historical charges.
