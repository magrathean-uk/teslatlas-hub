# Backup and recovery

Hub separates ordinary data backup from credential disaster recovery.

## Create and verify a data backup

The following commands are for a Debian package installation. Create a private
destination folder owned by the service account; each `backup` run adds one
encrypted file, `teslatlas-hub-backup-<time>-<generation>.tlbk`, to it. On a
volume that enforces ownership, `backup` refuses a folder that other accounts
can read, list or enter (any group or other mode bit, or an access control
list entry that allows anything; use mode 0700), and it refuses a destination
that is a symbolic link: name the folder itself. The checks apply to the folder
actually opened, so renaming a parent folder during the run cannot redirect it.
Scheduled backups and `backup-now` keep their own rule: no other account may
write to the folder.

Stop the Hub service first: backup and credential export take the exclusive Hub
instance lock and will refuse to run while Serve owns it. While the Hub runs,
`backup-now` makes the same encrypted backup through the service instead.

Backups are encrypted by default with a recovery password you keep (at least
12 characters, at most 1 KiB). The password is never a command argument or an
environment variable. On a terminal, `backup` asks for it twice with echo off,
and `verify-backup` and `restore-data` ask once. Delete or Backspace, Ctrl-W
and Ctrl-U edit the line; Ctrl-C (also Ctrl-\, Ctrl-Z, Ctrl-Y or Ctrl-D)
cancels the prompt. A line longer than 1 KiB is refused whole, never shortened.
The terminal settings are restored when the prompt ends, is cancelled or the
command is stopped by SIGINT, SIGTERM, SIGHUP or SIGQUIT; if they cannot be
restored the command says so, and `stty sane` restores them.

When standard input is not a terminal, the commands read the password from it
instead, for example from a protected password file:

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

Standard input is read in one of two modes:

- One line (the default): the first line is the password. It ends at the first
  line end (a CR before it is dropped) or at the end of the input, and the
  command continues as soon as that line end arrives, even if the sender keeps
  the pipe open. Input that arrives together with the line, after its end, is
  refused rather than ignored. An empty line, or a CR or NUL inside the line,
  is refused.
- A password file: when the first line is exactly `#pw-file`, everything after
  it up to the end of the input is read as the content of a backup password
  file, by the rule scheduled backups apply to their `password_file`: at most
  1 KiB, one trailing line end ignored, and any line ends or NUL inside kept.
  Use this mode for a password that scheduled backups accepted but one line
  cannot carry. The command waits for the end of the input, so close it after
  the file:

```sh
{ printf '#pw-file\n'; sudo -u teslatlas cat /etc/teslatlas-hub/backup-password; } | \
  sudo -u teslatlas -- /usr/bin/teslatlas-hub verify-backup \
  --source /srv/teslatlas-hub-backups/teslatlas-hub-backup-<time>-<generation>.tlbk
```

`verify-backup` authenticates and reads the whole bundle without writing
anything. A wrong password is refused.

Before it writes anything, `backup` checks that the destination can hold the
whole bundle, including each file's record and the encryption overhead, and
that the Hub's data folder can hold the run's short-lived pack records. If the
volume still fills during the run (another program took the space), the run
fails with an error, removes its unfinished file and leaves the earlier
generations and the Hub's data as they were. A run
reports success only after it has read the published file back in full: on a
volume that enforces ownership it must be the same file the run wrote,
unchanged; on a network share or a volume that ignores ownership the file under
the final name is read again after that and must match, byte for byte, what
the run wrote. Otherwise the run fails with `invalid_publication`. Stopping a
run (for example with the service) also stops that read-back, and a stopped run
removes no older generation. When rotation keeps fewer generations, each older
one is removed only after the newest and every other kept one are checked again
just before the removal; if one of them has changed, nothing more is removed
and the error says how many older generations had already gone.

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
encrypted default. This format has no file limit either: up to 4,096 files the
directory's manifest (`backup-v4.json`) lists every file, as earlier builds
expect; above that the manifest (`backup-v5.json`) names only the catalogue and
records how many files and bytes the directory holds, and `verify-backup` and
`restore-data` check every pack (named by its own digest), every no-op body and
the totals by walking the directory. Earlier builds cannot read a
`backup-v5.json` directory.

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
otherwise. History rows and packs are unchanged. The physical (schema 2.2)
snapshots are re-signed the same way; because a snapshot's receipt ID is
derived from its signed manifest, each current and retained snapshot gets a
new receipt ID, while its snapshot ID, sequence and packs stay the same. The
stored changed-set (delta) answers name the old receipt IDs and are dropped: a
request from a retained snapshot gets the signed full replacement instead. The
prepared map month of each current snapshot is issued again under the new key,
from its own rows, and is available as soon as a device has paired again; a
prepared month of an older snapshot is dropped, as it is never served. This
happens in the private staging directory before the restored directory
appears, so an interrupted restore leaves no destination behind: run it again.

The folder that will hold the restored directory must belong to the Hub's user
and must not be writable by other accounts. The restore stages its work in a
locked `.teslatlas-encrypted-restore-<id>.staging` folder there and writes
every file through folders it holds open, so renaming that folder or a parent
during the restore cannot redirect a file; if either moves, the restore stops,
removes what it wrote and publishes nothing. Before writing, the restore checks that the folder's volume can hold
every file with its allocation and metadata overhead, and before each file that
the rest still fits with a reserve to spare. If the volume still fills while
the files are written, the restore fails with an error, removes what it wrote
and publishes nothing; the backup is unchanged. Each restore first removes what an
interrupted restore left there (also `.teslatlas-data-recovery-<id>.staging`
folders from earlier builds and directory-format restores), but never one that
a running restore or backup holds locked. If such a folder cannot be listed,
inspected or removed, the restore stops before it reads the bundle and names
the folder: remove it, or fix its permissions, and run the restore again. A backup without any signed
sync state gets its key when the restored Hub first starts.

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
