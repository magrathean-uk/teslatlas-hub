---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design drive lifecycle parity

Blocked by: [Design vehicle state intervals](031-design-vehicle-state-intervals.md).

## Question

Which signals open, extend, suspend, resume, split, merge, discard, and close a
drive, including restart and malformed-data cases?

## Starting recommendation

Match reference transitions with replay fixtures, then strengthen persistence
so every open drive is recoverable without fabricating movement.

## Resolution

Open a drive only on validated `D`, `R`, or `N` shift state or positive speed;
the first accepted location is its start. Valid later locations extend it. A
parked/non-moving observation closes only a drive with a recorded position;
later driving opens a new drive, never a merge across an unobserved gap.
Charging takes precedence for inconsistent simultaneous evidence.

Use TeslaMate's observed fifteen-minute unavailable-drive timeout. Restart
resumes the durable open session and cursor. Regressed time, invalid coordinates,
and malformed transitions preserve the journal and quarantine rather than seal
or discard a drive. Completed drives and positions publish together only after
deterministic closure.
