---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design the wake and live-data probe

Blocked by: [Design side-by-side startup](061-design-side-by-side-start.md).

## Question

How does the script ask the user to wake the car, establish a baseline, wait
one minute, and prove Hub collected and committed genuinely new data?

## Starting recommendation

Pause with a clear prompt, never issue a wake command, capture before/after
cursors and database facts, then require a timestamped durable delta after one
minute. The probe may start or stop Hub-owned workers only; it may not change
TeslaMate or Docker state.

## Resolution

The manual probe records a baseline, waits for the owner to wake the vehicle,
then after one minute runs one Hub-owned no-wake collection. It passes only on
a new durable selected-vehicle fact and verified after-state; all failures
keep both Hub baseline and TeslaMate unchanged.
