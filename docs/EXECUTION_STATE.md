# Teslatlas Hub execution state

## Current phase

Slices 0 through 3 have a complete real-data happy-path proof on 2026-07-30.
The v0.1 is the single TeslaMate PostgreSQL to signed Hub packs to Teslatlas
bridge described in `docs/V0_1_DEVELOPMENT_PLAN.md`.

## Release-candidate evidence: 2026-07-30

- The repeatable command was run from `codex/hub-v0.1` with
  `scripts/test-simulator-tracer.sh`, using the local copied PostgreSQL source
  `tm_contrib_import_verify_20260716_1155`, car `1`, `PGSSLMODE=disable`, and
  `TESLATLAS_HUB_TEST_RUNS=2`. No production source was used.
- The selected source contains exactly 1 car, 2,644 drives, 8,812,983 raw
  positions with 8,764,495 attached to drives, 604 charging processes, and
  216,296 charge samples. Hub publishes exactly 1 car, 2,644 drives,
  8,764,495 positions, 604 charges, and 216,296 charge samples in 367 packs.
- Simulator pass 1 completed in 51.178 seconds. Hub was stopped and restarted;
  pass 2 completed in 50.105 seconds. The injected pairing URI was generated
  from the active Hub for each pass. XCTest also reopened the activated mirror
  through fresh Keychain credentials and a fresh Rust bridge.
- `cargo fmt --all -- --check`, `cargo test --workspace` (105 tests), and
  `cargo clippy --workspace --all-targets -- -D warnings` pass. The Rust TLS
  import E2E test passes. Focused Hub iOS tests pass on the iPhone 17 Pro
  Simulator with `/Applications/Xcode-beta.app`; framework provenance reports
  `TeslatlasCore.xcframework is fresh.`
- The single `scripts/mac-local-tls-hub.sh release-candidate` owner command
  completed against the same local source, generated the one-use pairing
  artifact, detected endpoint host `172.17.17.111`, started Hub, and passed
  `/readyz`. The pairing secret was not printed.

macOS is now the primary Hub development host and Simulator is the primary app
proof environment for the complete happy path, real-history import, resume,
corruption, cancellation, retry, and restart matrix. Debian is limited to one
final package, setup, reboot, TLS, and representative-import compatibility
smoke. Physical iPhone work is limited to one final camera, Wi-Fi/LAN TLS,
representative import, and relaunch smoke test.

The previous objective combined history migration, owner-token collection,
delta synchronization, iPhone integration, and multi-architecture release
engineering. Those remain long-term roadmap work, but they no longer share the
first executable gate.

A Debian amd64 VM is running with 8 vCPUs. Hub is healthy and serves the
completed direct import from the read-only copied TeslaMate PostgreSQL source.

## Verified existing work

- Hub v0.1 is isolated on `codex/hub-v0.1`. Rust suite has 103 passing tests;
  format, Clippy, bootstrap contract, and TLS end-to-end checks pass.
- Token credential adapter, supervised no-wake collection, pairing, signed
  packs, range resume, restart recovery, quarantine, repair, native TLS,
  bundled SQLite, and hardened systemd unit foundations exist.
- Teslatlas `origin/main` is at `ec831cff`.
  The app has a Hub onboarding path with pairing URI paste, leaf pinning,
  selected-vehicle manifests, resumable transfer, Rust staging, atomic
  activation, Keychain storage, foreground refresh, and overnight refresh.
- The exact isolated Teslatlas worktree exists at
  `/Users/bolyki/dev/source/teslatlas-hub-test` on `teslatlas-hub-test`, one
  commit ahead of `origin/main`.
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

- Hub v0.1: `codex/hub-v0.1`, setup baseline `03cdcd1`.
- Preserved pre-reset Hub work:
  `codex/hub-pre-reset-snapshot`, `fe3820d`.
- Teslatlas proof: `teslatlas-hub-test`, `82d88c44`, clean and one commit
  ahead of current `origin/main` at `ec831cff`.

## Completed proof

- Hub v0.1 format, 100 tests, warning-denying Clippy, and real TLS
  claim/manifest/range-resume test pass from the clean branch.
- Proven VM import corrections are present in v0.1; deferred collector and
  delta work remains only on the preserved snapshot branch.
- Teslatlas proof branch was rebased across 23 newer main commits.
- Fresh Xcode 27 Rust framework rebuild completed for device, Simulator,
  Catalyst, and macOS slices.
- Ten focused Hub client and Rust staging tests pass on iPhone 17 Pro
  Simulator after the rebase.
- The live Hub-to-Simulator tracer passes through the production pairing URI
  parser, one-use claim, exact private TLS leaf pin, vehicle discovery, signed
  manifest verification, pack download, Rust staging, atomic activation, and
  live SQLite row checks.
- `Teslatlas (Dev)` XCTest passed on the available iPhone 17 Pro Simulator.
- `db::tests::repair_clears_quarantined_sessions_and_removes_orphaned_packs`
  passes after the content-addressed repair check; Hub formatting passes.
- Hub library suite: 99 passing tests; Clippy is warning-free. Ledger tests
  cover ordered lifecycle mutations and sequence separation from full snapshot
  markers, plus retained-history snapshot-required recovery. No-change
  publication keeps the original manifest.
- `Teslatlas (Dev)` iPhone Simulator build and XCTest pass with Xcode 27.
  This is not a physical-device proof.
- Direct import sequence 3 published 435 verified immutable packs for
  10,606,913 unique mirror rows: 1 car, 3,137 completed drives, 10,313,305
  attached positions, 767 charging processes, and 289,703 charge samples.
- The direct importer created no multi-gigabyte JSON staging database.
- The production Hub client and Rust staging path imported that exact signed
  history on the iPhone 17 Pro Simulator in 104.658 seconds. All table counts
  matched and atomic activation completed.
- Parent rows repeated across FK-complete transport fragments are now accounted
  separately from the signed unique logical mirror-row total.
- Full-history retry reuses already verified content-addressed packs. Failed
  staging cleanup leaves the active mirror untouched.
- The Mac development path imported 8,984,040 real logical rows into 367 packs
  from the validated local TeslaMate copy. A second identical import retained
  the exact snapshot ID and sequence, and the object store contained exactly
  the 367 published packs afterward.
- The same Mac Hub served that complete history to the iPhone 17 Pro Simulator.
  Exact counts passed: 1 car, 2,644 drives, 8,764,495 positions, 604 charges,
  and 216,296 charge samples.
- The signed Simulator tracer saves the Hub profile only after activation,
  reloads it through Keychain using fresh credentials state, creates a fresh
  Rust bridge, and reopens the activated mirror.
- The focused Simulator failure matrix passes for TLS identity, manifest
  signature and vehicle binding, pack length/hash/range validation, unsafe
  fields, failed-stage cleanup, stored-profile validation, and resumable
  partial files.
- Debian package `0.1.0-vm8` installs over the prior package, survives reboot,
  starts enabled, serves TLS successfully, and preserves the published real
  history. Debian is no longer used for repeated development runs.

## Remaining gates

- Physical QR-camera, LAN TLS, and Debian deployment work are outside this
  Mac-first release-candidate scope and were not started.

Delta synchronization, the owner-token collector, Fleet, comparative
benchmarks, public release signing, arm64/Pi, and the broad install matrix are
deferred beyond the bridge-first v0.1.

## Blockers

- No current v0.1 development blocker. A registered iPhone is needed only for
  the final short physical smoke test.

## Next action

Commit the release-candidate evidence on the two isolated branches.
