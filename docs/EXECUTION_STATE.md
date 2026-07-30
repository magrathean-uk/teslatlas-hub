# Teslatlas Hub execution state

## Current phase

Scope reset in progress on 2026-07-30. The recommended v0.1 is the single
TeslaMate PostgreSQL to signed Hub packs to Teslatlas bridge described in
`docs/V0_1_DEVELOPMENT_PLAN.md`.

Simulator is now the primary proof environment for the complete happy path,
real-history import, resume, corruption, cancellation, retry, and restart
matrix. Physical iPhone work is limited to one final camera, Wi-Fi/LAN TLS,
representative import, and relaunch smoke test.

The previous objective combined history migration, owner-token collection,
delta synchronization, iPhone integration, and multi-architecture release
engineering. Those remain long-term roadmap work, but they no longer share the
first executable gate.

A Debian amd64 VM is currently running package `0.1.0-vm7`. Hub is healthy and
the real copied TeslaMate history import is still running as a background
measurement. It does not block the scope reset.

## Verified existing work

- Hub `main` at `f4093ed` has uncommitted in-scope work. Rust suite has 100 passing tests; format,
  Clippy, bootstrap contract, and TLS end-to-end checks pass.
- Token credential adapter, supervised no-wake collection, pairing, signed
  packs, range resume, restart recovery, quarantine, repair, native TLS,
  bundled SQLite, and hardened systemd unit foundations exist.
- Teslatlas `main` is clean and aligned with `origin/main` at `a2499f10`.
  The app has a Hub onboarding path with pairing URI paste, leaf pinning,
  selected-vehicle manifests, resumable transfer, Rust staging, atomic
  activation, Keychain storage, foreground refresh, and overnight refresh.
- The exact isolated Teslatlas worktree exists at
  `/Users/bolyki/dev/source/teslatlas-hub-test` on `teslatlas-hub-test`, based
  on `origin/main` at `a2499f10`.
- The iPhone-only pairing flow now offers QR scanning as well as paste. It is
  unavailable on Catalyst by design; camera permission is declared.
- Repair now discovers orphan objects in the actual `packs/sha256` layout and
  leaves staging and non-content-addressed files untouched.
- Lifecycle commits now append an ordered, durable source ledger with canonical
  JSON payloads for newly materialised drives, positions, charges, and charge
  samples. Snapshot sequence markers advance beyond all ledger entries.
- Projection-ledger retention now permits compaction only after a recoverable
  full snapshot exists. A cursor older than that retention floor is rejected
  as snapshot-required, never served as an incomplete delta.
- A signed, source-ordered generic incremental-pack producer now validates
  against durable projection changes. It is deliberately not yet enabled for
  collection: Core/iPhone delta application and cursor-aware manifest selection
  must land first.
- Identical compatibility snapshots retain their existing signed manifest and
  content-addressed pack, so a refresh needs only the small manifest request;
  it does not create or download another full pack.
- Manifest responses now expose a content ETag and return `304` for an exact
  authenticated conditional request; the iPhone has not yet persisted and
  sent that validator.
- A 192 MiB representative TeslaMate fixture was made from the authorized
  read-only VPS snapshot. It contains public schema plus only the fixed source
  contract tables (including required `car_settings`); private/token/settings/
  unrelated table data is excluded. A clean PostgreSQL 18 restore loaded the
  contract data, including the reviewed migration high-water marker.

## Architecture decisions

- Hub remains Mac-first and token-first. Tesla tokens stay on Hub; the iPhone
  receives only its paired bearer.
- Preserve the existing mirror unless staged import verification succeeds, then
  atomically activate.
- Native releases must use portable Rust, bundled SQLite, native TLS, signed
  artifacts, and no Homebrew or Docker runtime dependency.
- Production VPS access is read-only only; create a logical dump and minimum
  chmod-700, gitignored test bundle. Never copy PostgreSQL data files.

## Branch and commit state

- Hub: `main`, `f4093ed` (ahead of the checked Teslatlas app state).
- Teslatlas: `main...origin/main`, `a2499f10`.
- Teslatlas proof: `teslatlas-hub-test...origin/main`, `a2499f10`, with local
  uncommitted Hub onboarding proof work.

## Completed proof

- Hub unit/integration test suite: 100 passing.
- Hub format, Clippy, bootstrap contract, and TLS end-to-end validation.
- `Teslatlas (Dev)` XCTest passed on the available iPhone 17 Pro Simulator.
- `db::tests::repair_clears_quarantined_sessions_and_removes_orphaned_packs`
  passes after the content-addressed repair check; Hub formatting passes.
- Hub library suite: 99 passing tests; Clippy is warning-free. Ledger tests
  cover ordered lifecycle mutations and sequence separation from full snapshot
  markers, plus retained-history snapshot-required recovery. No-change
  publication keeps the original manifest.
- `Teslatlas (Dev)` iPhone Simulator build and XCTest pass with Xcode 27.
  This is not a physical-device proof.

## Remaining gates

- Freeze reproducible Hub and Teslatlas integration baselines.
- Complete one-command local identity, TLS, service setup, and terminal QR.
- Run the exact Hub HTTP and pack contract through the real Teslatlas client
  and Rust staging path in Simulator.
- Replace JSON staging with bounded direct typed-pack generation and verify the
  complete copied history in Simulator.
- Pass the full automated failure matrix in Simulator.
- Pass Debian install, setup, reboot, and LAN serving proof.
- Finish with one short physical QR-camera, LAN TLS, representative import, and
  relaunch smoke test.

Delta synchronization, the owner-token collector, Fleet, comparative
benchmarks, public release signing, arm64/Pi, and the broad install matrix are
deferred beyond the bridge-first v0.1.

## Blockers

- No current v0.1 development blocker. A registered iPhone is needed only for
  the final short physical smoke test.

## Next three actions

1. Freeze reproducible Hub and Teslatlas baselines around the exact v0.1 HTTP
   and pack contract.
2. Complete the one-command setup, protected local identity, terminal QR, and
   fully automated tiny-snapshot Hub-to-Simulator tracer.
3. Replace the multi-gigabyte JSON staging import with direct bounded typed
   pack generation, then prove the full copied TeslaMate history through the
   Simulator.
