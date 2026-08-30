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
4. Stop sidecar, then Edge, with the five-second service deadline.
5. Install both binaries atomically from the same tested build.
6. Run `teslatlas-edge --config /etc/teslatlas-edge/config.toml doctor`.
7. Start Edge, verify readiness, then start the sidecar.
8. Confirm intended listeners and prove one test record in a separate staging
   installation; never inject invented telemetry into a live user's Hub.

Version 1 has no state migration. If a future release introduces one, its
release notes must state the last rollback-compatible spool version. Do not run
an older binary against state already migrated by a newer one.

## Roll back a failed upgrade

Stop sidecar then Edge. Restore the previous binaries. Restore the pre-upgrade
state only into a new empty directory; never merge two live spool directories.
Run `doctor`, start Edge, verify readiness, then start the sidecar. Preserve the
failed upgrade state until delivery and record counts have been reconciled.

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

On startup, invalid ciphertext and orphan temporary files move to the
`spool/quarantine` directory. Edge increments aggregate corruption state and
`/readyz` returns 503. Valid pending files remain deliverable.

1. Stop the sidecar to halt admission.
2. Preserve a private copy of the full spool, key, and quarantine directory.
3. Run `doctor` and inspect only aggregate health/metrics; do not print
   ciphertext or decoded telemetry into logs.
4. Restore a verified backup into a new empty state directory when available.
5. If no valid key/backup exists, record the loss boundary and retain evidence;
   do not fabricate replacement records or silently clear degradation.

Retention expiry is also degraded and irreversible. Increase capacity or repair
connectivity before the configured retention boundary instead of relying on
expiry as queue management.
