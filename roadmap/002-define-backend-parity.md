---
type: wayfinder:task
status: closed
parent: 000-map
---
# Define backend parity

Blocked by: [Pin the TeslaMate parity reference](001-pin-teslamate-reference.md).

## Question

Does parity mean equivalent collected facts and lifecycle outcomes, exact
TeslaMate database compatibility, compatible operational outputs, or all three?

## Starting recommendation

Require behavioral and data parity, preserve explicit compatibility surfaces,
and allow a different internal schema when conformance evidence proves no
Teslatlas-visible loss.

## Resolution

Backend Parity means behavioral and durable-data equivalence with the pinned
TeslaMate reference. Hub must reproduce each declared collection and lifecycle
outcome, and preserve the facts needed for Teslatlas-visible history without
unexplained difference. Exact TeslaMate database-schema compatibility is not a
parity requirement: Hub may use a different internal schema when conformance
evidence proves no Teslatlas-visible loss. Operational outputs and every
compatibility surface remain explicit, separately decided contracts; they are
not implied by schema similarity or by a source-code mirror.
