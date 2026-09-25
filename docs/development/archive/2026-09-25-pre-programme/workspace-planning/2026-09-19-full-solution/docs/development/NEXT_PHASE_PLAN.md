# Full working ecosystem — execution and acceptance plan

State: **DRAFT_NOT_STARTED**. Overall state: **NOT_ACCEPTED**.
Revision: 2026-09-19, expanded by the owner's full-solution clarification.
Launch only when the owner sends [GOAL_PROMPT.md](GOAL_PROMPT.md).

The objective is a usable, maintainable Hub and all five active companion/dependency
products. Preserve G0–G7 and bounded HA evidence as completed intermediate milestones.
Finish the supported product behavior, not merely the v7 adoption boundary or L1–L3.

## Acceptance gates

| Gate | Owner | Required evidence |
| --- | --- | --- |
| F0 — requirements and support | Coordinator + product owners | Map current specifications, product guides and retained plans to one compact acceptance ledger: features, native/container/source install paths, architecture/OS/compiler floors, pinned dependencies and exact receipts. Each supported active claim has an owner and pass/block condition. No speculative new feature matrix or silent reduction of declared support. |
| F1 — standalone Hub | Hub | Ordinary setup on Debian ARM64 and Apple silicon; Mac control UI and CLI; service identity/permissions; Legacy/Fleet adapter and credential/trust behavior under authorized local emulation; TeslaMate import; durable storage, schema migrations, documented query/sync/export behavior, pairing/device controls, diagnostics, repair and retention. Native and ARM64 container lifecycle: restart, upgrade, failed-candidate handling, backup/restore and removal preserving identity/config/data. Real provider/data assertions map to F5. |
| F2 — Edge and runtime dependencies | Edge + Hub | Exact receiver/proxy source pins, configuration and compatible toolchains; supported native/container installation, mTLS reception, encrypted bounded spool/backpressure, commit-before-ACK, stable deduplication, gap/error handling, outage/restart and safe upgrade/rollback/removal. Use fresh isolated synthetic inputs for deterministic failure tests; real passive evidence is F5. |
| F3 — Protocol, TS and Swift | Protocol + SDK owners | Exact canonical/vendored bindings, conformance and package versions; reproducible packages; packed external Node and real browser consumers; external SwiftPM consumers; declared active OS/compiler floors, including SDK-owned iOS harness where the support claim requires it. Strict TLS/auth/credential ownership, current/history/null/zero/unit/cursor/ETag/error semantics and recovery across final supported Hub artifacts. No App access. |
| F4 — Home Assistant | HA + Hub | Complete the documented supported installation/distribution paths, ordinary UI config, scheduler, devices/entities and zero/unknown semantics, outage recovery, reauth/reconfigure, reload, disable/re-enable, restart, diagnostics redaction, component update/rollback and unload/removal. Preserve config-entry/device/entity identities and unrelated HA storage. Check HA OS/Supervisor/HACS-related compatibility where actually claimed, without inventing a native HA daemon or requiring public catalogue submission. |
| F5 — real data and parity | Hub + affected consumers | Fresh owner-supplied read-only named-source export with expected provenance, import/parity checks and fresh authorized passive collection evidence or owner-run capture/receipt. Verify persisted and exposed data semantics, counts/order/bounds and exclusions. Never reuse old private fixtures or substitute synthetic data for this gate. No vehicle commands are needed for passive acceptance. |
| F6 — usable source distribution | Hub + all products | Build and install from clean exported source with pinned dependencies, manifests, notices and documented tooling. Ordinary Hub-only and selected-companion source setup/install/update/status/rollback/removal must work. Admit exact reachable GitHub commit identities and compatible catalog, not floating main or an empty production catalog. Cover supported native/container packages and resource-package consumers. Remove deferred Viewer from active examples/dependency selection without working in Viewer. Complete setup, upgrade, recovery and troubleshooting documentation. |
| F7 — combined final acceptance | Coordinator + independent reviewer | From fresh ordinary installation, prove import/authorized ingestion -> durable Hub data -> TS Node/browser, Swift and HA consumption; then disconnection, restart, credential recovery, upgrade, backup/restore and safe removal. Exercise Hub alone and each supported optional-component composition. Match exact artifacts and all F0 claims to F1–F6 receipts; zero unresolved blocking functional defects. |

All eight gates are mandatory. Optional deployment does not make a repository optional
for completion. HA can be sequenced later but is required. F5 may wait for owner input;
continue independent work, prepare the concrete missing-input request and keep the
overall objective incomplete until the required evidence is available.

## Delivery order and existing slices

1. Bound F0 from existing docs and evidence once, then implement. Do not spend cycles
   producing successive plan rewrites.
2. L1 begins F1/F2: one Debian installed Hub baseline -> candidate journey, then Edge.
   Establish a real predecessor before claiming upgrade/rollback. If unavailable,
   complete fresh install/restart/backup/removal and record the exact missing baseline.
3. L2 closes Apple-silicon installed behavior and floors in F1/F3. Hub macOS 13 and
   Swift macOS 14 need separate sequential receipts, at most one Mac guest at a time.
   Current macOS 27 evidence proves neither floor. Additional SDK-owned iOS evidence
   must use the SDK's own harness and isolated supported tooling, never App tooling.
4. L3 contributes to F3/F4/F6: deterministic source/packages, ordinary consumers,
   HA replacement and rollback, exact source bootstrap/catalog. Native/container
   composition, remaining Hub capabilities and support claims are in scope too.
5. D1 is mandatory within F5. Prepare the import/parity and passive-data harness while
   named inputs are unavailable, then verify those assertions with fresh input.
6. Close F7 only after all mandatory product and combined-path gates pass.

## Completion and dependencies

FULL_SOLUTION_ACCEPTED requires F0–F7 accepted and every active product required by
F0 complete. A build, health check, ACK, source review, synthetic journey or v7 handoff
alone cannot satisfy that outcome. Do not move unfinished supported behavior into
'later' merely to close the goal. Do not invent dates or completion percentages.

If a necessary baseline, old-OS image/hardware, supported test runtime, named export,
passive capture or account authorization is unavailable, name the exact missing input
and affected assertions. Complete other ready work; request the narrow missing input
when the dependent work is concrete. Never mark that gate passed or the goal achieved.

## Working rules and source storage

Use Sol/max coordination, Sol/high implementation and independent review, Luna/max
bounded exploration, no fast mode. Default to two useful workers, one writer per repo,
one heavy job under the shared lock. Hub owns shared VM/runtime integration. Respect
measured disk budgets; at most one Debian and one needed Mac guest. Fresh one-use
identities/handoffs; stop idle runtimes and preserve exact redacted evidence.

The goal, when sent, authorizes focused implementation, tests, isolated package and
runtime work, needed lean VM work, and validated source commits/pushes plus catalog
updates to the existing GitHub repositories so ordinary source installation works.
GitHub remains source storage. No CI, binary releases/assets, tags, signing/notarization
or production deployments. No force push, reset, clean, stash or discarded dirty work.

App and Viewer are excluded; x86/amd64/Intel/Azure stay paused. Preserve repository-roots,
private data, receipts and shared/App tools. No real Tesla account, public ingress,
collection or vehicle action without fresh explicit authority. Test vehicle-command
logic with local emulation only; never send a real action as a health check.
Keep automation paused. Preparing this plan starts none of these actions.
