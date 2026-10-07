# Backup and recovery

Hub separates ordinary data backup from credential disaster recovery.

## Create and verify a data backup

The following commands are for a Debian package installation. Create a private
parent directory owned by the service account; each backup destination itself
must not already exist. Stop the Hub service first: backup and credential
export take the exclusive Hub instance lock and will refuse to run while Serve
owns it.

```sh
sudo systemctl stop teslatlas-hub.service
sudo install -d -o teslatlas -g teslatlas -m 0700 \
  /srv/teslatlas-hub-backups
sudo -u teslatlas -- /usr/bin/teslatlas-hub \
  --config /etc/teslatlas-hub/config.toml backup \
  --destination /srv/teslatlas-hub-backups/hub-data-2026-08-28
sudo -u teslatlas -- /usr/bin/teslatlas-hub verify-backup \
  --source /srv/teslatlas-hub-backups/hub-data-2026-08-28
```

The data backup contains the catalogue, encrypted token row, pairing database,
and all referenced immutable packs, including active or retained PhysicalV3,
changed-set, and prepared-map packs. Pairing invitations and active device
authority are removed.
It excludes the TeslaMate decryption key, cursor-signing key, TLS identity,
configuration, and service state.

## Export recovery credentials

Create a private export directory, then create a random raw 32-byte key in a
mode-0600 file and export a separately encrypted credential bundle:

```sh
sudo install -d -o teslatlas -g teslatlas -m 0700 \
  /srv/teslatlas-hub-recovery
sudo -u teslatlas -- sh -c \
  'umask 077; openssl rand 32 > /srv/teslatlas-hub-recovery/teslatlas-recovery.key'
sudo -u teslatlas -- /usr/bin/teslatlas-hub \
  --config /etc/teslatlas-hub/config.toml export-recovery-credentials \
  --destination /srv/teslatlas-hub-recovery/teslatlas-credentials.tthcr \
  --recovery-key-file /srv/teslatlas-hub-recovery/teslatlas-recovery.key
```

Store the data backup, encrypted credential export, and raw recovery key in
separate security domains after export. In particular, move the raw key off the
Hub host; do not leave it beside the encrypted credential bundle.

After the backup and credential export complete, restart the Debian service
and verify that systemd kept it active. Inspect its recent log before treating
collection as resumed:

```sh
sudo systemctl start teslatlas-hub.service
sudo systemctl is-active --quiet teslatlas-hub.service
sudo journalctl -u teslatlas-hub.service -n 50 --no-pager
```

## Restore

Stop the packaged service, then restore data into a new empty directory:

```sh
sudo systemctl stop teslatlas-hub.service
sudo install -d -o teslatlas -g teslatlas -m 0700 \
  /srv/teslatlas-hub-restore
sudo -u teslatlas -- /usr/bin/teslatlas-hub restore-data \
  --source /srv/teslatlas-hub-backups/hub-data-2026-08-28 \
  --destination /srv/teslatlas-hub-restore/hub-data
```

Copy the packaged configuration to a separate recovery file. The shell's
no-clobber option makes this fail if the recovery file already exists; the
installed configuration remains unchanged. Edit only the recovery copy and set
its `data_dir` to `/srv/teslatlas-hub-restore/hub-data`:

```sh
sudo sh -c '
  set -euC
  umask 027
  cat /etc/teslatlas-hub/config.toml \
    > /etc/teslatlas-hub/recovery-config.toml
  chown root:teslatlas /etc/teslatlas-hub/recovery-config.toml
'
sudoedit /etc/teslatlas-hub/recovery-config.toml
```

While the service is stopped, restore credentials with that configuration and
the raw key retrieved from its separate security domain:

```sh
sudo -u teslatlas -- /usr/bin/teslatlas-hub \
  --config /etc/teslatlas-hub/recovery-config.toml \
  restore-recovery-credentials \
  --source /srv/teslatlas-hub-recovery/teslatlas-credentials.tthcr \
  --recovery-key-file /media/teslatlas-recovery-key/teslatlas-recovery.key
```

Credential restore requires the matching installation ID and refuses to
overwrite an existing `secrets` directory.

The packaged unit reads `/etc/teslatlas-hub/config.toml` and can write only
under `/var/lib/teslatlas-hub`. Add a separate no-clobber systemd drop-in that
starts the Hub with the recovery configuration and grants write access to the
restored data directory. This leaves the packaged unit and original data path
unchanged:

```sh
sudo install -d -o root -g root -m 0755 \
  /etc/systemd/system/teslatlas-hub.service.d
sudo sh -c '
  set -euC
  umask 022
  cat > /etc/systemd/system/teslatlas-hub.service.d/90-recovery.conf
' <<'EOF'
[Service]
ExecStart=
ExecStart=/usr/bin/teslatlas-hub --config /etc/teslatlas-hub/recovery-config.toml serve
ReadWritePaths=/srv/teslatlas-hub-restore/hub-data
EOF
sudo systemctl daemon-reload
sudo systemctl cat teslatlas-hub.service
```

Confirm that `systemctl cat` shows both recovery overrides, then start the
service and check its state, recent log, and Hub readiness. The final command
must report `"ready": true` before recovery is treated as complete:

```sh
sudo systemctl start teslatlas-hub.service
sudo systemctl is-active --quiet teslatlas-hub.service
sudo journalctl -u teslatlas-hub.service -n 50 --no-pager
sudo -u teslatlas -- /usr/bin/teslatlas-hub \
  --config /etc/teslatlas-hub/recovery-config.toml status
```

Pair devices again and prove a fresh observation before declaring recovery
complete.

On macOS, stop the per-user service with the Mac app or the packaged CLI before
running backup and recovery commands. Run those commands as the signed-in user
with the packaged binary and per-user configuration paths documented in
[CLI reference](../guides/cli.md#platform-invocation). Restart the service only
after the command has completed and the intended data and configuration have
been checked.

## Interrupted PhysicalV3 publication

Ordinary PhysicalV3 replacement now commits its public head, retained predecessor
and optional changed-set in one catalogue transaction. A failure before commit
keeps the previous public head; a completed commit exposes the complete successor.
Its immutable packs are verified before admission and remain covered by normal
backup verification.

A store interrupted by the older two-transaction publication path can contain a
`blocked_rotation` head. Retrying the import verifies the exact persisted candidate
and predecessor packs before activating that candidate. If source history has
advanced, the verified candidate is activated first and becomes the predecessor
of the new publication. Existing signed identities and retained reader receipts
are preserved.

Automatic recovery requires that retained predecessor to remain within the
existing 24-hour retention window. An expired predecessor, corrupt artifact or
binding mismatch remains a failed publication; changing the clock, extending
receipt expiry or editing catalogue rows is not a supported repair. Preserve the
affected store and receipts. A verified backup restored into a new directory can
provide a known-good recovery point through the procedure above; recovery of an
expired blocked state without such a backup requires a separately reviewed repair
design. The new atomic production path prevents this gap for future replacements.
