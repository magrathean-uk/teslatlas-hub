# Programme reassessment — 2026-09-18

Read-only checkpoint plus the completed Hub source review. No new product build,
runtime, VM or consumer journey was run during this planning reset. There is no
credible single completion percentage: the missing work is integration/runtime
acceptance, while several source and scoped consumer objectives already passed.

## What is done and what is still missing

| Product | Preserved evidence | Next work |
| --- | --- | --- |
| Hub | Historical bounded ARM64 bootstrap, Mac restart/state and SDK/Edge synthetic journeys; current TypeScript source review accepted | Rebuilt environment, exact runnable standalone Debian/Mac route, ordinary pairing/data/restart proof and fresh consumer handoffs |
| TypeScript | Packed Debian Node consumer; prior 272 unit and 25 conformance cases; seven browser-free orchestration cases accepted by Hub r3 | Fresh real Apple-silicon browser journey: TLS/CORS, discovery/readiness, claim/current/drives, 304/cursors, rotation and cleanup |
| Edge | Prior narrow forwarding/recovery evidence; r11 exact inactive source/profile preparation accepted | Fresh zero-event baseline, bounded synthetic durable delivery and final-cohort recovery/deduplication proof |
| Swift | Scoped external Debian ARM64 and supported Mac SwiftPM journeys completed | Map evidence to final Hub cohort, close any actual delta and supply the App-facing integration reference; no repeated unchanged suites |
| Protocol | Scoped contract resource goal complete; recorded 134 + 31 local cases and 24/24 ARM64 current-profile fixture cases | Support concrete contract gaps and final exact bindings; no speculative contract rewrite |
| Home Assistant | Prior bounded ARM64/synthetic evidence; current environment/session unavailable | Secondary supported-runtime config/scheduler/reauth/unload journey after shared environment is ready |

Protocol/Swift completion applies to their earlier scoped goals, not full installed,
parity or App integration acceptance. Source-only and old deleted-guest evidence remains
useful historical evidence. Reuse it where the tested identity and assertion still apply;
do not pretend it proves a running rebuilt system.

## Exact key evidence

- [Hub TypeScript r3 source review](../../hub/docs/development/active-macos-typescript-browser-orchestration-source-review-2026-09-18-r3.json): accepted source-only; stale receipt/capture checks precede mutation; cleanup records every command and supervisor closure. Seven browser-free cases, preflight, syntax and Biome passed. No browser/runtime handoff issued. r7 remains closed.
- [Edge concrete-profile acceptance](../../teslatlas-edge/docs/development/edge-macos-one-event-r11-concrete-profile-acceptance-2026-09-18.json): inactive prepared profile, immutable/query-only store relationships and exact hashes; no guest runtime or event. Time-limited credentials are expired; the one-use preparation is not reusable.
- Prior Protocol evidence keys: `local_contract_gate`, `b1_native_current_hub_fixture`.
- Prior TypeScript evidence keys: `t6_full_verify`, `t4_packed_node_51_drive`.
- Prior Swift evidence keys: `swift_debian_r3_public_consumer`, `swift_macos_r2_public_consumer`.

Those evidence keys remain in the byte-preserved [pre-reset status archive](archive/2026-09-18-reset/manifest.json).
Current product STATUS files link their exact archived record. Do not repeatedly read
the entire archive; retrieve only a gate's relevant receipt.

## Source and environment

Independent `main` checkouts are dirty. Hub HEAD begins `7fe8cb20`, TypeScript `b1cd548f`,
Edge `67650cd9`, Swift `51494612`, Protocol `05225bd5`. TASKS.json records the complete
HEADs at reset. No commits or source cleanup were performed. Root Git reports a malformed
parent gitfile; operate each product checkout directly and do not repair parent/App storage.

The owner intentionally deleted the VMs. Replacement provisioning is now authorized,
so waiting for the old guest is not a valid dependency. Use ENVIRONMENT.md's measured
inventory and small shared-VM policy; issue new identities and handoffs.

## Coordination corrections

All seven former product task IDs were archived successfully through the Codex CLI.
The saved hourly automation is PAUSED. No active CLI product worker was found at reset;
new execution must still reconcile writer/lock ownership once before edits. Archival
preserves conversations and does not falsely complete their native goals.

The active plan/model rules have replaced contradictory restart/pause stacks and stale
Hub/Swift AGENTS model clauses. Old plan/status bytes remain in the reset archive.
New policy: Sol 5.6/max coordination, Sol/high implementation/review, Luna exploration,
no fast mode. Home Assistant remains secondary; App is untouched and Viewer has no work.

## Distance to readiness

Eight explicit adoption gates G0–G7 are not yet accepted for the new combined milestone.
G3 and G6 have substantial scoped evidence to carry forward after binding checks.
The immediate critical path is G0 → G1/G2 → missing browser and Edge journeys → final
Swift/contract binding → G7 handoff. No time estimate is defensible until the first
rebuilt Hub/consumer run identifies the remaining implementation defects.
