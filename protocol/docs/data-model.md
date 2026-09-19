# Rich-profile canonical data model

This document applies to the rich semantic profiles. The candidate current-Hub
HTTP profile has its own bounded resource schemas in
`profiles/hub-http-v1/1.0.0/`; it does not expose this observation, command, or
metadata model.

The public model separates provider facts, deterministic projections, visible
quality, mutable user metadata, and command jobs. A client can consume each
layer without knowing Hub storage tables or collector source formats.

## Boundary

| Domain | Mutability | Authority |
| --- | --- | --- |
| Provider observations | Immutable after admission | Canonical observation schema |
| Derived projections | Rebuilt by projector version | Projection and resource schemas |
| Data quality | Rebuilt with the projection | Data-quality schema |
| User metadata | Versioned and audited | Metadata schema and ETags |
| Vehicle commands | Asynchronous audited jobs | Command schema and command catalogue |

## Observations

`schemas/observation.schema.json` defines the source-neutral admitted fact.
Every observation carries identity, vehicle, source, provider and receive time,
source sequence, field, typed value, validity, quality flags, source-payload
hash, collector version, and projector version.

`raw_payload_hash` is lowercase SHA-256 of the exact admitted provider payload
bytes after transport decompression and before semantic transformation. The
payload itself is not required in this public contract.

An observation ID is immutable:

- Re-admitting the same ID with the same canonical content is an idempotent
  duplicate. It is not projected twice and is visible as a duplicate.
- Re-admitting the same ID with different content is a conflicting duplicate.
  It is rejected from the canonical input set and surfaced as
  `conflicting_duplicate`. The conflict is never silently overwritten.

Provider timestamps describe when a fact applied. Receive timestamps describe
arrival and are used for delay and gap quality. Arrival order is not semantic
order.

## Canonical ordering

Projectors order admitted observations by this tuple:

1. `provider_timestamp` ascending;
2. `source` by lowercase ASCII bytes;
3. `source_sequence`, with integers before strings before `null`; integers sort
   numerically and strings sort by UTF-8 bytes;
4. `observation_id` by lowercase ASCII bytes.

Receive order never appears in that key. Duplicate instances are removed before
ordering. The reordered fixture proves that input permutation does not change
the canonical input list.

If two sources disagree, the projector does not erase either observation.
Projector-version rules select or derive the public field and quality reports
the conflict. Source names do not imply a secret global priority.

## Typed values and units

Typed values use an explicit `kind`, JSON `value`, and unit where applicable.
Numbers are finite. v1 uses these public units:

- kilometres and kilometres per hour;
- kilowatt-hours and kilowatts;
- degrees Celsius;
- volts and amps;
- percentage points;
- compass degrees in `[0, 360)`.

Clients do not infer units from locale. Display conversion is a client concern.

## Projections

`schemas/projection.schema.json` wraps a resource payload with the ordered input
IDs, projector version, computed timestamp, and quality assessment.

A projection is reproducible when the same:

- admitted observation set;
- canonical ordering rules;
- projector semantic version; and
- explicit metadata inputs

produce the same payload and quality object.

`computed_at` is the maximum `received_timestamp` of the admitted input set. It
is not wall-clock execution time. `observed_at` is derived from provider time.
Projectors do not read local timezone, locale, random values, or current time.

Projection arrays are sorted by their schema-defined identifiers and times.
Object member order has no semantic meaning. When a representation hash is
needed, implementations use RFC 8785 JSON Canonicalization Scheme before
hashing. Projection IDs and HTTP ETags remain opaque to clients.

No projector silently interpolates a missing value. A derived field is named in
`derived_fields`. Missing values remain JSON `null` when the schema permits it.

## Data quality

`schemas/data-quality.schema.json` makes completeness consumable:

- `complete`: no known gaps or unresolved issues in the assessed scope;
- `partial`: usable result with a known delay, ordering issue, or gap;
- `degraded`: unresolved conflicts or loss materially reduce confidence.

`gap_count`, `largest_gap_seconds`, sources, derived fields, and issues are
always present. `complete` requires zero gaps and no issues. Gap thresholds are
part of the projector version; clients display them and do not recalculate
quality from arrival timing.

## Query resources

`schemas/resources.schema.json` defines vehicles, current state, drives,
positions, charging sessions, charge samples, state intervals, software
updates, and their cursor pages. IDs are opaque and stable within one Hub.

Positions are valid live data, but repository fixtures contain no precise real
locations. Redaction and live-data validity are separate concerns.

Deleted metadata becomes a persistent tombstone. Tombstones remain available
by ID and through change events so the deletion audit is consumable, but they
are excluded from live metadata lists. Their immutable live audit history and
single terminal deletion event are separate fields, so deletion identity is
not duplicated on the wire.

## Deterministic fixtures

`fixtures/v1/` covers normal, duplicate, delayed, missing, and reordered input.
Each fixture includes admitted IDs, duplicate IDs, canonical order, expected
projection, exact quality, and explicit redaction metadata.

Fixture JSON is emitted with sorted keys and fixed formatting. SHA-256 values in
`fixtures/manifest.json` protect exact bytes. The builder and tests reject real
identifiers; listed account, credential, key, secret, token, VIN, and raw
provider-payload fields; email addresses; VIN-shaped values; real home/work
labels; bearer credentials; and coordinates with more than two decimal places.
Each prohibited-data class has a negative scanner case; adding a prohibited
value makes the local gate fail.
