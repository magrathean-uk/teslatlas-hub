> OWNER RESTART — 2026-09-18: The owner explicitly resumed development on the six in-scope products and ecosystem orchestration. Do not use fast mode. Resume Hub with Sol/high and Protocol, TypeScript SDK, Swift SDK, Home Assistant and Edge with Luna/max from preserved checkpoints. Viewer remains excluded and all x86/amd64/Intel work remains paused. The App keeps its separately authorized scope.

> APP-ONLY RESTART EXCEPTION — 2026-09-12T16:53:09.669326+00:00: The App task reports the owner directly authorized continued App work. App v6, its localization workers and its own heartbeat may resume under that task. Hub, Protocol, TypeScript SDK, Swift SDK, Home Assistant, Edge and ecosystem coordination remain paused until explicit owner restart. Viewer remains excluded; x86 remains paused.

> OWNER PAUSE — 2026-09-12T16:51:03.456724+00:00: The owner paused this work and all development on this project. Stop development, reviews, tests, builds, runtime preparation, delegation and polling until explicit owner restart. This supersedes earlier restart and standing execution authority, including r11. Preserve dirty source, evidence and goals. Hourly coordination is PAUSED. Viewer remains excluded; x86 remains paused.

# Active scope: working ARM64 and Mac products

> OWNER SCOPE — 2026-09-12: Viewer is excluded from all active development, goals, plans, packaging and acceptance dependencies. Preserve its existing source and historical evidence; do not schedule or resume Viewer work. Six products remain: Hub, Protocol, TypeScript SDK, Swift SDK, Home Assistant and Edge.


> OWNER RESTART — 2026-09-12: The owner explicitly said restart on all. Development and hourly coordination resume from preserved checkpoints for Debian ARM64 and Apple-silicon Mac. The 2026-09-10 global pause is lifted; x86 remains paused. Hub Sol/high, companions Luna/max. Preserve existing goals, dirty checkouts and completed evidence; revalidate current runtime state and use fresh expiring inputs.

Owner direction: 2026-09-09. This revision takes precedence over older stage-order, platform-matrix and goal wording in this programme.

The owner asked: “pause on x86 bits and focus on arm64 and mac only. for all. our first aim is to get a working product(s). amend plans etc and goals for each. keep being the orchastrator and herd the sheep.”

## Active targets and deferred work

- Active: Debian 13 ARM64 and macOS on Apple silicon, using the existing `debian13-arm64` and `macos13-arm64` guests and an appropriate existing local Mac runtime. Preserve declared support floors; identify an actual tool/runtime mismatch before choosing another supported existing Mac lane.
- Paused for every product: x86, x86_64, amd64 and Intel Mac development, compilation, image acquisition, VM provisioning, runtime/lifecycle tests and acceptance work. These rows are deferred, not failed or passed. They do not block the active ARM64/Mac milestone. Resumption requires a new owner direction.
- The recently created `debian13-x86_64-20260909` VM and its artifacts remain preserved and unadmitted. This work pause does not request shutdown, deletion, repair or cleanup of existing runtime state. No task may use it for development or validation.
- Keep x86 code, schemas, historical receipts and evidence intact. Do not remove support or weaken shared validators just to obtain an ARM64 result. Mark planning rows and active goal scope as paused; do not rewrite history.
- Azure and a complete cross-architecture matrix are deferred. No Azure request or new VM is needed for the current aim.

## First aim and execution order

Deliver usable products on the two active platforms. Hub must work alone. Optional supported companions follow the working Hub-only path. Use the existing bootstrap, local-candidate installation and normal public interfaces. Complete the minimum packaging or dependency work needed to run those products; polished distribution and broad final matrices follow later.

