# Differential conformance v1

The harness runs every versioned virtual-clock scenario from
`docs/FAKE_TESLA_FAULTS.md` through two adapters: Hub and the clean pinned
TeslaMate reference at `7054517c10475f39f480edeae8f90c6f717985a3`. The reference
runner uses an isolated disposable checkout and database/fixtures owned by the
harness; it never reads from or writes to the user TeslaMate deployment,
database, services, Docker state, configuration, schedules, or credentials.
The input fixture, virtual trace, reference build digest, Hub build digest, and
extractor versions are sealed in each result.

## Normalized comparison

Both adapters emit a canonical source-scoped fact graph plus request/recovery
trace. The extractor compares the selected vehicle identity, availability and
activity intervals, completed/open drive and charge lifecycle, ordered
positions/charge samples, updates, configuration/metadata, addresses/geofences,
derived range/energy/efficiency/cost/terrain results, anomalies, and durable
recovery state. It also compares required source-order and transition behavior,
read-only/no-wake request classes, retry/rate/circuit outcomes, and the
post-crash state reachable from each named fault point.

Normalization permits only implementation identities and representations that
cannot be Teslatlas-visible: Hub UUIDs versus TeslaMate local keys, pack/schema
layout, private paths, and receipt/build artifact IDs. Timestamps normalize to
UTC integer milliseconds at the contract boundary; values use the documented
unit and formula conversion before exact canonical comparison. Ordering is by
the declared semantic key, never insertion or database row order. There is no
numeric tolerance, timestamp window, dropped field, or inferred state unless a
versioned contract explicitly states its conversion or loss reason.

## Database, recovery, and excluded integrations

The database comparator extracts a normalized selected-car relational graph,
not raw schema equivalence. It verifies parent/child links, counts, keyed
hashes, null/enum behavior, time ranges, aggregates, open/closed classification,
and every Teslatlas-visible field. Hub's immutable journal, projection, pack,
and manifest must agree with the normalized reference outcome and its own
durability invariants.

For every scenario and injected crash/restart point, the harness starts from
fresh private stores, advances the same virtual trace, restarts the failed
adapter, and compares readiness, retained acknowledgements, replay result, and
duplicate projected-fact count. A reference-only operational output is compared
only where it is an accepted compatibility surface. MQTT, Home Assistant,
Phoenix, Grafana, and web UI are excluded by the declared contract: the Hub
negative assertion is that no broker connection, topic, listener, credential,
or MQTT output exists.

## Results and failure policy

Each run emits a redacted canonical result containing scenario/seed, both
revisions, normalized expected/actual graph hashes, request traces, fault point,
timings, evidence artifacts, and a difference list. Differences are only
`exact`, a named mapping-loss/contract conversion, or an approved explicit
deviation with scope and replacement proof. Every other difference is fatal.
Reference updates, adapter revisions, fixture changes, or normalizer changes
invalidate the prior conformance series and require a complete rerun.
