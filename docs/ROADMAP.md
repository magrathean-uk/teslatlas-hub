# Teslatlas Hub roadmap

Current truth lives in [Current status](STATUS.md). This roadmap is ordered by
user-visible vertical slices. Work must not move to a later slice while an
earlier acceptance gate is still open.

## Development host policy

Product work (slices 0–3) is **Mac-first**: Hub logic, collector, migration,
and Teslatlas iOS Simulator/device proof run on the developer Mac and the
isolated iOS worktree. Do not use full-system QEMU/TCG as a day-to-day Linux
compile farm.

**Slice 4 (native packages and install matrix) is last.** Build Linux packages
with a native-speed path when possible (for example Colima arm64). Prefer
installing a prebuilt `.deb` on the disposable Debian VM rather than compiling
the full release under x86 TCG on Apple Silicon. Docker/Colima may be used only
as a Linux *build* host for packages; the Hub runtime itself remains native
Debian without Docker.

## Product boundary

Hub is a native Debian amd64/arm64 service with no Docker runtime. Teslatlas is
the face. Hub owns collection, storage, pairing, and fast synchronization.
TeslaMate is a read-only migration source and behavioural reference, never a
runtime dependency.

The first supported source is an existing owner token. Fleet is second.
Production VPS access is forbidden during development proof.

## Slice 0 — freeze the baseline

Status: **in progress**.

Implemented:

- Rust Hub, storage, typed pack protocol, authentication, signing, and server.
- Isolated Teslatlas third-source implementation.
- Native Debian packaging and local test harness.

Required:

- Commit the Hub baseline deliberately.
- Rebase the isolated iOS worktree onto current Teslatlas without touching the
  user's existing dirty worktree.
- Rebuild the Teslatlas Rust framework and record provenance after the final
  Rust/FFI commit.
- Run the full Hub, core, iOS compile, and static gate set from clean
  checkouts.

Acceptance: another task can clone both exact commits and reproduce the same
local builds without hidden workspace state.

## Slice 1 — usable token-first backend

Status: **partially implemented; core product gap**.

Implemented:

- Encrypted token ingestion.
- Read-only no-wake discovery.
- Bounded raw observations.
- Signed car-only publication.

Required:

- Choose and document the supported owner-token endpoint contract.
- Add a supervised, opt-in collector schedule with rate limits, backoff,
  sleep/offline handling, crash recovery, and zero wake calls.
- Build durable vehicle lifecycle state for driving, charging, updating,
  online, asleep, and offline transitions.
- Materialize completed drives, positions, charging processes, and charge
  samples from append-only observations.
- Recover open sessions deterministically after process or host restart.
- Add retention, disk ceilings, corrupt-state quarantine, and repair tooling.
- Prove with a local fake Tesla service, then with a user-supplied token on the
  disposable VM. Production credentials are not a test source.

Acceptance: a clean local VM records a complete synthetic drive and charge,
survives forced restarts at every transition, and publishes identical history
after replay.

## Slice 2 — working Teslatlas third source

Status: **implemented in an isolated worktree; not end-to-end proven**.

Implemented:

- Pairing URI, one-use claim, TLS pin, paired bearer, vehicle selection, and
  Keychain-backed profile.
- Signed manifest verification, strict content-addressed download, exact Range
  resume, bounded cache, typed Rust staging, full receipt validation, and
  atomic activation.
- Startup restore, foreground import, and manual refresh wiring.

Required:

- Run runtime XCTest rather than compile-only proof.
- Run a real local TLS Hub-to-Simulator import with interruption and resume.
- Reject a signed manifest whose vehicle ID differs from the requested and
  selected Hub vehicle.
- Repair zero-byte and truncated partial downloads before retry; prove restart
  and cleanup behaviour.
- Make nightly Hub refresh runnable without the sync-manager gate rejecting the
  Hub source.
- Test wrong certificate, wrong signature, stale cursor, corrupted pack,
  duplicate pack, missing chunk, cancellation, disk-full, and source swap.
- Prove no prior mirror loss on every failure path.
- Run the complete onboarding and refresh path on a physical iPhone.
- Measure first import and refresh against the same dataset used by TeslaMate
  PostgreSQL and MyTeslaMate.

Acceptance: a fresh physical-device install pairs with the local VM, imports
history, resumes a cut transfer, refreshes in background, and retains the old
mirror through injected failures.

## Slice 3 — migration and fast refresh

Status: **migration implemented and locally tested; delta refresh not started**.

Implemented:

- Bounded, read-only TeslaMate PostgreSQL capture and full-snapshot
  publication.

Required:

- Validate migration against a disposable representative TeslaMate database.
- Add a commit-ordered Hub ledger, tombstones, signed cursors, retention
  windows, and `snapshot_required` recovery.
- Apply deltas transactionally on iOS with the same all-or-nothing boundary.
- Declare and meet memory, disk, CPU, transfer-time, and bandwidth ceilings on
  4 GB amd64 and Raspberry Pi arm64.

Acceptance: initial migration is complete and repeatable; subsequent refresh
transfers only changed rows while a forced retention miss safely falls back to
a full snapshot.

## Slice 4 — one-command native release

Status: **package prototype proven on an earlier amd64 snapshot; deferred until
product slices 0–3 pass on Mac**.

Implemented:

- Native package, hardened systemd units, encrypted credentials, exact-commit
  source bootstrap, signed release-manifest verification, and local verifier.
- Bootstrap `--dry-run` contract aligned with install dry-run; shell contract
  tests. Colima arm64 package construction proven from committed source as a
  build-host path (install matrix still open).

Required (after slices 0–3):

- Repeat clean amd64 proof from the committed source (install prebuilt package
  preferred over TCG full compiles on Apple Silicon).
- Build and prove arm64 on Raspberry Pi-class Debian.
- Align the Git bootstrap `--dry-run` documentation and CLI contract, then add
  script-level contract tests.
- Create the real project signing key and independently distribute its public
  key.
- Publish signed amd64 and arm64 artifacts and a stable HTTPS bootstrap.
- Make the public command parameter-light while retaining a pinned trust root.
- Test clean install, reinstall, upgrade, downgrade refusal, rollback, token
  rotation, signing-key rotation, TLS renewal, backup, restore, and uninstall.
- Prove direct TLS remote pairing without using the production VPS.

Acceptance: one documented command on clean Debian installs a verified package,
prompts securely for the token, starts healthy, survives reboot, and can pair a
phone.

## Slice 5 — Fleet

Status: **design only; 0% implementation**.

Required:

- Obtain an operator-owned Tesla Developer application, exact callback, client
  credentials, application-domain public key, and regional registration.
- Implement state-bound authorization-code login, encrypted rotating refresh
  credentials, consent revocation, and recovery.
- Ingest Fleet Telemetry through the same lifecycle and projection boundary.
- Keep command scopes, virtual-key pairing, and vehicle control out of the
  telemetry-only release.

Acceptance: Fleet can replace the compatibility token lane without changing
the iPhone pack contract or weakening credentials.

## Permanent gates

- No production VPS access or writes during development and validation.
- No raw Tesla response, token, database credential, or signing key reaches
  the phone or logs.
- No live iPhone mirror replacement before full validation.
- No performance claim without measured amd64 and arm64 evidence.
- No “complete” label based only on compilation, unit tests, static checks, or
  package construction.
