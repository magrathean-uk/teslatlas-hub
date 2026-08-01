---
type: wayfinder:task
status: closed
parent: 000-map
---
# Freeze the no-wake and sleep policy

Blocked by: [Design rate-limit and transport recovery](028-design-rate-limit-recovery.md).

## Question

Exactly when may Hub query a sleeping, offline, suspended, charging, or
ambiguous vehicle without causing vampire drain?

## Starting recommendation

Never issue wake commands, preserve TeslaMate-equivalent sleep attempts, and
make every request transition auditable in deterministic state tests. Use the
locked freshness baseline: 5 seconds driving, 10 seconds charging, 75 seconds
ordinary online, and no freshness promise while asleep or offline.

## Resolution

Hub has no wake or command capability. Compatibility discovery may read the
product list, but `vehicle_data` follows only a same-collection exact `online`
state. Asleep, offline, suspended, unknown, and malformed states are recorded
without a data query; charging, driving, and updating still require `online`.
A vehicle-unavailable response returns to discovery and never escalates to
wake.

Driving, charging, and ordinary online use the locked five-, ten-, and
seventy-five-second ceilings. Asleep and offline report only last-observation
age. Every state guard and selected action becomes a durable non-secret
operation-journal event, replayable in deterministic tests.
