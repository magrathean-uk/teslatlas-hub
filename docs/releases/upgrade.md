# Upgrade and rollback

Build your own replacement package using [Build from source](../guides/build-from-source.md).
No prebuilt GitHub releases are provided. Retain your previous local package
and its source commit for recovery.

For 2026.36.1, Debian packages contain core/legacy collection only. They omit
Fleet companions. Do not replace an existing Fleet deployment with these
packages without separately verified compatible companions. Read the
[release limits](release-notes-2026.36.1.md) first.

On macOS, use the combined `TeslatlasHub-2026.36.1-arm64.pkg` for upgrades.
The app is ad-hoc signed and the installer is unsigned and unnotarised.
In-app installation, reinstallation, and update of the embedded service are
unavailable because this build lacks the required official-release metadata
and Gatekeeper trust.

## Prepare

1. Record the installed version, platform, configuration location, and current
   service status. Save the previous installer or package.
2. Create a data backup and retain separately recoverable credentials using
   [Backup and recovery](../operations/backup-and-recovery.md). Keep these
   private. This release does not claim a completed restored-backup rehearsal.
3. Inspect your new package and retain its checksum, source commit, toolchain
   versions and completed build/test results.
4. Stop the existing Hub service through the Mac app or
   `sudo systemctl stop teslatlas-hub.service` on Debian. Prevent simultaneous
   collectors from owning the same refresh credentials.

## Install and check

Install the matching package following the [macOS](../guides/install-macos.md)
or [Debian](../guides/install-debian.md) guide. Preserve existing data and
configuration. Confirm the installed binary reports `teslatlas-hub 2026.36.1`,
run `doctor` and `status` against the existing configuration, and inspect logs.
Start collection only after configuration and diagnostics are usable. On Mac,
confirm the app reports Running and Ready; on Debian inspect the systemd unit.
These local checks do not by themselves prove fresh vehicle data or recovery.

## Recover from a failed upgrade

Stop the failed service and preserve its diagnostics privately. Calendar
versions make no database, API, or sync compatibility promise, and an in-place
downgrade is not a verified recovery method. Recover the pre-upgrade data and
credentials with the matching previous software using the backup guide.
Never let old and new instances refresh the same credentials concurrently.

Migration's bounded readiness rollback covers its service transition. It does
not replace a separately verified backup-and-restore plan or establish general
cross-version downgrade compatibility.
