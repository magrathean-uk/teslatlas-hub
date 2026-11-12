# Backup and recovery

Hub separates ordinary data backup from credential disaster recovery.

## Create and verify a data backup

The following commands are for a Debian package installation. Create a private
destination folder owned by the service account; each `backup` run adds one
encrypted file, `teslatlas-hub-backup-<time>-<generation>.tlbk`, to it. Stop
the Hub service first: backup and credential export take the exclusive Hub
instance lock and will refuse to run while Serve owns it. While the Hub runs,
`backup-now` makes the same encrypted backup through the service instead.

Backups are encrypted by default with a recovery password you keep (at least
12 characters). The password is never a command argument or an environment
variable. On a terminal, `backup` asks for it twice with echo off, and
`verify-backup` and `restore-data` ask once. When standard input is not a
terminal, the commands read one line from standard input instead, for example
from a protected password file:

```sh
sudo systemctl stop teslatlas-hub.service
sudo install -d -o teslatlas -g teslatlas -m 0700 \
  /srv/teslatlas-hub-backups
sudo -u teslatlas -- /usr/bin/teslatlas-hub \
  --config /etc/teslatlas-hub/config.toml backup \
  --destination /srv/teslatlas-hub-backups
sudo -u teslatlas -- /usr/bin/teslatlas-hub verify-backup \
  --source /srv/teslatlas-hub-backups/teslatlas-hub-backup-<time>-<generation>.tlbk
```

`verify-backup` authenticates and reads the whole bundle without writing
anything. A wrong password is refused.

The data backup contains the catalogue, encrypted token row, pairing database,
and all referenced immutable packs, including active or retained PhysicalV3,
changed-set, and prepared-map packs. Pairing invitations and active device
authority are removed.
It excludes the source application decryption key, cursor-signing key, TLS identity,
configuration, and service state. A restore does not need the excluded keys
from the old host: it generates a new cursor-signing key itself (see
[Restore](#restore)).

A backup has no fixed limit on its number of files. The bundle streams them
one at a time, each with its own path, size and SHA-256 digest; its manifest
records only how many files and bytes follow, and `verify-backup` and
`restore-data` check every file as it arrives and the totals at the end. So
backup and restore use the same memory whether the Hub holds a few hundred
packs or many thousands.

Bundles made before this format list every file in their manifest. They still
verify and restore unchanged; the manifest's format version tells the two
apart. Such a bundle never held more than 4,096 files, and a listed manifest
above that bound is refused.

### Older plaintext directory backups

Before encrypted backups, `backup` wrote a plaintext directory whose
completion marker is an unkeyed digest, so it is not authenticated.
`verify-backup` and `restore-data` refuse such a directory
(`plaintext_directory_refused`) unless you name that format with
`--plaintext-directory`; the flag is refused for an encrypted bundle. A new
backup in the older format needs its own command,
`backup-plaintext-directory --destination <new directory>`. Prefer the
encrypted default. The directory's manifest lists every file, so this older
format keeps its 4,096-file limit; a larger Hub needs the encrypted bundle.

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

Stop the packaged service, then restore data into a new directory that does
not exist yet. The command asks for the recovery password as `backup` does; a
wrong password is refused before anything is written:

```sh
sudo systemctl stop teslatlas-hub.service
sudo install -d -o teslatlas -g teslatlas -m 0700 \
  /srv/teslatlas-hub-restore
sudo -u teslatlas -- /usr/bin/teslatlas-hub restore-data \
  --source /srv/teslatlas-hub-backups/teslatlas-hub-backup-<time>-<generation>.tlbk \
  --destination /srv/teslatlas-hub-restore/hub-data
```

Fleet request counts and reservations come from the backup, as the restore report explains in `fleetRequestCountsNotice`. A backup from earlier in the same UTC calendar month omits requests billed after it was made, so total spending can exceed the intended local monthly cap. A backup from before request counting was introduced contains no saved counts. Restore does not force a new cap stop. Before restarting Fleet polling, account for requests already billed when setting `monthly_cap_cents` for the rest of the month. Tesla's billing remains authoritative. A preserved collector lease is converted into a collection gap in the copied catalogue; the restored Hub's first collector start closes that gap.

A restored Hub also has the sign-in as it was when the backup was made. If the original
Hub renewed its token since, the copy still holds the earlier refresh token, which Tesla no
longer accepts: the restored Hub's first renewal is rejected and it shows
`collector_auth_terminal` (and `"ready": false`) until you sign in again (`setup` for the
Owner API, `setup-fleet` for Fleet). That is expected after a restore from an older backup:
sign in again before the readiness check below.

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

The restore needs no credential export from the old host. When the backup
holds signed sync history, `restore-data` writes a new cursor-signing key into
the restored directory (`secrets/hub-cursor.key`, mode 0600) and re-signs every
stored sync cursor and schema 2.2 no-op body under it, with the same contents
otherwise. History rows and packs are unchanged. This happens in the private
staging directory before the restored directory appears, so an interrupted
restore leaves no destination behind: run it again. A backup without sync
history gets its key when the restored Hub first starts.

Every device paired before the restore is refused afterwards: its bearer no
longer exists and its sync cursor no longer verifies. Pair each device again;
it then downloads a fresh snapshot. The manifest-signing key, and with it the
signature `key_id`, changes with the cursor key. `restore-recovery-credentials`
is not part of this procedure; it refuses to write into a restored directory
that already has a `secrets` directory.

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
must report `"ready": true` before recovery is treated as complete (after signing in again,
if the backup is older than the last token renewal):

```sh
sudo systemctl start teslatlas-hub.service
sudo systemctl is-active --quiet teslatlas-hub.service
sudo journalctl -u teslatlas-hub.service -n 50 --no-pager
sudo -u teslatlas -- /usr/bin/teslatlas-hub \
  --config /etc/teslatlas-hub/recovery-config.toml status
```

Pair devices again and prove a fresh observation before declaring recovery
complete.

On macOS, choose **Stop Hub** in the app before running backup and recovery
commands. Run those commands as the signed-in user with the app-embedded binary
and per-user configuration paths documented in
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
