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

[`hub-sync-v1@1.5.0`](../../profiles/hub-sync-v1/1.5.0/) is a candidate that
adds a live overlay to a PhysicalV3 head. No Hub serves it yet. A Hub lists the
discovery capability `sync.live` if and only if it serves the overlay and the
discovery profile it names defines that token; a Hub that does not serve the
overlay answers the 1.5 selector with an empty `406`. `hub-http-v1@1.2.0` and
`1.1.0` do not define `sync.live`, so a Hub that serves 1.5 under either of them
does not advertise it; a later `hub-http-v1` minor adds the token, and the exact
capability equality of the 1.1.0 actual-Hub matrix is adjusted then. A client
selects it with `x-teslatlas-sync-profile: hub-sync-v1@1.5.0` and the same
`x-teslatlas-supported-schemas: 2.1,2.2` header, and every changes-since request
adds the required `base_live_sequence` (0 through 9007199254740991). A request
without it keeps the 1.4 behaviour and never receives an overlay; the closed 1.4
and 1.5 request schemas make the two shapes mutually exclusive.

The overlay is a second position, `live_sequence`, against one exact head. A
checkpoint is `(receipt_id, manifest_sha256, live_sequence)`. When the head
matches and the client is behind, the Hub returns the next live changed set: a
`physical_changed_set` whose `base` and `target` are both that head, byte-equal,
with `from_live_sequence`, `to_live_sequence` and `live_sequence` added, where
`to_live_sequence` is `from_live_sequence` plus 1, `live_sequence` equals
`to_live_sequence`, and `from_live_sequence` equals the request's
`base_live_sequence`. A client rejects a gap or a receipt naming another head.
When the client is current the Hub returns a signed
live `no_op`, which names the exact `receipt_id`, `manifest_sha256`, `sequence`
and `live_sequence` it proves current; the 1.3 no-op is never a live answer. A
live changed set never deletes: its pack uses the unchanged 1.4 pack contract
with both metadata sides naming the head, an empty tombstone table and only
closed impacted roots, because only closed drives and charges are published
live and a root published while open could later need a delete. A synthesised
stationary position for a live charge is an ordinary `changed` row.

A `409` `rebase_required` is returned only when the base head is no longer the
one to serve, whatever its `base_live_sequence`: reason `head_changed` after a
later import, `live_retention` after a fold that pruned the live journal of the
requested head (a client offline beyond the live-journal retention), and
`live_ahead` when the head was rotated away because of a regression and the
client's `base_live_sequence` is above the position the Hub recorded for that
head but not above its high-water mark (below). The reason is diagnostic; a
client takes the same action for every reason. None of them is an error, even
for a head whose packs have expired, because a Hub keeps the identity of every
head it has published for the vehicle (`manifest_id`, `receipt_id`,
`manifest_sha256`, `sequence` and source binding), the position it holds for the
head and the highest `live_sequence` it ever signed for it (the high-water mark),
for the life of the vehicle. The immediate predecessor is included: the 1.4
single-step head transition is not offered to a 1.5 client, so a base that is not
the head to serve always receives a signed complete replacement, a head that
already exists, so answering costs no rebuild. The replacement is a head with a
different `receipt_id` and a greater sequence, so a rebase cannot loop on an
unchanged head, and the exact `retry_request` carries `base_live_sequence` 0. A
Hub therefore keeps every live changed set of its current head from
`live_sequence` 1, or folds the live rows into a new head before pruning them.
A watermark over paired devices is not enough: a new, reinstalled or rebased
device and a 1.4 upgrade start at `live_sequence` 0 and need step 1, which no
paired device's acknowledgement protects. A live publication that cannot fit the 1.4 admission limits or closure in one
step is split into consecutive live steps, each admitted by the 1.4 limits and
closed over its own roots (a root is never split across steps). Only a single
root whose closure alone exceeds the 1.4 limits is answered with a fold into a
new head and `head_changed`; a live step is never answered with a replacement of
the same head.
Only a receipt the Hub did not record for the vehicle, a known receipt with a
different manifest ID, manifest digest or sequence (such as the digest of a 1.4
manifest), or, for the head to serve, a `base_live_sequence` above the head's
high-water mark fails closed as the unsigned `422 unknown_base_receipt` of 1.3
and 1.4. A client answered that way drops its checkpoint, does a full bootstrap
under the 1.5 selector and then requests with `base_live_sequence` 0; it never
retries the same request. The Hub resolves a base in this order, first match
wins: identity, then whether the head is still the one to serve, then a paused
rotation, then the high-water mark, then a regression, then no-op or the next
changed set. Error statuses are inherited from 1.3.0 and are unsigned; the 1.5
OpenAPI lists only `200` and `409` for changes-since and `200` and `406` for the
manifest, as 1.4 does. The wire key document is the 1.3.0 signing-keys route
and schema; the 1.4.0 key schema the profile refers to covers only the fixture
key set.

