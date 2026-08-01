---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design charging-cost parity

Blocked by: [Freeze energy and efficiency semantics](041-freeze-energy-efficiency.md).

## Question

How are per-energy, per-minute, session, flat, geofence, currency, manual
override, and historical charging costs represented?

## Starting recommendation

Store input tariffs and calculated results separately so changes remain
auditable and old sessions can be reproduced.

## Resolution

TeslaMate costs import unchanged. Hub tariff, result, currency, and override
facts are separate versioned records. Per-kWh, per-minute, session fee, and
free-Supercharging rules match TeslaMate; absent data stays absent. Flat and
compound tariffs are explicit later formulas, while reprice makes audited new
results without rewriting history.
