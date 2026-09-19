# Hub adoption readiness plan

Revision: 2026-09-18. Owner: new GPT-5.6 Sol/max coordinator.

Deliver a usable Hub ecosystem so the App owner can begin v7 Hub adoption without
changing `app/` in this programme. Preserve working source and accepted narrow evidence;
replace the old coordination loop with bounded implementation and acceptance work.

Read [REASSESSMENT.md](REASSESSMENT.md) for current evidence and
[TASKS.json](TASKS.json) for gates and assigned owners. Product behavior is defined in
[PRODUCT_SPEC.md](PRODUCT_SPEC.md). Earlier plans are historical under
[SUPERSEDED_PLANS.md](SUPERSEDED_PLANS.md).

## Delivery sequence

1. **Restore the test foundation.** Reconcile source/writers, inventory disposable
   outputs, and provision one shared small Debian ARM64 VM. Use the host Mac for
   supported source/consumer work; create one Mac guest when isolated or minimum-OS
   acceptance needs it. Do not wait for deleted VMs.
2. **Make Hub work alone.** Complete the next observed setup/start/pair/read/history,
   reconnect, restart and data-preservation gaps on active targets through ordinary
   public interfaces. Minimum service/bootstrap work belongs here.
3. **Prove public consumers.** Complete packed TypeScript Node and browser journeys.
   Protocol supports exact profile/type/error/cursor agreement from the start.
4. **Prove Edge forwarding.** Fresh preparation, zero-event readiness, then a bounded
   synthetic delivery through receiver/spool/Hub plus deduplication and outage/restart
   recovery. An ACK alone is insufficient; verify committed Hub state/projection.
5. **Seal the Swift adoption boundary.** Reuse supported Mac/Debian Swift consumer
   evidence where identities match; close gaps against the final Hub cohort and write
   the App-facing integration reference outside `app/`.
6. **Accept the combined path.** Review the gates below and publish a local adoption
   handoff with exact versions, setup, limitations and reproducing commands. Then
   finish secondary Home Assistant and later product hardening/distribution.

TypeScript, Edge and Swift source work may overlap when independent. Do not move
Protocol to the end if a contract gap would force rework. Home Assistant is secondary;
Viewer has no active gate. No arbitrary percentage or deadline replaces evidence.

## Adoption gates

| ID | Required evidence | Owner |
| --- | --- | --- |
| G0 | Current ARM64 test environment, verified toolchains, measured disk use and single runtime owner | Hub/environment |
| G1 | Hub standalone Debian ARM64 setup, pairing, current/history, restart and retained identity/data | Hub |
| G2 | Equivalent supported Apple-silicon Mac Hub journey; minimum-OS limitations explicit | Hub |
| G3 | Exact Protocol profile and package bindings for public TS/Swift/Edge behavior; errors, trust and cursors agree | Protocol + consumers |
| G4 | Packed TS Node and real browser consumer, TLS/pairing/current/history/recovery and owned cleanup | TypeScript + Hub |
| G5 | Edge durable forwarding, committed/readable Hub result, duplicate/retry/outage/restart proof | Edge + Hub |
| G6 | External SwiftPM consumer against final supported Hub; ordinary API/auth/trust/recovery, no App modification | Swift + Hub |
| G7 | App v7 adoption reference: setup, contract/version bindings, code examples, limits, troubleshooting and exact evidence | Coordinator + Swift |

All eight gates must be accepted for `APP_V7_ADOPTION_READY`. Existing scoped passes
reduce work but do not automatically accept a new combined cohort. G7 is documentation
for the App owner, not App implementation or iOS runtime acceptance.

## Later obligations

Home Assistant real scheduler/config/reauth/unload flows; active-platform installed
upgrade/rollback/removal; named-source TeslaMate import/parity and passive data; polished
packages and distributions. Missing private source inputs must not stall independent
work. Full real-data/product acceptance is distinct from bounded synthetic adoption
readiness. x86/Intel/Azure remain paused. Viewer is considered only after other products
work and a later owner start.

## Execution

Follow [COORDINATION.md](COORDINATION.md), [ENVIRONMENT.md](ENVIRONMENT.md) and each
product PLAN. Correct code and deliver a tested slice every development cycle when
ready work exists. Do not spend cycles only rewriting status. If a gate cannot run,
name its missing input and continue an independent gate or source correction.
