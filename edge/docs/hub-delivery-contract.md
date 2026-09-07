# Hub delivery contract

Wire version 1 remains compatible for record-only queues. Wire version 2 adds
stable retry identity, monotonic sequence, and durable gap acknowledgement.

The source-neutral public bundle is `edge-delivery-v2@2.0.0` in the sibling
`teslatlas-protocol/profiles/edge-delivery-v2/2.0.0` repository path. Its
`SHA256SUMS` file has SHA-256
`e304fb6ebe074ee2e71d35b1f52d408f87fa1f0624b8ebcdba2ca2eb1fced224`.
The bundle freezes schemas, OpenAPI, stable and legacy IDs, merged sequence
order, exact acknowledgement bytes, disposition requirements, and positive and
negative replay vectors without importing Hub implementation.

> **Required:** TLS client authentication and a valid bearer are both
> mandatory. Hub must persist the record or gap decision before
> acknowledgement. A green HTTP exchange without that ordering does not
> satisfy the contract.

## Authenticate the Hub

Edge serves the Hub API only on the configured TLS listener. The server
certificate identifies Edge. Edge accepts client certificates only from
`hub_client_ca_path`. This must be a dedicated CA for one Hub installation, not
a shared organizational CA. Every request also carries:

```http
Authorization: Bearer tte1.<credential-id>.<secret>
```

Create, rotate, and revoke the bearer locally:

```bash
teslatlas-edge --config /etc/teslatlas-edge/config.toml credential enrol home-hub
teslatlas-edge --config /etc/teslatlas-edge/config.toml credential rotate 123e4567-e89b-12d3-a456-426614174000 --overlap-seconds 300
teslatlas-edge --config /etc/teslatlas-edge/config.toml credential revoke 123e4567-e89b-12d3-a456-426614174000
```

The enrol and rotate commands print the new bearer once. Edge stores only its
domain-separated SHA-256 digest. Verification reloads credential state on each
request, so revocation is immediate. Current credentials are intentionally
whole-queue grants; labels are operational names, not vehicle scopes.

## Pull the next batch

`GET /v1/hub/batches/next` returns HTTP 200, including for an empty queue.

```json
{
  "version": 1,
  "batch_id": "6e06fa3b7fcb563eeab98be97b1af913b99e37705c2b5d4f389cdf014cefc8ea",
  "records": [
    {
      "record_id": "ac89a19968e0d88fe632e2cf59046dd333213da1e4ecd34f97430341bc70a0bd",
      "received_at_ms": 1800000000100,
      "envelope": {
        "version": 1,
        "vin": "5YJ3E1EA7KF000001",
        "txid": "edge-projected-0001",
        "tx_type": "V",
        "received_at_ms": 1800000000100,
        "timestamp_ms": 1800000000000,
        "payload": {
          "vin": "5YJ3E1EA7KF000001",
          "createdAt": "2027-01-15T08:00:00Z",
          "data": {"Soc": {"intValue": "80"}}
        }
      }
    }
  ]
}
```

Batches are oldest-admission-first and bounded by configured record and encoded
byte limits. Repeating the GET before an acknowledgement returns the same batch
while the queue is unchanged.

## Deduplicate and commit

For every record, Hub performs one durable transaction:

1. Insert `record_id` into a unique, persistent receipt table.
2. If the insert is new, apply the envelope to Hub ingestion.
3. If the insert conflicts, treat it as an already-applied duplicate.
4. Commit the receipt and any resulting Hub state.
5. Only after commit, include that record in the Edge acknowledgement.

Hub must not use `txid`, arrival order, VIN plus timestamp, or an in-memory cache
as the primary delivery deduplication key.

## Compute `record_id`

Edge computes:

```text
lower_hex(SHA-256(
  UTF-8("teslatlas-edge-record-v1\0") ||
  JCS(receiver_envelope)
))
```

`JCS` is JSON Canonicalization Scheme serialization of the full strict
receiver envelope. Optional `device_client_version` and `firmware_version`
fields participate when present and are omitted when absent. Object key order
does not affect the result. A content change produces a different ID.

`batch_id` is lowercase SHA-256 hex over the ordered record IDs with the domain
`teslatlas-edge-batch-v1\0` and a zero byte after every ID. Hub should treat it
as an opaque batch correlation value, not a record identity.