A client applies a live changed set only when its stored `(receipt_id,
manifest_sha256, live_sequence)` equals the receipt's base and
`from_live_sequence`, and then atomically activates `(receipt_id,
manifest_sha256, to_live_sequence)`. The target of a live receipt is the head the
client already has active, so the 1.4 rule that replay of an already activated
target is idempotent does not apply: a live receipt is a replay only when the
client's stored `live_sequence` already equals `to_live_sequence`. The
`target_raw_sha256` of a live changed set is the head's own all-history target
commitment, the value that the receipt which published the head carries, and it
is the same in every live step of the head; the step itself is bound by the
signed receipt, not by this member. As in 1.4 a client is not required to
reproduce it, and a client that holds the head's commitment compares it.

A live position is a bare counter under an unchanged head, so the profile fixes
its meaning. A `(receipt_id, manifest_sha256, live_sequence)` is signed for at
most one changed set: a Hub that answers the same step twice returns identical
content, which makes crash replay idempotent, and never re-issues a
`live_sequence` of a head with different content. An operation that can lower a
head's live position (a backup restore, a lost journal tail) rotates the head
before live service resumes, and a restore also rolls back the head ledger, so a
head the restored ledger does not record is answered with the unsigned `422` and
the client recovers by a full 1.5 bootstrap. A Hub that finds `base_live_sequence`
above the position it holds for the head to serve but not above the head's
high-water mark has detected a regression it did not rotate on: it never answers
a changed set or a no-op from that head. It never builds the replacement inside
the request, because a fold is O(history) and a client chooses
`base_live_sequence`; it answers with a transient failure that this profile does
not define, starts the rotation outside the request and pauses live service for
the head until the rotation completes, and a client keeps its checkpoint and
retries the same request later. A `409` `live_ahead` therefore names a
replacement that already exists: once the head has been rotated away, a request
that names it is answered with it. A `base_live_sequence` above the high-water
mark of the head to serve is not recorded by this Hub as issued and is answered
at once, without rebuilding anything, with the unsigned `422`, so a client cannot
make a Hub rotate by naming a position it holds no record of issuing. A client
that holds two receipts for one step with different content rejects the second.

An import folds live rows into the new stage before publication. The 1.4 head
transition from head N to head N+1 stays as in 1.4 and is computed from head N's
packs at `live_sequence` 0, because a 1.4 client never holds live rows: it
carries every folded live row of N+1 as a changed row and never tombstones a live
row. A live duplicate that an import replaces is absent from N+1 and from that
transition and reaches no client as a tombstone, because a 1.5 client above
`live_sequence` 0 never receives a head transition and always receives a complete
replacement.

