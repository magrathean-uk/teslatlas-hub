> OWNER RESTART — 2026-09-18: The owner explicitly resumed development on the six in-scope products and ecosystem orchestration. Do not use fast mode. Resume Hub with Sol/high and Protocol, TypeScript SDK, Swift SDK, Home Assistant and Edge with Luna/max from preserved checkpoints. Viewer remains excluded and all x86/amd64/Intel work remains paused. The App keeps its separately authorized scope.

> APP-ONLY RESTART EXCEPTION — 2026-09-12T16:53:09.669326+00:00: The App task reports the owner directly authorized continued App work. App v6, its localization workers and its own heartbeat may resume under that task. Hub, Protocol, TypeScript SDK, Swift SDK, Home Assistant, Edge and ecosystem coordination remain paused until explicit owner restart. Viewer remains excluded; x86 remains paused.

> OWNER PAUSE — 2026-09-12T16:51:03.456724+00:00: The owner paused this work and all development on this project. Stop development, reviews, tests, builds, runtime preparation, delegation and polling until explicit owner restart. This supersedes earlier restart and standing execution authority, including r11. Preserve dirty source, evidence and goals. Hourly coordination is PAUSED. Viewer remains excluded; x86 remains paused.

# Teslatlas Hub ecosystem master development plan

> OWNER SCOPE — 2026-09-12: Viewer is excluded from all active development, goals, plans, packaging and acceptance dependencies. Preserve its existing source and historical evidence; do not schedule or resume Viewer work. Six products remain: Hub, Protocol, TypeScript SDK, Swift SDK, Home Assistant and Edge.


> OWNER RESTART — 2026-09-12: The owner explicitly said restart on all. Development and hourly coordination resume from preserved checkpoints for Debian ARM64 and Apple-silicon Mac. The 2026-09-10 global pause is lifted; x86 remains paused. Hub Sol/high, companions Luna/max. Preserve existing goals, dirty checkouts and completed evidence; revalidate current runtime state and use fresh expiring inputs.

Date: 2026-09-09. **Active aim: working products on Debian ARM64 and Apple-silicon Mac. All x86 development and acceptance work is paused by the owner.** [ACTIVE_SCOPE.md](ACTIVE_SCOPE.md) is the current scope amendment. Development and hourly coordination continue under [START_AUTHORIZATION.md](START_AUTHORIZATION.md).

## First outcome

Deliver a usable Hub that works alone, then Hub with the selected optional companions. The ordinary user can set up the product, start it, pair where needed, see clearly labelled data, recover from common failures and restart without losing data. First prove those routes on Debian 13 ARM64 and supported Apple-silicon macOS. Complete the minimum installation/dependency/service work needed to use them; polished distribution comes afterward.

Hub is the main product. Protocol, TypeScript SDK, Swift SDK, Home Assistant and Edge are optional products with separate ownership. The iOS App in `app/` is excluded. See [PRODUCT_SPEC.md](PRODUCT_SPEC.md).

## Current execution scope

The earlier R1-only sequencing is superseded for independent ARM64/Mac working-product steps. B1/B2/R1/D1/X1 remain evidence labels, not a reason to block the next usable route on missing unrelated inputs. Named-source TeslaMate parity remains open until the owner supplies the source bundle; its absence does not block Mac startup, ordinary consumers, packaging needed to run a product, or other independent active-target work.

Existing evidence includes the Debian ARM64 Hub/Viewer bootstrap, selected SDK/HA/Edge consumer paths, the bounded synthetic r13 name-only delivery and cleanup, synthetic data-only recovery, Viewer browser data-state/recovery and isolated Docker runtime, optional component source bindings, and the HA disposable local-candidate adapter. These are scoped observations, not a full product or parity pass. One r13 actor datum produced three classified ingress sequences; no numeric telemetry or passive provenance is implied. The historical [R1 release decision](coordination/2026-09-09-r13-pass-r1-release.json) retains that boundary. Viewer evidence is historical only; no Viewer work is in current scope.

