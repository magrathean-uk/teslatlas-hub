# Current status

Status snapshot: 2026-07-29.

Teslatlas Hub is a development prototype. It is not yet a usable TeslaMate
replacement and it is not ready for public installation. Overall progress
toward the requested token-first, native, iPhone-ready v0.1 is approximately
45%.

This file records proof. [Roadmap](ROADMAP.md) records execution order.

## Development host policy

- **Mac-first** for product slices (Hub code, collector, migration, iOS
  Simulator/device). Day-to-day work does not compile the release under
  full-system QEMU/TCG on Apple Silicon.
- **Debian packaging and install matrix are final (roadmap Slice 4)** after
  Mac product gates for slices 0–3. Colima/Docker may build packages; the Hub
  runtime itself remains native Debian without Docker.
- Production VPS remains untouched.

## Status rules

- **Implemented** means source exists in the isolated Hub or iOS worktree.
- **Verified** means a relevant command completed successfully.
- **Complete** means the end-to-end user outcome passed, not just compilation.
- A passing unit test, package build, or `build-for-testing` result does not
  count as live Tesla, runtime iOS, physical-device, arm64 install, or
  public-release proof.

## What is implemented and verified

### Native Hub foundation

- Rust 2024 service with bundled SQLite, WAL storage, immutable zstd SQLite
  packs, SHA-256 content addressing, ETags, strict byte ranges, signed
  manifests, one-use pairing, and per-device bearer authentication.
- Bounded raw observations and typed `cars`, `drives`, `positions`, `charges`,
  and `charge_samples` pack contracts.
- Local Rust proof (Mac): 98 tests passed (96 library and 2 CLI tests);
  formatting and warning-denying Clippy passed.
- Hub baseline is committed on `main` (reproducible clean-checkout gates
  recorded). Release signing provenance for public install is still open.

### Token-first compatibility seam

- Host-encrypted owner-token ingestion through systemd credentials.
- GET-only vehicle discovery and present-state reads. No wake or command
  endpoint exists.
- Bounded raw persistence and authenticated snapshot publication.
- Fake-source tests cover token isolation, redirect refusal, response bounds,
  per-vehicle failure isolation, and deduplication.
- Supervised opt-in collector loop (`collect-supervised`) with backoff.
- Durable lifecycle materialisation for drives, positions, charges, and charge
  samples with crash-safe open-session recovery (synthetic Mac proof).

Optional live owner-token collection on a disposable host remains open;
production tokens are forbidden.

### TeslaMate history migration

- Read-only TLS PostgreSQL source, separate encrypted password credential,
  schema probe, repeatable-read capture, keyset paging, private bounded stage,
  parent-complete fragments, and signed full-snapshot publication.
- Unit and integration-style Rust tests cover the staged import and pack
  contract.

It still needs proof against a disposable real TeslaMate database containing
representative large history. The production VPS remains out of scope.

### Teslatlas iPhone source

- Isolated iOS worktree branch commits Hub pairing, TLS pin, signed-manifest
  verification with **selected-vehicle binding**, vehicle selection,
  Keychain-backed profile, Range resume with **zero-byte/broken partial repair**,
  bounded pack cache, Rust staging, **failed-stage cleanup**, atomic activation,
  and overnight Hub refresh **without requiring SyncManager**.
- Runtime XCTest on Simulator (iPhone 17, iOS 27) for Hub suites: vehicle
  mismatch, partial resume, stage cleanup, protocol, and Rust bridge reports.
- Remaining: full Hub→Simulator TLS import with cut/resume and injected
  failures; physical iPhone onboarding/refresh; source-switch recovery.

### Native delivery

- Debian package layout, hardened systemd units, credential-safe installer,
  exact-commit native Git bootstrap (including `--dry-run` contract tests),
  detached Minisign release-manifest verifier, health verifier, and
  data-preserving removal policy exist.
- Colima arm64 package construction from current committed source produced
  `teslatlas-hub_0.1.0_arm64.deb` (build-host proof only).

Install matrix (clean install, reboot, upgrade, rollback, token rotation) on
amd64 and arm64 Debian, signed public release, and stable public bootstrap URL
remain **Slice 4 / final** and are not required while Mac product slices are
open.

### Fleet

- OAuth, credential, scope, revocation, and Fleet Telemetry boundaries are
  documented from Tesla's official requirements.
- Fleet implementation and proof are 0%.

## Production boundary

- Production VPS writes: none.
- Production VPS deployment: none.
- Production token extraction: none.
- The VPS must remain untouched until the user explicitly authorizes a later
  production deployment.

## Required v0.1 outcome

v0.1 is complete only when all of these pass:

1. A committed, reproducible Hub baseline and a committed Teslatlas integration
   branch exist.
2. One command installs a signed amd64 or arm64 package on clean Debian without
   Docker.
3. Token onboarding succeeds without exposing the token in argv, environment,
   configuration, logs, or plaintext temporary files.
4. The native collector records real vehicle, drive, position, charge, and
   charge-sample history with crash-safe lifecycle recovery and no wake calls.
5. A new Teslatlas install pairs, selects a vehicle, downloads or resumes data,
   verifies it, and atomically activates the local mirror.
6. Refresh transfers only necessary data or proves that the bounded
   full-snapshot fallback meets the declared time, memory, disk, and bandwidth
   ceilings.
7. Runtime Simulator tests, physical-iPhone proof, amd64 proof, arm64/Raspberry
   Pi proof, clean install, upgrade, interruption, rollback, and credential
   rotation tests pass.
8. No production VPS access is used during development proof.
