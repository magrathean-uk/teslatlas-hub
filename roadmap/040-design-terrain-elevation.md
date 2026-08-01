---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design terrain and elevation parity

Blocked by: [Design geofence parity](039-design-geofences.md).

## Question

How are elevation samples sourced, cached, corrected, and used for ascent and
descent without delaying collection?

## Starting recommendation

Treat terrain enrichment as a replayable projection with source provenance and
bounded fallback when elevation data is unavailable.

## Resolution

Source elevation wins. Missing completed-drive samples may receive optional,
local SRTM-compatible derived values with dataset and tile provenance. Failed
lookups remain absent. Ordered deltas calculate ascent/descent, with TeslaMate
smallint-overflow behavior preserved; new terrain data creates a new derived
revision, never a silent history rewrite.