| Product | Immediate working-product objective | Next owner handoff |
| --- | --- | --- |
| [Hub](../../hub/docs/development/PLAN.md) | Usable Hub-only on Debian ARM64 and Mac; identify the next actual Mac startup/admin/pairing gap | Exact runnable candidate, normal invocation, target/runtime, private trust/pairing locations and cleanup owner for the affected consumer |
| [Protocol](../../teslatlas-protocol/docs/development/PLAN.md) | Usable current-Hub contract/developer resources for active ARM64/Mac consumers | Minimal contract correction or exact consumer evidence; keep unsupported profiles and formal admission separate |
| [TypeScript SDK](../../teslatlas-sdk-typescript/docs/development/PLAN.md) | Working packed Node/browser public consumers on ARM64/Mac, supporting public SDK consumers | Matching archive/profile plus only the active target's required input; deferred x86 cells are not prerequisites |
| [Swift SDK](../../teslatlas-sdk-swift/docs/development/PLAN.md) | Normal SwiftPM consumption on supported Apple-silicon macOS and supported Debian ARM64 | Exact declared runtime/floor and any missing Hub runner/input; preserve App and deployment floors |
| [Home Assistant](../../teslatlas-home-assistant/docs/development/PLAN.md) | Usable integration in the selected ARM64 HA Container/runtime, with ordinary scheduler and auth/lifecycle recovery | Explicit supported HA config/runtime and immutable payload; no native Mac HA daemon or x86 work |
| [Edge](../../teslatlas-edge/docs/development/PLAN.md) | Usable ARM64 receiver/spool/Hub forwarding and supported Apple-silicon Mac operation | Fresh bounded Hub-owned development handoff for the next actual gap, with custody and cleanup ownership |

## Ordered working milestones

| Priority | Deliverable | Evidence needed |
| --- | --- | --- |
| 1 | Working Debian ARM64 Hub alone | Documented setup/start, normal pairing/current/history, safe rerun, restart and retained identity/data |
| 2 | Working Apple-silicon Mac Hub and supported Swift consumer | Actual normal startup/admin/browser/consumer route on a supported existing Mac runtime; trust/pairing and restart evidence |
| 3 | Selected optional products working on active targets | Protocol/SDK resources usable; HA normal polling/auth/unload; Edge durable commit/ACK/recovery; Hub works when each is deselected |
| 4 | Reliability and real-data acceptance | Relevant active-target recovery plus separately admitted source/backup/import/passive evidence; unavailable source events remain pending |
| 5 | ARM64/Mac delivery and lifecycle completion | Required dependency selection, core-only install, safe rerun/upgrade/rollback and data-preserving removal; polished choices after ordinary use works |
| Paused | x86/amd64/Intel Mac and full cross-architecture/Azure acceptance | Preserve backlog and evidence; resume only on new owner direction |

Work may run independently across products while Hub sequences shared host changes. Do not redo completed steps solely because the priority wording changed. Select the first unresolved working-product gap and verify it at the appropriate level.

## Ordinary setup and integration

Reuse the existing Hub setup/service commands, `hub/scripts/bootstrap-dev.sh`, `hub/scripts/bootstrap-companions.py`, `hub/tools/companions/`, public SDKs. Use the explicit local-candidate path where the public catalog is empty; do not fabricate publication or add another resolver. Hub records the actual final invocation and candidate identity in each handoff.

For Hub, cover core-only startup, normal public interfaces, supported consumers, restart and identity/data preservation. Synthetic data proves only bounded usability, not collection parity.

Protocol and SDKs are developer resources, not daemons. Swift uses a declared supported runtime; an unsupported SDK/runtime pairing is an exact local gap, not permission to change the App or a global blocker. HA runs within a selected supported HA runtime; Mac may use the ARM64 HA instance. Edge uses the real pinned receiver/bridge and encrypted spool; use existing matching evidence and fresh bounded inputs only for an unresolved delivery/recovery case. Closed r12/r13 roots, credentials and records are not reusable.

