# Hub changes-since contract

[`hub-sync-v1@1.4.0`](../../profiles/hub-sync-v1/1.4.0/) adds a negotiated
PhysicalV3 changed-set representation for a schema 2.2 base. A client selects
the 1.4 bootstrap with `x-teslatlas-sync-profile: hub-sync-v1@1.4.0` and the
existing `x-teslatlas-supported-schemas: 2.1,2.2` header. Its changes-since
request pins the base manifest ID, exact signed manifest bytes SHA-256, receipt,
sequence and source binding, and advertises
`teslatlas-physical-v3-delta-v1`. A 1.3 client never receives the new
`physical_changed_set` response kind. The 1.3 signed no-op remains valid; a
1.4 signed 409 replacement supplies the exact 1.4 retry request.

The 1.4 selector admits only a signed schema 2.2 PhysicalV3 bootstrap; the
retained `2.1,2.2` header spelling does not permit a 2.1 base. Before a signed
no-op or changed set, the Hub checks the base manifest ID, SHA-256 of the exact
signed manifest bytes, receipt, sequence, and source binding. Only the immediate
predecessor can receive a `physical_changed_set`; an older retained base gets a
signed `409` complete schema 2.2 replacement.

The 1.4 changed-set pack has complete typed rows for each upsert across the
11-table, 168 mapped-source-field PhysicalV3 slice, plus explicit typed
tombstones and unchanged context rows needed for impacted drive and charge
roots. `addresses.raw` remains outside that slice. The signed affected-ID
witness defines the projection recomputation scope. It may include unchanged
dependents; context alone does not authorize a reader write. The signed target
raw digest commits the Hub's all-history state. A phone retaining a 30-day
window verifies and applies its bounded candidate separately, and uses the
signed full replacement when widening to 365 days or when a delta cannot be
admitted. The [field catalog](../../profiles/hub-sync-v1/1.4.0/physical-field-catalog.json),
[pack contract](../../profiles/hub-sync-v1/1.4.0/physical-delta-pack-v1-contract.json),
and [SQL layout](../../profiles/hub-sync-v1/1.4.0/physical-delta-pack-v1.sql)
are the wire authority.

Admission is bounded before pack fetch: at most 64 contiguous packs, 256 MiB
compressed and 2 GiB uncompressed in total, 2,000,000 unique typed rows and
tombstones (counting context once), and 10,000 impacted roots. Each pack is at
most 16 MiB compressed and 256 MiB uncompressed. A valid change that cannot
fit uses the signed `409` complete replacement; clients do not raise these
limits locally.

[`hub-sync-v1@1.3.0`](../../profiles/hub-sync-v1/1.3.0/) is the candidate,
source-neutral successor to retained `hub-sync-v1@1.0.0`, `1.1.0`, and `1.2.0` candidates. It
has no exact Hub product-version pin. A client selects its bootstrap
representation with both `x-teslatlas-sync-profile: hub-sync-v1@1.3.0` and
`x-teslatlas-supported-schemas: 2.1,2.2`. The schema header by itself remains
legacy `SyncManifest` negotiation and MUST NOT select the 1.3 representation.
This preserves existing clients that already send the schema list. Once a
profile selector is present, both headers must occur exactly once with those
exact values. A missing companion, different value, or duplicate selector or
schema header returns an empty `406` with `Cache-Control: no-store`; it never
falls back to the legacy representation. A client
applies the selected signed initial manifest, then persists its `receipt_id`, `sequence`, and `schema_version` as
the changes-since base. It sends those exact values plus its accepted manifest
schema range to `POST /v1/vehicles/{vehicle_id}/sync/changes-since` with a
paired bearer. The JSON body limit is 8192 bytes inclusive; 8193 bytes returns
`413 request_too_large`. A missing, invalid, expired, or revoked bearer returns
the unsigned empty `401` response.

A `200` response is either a signed changed-set receipt or a signed no-op
receipt. A changed set advances the sequence and contains exactly one pack
reference in this slice. A no-op preserves the base receipt and sequence. The
client verifies the Ed25519 signature over the canonical receipt before
trusting either result, then persists a new changed-set checkpoint only after
applying its pack. Every signed response `vehicle_id` and the selected signing
key set `vehicle_id` must equal the route `vehicle_id`; a valid signature for a
different vehicle is rejected.

The base and delta schemas are continuous. A changed-set or no-op receipt MUST
use the request's `base_manifest_schema`; in particular, a schema 2.2 base
cannot receive a schema 2.1 delta. A schema transition occurs only through a
signed rebase whose replacement schema falls inside the requested range.

Receipt resolution is scoped to the route vehicle. A current-lineage checkpoint
is current. Any base or delta checkpoint in an unexpired retained prior lineage
was compacted and returns a signed `409` `rebase_required` hint. A receipt absent
from both the current lineage and every unexpired retained prior lineage returns
unsigned `422 unknown_base_receipt`.

The rebase replacement is a complete admitted snapshot for its declared schema
and limits, never a delta, compacted delta, or partial lineage. A schema 2.1
replacement contains one pack. A schema 2.2 replacement contains one or more
ordered chunks, so a large compacted base does not need to fit one pack. The
client applies the complete replacement, persists its receipt, sequence, and
manifest schema, and sends the exact `retry_request` from the hint. It must not
infer another checkpoint or replace this with a full-history request. A Hub must
not admit a checkpoint that it cannot replace with a complete snapshot while
the checkpoint can remain valid.

The `1.3.0` fixtures carry deterministic, valid Ed25519 signatures over their
canonical payloads. Production clients retrieve the vehicle-bound key set from
authenticated `GET /v1/vehicles/{vehicle_id}/sync/signing-keys` over the paired
Hub channel. The returned `vehicle_id` MUST match the route. Every stable
`key_id` is `ed25519-sha256-` followed by lowercase SHA-256 hex of the raw
32-byte public key. The successful key response carries the
`Cache-Control: no-store` header. Key rotation publishes a key before first
use and retains retired keys while old signed objects remain valid.

