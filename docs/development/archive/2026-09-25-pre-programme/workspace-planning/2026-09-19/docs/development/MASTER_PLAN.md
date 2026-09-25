# Hub ecosystem master plan

Revision: 2026-09-19. **APP_V7_ADOPTION_READY is accepted.** G0–G7 and secondary
Home Assistant Container lifecycle passed within their recorded scope. The App
owner can use [APP_V7_READINESS.md](APP_V7_READINESS.md) in a separate task.

This task reconciles plans/history and uploads authorized source branches. The next
campaign is **DRAFT_NOT_STARTED**: [NEXT_PHASE_PLAN.md](NEXT_PHASE_PLAN.md), with
an unsent [/goal prompt](GOAL_PROMPT.md). Do not restart completed gates because
older plans still describe them as pending.

## Accepted milestone

| Gate | Accepted scope | Evidence |
| --- | --- | --- |
| G0 | Shared Debian 13.6 ARM64 guest, strict access, restart/persistence and disk budget | [VM access](VM_ACCESS.md) |
| G1 | Source-built synthetic Debian Hub setup, pairing, current/history and identity/data restart | [Hub receipt](../../hub/docs/development/active-debian-arm64-hub-standalone-2026-09-18-r1.json) |
| G2 | Source-built synthetic macOS 27 Apple-silicon Hub journey and restart | [Mac receipt](../../hub/docs/development/active-macos-arm64-hub-typescript-g2-g4-2026-09-18-r1.json) |
| G3 | Exact five-product profile and content-bound compatibility admission | [Protocol r2](../../teslatlas-protocol/docs/development/g3-compatibility-admission-2026-09-19-r2.json) |
| G4 | Packed TypeScript Node and real Chrome consumer, trust/CORS/auth/history/recovery and cleanup | [SDK receipt](../../teslatlas-sdk-typescript/docs/development/macos-arm64-packed-node-browser-g4-2026-09-18-r1.json) |
| G5 | One synthetic Debian Edge record, durable Hub projection before ACK, deduplication and outage/restart | [Edge r5](../../teslatlas-edge/docs/development/g5-debian-arm64-edge-hub-acceptance-2026-09-18-r5.json) |
| G6 | External SwiftPM consumer on macOS 27 arm64, trust/auth/current/history/rotation/restart | [Swift r2](../../teslatlas-sdk-swift/docs/development/g6-macos-arm64-external-consumer-acceptance-2026-09-19-r2.json) |
| G7 | Reviewed App adoption contract, examples, limits and troubleshooting | [Adoption handoff](APP_V7_READINESS.md) |
| HA | HA Container 2026.8.3 on Debian ARM64: config, polling, reauth, reload, restart and diagnostics | [HA receipt](../../teslatlas-home-assistant/docs/development/ha-debian-arm64-container-lifecycle-acceptance-2026-09-19-r1.json) |

These are historical, closed runtime cohorts. A source upload does not reopen them,
create an installed service, or certify a new Git commit. Keep original receipt hashes
and tested dirty-tree identities intact. Publication identity is separate in
[UPLOAD_STATUS.json](UPLOAD_STATUS.json).

## Remaining programme

1. **L1 — Debian installed lifecycle.** Ordinary Hub installation/service setup,
   upgrade, backup/restore or rollback, and data-preserving removal. Then optional
   Edge and the real package-to-consumer boundary.
2. **L2 — Apple-silicon lifecycle and supported floors.** Use one isolated Mac guest
   when needed. Track Hub macOS 13 and Swift macOS 14 floors separately; accepted
   macOS 27 runs prove neither. No signing, notarization or release publication.
3. **L3 — Reproducible source/package handoff.** Local source exports, package
   instructions, exact compatibility and optional dependencies for all six products;
   HA replacement-upgrade where a real delta exists. Rerun changed/missing assertions.
4. **D1 — Named-source parity.** Needs a fresh owner-supplied read-only TeslaMate
   export with provenance. Prepare independent checks but do not claim real parity
   from synthetic data or reuse old private fixtures. No vehicle/production actions.

The draft goal starts with L1. [NEXT_PHASE_PLAN.md](NEXT_PHASE_PLAN.md) defines owners,
acceptance and stop conditions. Full product readiness remains distinct from App
adoption readiness. No percentage or deadline substitutes for evidence.

## Execution boundaries

Sol 5.6/max coordinates; Sol/high implements/reviews; Luna/max explores. No fast mode.
Default to two useful workers maximum, one writer per repository, and one heavy job
under the shared lab lock. Hub owns runtimes. Protocol supports concrete deltas.
HA is secondary. App, Viewer, x86/Intel/amd64 and Azure remain excluded.

Follow [COORDINATION.md](COORDINATION.md), [ENVIRONMENT.md](ENVIRONMENT.md),
[START_AUTHORIZATION.md](START_AUTHORIZATION.md) and each selected product PLAN/STATUS.
Keep automation paused. Old plans/chats are historical evidence, not runnable queues:
[HISTORY_REVIEW.md](HISTORY_REVIEW.md), [SUPERSEDED_PLANS.md](SUPERSEDED_PLANS.md).
