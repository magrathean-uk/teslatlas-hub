# Teslatlas Hub v1.0.0-beta.1

Released 2026-08-30.

This is the first public beta of the independent, self-hosted telemetry
collector and local Teslatlas sync hub.

## Highlights

- native Apple-silicon macOS app and service;
- Debian 13 packages for amd64 and ARM64;
- multi-vehicle legacy and Fleet API operation;
- legacy driving stream and native Fleet Telemetry push;
- explicit signed vehicle commands in Fleet mode;
- optional read-only TeslaMate 4.1.1 history and credential migration;
- integrity diagnostics, repair, backups, restore, and encrypted credential
  recovery;
- exact Fleet Telemetry upstream and all 45 locked Go runtime-module source
  archives in the detailed release evidence;
- bounded redacted logs and explicit stopped-state lifecycle.

## Upgrade from alpha.1

Treat this as a new beta installation. Back up alpha data and credentials,
retain the alpha artifact and source, and follow the beta install or migration
guide. Do not assume an alpha binary understands the beta schema after forward
migration.

## Known limits

- beta interfaces and formats may change;
- macOS requires Apple silicon and macOS 13 or later;
- Linux support is Debian 13 amd64/ARM64;
- Fleet Telemetry is a selected push policy, not a full `vehicle_data` mirror;
- signal loss at the vehicle can create genuine live-route gaps;
- TeslaFi import, Grafana, MQTT, and App Store delivery are not included.

## Safety

Back up and test recovery. Keep plaintext HTTP on loopback. Run only one owner
of a legacy refresh-token pair. Do not use Hub for safety-critical or autonomous
decisions. Vehicle commands affect physical property and require deliberate
confirmation.

## Legal

Licensed under GNU AGPL version 3 only (`AGPL-3.0-only`) with the permitted
section 7 notices in `ADDITIONAL_TERMS.md`. The project is unofficial and is
not affiliated with, endorsed by, or supported by Tesla, Inc. or TeslaMate.

The GitHub prerelease is complete only when it contains the signed tag,
platform artifacts, exact source, checksums and signature, SPDX SBOM,
dependency notices, provenance, and notarisation evidence described in
`RELEASE_VERIFICATION.md`. If any item is absent, do not install it. The Fleet
notice embedded in the platform packages names
`fleet-telemetry-go-module-sources.tar.gz`; that exact archive is supplied in
the detailed release evidence and contains the source ZIP plus `go.mod` for all
45 locked runtime modules, including Eclipse Paho under EPL-2.0. This
platform-invariant source/legal corpus is not a native Linux reproducibility
receipt. The detailed evidence separately includes `linux-amd64` and
`linux-arm64` Go command-proxy rebuild evidence, Fleet subject/source/legal
evidence, and signed native Debian package attestations; release verification
requires all of them.

The version-bound release page is the Corresponding Source landing page. The
complete source offer is the pair
`teslatlas-hub-v1.0.0-beta.1-source.tar.gz` (tagged workspace) and
`teslatlas-hub-v1.0.0-beta.1-evidence.tar.gz` (locked Rust sources, Go/Fleet
sources and overlays, build inputs, inventories, and source manifests). Neither
asset alone is described as complete Corresponding Source.