1. **Working Debian ARM64 stack:** verify the ordinary Hub-only start, pair, display, restart and data-preservation path. Reuse current matching evidence; correct the next observed gap rather than rebuilding completed fixtures.
2. **Working Mac path:** provide a runnable Hub path on Apple silicon, with setup, trust/pairing, usable administration/browser UI, restart and retained data. Exercise the supported Swift consumer on an available declared runtime. Minimum launch/service integration needed for use is in scope.
3. **Optional products on active targets:** Protocol and SDKs are usable developer resources; HA integrates with an explicitly selected supported HA runtime; Edge starts and forwards durably under a bounded Hub-owned development handoff. HA is not a native macOS daemon. A Mac can use the selected ARM64 HA runtime; do not create a replacement VM to simulate native HA.
4. **Reliability and real data:** continue relevant ARM64/Mac recovery work and source corrections. Named-source TeslaMate admission, real import and passive provenance remain explicit acceptance gaps until their required inputs exist. Missing source credentials/backup must not stall independent working-product tasks. Seeded/synthetic data can prove operation, never real collection parity.
5. **Delivery and final acceptance:** finish ARM64/Mac packaging and lifecycle after the working paths. Keep x86 and full-matrix obligations in the deferred backlog.

B1/B2/R1/D1/X1 identifiers remain useful evidence labels. Their old strict sequence is superseded where it would prevent ready ARM64/Mac usability work. No stage is promoted merely by reprioritization.

## Goal amendments and next ownership

Each existing task updates its sole `docs/development/PLAN.md`, `STATUS.json` saved `goal_objective`, active targets, deferred targets, next action and blocker.

The owner subsequently deleted all seven old native goals and explicitly authorized their replacement on 2026-09-09. Each existing task must verify the native state, create exactly one fresh goal for its usable ARM64/Mac outcome when no unfinished goal remains, and verify the resulting objective and status. No token budget was requested. Record actual native state separately from execution progress; do not copy the deleted goal's blocked status. This supersedes the earlier workaround retaining immutable full-plan wording. Do not create duplicate tasks or falsely complete an unfinished goal.

The fresh goals cover the stated working-product outcomes and the minimum setup, integration and reliability needed to use them. Named-source parity, polished distribution and deferred architecture gates remain separate plan obligations; they are not prerequisites for completing these working-product goals. Complete a fresh goal only when its actual usable outcome has been verified.

| Task | Immediate owned objective |
| --- | --- |
| Hub | Own the working ARM64/Mac Hub-only routes; identify the next missing Mac step and supply the smallest fresh consumer handoff. Coordinate all shared guest/runtime changes. |
| Protocol | Keep current-Hub contracts and developer assets aligned with the active ARM64/Mac consumers; supply concrete contract fixes or consume new runtime evidence. Deferred x86 rows cannot block usable resources. |
| TypeScript SDK | Deliver usable packed Node/browser consumers for ARM64/Mac through its public SDK interfaces. Select active target rows only; named-source parity remains a separate gap. |
| Swift SDK | Deliver a normal SwiftPM consumer on supported Apple-silicon macOS and, where supported, Debian ARM64. Surface the exact runtime-floor issue if present; do not change the App or silently raise/lower deployment floors. |
| Home Assistant | Keep the accepted ARM64 Container integration usable and complete the next active-target integration/lifecycle gap. Preserve the running lanes; no x86 work or invented native Mac HA service. |
| Edge | Deliver usable ARM64 service/receiver/forwarding and supported Apple-silicon Mac operation. Use fresh bounded development inputs only when the remaining test requires them; no reuse of closed r12/r13 runs. |

## Coordination and resource rules

The coordinator continues hourly and routes changed dependencies. Hub uses Sol/High; the five in-scope companions use Luna/Max. Keep one writer per repository: Hub owns all Hub/shared installer/runner files and shared VM changes. Companions propose bounded changes to Hub and do not direct each other to bypass the coordinator's scope.

Only one heavy build or VM installation may run at once. Acquire `~/dev/teslatlas-lab/locks/heavy-build` atomically and record task/PID before starting, including guest builds. A plan, task title or assistant checkpoint is not an authorization handoff. Shared runtime work needs a concrete Hub-owned target, exact action and cleanup owner recorded before execution. Reuse the two active guests; do not create new ones.

Continue ready work without repetitive green-test reruns or peer polling. When no executable work remains, report the exact dependency once and end the turn; the coordinator resumes the affected task when inputs change. Preserve dirty independent `main` checkouts. App work, production/vehicle actions, commits, pushes, releases, CI, publication and usage resets remain outside the authorization.
