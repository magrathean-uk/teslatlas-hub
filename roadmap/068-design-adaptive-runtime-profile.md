---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design the adaptive runtime profile

Blocked by: [Build the performance baseline](067-build-performance-baseline.md).

## Question

Which measured host facts may safely tune worker counts, buffers, cache,
checkpoint cadence, validation depth, pack size, and migration concurrency?

## Starting recommendation

Select from conservative bounded profiles at startup, persist the chosen
profile and reasons, and never adapt away durability or validation guarantees.

## Resolution

Select a versioned, baseline-proven bounded profile at run start from exact
host, storage, network, corpus, and revision facts. It may tune resource bounds
and parallelism only; validation and all safety properties stay fixed. Unknown
or changing conditions use the safe profile, and active work only slows or
fails under pressure.
