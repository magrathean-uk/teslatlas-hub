# Hub post-adoption plan — 2026-09-19

Objective: Preserve the accepted App v7 adoption baseline and prepare the first
installed Hub lifecycle deliverable on Debian ARM64.

Authority: [master plan](../../../docs/development/MASTER_PLAN.md),
[coordination](../../../docs/development/COORDINATION.md),
[App v7 handoff](../../../docs/development/APP_V7_READINESS.md), and
[STATUS.json](STATUS.json).

## Current position

The 2026-09-18 adoption campaign is complete. G0-G6 are accepted for Hub and G7
is accepted at workspace level. The exact accepted profile is
`hub-http-v1@1.0.0` for product `2026.36.2`. The secondary Home Assistant
Debian ARM64 Container lifecycle is also accepted.

The accepted Hub evidence is source-built and synthetic. It proves standalone
Debian ARM64 and Apple-silicon Mac operation, packed TypeScript consumers,
durable Edge forwarding, an external SwiftPM consumer, exact compatibility
bindings, restart continuity, and cleanup. It does not prove a Debian package,
system service, version-to-version upgrade/rollback, macOS
installer/notarization, minimum macOS floor, real data, or production operation.

Authoritative receipts:

- `active-debian-arm64-hub-standalone-2026-09-18-r1.json`
- `active-macos-arm64-hub-typescript-g2-g4-2026-09-18-r1.json`
- `g5-debian-arm64-edge-hub-acceptance-2026-09-18-r5.json`
- `g6-macos-arm64-swift-hub-acceptance-2026-09-19-r2.json`
- `../teslatlas-protocol/docs/development/g3-compatibility-admission-2026-09-19-r2.json`
- `ha-debian-arm64-container-hub-handoff-2026-09-19-r1.json`

Do not rerun these accepted gates unless their bound source, profile, product
version, target, or evidence changes.

## Next goal draft — not started

L1: deliver one Debian 13 ARM64 installed Hub lifecycle receipt. Before runtime,
freeze exact previous-working and candidate packages, payload manifests, Hub
binaries, profile, configuration defaults, service units and target identity.
Install the baseline, exercise the installed service, upgrade to the candidate,
verify retained identity/data and ordinary pairing/current/history, create and
restore a verified backup, force one bounded failed-candidate path and rollback,
then remove package code and services while preserving data/configuration by
default. Do not run purge.

Acceptance requires:

- exact package/payload hashes, `dpkg-query` identities and actual systemd
  units, executable, argv, user/group, modes and listeners;
- health/readiness, one-use pairing, current/history and restart continuity
  through installed services;
- successful upgrade, fail-closed interrupted/invalid-candidate handling and
  rollback to the recorded working baseline;
- backup/restore with the recorded identity and data readable afterward;
- removal with services/listeners absent while data, identity and configuration
  fingerprints remain unchanged;
- redacted receipts, process/child zero, owned cleanup and no reuse of a closed
  cohort.

This draft does not authorize implementation, VM start, package installation,
build, test, or runtime work. The coordinator must create and start a new goal.

## Later work

L2 covers Apple-silicon installed lifecycle in an isolated guest, with Hub's
macOS 13 floor kept distinct from Swift's macOS 14 floor. L3 covers
reproducible source-only packages and changed consumer recovery paths. Named
TeslaMate/source parity remains blocked on a fresh owner-provided input and must
never reuse an old fixture. ARM64 Docker and aggregate optional-component
lifecycles remain separate lanes.

## Boundaries

Preserve the independent dirty `main` checkout. Hub owns shared VMs, fixtures,
installers and integration runtimes. No App or Viewer work, x86/Intel/Azure,
production or vehicle action, commit, push, CI, release, publication, or public
ingress. Historical private inputs and closed cohorts remain closed.
