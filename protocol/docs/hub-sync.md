# Hub changes-since contract

[`hub-sync-v1@1.3.0`](../profiles/hub-sync-v1/1.3.0/) is the candidate,
source-neutral successor to retained `hub-sync-v1@1.0.0`, `1.1.0`, and `1.2.0` candidates. It
has no exact Hub product-version pin. A client applies a signed initial
manifest, then persists its `receipt_id`, `sequence`, and `schema_version` as
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

[`fixture-signing-keys.json`](../profiles/hub-sync-v1/1.3.0/fixture-signing-keys.json)
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
prepared artefact may contain at most 497 route spans in addition to 120 map
months. These maxima keep their largest compact JSON forms inside 2 MiB.

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

`1.2.0` adds the `map_months_and_routes` prepared-artefact pack receipt. It
binds source manifest and sequence, the input window, generation identity and
time, fixed units, algorithm version, a single pack, and one signature. Its
dirty spans admit only changed map months and changed route identifiers inside
that window. Other prepared-compute artefact types remain separate contracts.
