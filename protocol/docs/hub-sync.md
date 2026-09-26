# Hub changes-since contract

[`hub-sync-v1@1.1.0`](../profiles/hub-sync-v1/1.1.0/) is the candidate,
source-neutral successor to the retained `hub-sync-v1@1.0.0` candidate. It
has no exact Hub product-version pin. A client sends the accepted manifest schema range and
its persisted receipt checkpoint to `POST
/v1/vehicles/{vehicle_id}/sync/changes-since` with a paired bearer. A missing,
invalid, expired, or revoked bearer returns the unsigned empty `401` response.

A `200` response is a signed changed-set receipt. It advances the sequence and
contains exactly one pack reference in this slice. The client verifies the
Ed25519 signature over the canonical receipt before trusting the pack, then
persists the new receipt checkpoint only after applying that pack.

If compaction removed the requested base, the Hub returns a signed `409`
`rebase_required` hint. The client applies its replacement pack, persists the
replacement receipt and sequence, and sends the exact `retry_request` from the
hint. It must not infer another checkpoint or replace this with a full-history
request.

The checked-in signature fields are deterministic transport-shape fixtures.
They are intentionally not cryptographic evidence. Production clients must
verify the Ed25519 signature with the key selected by `key_id` and reject an
invalid signature, digest, or canonical payload.

The `1.1.0` successor adds signed no-op receipts, `406` empty no-store
unavailability, pack range semantics, status tables, and one signed schema 2.2
manifest containing ordered chunks. Each chunk is 8–16 MiB compressed; the
outer manifest has the only signature. Prepared-compute artifacts remain a
separate contract.
