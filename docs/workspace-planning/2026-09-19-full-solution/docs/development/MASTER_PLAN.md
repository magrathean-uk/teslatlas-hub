# Hub ecosystem master plan — full working solution

Revision: 2026-09-19, owner clarification. **The overall solution is not complete.**
APP_V7_ADOPTION_READY and the bounded HA Container journey are accepted intermediate
milestones. They remain useful evidence, but neither is the programme finish line.

The required outcome is a fully working Hub and every active dependency: Protocol,
TypeScript SDK, Edge, Swift SDK and Home Assistant, on their in-scope supported targets.
An optional installed component is still a required completed product. HA may follow
the primary path in execution order; it is mandatory for full completion.

Planning is **DRAFT_NOT_STARTED**. The owner asked for a goal to send, not for this task
to execute it. [GOAL_PROMPT.md](GOAL_PROMPT.md) is the full unsent goal;
[NEXT_PHASE_PLAN.md](NEXT_PHASE_PLAN.md) defines the complete acceptance programme.

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

## Full-solution acceptance

| Gate | Required outcome |
| --- | --- |
| F0 | One bounded ledger of documented product behavior, supported active targets/floors, install paths and dependencies, with each claim mapped to accepted evidence or required work. No dropping supported features to manufacture completion. |
| F1 | Hub works alone through ordinary native/container setup and supported Mac UI/CLI; collection adapters, import/storage/migrations, documented API/sync/export, diagnostics/repair/retention and full service/data lifecycle work within their declared contracts. |
| F2 | Optional Edge and its pinned receiver/proxy dependencies install and operate through native/container paths: mTLS, encrypted bounded spool, durable committed projection, deduplication, outage/restart, upgrade/rollback/removal. |
| F3 | Protocol and both SDKs have exact compatible public contracts, reproducible usable packages and ordinary external consumers across their claimed active targets/floors, with auth/trust/query/error/recovery semantics verified. |
| F4 | HA works through the supported installation paths and normal configuration UI, scheduler, entity semantics, reauth/reconfigure, upgrade/rollback, restart/unload/removal, with registry and unrelated state preserved. |
| F5 | Fresh named-source import/parity and passive data evidence validate actual data behavior. Missing owner-provided input or authorization blocks this gate, not independent work; synthetic data cannot accept it. |
| F6 | Reproducible source-built distributions, exact reachable dependency catalog, ordinary source bootstrap install/update/status/rollback/removal, complete instructions and truthful support claims. No Viewer prerequisite. |
| F7 | Combined installed end-to-end acceptance: fresh setup -> authorized ingestion/import -> durable Hub data -> public TS/browser/Swift/HA consumption -> failures/recovery -> upgrade/backup/restore/removal. Independent review confirms every mandatory gate and scoped product claim. |

All F0–F7 must pass for **FULL_SOLUTION_ACCEPTED**. L1–L3 remain implementation slices
inside this programme, not an alternative completion condition. D1 is part of F5,
not optional work after completion. External inputs stay named and blocking until
supplied; do not weaken the goal or call the whole solution complete without them.

Start with the installed Debian Hub slice, then the shared dependency and consumer
paths. Advance independent ready work with a small team. Existing acceptance is
reused only where source, target, package, contract and assertion still match.

## Authority and boundaries

The scope is all six active repositories and their necessary pinned dependencies.
App and Viewer remain excluded. x86/amd64/Intel/Azure stay paused; broad wording about
all dependencies does not reopen them. SDK-owned iOS support verification may be
needed for existing SDK claims; it must not use App source, plans or tooling.

Sol/max coordinates; Sol/high implements/reviews; Luna/max explores. Default to at
most two useful workers, one writer per repo, one heavy job under the shared lab lock.
Hub owns shared runtimes. Preserve source, original receipts and closed private cohorts.
The source-only distribution policy remains: no binary release, tag, CI, signing,
notarization or production deployment. The drafted goal explicitly allows necessary
source GitHub commits/pushes and exact catalog updates when the owner sends it.
No Tesla account access, public ingress, real collection or vehicle action is silently
authorized; obtain exact fresh input/permission when such acceptance is ready.

Follow [COORDINATION.md](COORDINATION.md), [ENVIRONMENT.md](ENVIRONMENT.md),
[START_AUTHORIZATION.md](START_AUTHORIZATION.md) and product PLAN/STATUS. Keep automation
paused. Prior plans/chats remain historical: [HISTORY_REVIEW.md](HISTORY_REVIEW.md).
