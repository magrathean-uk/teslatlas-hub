---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design destination reconciliation

Blocked by: [Design comprehensive source validation](058-design-source-validation.md).

## Question

How will Hub prove row, relationship, aggregate, lifecycle, time-range, spatial,
and sampled value equivalence after migration?

## Starting recommendation

Combine exact counts, keyed checksums, invariant queries, aggregate comparisons,
and deterministic deep samples with zero unexplained differences. Compute source
summaries during the same read-only snapshot and destination summaries during
bulk finalization so reconciliation does not require another full serial pass.

## Resolution

Streaming source and destination summaries prove exact retained rows,
relationships, lifecycle, aggregates, ranges, spatial data, deterministic deep
samples, packs, and manifest binding. Any unexplained difference is fatal and
cannot publish.
