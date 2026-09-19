# Upgrade, backup, and recovery

The encrypted spool is recoverable only with its exact spool key. A valid
backup contains the spool, spool key, Hub credential state, configuration, and
both TLS identities. Losing the spool key makes pending records unrecoverable.

> **Do not** put private keys, bearer files, credential state, or encrypted
> spool files into ordinary telemetry/log archives. Store operational backups
> in an access-controlled encrypted destination.

## Create a consistent backup

Stop the Tesla receiver first so no new record can arrive, then stop Edge:

```bash
sudo systemctl stop teslatlas-fleet-telemetry.service
sudo systemctl stop teslatlas-edge.service
systemctl is-active teslatlas-edge.service teslatlas-fleet-telemetry.service
```

Confirm both report `inactive`. With `umask 077`, copy these paths into a new
encrypted backup destination:

```text
/etc/teslatlas-edge/config.toml
/etc/teslatlas-edge/fleet-telemetry.json
/etc/teslatlas-edge/*.crt
/etc/teslatlas-edge/*.key
/var/lib/teslatlas-edge/receiver-token
/var/lib/teslatlas-edge/spool-key
/var/lib/teslatlas-edge/hub-credentials.json
/var/lib/teslatlas-edge/spool/
```

Record file counts and SHA-256 digests inside the encrypted destination. Test
that the archive can be listed and extracted into a temporary empty directory
before restarting services. Keep the spool key and encrypted backup access
credential in separate security domains.

On macOS, boot out both launch agents before copying
`/Users/Shared/TeslatlasEdge`:

```bash
launchctl bootout "gui/$UID/uk.co.magrathean.teslatlas-fleet-telemetry"
launchctl bootout "gui/$UID/uk.co.magrathean.teslatlas-edge"
```

## Upgrade without losing pending records

1. Build and verify both new binaries before stopping the old version.
2. Create and verify a consistent backup.
3. Record whether each service was active and enabled.
4. Stop sidecar, then Edge. The application drains for up to five seconds and
   the supplied service managers allow ten seconds for the drain and final
   spool sync.
5. Install both binaries atomically from the same tested build.
6. Run `sudo -u teslatlas-edge /usr/bin/teslatlas-edge --config
   /etc/teslatlas-edge/config.toml doctor`.
7. Start Edge, verify readiness, then start the sidecar.
8. Confirm intended listeners and prove one test record in a separate staging
   installation; never inject invented telemetry into a live user's Hub.

This release uses spool format `3`. It preserves the existing pending envelopes
and sequence history, and persists each new acknowledgement receipt with the
accepted `(spool_seq, stable_record_id, legacy_record_id)` occurrence. A replay
therefore cannot delete a later admission of the same stable event. The plain
`spool/FORMAT` marker is replaced atomically from `2` to `3` only after recovery
succeeds. Backup before first start. Wire v1 remains available, but storage
rollback is not compatible: do not run an older binary against a spool once
that marker exists. The supplied systemd unit and macOS LaunchAgent execute
`run-with-spool-format-guard.sh`; it allows the explicit `2` to `3` migration
and otherwise compares the marker to the candidate binary's `storage-format`
output before launching Edge.

Format-2 acknowledgement receipts contain only stable or legacy IDs and do
not prove which admission occurrence was acknowledged. If any receipt exists,
the v3 binary fails with `ReceiptRecoveryRequired` before recovery or marker
replacement, preserving the receipt and pending bytes for a reviewed Hub
lineage reconciliation. Do not delete receipts, reset the Hub frontier, or
retry with an older binary to bypass this error. A format-2 spool with no
receipts may migrate through the normal guarded start; keep the original
backup until the v3 marker and readiness have been verified. When the old
marker has no receipts, the migration conservatively marks the v1 receipt
history as unknown/pruned; v1 acknowledgements then return the same explicit
upgrade response until the Hub uses v2.

V1 receipt idempotency is intentionally bounded by the 1,024 retained receipt
files. If pruning removes a v1 receipt, Edge persists that history boundary and
rejects subsequent v1 acknowledgements with
`V1AcknowledgementHistoryPruned`; use the v2 endpoint, whose sequence-bound
receipt carries the original admission identity. This prevents an old v1 ACK
from deleting a later event whose legacy ID produces the same public batch
digest. Retained v1 receipts continue to support exact replay.

## Roll back a failed upgrade

Stop sidecar then Edge. Restore the previous binaries. Restore the pre-upgrade
state only into a new empty directory; never merge two live spool directories.
For rollback to a pre-v2 binary, verify that the restored spool has no `FORMAT`
marker. To roll back from v3, restore the complete pre-upgrade backup into a new
empty directory; never point a v2 or older binary at a v3 spool. Pre-v2 binaries
do not understand the marker themselves, so never restore a legacy unit or
plist that bypasses the supplied launch guard. The guard rejects an older
binary before it can open a v3 spool. Run `doctor`, start Edge, verify
readiness, then start the sidecar. Preserve the failed upgrade state until
delivery and record counts have been reconciled.

## Restore onto an empty host

1. Install the same or a declared-compatible Edge and sidecar versions.
2. Create the service identity and empty private directories.
3. Stop both services.
4. Restore config, TLS material, receiver bearer, spool key, credential state,
   and the entire encrypted spool without changing file names.
5. Set directories to 0700, secrets/private keys to 0600, public certificates
   to 0644, and ownership to the service identity.
6. Run `doctor`.
7. Start Edge first. Check `/readyz` and queue counts. Start the sidecar last.
8. Let Hub deduplicate all replayed records before acknowledgement.

A restore with the wrong key fails closed as a key mismatch. Do not rotate or
replace the key while pending files exist.

## Handle corruption or degraded readiness

On startup, orphan temporary files move to `spool/quarantine` and block delivery
because their sequence outcome cannot be proven. Invalid v2 ciphertext with a
recoverable sequence first creates an encrypted payload-free gap notice, then
moves to quarantine. Later v2 records remain deliverable with that notice.
Invalid pending data without a recoverable sequence, corrupt gap state,
wrong-key state, or full auxiliary storage blocks delivery and keeps `/readyz`
at 503.

1. Stop the sidecar to halt admission.
2. Preserve a private copy of the full spool, key, and quarantine directory.
3. Run `doctor` and inspect only aggregate health/metrics; do not print
   ciphertext or decoded telemetry into logs.
4. Restore a verified backup into a new empty state directory when available.
5. If no valid key/backup exists, record the loss boundary and retain evidence;
   do not fabricate replacement records or silently clear degradation.

Retention loss is irreversible and increments a durable historical counter.
Edge persists one gap notice before deleting each expired record; readiness is
degraded until an exact v2 Hub acknowledgement removes that active notice. V1
delivery returns 409 while any notice is pending. Increase capacity or repair
connectivity before the stored retention deadline instead of relying on expiry
as queue management. Changing configuration never changes deadlines already
stored with pending records.