## Acknowledge committed records

`POST /v1/hub/acks` accepts at most 256 unique record IDs and a 128 KiB body.
Unknown JSON fields, duplicate JSON keys, duplicate IDs, unsupported versions,
and non-current batch IDs are rejected.

```json
{
  "version": 1,
  "batch_id": "6e06fa3b7fcb563eeab98be97b1af913b99e37705c2b5d4f389cdf014cefc8ea",
  "accepted_record_ids": [
    "ac89a19968e0d88fe632e2cf59046dd333213da1e4ecd34f97430341bc70a0bd"
  ]
}
```

Success returns the exact deletion result:

```json
{
  "version": 1,
  "acknowledged_record_ids": [
    "ac89a19968e0d88fe632e2cf59046dd333213da1e4ecd34f97430341bc70a0bd"
  ],
  "unknown_record_ids": []
}
```

Accepted IDs may be reordered or be a subset, but every ID must belong to the
current batch. If an acknowledgement response is lost, Hub may GET again and
reconcile the current queue. A 400 after Edge restart means Hub should discard
the stale batch correlation, GET the current batch, deduplicate, and retry.

## Pull version 2 records and gaps

`GET /v2/hub/batches/next` uses the same authentication and bounds. It returns
stable record IDs, retained v1 aliases, persistent sequences, and payload-free
gap notices:

```json
{
  "version": 2,
  "batch_id": "1966d5239605c0b8e9eab5b5ca8105f39df35725ffa6ab12f93cb9e4c35599d1",
  "records": [
    {
      "record_id": "8284fe7aea66b79f09cfa5b2fe3ca99fc79fdac24631e06b8365aa8a0c64e5c9",
      "legacy_record_id": "ac89a19968e0d88fe632e2cf59046dd333213da1e4ecd34f97430341bc70a0bd",
      "spool_seq": 10,
      "received_at_ms": 1800000000100,
      "envelope": {
        "version": 1,
        "vin": "5YJ3E1EA7KF000001",
        "txid": "edge-projected-0001",
        "tx_type": "V",
        "received_at_ms": 1800000000100,
        "timestamp_ms": 1800000000000,
        "payload": {
          "vin": "5YJ3E1EA7KF000001",
          "createdAt": "2027-01-15T08:00:00Z",
          "data": {"Soc": {"intValue": "80"}}
        }
      }
    }
  ],
  "gaps": [
    {
      "notice_id": "73002ffc20a769d62ab2800675b51e8fc3a895ff762b370e5edf11185b864ab2",
      "spool_seq": 11,
      "occurred_at_ms": 1800000060200,
      "reason": "retention_expired",
      "evidence_sha256": "c2fceaa38e73c41b78386cad195a383e5ae4505db9f71c0e43e7f669a8380e53"
    }
  ]
}
```

Every record and gap carries `spool_seq`. Hub merges both arrays by sequence
before applying them. Each sequence occurs exactly once across both arrays.
Batch record and byte limits count both kinds. Repeating the GET without
acknowledgement returns the same current items.

The default batch limits are 256 total items and 1 MiB of encoded item bodies.
Configuration can raise them only to hard limits of 1,024 total items and
4 MiB. Including the fixed compact JSON wrapper and at most 1,023 item commas,
the whole response is at most 4,195,441 bytes. A consumer must bound the entire
encoded response before parsing and
must reject duplicate sequences, duplicate stable IDs, or conflicting retained
v1 aliases without applying or acknowledging any item.

The outer record `received_at_ms` is Edge's durable admission time. The
envelope's `received_at_ms` is the receiver-supplied field hashed into the v1
alias and excluded from the v2 stable identity. Neither arrival field is an
event-time substitute; telemetry event time remains `timestamp_ms` and the
Fleet protojson `createdAt` value where present.

The v2 batch ID binds that merged order:

```text
lower_hex(SHA-256(
  UTF-8("teslatlas-edge-batch-v2\0") ||
  for each merged item by spool_seq:
    kind_byte("r" for record, "g" for gap) ||
    spool_seq_as_u64_big_endian ||
    UTF-8(record_id or notice_id) || 0x00
))
```