After a bootstrap the client likewise requests the overlay at once from the
manifest's position: the 1.5 bootstrap manifest adds a signed `live_sequence`,
which is 0 for the life of its head because head packs are never rewritten. The
manifest therefore does not change as live changed sets are published, but it
differs from the 1.4 manifest of the same head, so the exact-bytes
`manifest_sha256` of a 1.4 checkpoint is never a 1.5 base. A client upgrades
without a rebase: it fetches the 1.5 bootstrap manifest of the head the Hub
serves (one signed document, no packs), verifies it, and adopts it as its 1.5
checkpoint at `live_sequence` 0 when its vehicle, `manifest_id`, `receipt_id`,
sequence and schema version equal those of the 1.4 checkpoint. Both kinds of 1.4
checkpoint qualify. A client that holds the signed 1.4 bootstrap manifest of the
head also compares the chunk list. A client that reached the head through a 1.4
changed set holds only the receipt's `target`, which carries no chunk list, so it
compares the identity members alone: the signed 1.5 manifest binds that identity
and a head identity names the head's content. The client stores the SHA-256 of
the 1.5 manifest bytes, keeps its materialised state, and asks for the overlay
with `base_live_sequence` 0. If any compared member differs it does a full
bootstrap under the 1.5 selector, and if the Hub answers the selector with `406`
it keeps its 1.4 checkpoint and sends 1.4 requests. It keeps the 1.4
`manifest_sha256` beside its 1.5 checkpoint until the head changes: if the Hub
later stops serving 1.5, a client at `live_sequence` 0 holds exactly the head's
packs and resumes 1.4 requests from that checkpoint, while a client that applied
any live changed set holds rows no 1.4 head contains and does a full 1.4
bootstrap. A client that reached its head under 1.5 has no kept 1.4 checkpoint:
at `live_sequence` 0 it fetches the 1.4 manifest of the same head and adopts it
when its identity members and chunk list equal those of its 1.5 manifest, and
otherwise does a full 1.4 bootstrap. A 1.5 request that carries a 1.4 manifest
digest is answered, like any known receipt with a different digest, with the
unsigned `422 unknown_base_receipt`.

The profile refers to the unchanged 1.4 field catalog, value semantics, pack
contract and SQL, fixture signing keys and signing-keys schema by relative path
and pins each by digest, and chains on the digest of the 1.4 `SHA256SUMS`. Its
limits are the 1.4 limits. It commits no live pack: the advance example points at
the committed 1.4 positive pack digest as a placeholder, and pack-level live rules
are exercised by conformance that derives a live pack at run time.

[`hub-sync-v1@1.3.0`](../../profiles/hub-sync-v1/1.3.0/) is the candidate,
source-neutral successor to the `hub-sync-v1@1.0.0`, `1.1.0`, and `1.2.0`
candidates, which were retired and are no longer in this package. It
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
limit. A schema 2.2 manifest or rebase may contain at most 1,771 chunks, and the
retired `1.2.0` prepared artefact could contain at most 497 route spans in
addition to 120 map months. These maxima keep their largest compact JSON forms
inside 2 MiB.

Candidate `1.4.0` applies these limits jointly. Its individual schema maxima
MUST NOT be treated as independently combinable. Before publishing or admitting
a PhysicalV3 checkpoint/rebase journey, the producer and receiver MUST ensure
the original request and mandatory exact retry each fit 8192 encoded bytes, and
the complete replacement manifest, rebase and other control responses each fit
2,097,152 encoded bytes. Admission checks raw bodies before parsing, including
whitespace and numeric spellings. It also checks the complete compact rebase
and exact retry before accepting their parsed bindings. A schema-valid
combination that exceeds any of these joint limits is inadmissible; it MUST NOT
produce an oversized retry or an incomplete replacement. The candidate's
4096-character identity and 1771-chunk schema maxima remain intact.

Candidate `1.5.0` keeps the same bounds. Its 8192-byte request bound counts the
`base_live_sequence` member, the exact retry carries it as 0, and the signed
`live_sequence` of a bootstrap or replacement manifest counts toward the
2,097,152-byte bound.

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

The retired `1.2.0` published its `map_months_and_routes` receipt unchanged.

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
