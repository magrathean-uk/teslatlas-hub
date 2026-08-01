---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design address and geocoding parity

Blocked by: [Design event anomaly handling](037-design-event-anomaly-handling.md).

## Question

Which geocoder, cache, privacy boundary, retry policy, deduplication rule, and
address attachment semantics replace TeslaMate behavior?

## Starting recommendation

Make geocoding optional and asynchronous, cache by bounded spatial identity,
and never block durable drive or charge completion on an external service.

## Resolution

Imported TeslaMate address labels remain source history. Hub collection keeps
geocoding off by default; an explicit provider runs asynchronous enrichment,
coalesces bounded spatial keys, keeps provider/language/feature provenance,
and attaches only versioned optional results. Errors and rate limits never
change coordinates or lifecycle facts.
