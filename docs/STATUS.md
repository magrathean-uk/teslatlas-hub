# Current status

Status snapshot: 2026-07-29.

Teslatlas Hub is a development prototype. It is not yet a usable TeslaMate
replacement and it is not ready for public installation. Overall progress
toward the requested token-first, native, iPhone-ready v0.1 is approximately
30%.

This file records proof. [Roadmap](ROADMAP.md) records execution order.

## Status rules

- **Implemented** means source exists in the isolated Hub or iOS worktree.
- **Verified** means a relevant command completed successfully.
- **Complete** means the end-to-end user outcome passed, not just compilation.
- A passing unit test, package build, or `build-for-testing` result does not
  count as live Tesla, runtime iOS, physical-device, arm64, or public-release
  proof.

## What is implemented and verified

### Native Hub foundation

- Rust 2024 service with bundled SQLite, WAL storage, immutable zstd SQLite
  packs, SHA-256 content addressing, ETags, strict byte ranges, signed
  manifests, one-use pairing, and per-device bearer authentication.
- Bounded raw observations and typed `cars`, `drives`, `positions`, `charges`,
  and `charge_samples` pack contracts.
- Local Rust proof: 92 total tests passed (90 library and 2 CLI tests);
  formatting and
  warning-denying Clippy passed after resolving the newest Rust 1.97-compatible
  dependency lock.
- The Hub source tree is still an uncommitted baseline. It has no release
  provenance yet.

### Token-first compatibility seam

- Host-encrypted owner-token ingestion through systemd credentials.
- GET-only vehicle discovery and present-state reads. No wake or command
  endpoint exists.
- Bounded raw persistence and authenticated car-only snapshot publication.
- Fake-source tests cover token isolation, redirect refusal, response bounds,
  per-vehicle failure isolation, deduplication, and car-only publication.

This is not a replacement collector yet. It has no continuous scheduler,
durable drive/charge lifecycle normalizer, live-token proof, or completed-trip
history.

### TeslaMate history migration

- Read-only TLS PostgreSQL source, separate encrypted password credential,
  schema probe, repeatable-read capture, keyset paging, private bounded stage,
  parent-complete fragments, and signed full-snapshot publication.
- Unit and integration-style Rust tests cover the staged import and pack
  contract.

It still needs proof against a disposable real TeslaMate database containing
representative large history. The production VPS remains out of scope.

### Teslatlas iPhone source

- Isolated iOS worktree contains Hub pairing, TLS certificate pinning,
  manifest signature verification, vehicle selection, Keychain-backed source
  profile, strict Range resume, bounded pack cache, Rust pack staging, receipt
  validation, atomic full-snapshot activation, startup restore, foreground
  import, and manual refresh wiring.
- Focused Hub targets previously passed `build-for-testing`.
- Rust FFI contract tests previously passed.

The iOS work is uncommitted and not merged into Teslatlas. Its nightly
background path is currently blocked because the Hub source has no sync manager.
The client also needs an exact selected-vehicle check on the signed manifest,
repair of a zero-byte interrupted partial retry, and complete failed-stage
cleanup. Runtime XCTest, end-to-end Hub-to-Simulator transfer, physical-iPhone
proof, source-switch recovery, and release-framework provenance are still open.

### Native delivery

- Debian package layout, hardened systemd units, credential-safe installer,
  exact-commit native Git bootstrap, detached Minisign release-manifest
  verifier, health verifier, and data-preserving removal policy exist.
- An earlier amd64 Debian 12 bench install reached `install ok installed`,
  active systemd service, and a passing native verifier.

The local VM is currently off, so current-source package proof must be repeated.
No arm64/Raspberry Pi package proof, signed public release, embedded project
trust key, stable public bootstrap URL, upgrade/rollback matrix, or remote TLS
deployment proof exists. The delivery documentation also attributes
`--dry-run` to the Git bootstrap even though only the installer implements that
flag; the contract and its tests must be aligned.

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
