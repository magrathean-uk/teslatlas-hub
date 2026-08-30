# Backup and recovery

Hub separates ordinary data backup from credential disaster recovery.

## Create and verify a data backup

The following commands are for a Debian package installation. Create a private
parent directory owned by the service account; each backup destination itself
must not already exist.

```sh
sudo install -d -o teslatlas -g teslatlas -m 0700 \
  /srv/teslatlas-hub-backups
sudo -u teslatlas -- /usr/bin/teslatlas-hub \
  --config /etc/teslatlas-hub/config.toml backup \
  --destination /srv/teslatlas-hub-backups/hub-data-2026-08-28
sudo -u teslatlas -- /usr/bin/teslatlas-hub verify-backup \
  --source /srv/teslatlas-hub-backups/hub-data-2026-08-28
```

The data backup contains the catalogue, encrypted token row, pairing database,
and immutable packs. Pairing invitations and active device authority are removed.
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

## Restore

Restore data into a new empty directory:

```sh
sudo install -d -o teslatlas -g teslatlas -m 0700 \
  /srv/teslatlas-hub-restore
sudo -u teslatlas -- /usr/bin/teslatlas-hub restore-data \
  --source /srv/teslatlas-hub-backups/hub-data-2026-08-28 \
  --destination /srv/teslatlas-hub-restore/hub-data
```

Create a separate mode-0600 configuration owned by `teslatlas` whose
`data_dir` is `/srv/teslatlas-hub-restore/hub-data`. While the service is
stopped, restore credentials with that configuration and the raw key retrieved
from its separate security domain:

```sh
sudo -u teslatlas -- /usr/bin/teslatlas-hub \
  --config /srv/teslatlas-hub-restore/config.toml \
  restore-recovery-credentials \
  --source /srv/teslatlas-hub-recovery/teslatlas-credentials.tthcr \
  --recovery-key-file /media/teslatlas-recovery-key/teslatlas-recovery.key
```

Credential restore requires the matching installation ID and refuses to
overwrite an existing `secrets` directory. Run `doctor`, pair devices again,
and prove a fresh observation before declaring recovery complete.

On macOS, run backup and recovery commands as the signed-in user with the
packaged binary and per-user configuration paths documented in
[CLI reference](../guides/cli.md#platform-invocation).
