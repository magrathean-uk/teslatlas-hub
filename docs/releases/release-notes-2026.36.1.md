# Teslatlas Hub 2026.36.1

The first Hub calendar release uses `YEAR.WEEK.REVISION`, with source tag
`v2026.36.1`. Historical `v1.0.0` and prerelease tags remain unchanged.

## Distribution and limits

- macOS: Apple silicon, macOS 13 or later. The combined app/service installer
  is `TeslatlasHub-2026.36.1-arm64.pkg`. The app is ad-hoc signed; the combined
  installer is unsigned and unnotarised. Use the combined installer for both
  installation and upgrades. In-app service installation, reinstallation, and
  update are unavailable in this distribution because the embedded installer
  requires official-release metadata and Gatekeeper trust.
- Debian 13: `teslatlas-hub_2026.36.1_arm64.deb` and
  `teslatlas-hub_2026.36.1_amd64.deb` contain the Hub core for legacy collection.
  They do **not** include the Fleet command proxy or Fleet Telemetry receiver.
  Do not use these packages as a complete Fleet installation or to replace an
  existing Fleet deployment without a separately verified companion plan.
- The macOS distribution reuses unchanged Fleet companions with their verified
  dependency evidence and legal material. Linux companion admission did not
  satisfy the strict Go evidence host gate; this is why those helpers are absent.
- No live Tesla acceptance or restored-backup acceptance is claimed for this
  release. Automated tests and package inspection do not establish either.

The published `SHA256SUMS` and sanitised `BUILD-INFO.md` identify the exact
artifact bytes, source commit, build environment, and completed verification.
Use those records with [Verify a release](verification.md); a document or
version number alone is not evidence that publication or a test completed.

## Changes

- Redesigned the macOS AppKit interface, including native window behaviour,
  keyboard navigation, SSH selection during migration, and animation handling.
- Made subprocess pipe cancellation safer during app and service operations.
- Improved streaming outage retry while preserving vehicle sleep state.
- Made the macOS service supervisor exit cleanly for missing or unsafe
  configuration, preventing repeated restarts before setup is usable.
- Bounded migration completion around a fresh collector readiness check, with
  rollback when the transition cannot safely complete.
- Aligned product and package versions with the new calendar version policy.

## Install or upgrade

Verify the package matching your platform before installation. Follow
[macOS installation](../guides/install-macos.md),
[Debian installation](../guides/install-debian.md), or the
[upgrade and rollback guide](upgrade.md).

Back up data and recoverable credentials before an upgrade. Database, API,
and sync compatibility are not guaranteed by calendar numbering. An in-place
downgrade is not an established recovery path; retain the previous binary,
configuration, and a pre-upgrade backup. Guided TeslaMate migration requires
TeslaMate 4.2.0 or newer and exclusive token-refresh ownership at cutover.

## Verification scope

Fresh macOS verification completed with 931 Rust tests passed and 2 ignored,
and 221 AppKit tests passed with no failures. Read the release's `BUILD-INFO.md`
for the full completed-check record and exact artifact/source association.
Earlier hardening runs are not substituted for current release verification.
No claim is made here that a physical
vehicle, production deployment, or backup restoration was accepted.