The v2 stable record ID is:

```text
lower_hex(SHA-256(
  UTF-8("teslatlas-edge-record-v2\0") ||
  JCS({
    version: 2,
    vin,
    txid,
    tx_type,
    timestamp_ms,
    payload,
    device_client_version?,
    firmware_version?
  })
))
```

It excludes only `received_at_ms`. Hub should store both the v2 ID and v1 alias
during migration.

Gap reasons are `retention_expired` and `integrity_quarantine`. A notice has no
VIN, transaction ID, coordinates, or payload. Hub durably stores the Edge
identity, sequence, notice ID, reason, occurrence time, and evidence digest
before acknowledgement. A gap is evidence of missing telemetry, never a
replacement telemetry event.

Accepted receiver transaction types normalize by removing non-ASCII-
alphanumeric characters and lowercasing; the result must be one of `v`,
`data`, `vehicledata`, `connectivity`, `alerts`, or `errors`. A valid `V`,
`data`, or `vehicle_data` record still projects only when its payload is valid
Fleet protojson for fields the Hub supports. Valid alerts, errors,
connectivity, and other valid but unprojectable records require a durable
non-projection disposition containing the stable identity, retained v1 alias,
sequence, reason, and canonical payload digest before acknowledgement. They
must not create fictitious current-state values or disappear silently.

Hub owns one atomic transaction for the application receipt, sequence
disposition, retained alias, resulting data effect, and recoverable derived
export work. The permitted durable dispositions are `projected_telemetry`,
`durable_non_projection_event`, `durable_gap`, and `duplicate`. A malformed
envelope, identity mismatch, sequence conflict, alias conflict, or failed
authentication is `rejected_invalid_envelope`: it remains visibly blocked and
cannot advance the acknowledgement prefix.

`POST /v2/hub/acks` accepts exact IDs from the current v2 batch:

```json
{
  "version": 2,
  "batch_id": "1966d5239605c0b8e9eab5b5ca8105f39df35725ffa6ab12f93cb9e4c35599d1",
  "accepted_record_ids": [
    "8284fe7aea66b79f09cfa5b2fe3ca99fc79fdac24631e06b8365aa8a0c64e5c9"
  ],
  "accepted_gap_notice_ids": [
    "73002ffc20a769d62ab2800675b51e8fc3a895ff762b370e5edf11185b864ab2"
  ]
}
```

Edge persists an encrypted receipt before deleting accepted record or gap
files. The accepted IDs must be one contiguous prefix of the merged
`spool_seq` order. Hub may acknowledge no items, the first item, or any longer
prefix; it may not acknowledge a later item while leaving an earlier item
pending. Receipt replay is idempotent across restart. Unknown, duplicate,
out-of-order, oversized, stale-batch, or wrong-version input returns 400 without
deletion.

V1 cannot represent a gap. `GET /v1/hub/batches/next` therefore returns 409
`protocol_upgrade_required` whenever a durable gap is pending. Hub must use v2,
persist and acknowledge the notice, then may continue using either record
contract.

## Handle status and retry

| Result | Meaning | Hub action |
| --- | --- | --- |
| TLS failure | Client or server identity not trusted | Stop; repair certificate trust |
| 200 | Batch or acknowledgement response | Continue contract |
| 400 | Invalid or stale acknowledgement | GET current batch; repair protocol if repeated |
| 401 | Bearer missing, expired, rotated out, or revoked | Stop; enrol or rotate credential |
| 413 | Request body exceeds fixed limit | Stop; repair client |
| 409 | V1 cannot consume a pending durable gap | Use v2 and commit the gap notice |
| 503 | Spool or credential state unavailable | Retry with bounded exponential backoff |
| Timeout/disconnect | Commit/response outcome unknown | Retry GET; deduplicate before any apply |

Use jittered exponential backoff capped at 30 seconds. Never acknowledge on a
failed or uncommitted Hub transaction.

## Version negotiation

Versions 1 and 2 use distinct paths and numeric body versions. There is no
implicit downgrade or content-type negotiation. Unknown versions fail closed.
Hub v2 implementation and live end-to-end proof are not supplied by the Edge
repository's Rust tests.
