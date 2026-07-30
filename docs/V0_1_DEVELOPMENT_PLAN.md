# Teslatlas Hub v0.1 development plan

Status: recommended scope reset, 2026-07-30.

## Product decision

v0.1 is a native TeslaMate-to-Teslatlas bridge.

It is not yet a TeslaMate replacement, a Tesla API collector, a Fleet
application, or a public multi-platform release.

The user outcome is:

1. Install Hub on the Linux machine that can read TeslaMate PostgreSQL.
2. Run one setup command.
3. Hub creates its local identity, starts, and displays a QR code.
4. Teslatlas scans once, claims once, and automatically selects the only car.
5. Hub prepares and publishes a complete signed history snapshot.
6. Teslatlas downloads, verifies, atomically activates, and opens the data.

No user should manually create a signing key, TLS certificate, URL, JSON file,
systemd drop-in, or pairing payload.

## Proof policy

Simulator is the primary development and release-candidate environment.

All repeatable Hub-to-Teslatlas behavior runs there:

- pairing URI parsing and one-use claim
- private-LAN TLS pinning
- vehicle auto-selection
- complete real-history import
- interruption and byte-range resume
- expired or reused pairing
- wrong certificate, signature, vehicle, bearer, or pack hash
- cancellation, retry, low-disk injection, and old-mirror survival
- app termination, relaunch, and Keychain-backed profile restoration

Physical iPhone proof is one final short smoke test for behavior the Simulator
cannot prove faithfully:

- scanning the QR with the camera
- reaching the Hub over the real Wi-Fi/LAN path
- completing one representative signed import
- reopening the imported data after app relaunch

Physical hardware does not own the failure matrix or daily development loop.

## Why the reset is needed

The current objective combines five products:

- TeslaMate history migration
- a standalone Tesla owner-token collector
- a signed incremental synchronization protocol
- iPhone onboarding and durable mirroring
- signed amd64/arm64 native release engineering

That is not a small service. The current repository is already about 20,000
lines before the first boring end-to-end user loop. v0.1 must prove one loop
and defer every second lane.

## Minimal architecture

```text
TeslaMate PostgreSQL (read-only repeatable-read snapshot)
                         |
                         v
              bounded typed pack writer
                         |
                         v
         verified content-addressed pack generation
                         |
                         v
              atomic signed manifest publish
                         |
                         v
        TLS Hub: claim, vehicles, manifest, range packs
                         |
                         v
       Teslatlas: verify, stage, atomic mirror activation
```

The importer streams ordered source rows directly into bounded typed pack
writers. It must not first duplicate millions of source rows as JSON in a large
temporary SQLite database and then project them a second time.

Pack files are written under an unpublished generation, individually verified,
then made reachable by one atomic manifest commit. Failure may leave only
unreferenced immutable objects; repair can remove those safely.

## Stable v0.1 phone contract

The QR contains only:

- HTTPS endpoint
- one-use pairing ID
- one-use claim secret
- TLS leaf fingerprint

Claim returns a device ID and bearer. The paired device can then use only:

- vehicle list
- selected-vehicle signed manifest
- immutable content-addressed pack download with byte-range resume

When one vehicle exists, it is selected automatically. The phone shows only:
preparing, downloading, verifying, ready, or retry. It stores the bearer and
source profile only after successful atomic activation.

## Vertical development slices

### Slice 0 — freeze and reproduce

Type: AFK.

- Preserve current working evidence.
- Commit one deliberate Hub baseline and one Teslatlas integration baseline.
- Freeze the HTTP and pack contract used by the tracer test.
- Stop collector, delta, Fleet, benchmark-comparison, and public-release work.

Acceptance:

- Both exact commits build from clean worktrees.
- Existing focused unit tests pass.
- No hidden VM-only source fix remains.

Time box: half day.

### Slice 1 — setup, QR, tiny snapshot

Type: AFK.

- Add one `setup` owner flow that creates local TLS and signing identities,
  writes protected configuration/credentials, enables the service, and chooses
  a reachable LAN address.
- Add one `pair` flow that renders a terminal QR and can emit the URI only in
  an explicit debug mode.
- Publish a tiny deterministic fixture snapshot.

Acceptance:

