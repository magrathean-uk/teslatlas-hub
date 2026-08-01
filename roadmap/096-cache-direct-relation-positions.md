---
type: wayfinder:task
status: closed
parent: 000-map
---
# Cache direct relation positions

Blocked by: [Stream history positions through binary COPY](095-stream-history-positions-through-binary-copy.md).

## Question

How can direct projection avoid repeated source lookups for one referenced
position without retaining unrelated position history?

## Starting recommendation

Keep one bounded per-import cache keyed by already-referenced source position
ID. Cache only successfully decoded selected-car rows; missing positions still
fail closed.

## Resolution

Direct drive and charge projection now share one per-import related-position
cache. Only successfully decoded selected-car rows enter it; absent or
inconsistent source rows still fail closed.
