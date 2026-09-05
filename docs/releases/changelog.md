# Changelog

All notable released changes are recorded here. From 2026.36.1, Hub follows
[calendar versioning](versioning.md): `YEAR.WEEK.REVISION`.

## 2026.36.1 — 2026-09-05

- Redesigned the macOS AppKit interface, native windows, keyboard navigation,
  migration SSH selection, and animations.
- Hardened subprocess pipe cancellation and streaming outage retries while
  preserving vehicle sleep state.
- Prevented macOS supervisor restart storms for missing or unsafe configuration.
- Bounded migration completion with fresh collector readiness and rollback.
- Adopted calendar versions across source, app, and packages.
- Prepared an Apple-silicon macOS installer and Debian 13 ARM64/amd64 core-only
  packages. Debian packages omit Fleet companions; no live Tesla or restored
  backup acceptance is claimed. See [release notes](release-notes-2026.36.1.md)
  and the published build record for distribution and verification details.

## 1.0.0 — 2026-08-31

First stable source release. The immutable source boundary is the annotated
`v1.0.0` tag. No GitHub Release or release assets are published.

### Added

- a determinate progress flow for read-only TeslaMate 4.2.0+ migration;
- a combined macOS product installer containing the app and service;
- import phase diagnostics and bounded service-transition diagnostics.

### Changed

- requires a trusted OpenSSH known-host entry before guided TeslaMate migration
  sends SSH authentication or reads TeslaMate database credentials;
- requires TeslaMate 4.2.0 or newer for guided migration;
- substantially reduces repeated serialization and database lookups during
  large imports and repeat imports;
- preserves existing Hub credentials during repeat imports;
- installs the macOS app and service together from `TeslatlasHub.pkg`;
- retains the existing `v1.0.0-beta.1` tag as history.

### Fixed

- service start, stop, restart, dashboard refresh, and import completion races;
- false dashboard attention state while the service is starting;
- shutdown ownership for streaming, listeners, tunnels, and companion tasks;
- macOS application-data deletion on systems without `/usr/bin/test`.

## 1.0.0-beta.1 — 2026-08-30

Historical beta source tag.

### Added

- multi-vehicle collection, state, sync, and explicit vehicle selection;
- Fleet API onboarding, token rotation, Fleet Telemetry push, and signed
  command-proxy integration;
- Debian 13 packages and hardened systemd units for amd64 and ARM64;
- guided read-only TeslaMate 4.1.1 migration and bounded charge-cost write-back;
- data-only backup, immutable verification, restore, and separately encrypted
  credential recovery;
- native macOS diagnostics, bounded redacted logs, service controls, and
  uninstall flow;
- release evidence, SBOM, dependency notices, provenance, packaging, and
  exact-version admission gates;
- a macOS release-key vault helper for a detached AES-256 encrypted APFS vault.

### Changed

- macOS minimum version is 13 and the release is Apple-silicon only;
- collection and service shutdown now cancel streaming, listeners, active
  connections, and supervised companions with a bounded grace period;
- Debian Hub and companion units now recover through direct bounded restarts,
  stop every companion after restart exhaustion, and still permit standalone
  command-proxy enrolment while Hub is stopped;
- terrain and geocoding are disabled by default; geocoding has no implicit
  public provider and requires an operator-selected HTTPS endpoint;
- Hub is a complete new-install collector and no longer depends on TeslaMate at
  runtime;
- licence expression is `AGPL-3.0-only` for this release line.

### Beta limits

- storage, sync, authentication, and operational interfaces may change before
  v1.0.0;
- Fleet Telemetry contains selected pushed fields and is not identical to a
  complete `vehicle_data` response;
- TeslaFi import, Grafana, MQTT, App Store distribution, Intel macOS, and Linux
  distributions other than Debian 13 are not included.

## 1.0.0-alpha.1 — 2026-08-16

- Apple-silicon macOS alpha with one vehicle, legacy credentials, Owner API
  polling, Tesla streaming, local persistence, AppKit control, and read-only
  TeslaMate history import.
- No Fleet API, Fleet Telemetry, Linux package, or multi-vehicle operation.