- Fresh Debian package to visible QR takes under two minutes.
- No manual URL, certificate, key, JSON, or systemd editing.
- QR claim is single-use and expires.
- Service survives reboot.
- Simulator can receive the exact QR payload through a test injection seam.

Time box: one day.

### Slice 2 — exact Hub-to-Teslatlas tracer

Type: AFK on Simulator.

- Lock the JSON field names and TLS-pin behavior in a shared golden fixture.
- Simulator injects the pairing URI, claims, auto-selects one car, downloads,
  verifies, activates, and opens the fixture.
- Remove the extra import confirmation from the single-car happy path.
- Prove retry while keeping the old mirror.

Acceptance:

- One automated Hub process drives the real Teslatlas networking and Rust
  staging path.
- Wrong pin, reused QR, bad signature, bad pack hash, cancellation, and a cut
  transfer cannot replace the old mirror.
- Relaunch after success restores the Keychain-backed profile and local data.
- The entire happy path and failure matrix run without a physical phone.

Time box: one day.

### Slice 3 — direct real-history importer

Type: AFK.

- Replace the JSON staging/projection lane with direct bounded typed pack
  generation from one read-only PostgreSQL snapshot.
- Stream positions ordered by drive and charge samples ordered by charging
  process.
- Keep only bounded writer state; verify and publish once.
- Keep the existing staged importer only as a temporary comparison until the
  direct path passes.
- Import the resulting real-history manifest into the Simulator through the
  production Hub client and Rust staging path.

Acceptance:

- The copied real database produces the expected car, drive, position, charge,
  and charge-sample counts.
- Re-running unchanged input produces the same logical snapshot and no phone
  pack redownload.
- Kill during import leaves the previous manifest live.
- No multi-gigabyte JSON staging database is created.
- Peak memory, temporary disk, output bytes, and elapsed time are recorded.

Time box: one to two days. If the direct path cannot beat the staged path
within two days, stop and inspect the pack contract instead of adding patches.

### Slice 4 — Linux release candidate and physical smoke

Type: AFK for Debian and Simulator proof; short HITL for camera/LAN smoke only.

- Install the committed package in the Debian VM.
- Use 8 virtual CPUs on the next VM launch; build with `-j4`.
- Run setup, reboot, import the copied real database, and serve it over LAN.
- Complete real-history import, interruption, resume, corruption, and restart
  proof in the Simulator.
- Only after those pass, scan on the physical iPhone and complete one
  representative import over real Wi-Fi.

Acceptance:

- Linux starts from one owner command and displays a usable QR.
- Phone needs no manual endpoint, key, or server setting.
- Simulator counts match the full source and data remains available after Hub
  or app restart.
- Simulator proves a cut download resumes and an injected corrupt pack
  preserves prior data.
- Physical iPhone proves camera scan, LAN TLS, one successful import, and
  relaunch only.

Time box: one day for automated proof, then at most two hours for the physical
smoke test.

## Explicitly deferred

- Owner-token collection and lifecycle reconstruction
- Fleet OAuth and Fleet Telemetry
- delta ledgers, tombstones, retention cursors, and background refresh
- multiple-vehicle onboarding polish
- WAN hosting and public discovery
- TeslaMate/MyTeslaMate/Hub comparative benchmark suite
- public signed bootstrap, arm64/Raspberry Pi matrix, key rotation, rollback
  matrix, and release publication

These remain roadmap work. None may enter a v0.1 slice unless it blocks the
single bridge path.

## Working rules

- One end-to-end tracer is always runnable.
- No abstraction is added for a deferred consumer.
- Each slice ends with executable proof, not a percentage.
- Simulator is the default proof target; hardware is used only for
  hardware-specific behavior.
- QEMU proves Linux behavior; it is not a performance benchmark.
- The real TeslaMate copy is read-only source data.
- Existing mirror data remains live until a complete verified activation.
- A failed time box triggers scope or architecture review, not another layer.

## Expected schedule

Four focused development days plus one short physical smoke:

- Day 1: baseline plus setup and QR
- Day 2: real Hub-to-Simulator tracer
- Day 3: direct real-history importer
- Day 4: Debian plus full Simulator release-candidate proof
- Final short gate: physical QR, Wi-Fi/LAN TLS, import, relaunch

The first usable QR-to-fixture loop should exist by the end of Day 2. Full
history performance and failure proof stay automated in the Simulator.
