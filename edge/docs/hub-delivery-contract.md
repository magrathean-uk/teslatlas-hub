# Hub delivery contract

Wire version 1 lets a home Hub pull already-admitted telemetry from Edge and
acknowledge it only after durable deduplication and commit.

> **Required:** TLS client authentication and a valid scoped bearer are both
> mandatory. Hub must persist `record_id` before acknowledgement. A green HTTP
> exchange without that ordering does not satisfy the contract.

## Authenticate the Hub

Edge serves the Hub API only on the configured TLS listener. The server
certificate identifies Edge. Edge accepts client certificates only from
`hub_client_ca_path`. Every request also carries:

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
request, so revocation is immediate.

## Pull the next batch

`GET /v1/hub/batches/next` returns HTTP 200, including for an empty queue.

```json
{
  "version": 1,
  "batch_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "records": [
    {
      "record_id": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      "received_at_ms": 1800000000100,
      "envelope": {
        "version": 1,
        "vin": "5YJ3E1EA7KF000001",
        "txid": "receiver-0001",
        "tx_type": "vehicle_data",
        "received_at_ms": 1800000000100,
        "timestamp_ms": 1800000000000,
        "payload": {"Soc": 80}
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
  "batch_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "accepted_record_ids": [
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
  ]
}
```

Success returns the exact deletion result:

```json
{
  "version": 1,
  "acknowledged_record_ids": [
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
  ],
  "unknown_record_ids": []
}
```

Accepted IDs may be reordered or be a subset, but every ID must belong to the
current batch. If an acknowledgement response is lost, Hub may GET again and
reconcile the current queue. A 400 after Edge restart means Hub should discard
the stale batch correlation, GET the current batch, deduplicate, and retry.

## Handle status and retry

| Result | Meaning | Hub action |
| --- | --- | --- |
| TLS failure | Client or server identity not trusted | Stop; repair certificate trust |
| 200 | Batch or acknowledgement response | Continue contract |
| 400 | Invalid or stale acknowledgement | GET current batch; repair protocol if repeated |
| 401 | Bearer missing, expired, rotated out, or revoked | Stop; enrol or rotate credential |
| 413 | Request body exceeds fixed limit | Stop; repair client |
| 503 | Spool or credential state unavailable | Retry with bounded exponential backoff |
| Timeout/disconnect | Commit/response outcome unknown | Retry GET; deduplicate before any apply |

Use jittered exponential backoff capped at 30 seconds. Never acknowledge on a
failed or uncommitted Hub transaction.

## Version negotiation

Only numeric `version: 1` is accepted. There is no implicit downgrade or
content-type negotiation in beta. A future incompatible version requires a
coordinated Edge and Hub upgrade; unknown versions fail closed.
