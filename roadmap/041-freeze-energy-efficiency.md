---
type: wayfinder:task
status: closed
parent: 000-map
---
# Freeze energy and efficiency semantics

Blocked by: [Design terrain and elevation parity](040-design-terrain-elevation.md).

## Question

Which gross, net, rated, ideal, estimated, usable, and battery-derived energy
and efficiency values must Hub calculate and expose?

## Starting recommendation

Match TeslaMate outputs formula-by-formula, preserve raw inputs, and version any
Teslatlas-specific improvements as explicit additional metrics.

## Resolution

Raw battery, range, cumulative added-energy, and electrical values stay
separate. TeslaMate-compatible charge energy and efficiency formulas are fixed,
including interval integration, phase correction, and confidence tiers. Source
aggregates win; fallback and Teslatlas-only metrics are explicit versioned
derivations and never replace source facts.
