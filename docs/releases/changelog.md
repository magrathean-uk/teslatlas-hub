# Changelog

All notable released changes are recorded here. From 2026.36.1, Hub follows
[calendar versioning](versioning.md): `YEAR.WEEK.REVISION`.

## Unreleased

- Retired Edge delivery. A `[collector.edge]` table now fails with a clear error; remove
  it and use the receiver in `[collector.fleet_telemetry]`. `status` no longer reports
  `edgeDelivery` and `doctor` no longer reports an `edge` collector block. The source-run
  `edge` development mode and the `edge-test-faults` feature are gone.
- The receiver route is stricter and keeps more: uppercase-only VINs, exact envelope keys,
  alert and error events kept (and kept through later pruning) without touching current
  state, accumulator state kept across restarts, connection boundaries enforced, and the energy counter filled only by
  `DCChargingEnergyIn`. Unchanged vehicle identities cause no registration writes.
- The receiver orders and de-duplicates the observations it stores by the newest
  event time its accumulator has seen, not by the drive timestamp or the Hub's receipt
  time (a coordinator decision, G2-6 6A fix round 1; it keeps a delayed drive record
  that arrives after a newer charge record). Records stored before this change used the
  older key, so on an upgraded Hub the first records after the upgrade may be skipped
  until vehicle time passes the last receipt-time key already stored.
- A Hub that serves the receiver route refuses `offline_drive_timeout_seconds = 0` with
  a startup error instead of answering every vehicle and connectivity POST with 503.
- The collector-less publication task counts and logs one vehicle's failed store read
  once (with the Hub vehicle identifier) and goes on to the next vehicle; an error of the
  whole pass is logged once per back-off step. A v74 migration step that fails for any
  reason but the clock reports a migration error and rolls back.
- Store schema 73 adds the accumulator-state table; schema 74 copies the retired Edge
  loss evidence into `retired_delivery_gaps` and drops the six Edge tables. Older
  binaries refuse a store at these versions.
- Derived publication of pending work no longer waits for a collector: a Hub without an
  eligible collector publishes it when it starts serving and keeps checking while it
  serves, one bounded step at a time.
- Separated service readiness from collection attention. `/readyz` keeps its published
  bodies and six reasons; status, doctor and the Mac app show the one condition you can
  fix, including a usable sign-in that must be renewed, an unknown Fleet token refresh and
  a broken collector cadence.
- Loading a configuration no longer refuses a collector cadence that breaks a rule. An
  otherwise eligible Serve waits, holding its lease and collecting nothing, until the
  setting is corrected and the service restarted; a bounded `observe` refuses at once.
- Collector defaults changed: driving reads every 10 s (was 2.5 s), charging every 30 s
  (was 5 s), the vehicle list every 300 s (was 60 s), state checks of a sleeping car every
  30 s (was 21 min), updating every 120 s (was 15 s), request timeout 15 s (was 20 s),
  longest back-off 300 s (was 900 s). An owner relying on the defaults collects at a
  different density.
- Fleet polling now follows `[collector.fleet_polling]` (30, 120, 300 and 120 s) with a
  default monthly cap of 1,000 cents, which stops collection around day 14 of the month.
- A Legacy token without a readable issuer needs `stream_region`; a non-Tesla
  `owner_api_base_url` is Provider mode and needs `[collector.legacy_auth] token_url`.
  Otherwise the collector sends nothing (`collector_auth_configuration_error`).
- New commands `collector retry`, `collector resume <vehicle UUID>` and
  `collector gaps acknowledge`.
- A `max_backoff_seconds` at or above `offline_drive_timeout_seconds` (the previous
  default of 900, the old guide's example, or this release's default of 300 under a lower
  timeout) is moved to one second below it, with one `config_value_clamped` notice, for
  this release. The other limits now enforced
  are errors: `driving_poll_milliseconds` below 1000, `request_timeout_seconds` above 120,
  charging intervals of 600 s or more, and session intervals at or above the offline
  timeout.
- A Hub restored from a backup older than its last token renewal must sign in again and
  shows `collector_auth_terminal` until it does.
- Readers use the loaded configuration, and the sign-in state in force now (new
  credentials, a refusal's mode and the token's usability), without writing anything.
- Documented the restore of Fleet request counts and the `store_unavailable` sign-in
  reason.
- A start that changes the mode, the provider or credential identity lifts a standing
  refusal in one transaction, Fleet included, and a refusal does not come back when you
  return to the earlier mode; a waiting cadence fault lifts nothing. A resident vehicle
  action is refused while a refusal for the configured mode stands. A Fleet token refresh
  with an unknown outcome no longer stops the service from starting: it runs as a stopped
  collector until the credentials are reconnected.
- Removed the `write-back charge-cost` command. Hub never writes to the source
  database; migration stays read-only.

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
