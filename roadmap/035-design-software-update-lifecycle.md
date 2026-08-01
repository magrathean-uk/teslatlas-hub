---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design software-update lifecycle parity

Blocked by: [Design charging lifecycle parity](034-design-charge-lifecycle.md).

## Question

How are pending, downloading, installing, completed, failed, repeated, and
version-only software update observations represented?

## Starting recommendation

Use explicit update intervals and preserve source version evidence so restarts
cannot create duplicate completed updates.

## Resolution

Keep every software-update response as a raw fact. `available`, `downloading`,
and scheduled data are pending evidence; `installing` opens/extends one update
interval at the source timestamp. Returning to available cancels it. A later
non-installing observation completes it only with a valid new `car_version`.
Unknown status is retained without inventing a transition.

A version-only observation yields a zero-duration missed-update record only
when normalized version differs from the latest completed version, matching
TeslaMate behavior. Completion identity uses vehicle, installation start fact,
final version, and end fact. Durable uniqueness/cursor prevents restart
duplicates; missing version, regressed time, or contradictory state quarantines
the derivative while preserving the journal.
