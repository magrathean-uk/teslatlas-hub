# Remaining and runtime-only risks

This ledger is cumulative. Entries may be removed only by a later review finding with exact code/test/runtime evidence; otherwise they remain merge gates or accepted limitations.

## Open P0/P1 risks after S1

None identified in S1 after applying HUB-001. This is not a whole-repository conclusion; migration, collector, storage, protocol, security, AppKit and packaging stages remain under review.

## S1 runtime and implementation risks

### S1-R01 — exact-head execution absent

Severity: P1 merge gate

Confidence: confirmed

The S1 patches have source/diff evidence only. No formatter, compiler, Clippy, unit test, macOS process-admission test, Linux target check or runtime lifecycle test has executed for the review head. The exact commands and acceptance criteria are in `04-VALIDATION.md`.

### S1-R02 — Linux service lifecycle is not implemented

Severity: P2

Confidence: confirmed

The cross-platform CLI still exposes `serve`, but the review branch now fails it explicitly outside macOS. A production Linux service needs a retained Unix instance authority, SIGTERM/Ctrl-C ownership, non-root data/secrets layout, systemd readiness/watchdog policy, logging/rotation, TLS trust handling and package/update/rollback design. Removing the fail-closed gate before those controls exist would reintroduce HUB-002.

### S1-R03 — host CPU profiling is not evidenced in the migration call path

Severity: P2

Confidence: strong inference

`src/performance_profile.rs` can derive a CPU/data-directory profile and only lower COPY lanes. The reviewed macOS migration path calls `config.teslamate.read_limits()` directly. Repository code search found the derivation function's definition/export but no production call. The S1 fix makes the configured maximum safe and honours `enabled`; it does not claim that automatic CPU discovery is active. Re-check in S2/S8 and either wire the bounded derivation with tests or narrow the configuration/documentation claim.

### S1-R04 — system sleep/logout/reboot behaviour remains runtime-only

Severity: P2

Confidence: unverified

The macOS supervisor has source-level ownership for collector/server tasks and SIGTERM/Ctrl-C tests, but this session cannot prove behaviour during machine sleep/wake, user logout, LaunchAgent unload, abrupt reboot, clock movement or network interface replacement. Validate against a disposable migrated store and verify restart continuity plus a newer durable observation.

### S1-R05 — repository enforcement is external and currently absent

Severity: P2

Confidence: confirmed

HUB-004 remains blocked: no committed workflow supplied exact-head checks at baseline and `main` was reported unprotected. Do not treat the review documents or draft state as a substitute for protected required checks and owner review.

### S1-R06 — failed pairing presentation can leave an active invitation

Severity: P3

Confidence: strong inference

The CLI creates the hashed pairing invitation before reading/hashing the configured leaf certificate and rendering the QR. A certificate-read/PEM/QR failure can therefore leave an invitation row until expiry. The raw one-time secret exists only in the failed process, so this is not an unauthorised-claim path from the reviewed code. Consider validating the certificate and QR payload prerequisites before committing the invitation, or add an explicit revocation-on-presentation-failure path, during S6.