## Reliability and acceptance boundaries

[R1 named-source plan](../../hub/docs/development/r1-named-source-read-only-evidence-plan-2026-09-09.md) defines admission, source inventory and later offline import/comparison. Missing source/backup or credentials keep those rows pending. Continue independent ARM64/Mac implementation and usability work. No source, collector, vehicle command, production cutover or competing token-refresh owner is authorized by this scope amendment.

Keep compilation, source tests, disposable local installation, installed guest evidence and real collection separate. A receipt is accepted only for its exact source/profile/runtime and stated behavior. The legacy final ledger's 0/21 installed cells, 0/444 case decisions and 0/10 cohorts are historical counters; they do not represent active ARM64/Mac readiness or a new full-matrix obligation.

D1 uses the single Hub-owned `packaging/components.json` and existing catalog/recipe/transaction machinery. The HA planning command never activates; its explicit install adapter has only disposable local-candidate evidence. No static manifest, source snapshot or package build is aggregate/runtime acceptance. Source storage stays on GitHub without CI, releases, tags, uploads, commits or pushes unless separately requested.

## Deferred x86 work

All x86, x86_64, amd64 and Intel Mac build, test, package/image, VM and lifecycle work is paused for all six in-scope tasks. Keep support code, schemas, prior observations and later obligations intact. Deferred x86 rows do not count as failures and do not block the current working-product milestone.

The unadmitted `debian13-x86_64-20260909` VM, HA runtime and isolated Hub build artifacts remain preserved. No development, new inspection, restart, shutdown, cleanup, repair or expansion is requested for that lane. Historical retained/quarantined guests also remain untouched. Do not request Azure or create another guest for this scope.

## Ownership, goals and orchestration

The coordinator owns this master/specification, active scope, task registry, environment policy and hourly handoffs. Hub alone writes Hub/shared installer/runner files and owns shared VM changes. Each companion owns its repository. Use a concrete action/target/cleanup handoff before changing a shared runtime; another agent's checkpoint or a broad task title is not that handoff.

The owner deleted all seven old native goals and authorized replacement on 2026-09-09. Each existing task verifies native state, creates one fresh usable ARM64/Mac goal when none is unfinished, and confirms the actual objective/status in its saved record. [ACTIVE_SCOPE.md](ACTIVE_SCOPE.md) defines the revised outcomes. Historical full-plan blockers, named-source parity, polished distribution and deferred architecture gates do not become prerequisites for those working-product goals. Do not falsely complete an unfinished goal or duplicate a task. Task IDs/models remain in [TASKS.json](TASKS.json): Hub Sol/High, companions Luna/Max.

Only one heavy build or VM installation runs at a time, including guest builds. Acquire the shared lab lock before starting and record the real task/PID. Preserve dirty independent `main` checkouts and exclude `app/`. Continue ready owned work, avoid repeating unchanged green suites and peer polling, and send one exact dependency if blocked. The hourly coordinator resumes affected tasks on changed inputs and notifies only meaningful progress, failure or required owner action.

## Environments and reading order

Use `~/dev/teslatlas-lab`, shared Rust homes and [ENVIRONMENT.md](ENVIRONMENT.md). The active guests are `debian13-arm64` and `macos13-arm64`; access and custody are in [VM_ACCESS.md](VM_ACCESS.md). The appropriate existing local Apple-silicon Mac runtime is also available for supported host-only consumers. Do not create duplicate VMs. Preserve historical preparation measurements as history and verify present state before changes.

Read this active scope, the product specification, your own PLAN/STATUS and only relevant receipts. Original assessments and redirected plans are historical inputs; [SUPERSEDED_PLANS.md](SUPERSEDED_PLANS.md) indexes them. This amendment changes priorities and active targets, not the truth of old evidence.
