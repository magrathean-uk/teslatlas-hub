# Upgrade and rollback

Build your own replacement package using [Build from source](../guides/build-from-source.md).
No prebuilt GitHub releases are provided. Retain your previous local package
and its source commit for recovery.

The current `2026.36.2` Debian core package contains core/Legacy collection
only. It omits Fleet companions. Do not replace an existing Fleet deployment
with a core-only package without separately verified compatible companions.
The retained `2026.36.1-1` ARM64 package is a real predecessor for the current
candidate; keep its exact package, source commit and matching data backup if
you need that recovery route.

The last historical macOS package remains
`TeslatlasHub-2026.36.1-arm64.pkg`; this Debian candidate does not establish a
new Mac package. On macOS, use the matching combined package for upgrades.
The app is ad-hoc signed and the installer is unsigned and unnotarised.
In-app installation, reinstallation, and update of the embedded service are
unavailable because this build lacks the required official-release metadata
and Gatekeeper trust.

## Prepare

1. Record the installed version, platform, configuration location, and current
   service status. Save the previous installer or package.
2. Create a data backup and retain separately recoverable credentials using
   [Backup and recovery](../operations/backup-and-recovery.md). Keep these
   private. A data-only restore does not restore collector authority, TLS,
   configuration or service state; retain and rehearse those inputs separately.
3. Inspect your new package and retain its checksum, source commit, toolchain
   versions and completed build/test results.
4. Stop the existing Hub service through the Mac app or
   `sudo systemctl stop teslatlas-hub.service` on Debian. Prevent simultaneous
   collectors from owning the same refresh credentials.

## Install and check

Install the matching package following the [macOS](../guides/install-macos.md)
or [Debian](../guides/install-debian.md) guide. Preserve existing data and
configuration. For the current Debian candidate, confirm the installed binary
reports `teslatlas-hub 2026.36.2`; use the version matching the selected package
on other platforms. Run `doctor` and `status` against the existing
configuration, and inspect logs.
Start collection only after configuration and diagnostics are usable. On Mac,
confirm the app reports Running and Ready; on Debian inspect the systemd unit.
These local checks do not by themselves prove fresh vehicle data or recovery.

## Recover from a failed upgrade

Stop the failed service and preserve its diagnostics privately. Calendar
versions make no database, API, or sync compatibility promise. Never start an
older binary against a data directory already migrated by a newer version.
Verify the previous backup, restore it into a new private directory, restore
its separately exported recovery credentials when applicable, then select that
restored directory while the service remains stopped before installing the
matching previous package. The exact synthetic Debian ARM64
`2026.36.1-1 -> 2026.36.2-1` journey was rolled back this way; it is not a
general cross-version downgrade promise. Never let old and new instances
refresh the same credentials concurrently.

Migration's bounded readiness rollback covers its service transition. It does
not replace a separately verified backup-and-restore plan or establish general
cross-version downgrade compatibility.