[`fixture-signing-keys.json`](../../profiles/hub-sync-v1/1.3.0/fixture-signing-keys.json)
uses the same binding and identifier rules for the public test key. It is a
test trust anchor only. Clients reject a vehicle mismatch, unstable or unknown
key identifier, digest mismatch, or invalid signature.

The `1.1.0` successor adds signed no-op receipts, `406` empty no-store
unavailability, pack range semantics, status tables, and one signed schema 2.2
manifest containing ordered chunks. A compressed pack is 1 byte through 16
MiB inclusive. Writers should target at least 8 MiB for each non-final snapshot
chunk. A complete small history, final chunk, changed set, or prepared artefact
may be smaller, and a schema 2.2 manifest may therefore contain one chunk. The
outer manifest has the only signature.

Every JSON control response is limited to 2 MiB (2,097,152 encoded bytes)
before parsing. Pack byte responses use the separate 16 MiB compressed pack
limit. A schema 2.2 manifest or rebase may contain at most 1,771 chunks, and a
the retained `1.2.0` prepared artefact may contain at most 497 route spans in
addition to 120 map months. These maxima keep their largest compact JSON forms
inside 2 MiB.
Every integer-valued wire member is also bounded to the I-JSON exact-integer
range. This profile uses non-negative integers, so `sequence`, request and
rebase sequence fields, prepared-artefact timestamps and input sequence, pack
sizes, and chunk indexes are all at most `9007199254740991`, with narrower
limits where specified. A client rejects an out-of-range value before RFC 8785
canonicalization or signature verification.

Changes-since returns stable JSON error bodies for invalid JSON (`400`), an
unknown vehicle (`404`), an unsupported schema range (`406`), an oversized
body (`413`), and a schema-invalid body, reversed range, or range that excludes
the base schema (`422`). A well-formed receipt that is unknown for the route
vehicle also returns `422`. Schema-invalid bodies use `invalid_request`; the two
range validation failures use `invalid_schema_range`; the unknown receipt uses
`unknown_base_receipt`. A syntactically valid range with no overlap with schemas
2.1 through 2.2 returns `406 schema_range_unsupported` with
`Cache-Control: no-store`. Authentication
failure remains the unsigned empty `401`; compaction remains the signed `409`
rebase flow.

`1.2.0` retains its published `map_months_and_routes` receipt unchanged.

Candidate `1.3.0` replaces that prepared payload for newly negotiated 1.3
clients with the map-month-only `teslatlas-prepared-v1` contract. The signed
receipt binds the source manifest, receipt, manifest schema and sequence, input
window, generation identity, fixed units, algorithm version, map style, exact
changed months, media type, compressed and uncompressed sizes, and the SHA-256
of one `.sqlite.zst` pack.
Snapshot schemas 2.1 and 2.2 remain separate and do not contain prepared rows.

[`prepared-pack-v1-contract.json`](../../profiles/hub-sync-v1/1.3.0/prepared-pack-v1-contract.json)
is the machine-readable content contract and
[`prepared-pack-v1.sql`](../../profiles/hub-sync-v1/1.3.0/prepared-pack-v1.sql)
is its exact SQLite schema. Admission checks the signed receipt, compressed
size and digest, declared Zstandard content size, then opens SQLite read-only
with `trusted_schema` disabled and `query_only` enabled. It compares the exact
`sqlite_schema` fingerprint and `table_xinfo`, checks receipt-bound metadata and
month outcomes, and enforces the current App
`CanonicalBlockPublication::ReadyData` admission bounds.
Writers create a fresh canonical image with 4096-byte pages,
`secure_delete=ON`, a final `VACUUM`, no freelist pages, and an exact
`page_count * page_size` file length, then zero structurally unallocated bytes,
fragments, freeblock bodies, and unused overflow tails without changing live
records or structural pointers. Readers do not compare bytes emitted by
their local SQLite version. They validate the file format directly with a
bounded walk of every B-tree and overflow page, require every page to be
reachable exactly once, and require every unused or reserved byte to be zero.
They decode every physical SQLite record, require exact field arity, canonical
varints and serial encodings, no trailing header or body fields, and equality
with the complete logical rows.
SQLite writer-version header bytes may contain only the explicitly supported
3.53.1, 3.53.2, or 3.54.0 values. This rejects deleted content
retained in freed pages or live-page freeblocks even when the live schema and
rows match.
`readyData` months carry a non-zero tile count; `readyEmpty` months carry zero
tiles. Both retain their non-negative `drive_count`. A one-month all-empty
fixture is included. The deterministic data fixture uses fixed abstract signed
little-endian `i16` segment tuples and contains no source location rows.

The public candidate session identity is the explicitly versioned drawing style
`route-stroke-v8-opaque`, tile geometry version
`raster-v9-rounded-tile-px`, and artifact schema version `1`. They match the
current App constants `TripsRouteStrokeStyle.renderCacheVersion`,
`CURRENT_MAP_STYLE_VERSION`, and
`CanonicalMapBuildRequest.artifactSchemaVersion`; product adoption must retain
all three exact matches. Tile keys are zoom 2 through 13 and sort by `(zoom, x, y)`.
Each tile contains unique, ascending signed little-endian `i16` tuples
`(x1, y1, x2, y2)`, with at most 200,000 segments per tile. Each month's App
repository publication size is `8 + sum(40 + segment bytes)` and is at most
8 MiB.
Prepared route identifiers and route payloads are not admitted by
`teslatlas-prepared-v1`. They require a separate contract decision.
