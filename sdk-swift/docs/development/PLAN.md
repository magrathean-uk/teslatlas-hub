# Swift SDK post-adoption plan — 2026-09-19

Objective: Preserve the accepted App-facing Swift boundary and verify the
declared macOS 14 minimum floor.

Authority: [master plan](../../../docs/development/MASTER_PLAN.md),
[coordination](../../../docs/development/COORDINATION.md),
[App v7 handoff](../../../docs/development/APP_V7_READINESS.md), and
[STATUS.json](STATUS.json).

## Current position

G3, G6 and G7 are accepted. The exact `TeslatlasCurrentHub` source and binding
passed a maintained external SwiftPM consumer on macOS 27 arm64 with discovery,
normal TLS, one-use claim/replay rejection, current/history, cursor/ETag `304`,
rotation, Hub restart continuity and cleanup. The App adoption reference is
accepted outside `app/`.

This proves the current host only. `Package.swift` declares macOS 14 and iOS
17, but macOS 14 was not independently exercised and there is no App, iOS
runtime, installer, notarization, real-data or production acceptance. Do not
rerun G3/G6/G7 on the current host without a binding or source delta.

## Next goal draft — not started

L2: verify the declared macOS 14 Apple-silicon floor using the exact accepted
SwiftPM source, `TeslatlasCurrentHub` product, profile and Hub product version.
Use an isolated supported guest, a fresh Hub-owned cohort and a clean external
consumer root. Prove package resolution/build and the bounded public journey
needed to show the floor is real: discovery, TLS, claim/replay rejection,
current/history, rotation, Hub restart recovery and cleanup.

Acceptance requires:

- exact macOS 14.x arm64, Xcode/Swift, source manifest, `Package.swift` and
  profile identities;
- a release executable linked only through the public SwiftPM product;
- public API/auth/trust/recovery results equivalent to the accepted boundary,
  with no App source or private SDK boundary;
- all credentials, private roots, processes and listeners absent at close;
- compatibility updated only if the floor receipt passes.

This draft does not authorize VM creation/start, source changes, build, test,
Hub runtime, or App work. The coordinator must create and start a new goal.

## Later work

L3 covers reproducible SwiftPM source packaging and changed consumer recovery
only. iOS 17 floor/host evidence, broader Swift/compiler support, package
release and real-data semantics are separate lanes. The macOS 13 Hub floor is
below the Swift package floor and is not a Swift acceptance target.

## Boundaries

Preserve the dirty `main` checkout and exact contract separation among
`TeslatlasCurrentHub`, `TeslatlasHubSDK` and
`TeslatlasHubV1Compatibility`. No App or Viewer work, x86/Intel/Azure,
production or vehicle action, commit, push, CI, release, publication, or reuse
of closed private handoffs.
