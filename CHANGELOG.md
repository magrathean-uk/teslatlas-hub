# Changelog

All notable released changes and current release-candidate changes are recorded
here. The project follows Semantic Versioning for release identifiers; beta
interfaces may still change.

## Unreleased — v1.0.0-beta.1 candidate

Prepared as the first public beta. It is not released until the signed tag and
the complete prerelease evidence set are published atomically.

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
- release tags and checksum manifests use the protected Ed25519 OpenPGP
  identity of György Bolyki;
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
- No Fleet API, Fleet Telemetry, Linux package, multi-vehicle operation, or
  signed/notarised public installer.
