---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design vehicle state intervals

Blocked by: [Design the collection state machine](030-design-collection-state-machine.md).

## Question

How are online, offline, asleep, suspended, driving, charging, updating, and
unknown intervals opened, extended, corrected, and closed?

## Starting recommendation

Represent non-overlapping intervals with explicit provenance and deterministic
closure after restart or missing transitions.

## Resolution

Model availability and activity as separate non-overlapping per-vehicle
dimensions. Availability is online, offline, asleep, suspended, or unknown;
activity is idle, driving, charging, or updating. This matches TeslaMate's
separate availability history and drive/charge entities without forcing false
overlap. Each interval records start/end observation IDs and times plus direct
or derived provenance and confidence.

An equal state extends the open interval. A later changed state transactionally
closes it at the new observation time and opens its successor. Missing,
malformed, or non-monotonic evidence creates no guessed gap and quarantines the
derived cursor. Late observations trigger deterministic ordered rebuild before
publication; restart resumes from the durable interval cursor idempotently.
