# Edge architecture

Teslatlas Edge is a narrow, optional ingress between Tesla Fleet Telemetry and
a user's home Hub. The public host receives vehicle telemetry; the home Hub
initiates the delivery connection.

## Data path

```mermaid
flowchart LR
    V[Tesla vehicle] -->|Tesla mTLS WebSocket| R[Pinned Tesla receiver sidecar]
    R -->|Bearer + strict JSON on 127.0.0.1| A[Edge durable admission]
    A -->|XChaCha20-Poly1305 files| S[Bounded spool]
    H[Home Hub] -->|Outbound mTLS + scoped bearer| D[Edge pull and ack API]
    S --> D
    D -->|Versioned batches| H
```

The pinned Tesla Fleet Telemetry v0.9.4 sidecar owns Tesla's WebSocket,
protobuf, vehicle certificate, and reliable-ack protocol. Its Teslatlas patch
posts a decoded envelope only to
`http://127.0.0.1:8080/v1/internal/fleet-telemetry`. It acknowledges a vehicle
record only after Edge returns an HTTP 2xx response.

Edge acknowledges that loopback request only after an encrypted file has been
written, synced, and atomically renamed into the pending spool. New records
carry the unchanged v1 ID, a stable v2 ID that excludes receiver arrival time,
and a persistent monotonic `spool_seq`. A home Hub pulls a deterministic batch,
commits each new identity, and posts an exact acknowledgement. Edge persists an
encrypted acknowledgement receipt before deleting accepted records.

## Trust boundaries

| Boundary | Authentication | Data allowed |
| --- | --- | --- |
| Vehicle to sidecar | Tesla vehicle mTLS | Fleet Telemetry only |
| Sidecar to Edge | Loopback plus private bearer | Strict envelope, 256 KiB maximum |
| Hub to Edge | Trusted Hub client certificate plus rotating bearer | Batch pull and acknowledgement only |
| Edge disk | Mode-0600 key plus XChaCha20-Poly1305 | Pending envelopes, sequence state, receipts, payload-free gaps |

The public Hub listener never serves plaintext HTTP. The loopback listener
contains receiver admission, liveness, readiness, and privacy-safe aggregate
metrics. It must not be exposed by a reverse proxy or firewall rule.

## Reliability model

- Receiver retries are idempotent by a stable v2 ID that excludes only the
  sidecar-local arrival time. The v1 ID remains unchanged for compatibility.
- Pending records survive process and host restart when the spool and key
  survive.
- Disconnect before acknowledgement returns the same oldest-first batch.
- Hub persists its unique `record_id` decision before it acknowledges.
- Edge deletes accepted records only after a valid acknowledgement for the
  current batch.
- Capacity or storage-full admission fails with 507; Edge never evicts an
  unexpired pending record to make space.
- Before deleting an expired or sequenced-corrupt record, Edge durably writes a
  payload-free gap notice. V1 delivery returns 409 while a gap awaits v2 Hub
  acknowledgement. The cumulative expiry counter survives acknowledgement, but
  readiness recovers after the active gap is committed and acknowledged.
- Records and gaps share one globally unique sequence. A durable gap is
  authoritative if its source file reappears after a crash.
- Corruption without a recoverable v2 sequence, including an orphan atomic-write
  temporary file, blocks delivery and readiness.
- V2 acknowledgements may advance only through a contiguous prefix of the merged
  record-and-gap sequence.
- Pending records, gaps, acknowledgement receipts, and quarantine are all
  independently bounded. Full auxiliary storage fails closed.
- Shutdown asks both listeners to drain for at most five seconds, aborts any
  remaining task, then syncs spool directories.

The spool root contains a `FORMAT` marker with value `2`. This makes the
forward-only storage transition visible to the supplied launch guard. The guard
queries the candidate binary's supported format before allowing it to open the
spool. An older binary must use a restored pre-upgrade state directory, never
the v2 directory.

## Privacy boundary

Spool file names contain sequence and a content digest, never VIN or payload
text. Gap notices contain sequence, fixed reason, time, and one-way evidence
only. Metrics and health expose only numeric queue, gap, corruption, expiry,
and fixed-reason values. The pinned sidecar overlay removes upstream raw-payload
debug logging. Logs and errors must not include VIN, transaction ID, bearer,
certificate material, coordinates, or raw payload.

## Platform boundary

The Linux service policy makes the vehicle private key and sidecar config
inaccessible to the Rust Edge process, and makes Edge spool, key, Hub identity,
and credentials inaccessible to the sidecar. macOS LaunchAgents share the
interactive user identity and are therefore a development deployment, not a
production process-isolation boundary.

## Explicit exclusions

Edge has no Tesla access token, refresh token, OAuth client secret, command
key, vehicle-command endpoint, consumer API, drive/charge projection, or
mandatory central relay. Tesla account administration happens separately and
its credentials are never copied to Edge.
