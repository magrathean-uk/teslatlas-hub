---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design charging lifecycle parity

Blocked by: [Design position sampling](033-design-position-sampling.md).

## Question

Which signals open, extend, pause, resume, split, merge, calculate, and close a
charging process and its charge samples?

## Starting recommendation

Replay TeslaMate charging scenarios exactly, then add stronger crash recovery
and invariant checks around energy, phase, duration, and state-of-charge.

## Resolution

Open a charging process on TeslaMate-compatible `Starting` or `Charging`; retain
the first and each ordered charging sample. `Complete`, `Disconnected`,
`Stopped`, and `NoPower` terminate it; sleep, offline, updating, or later
non-charging evidence closes with the last observed values. A subsequent
starting/charging state after closure opens a new process, never a merge across
an unobserved gap.

Derive energy, SoC, range, duration, maximum power, phase, and DC evidence from
ordered samples only. Regressed time, decreasing cumulative energy, or impossible
electrical values quarantine the projection while retaining source facts. Open
state/samples commit atomically; restart resumes by durable cursor and publishes
only completed processes with all attached samples.
