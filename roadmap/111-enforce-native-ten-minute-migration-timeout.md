---
type: wayfinder:task
status: closed
parent: 000-map
---
# Enforce native ten-minute migration timeout

Blocked by: [Add set-based direct count gate](110-add-set-based-direct-count-gate.md).

## Question

Can the representative native proof stop and fail at the target instead of
only reporting a duration after a slow migration eventually returns?

## Starting recommendation

Wrap the opt-in proof operation in the ten-minute timeout and retain the
postcondition duration assertion as a second guard.

## Resolution

The opt-in representative native test now actively times out direct capture,
projection, pack production, and count reconciliation at ten minutes. It
continues to check the elapsed postcondition and exact expected result.
The prior native ten-million proof completed within this enforced bound; the
ordinary test run confirms the opt-in gate compiles while remaining inert
without its explicit environment switch.
