> OWNER RESTART — 2026-09-18: The owner explicitly resumed development on the six in-scope products and ecosystem orchestration. Do not use fast mode. Resume Hub with Sol/high and Protocol, TypeScript SDK, Swift SDK, Home Assistant and Edge with Luna/max from preserved checkpoints. Viewer remains excluded and all x86/amd64/Intel work remains paused. The App keeps its separately authorized scope.

> APP-ONLY RESTART EXCEPTION — 2026-09-12T16:53:09.669326+00:00: The App task reports the owner directly authorized continued App work. App v6, its localization workers and its own heartbeat may resume under that task. Hub, Protocol, TypeScript SDK, Swift SDK, Home Assistant, Edge and ecosystem coordination remain paused until explicit owner restart. Viewer remains excluded; x86 remains paused.

> OWNER PAUSE — 2026-09-12T16:51:03.456724+00:00: The owner paused this work and all development on this project. Stop development, reviews, tests, builds, runtime preparation, delegation and polling until explicit owner restart. This supersedes earlier restart and standing execution authority, including r11. Preserve dirty source, evidence and goals. Hourly coordination is PAUSED. Viewer remains excluded; x86 remains paused.

# Product task coordination

> OWNER SCOPE — 2026-09-12: Viewer is excluded from all active development, goals, plans, packaging and acceptance dependencies. Preserve its existing source and historical evidence; do not schedule or resume Viewer work. Six products remain: Hub, Protocol, TypeScript SDK, Swift SDK, Home Assistant and Edge.


> OWNER RESTART — 2026-09-12: The owner explicitly said restart on all. Development and hourly coordination resume from preserved checkpoints for Debian ARM64 and Apple-silicon Mac. The 2026-09-10 global pause is lifted; x86 remains paused. Hub Sol/high, companions Luna/max. Preserve existing goals, dirty checkouts and completed evidence; revalidate current runtime state and use fresh expiring inputs.

## Current authorization and models

The owner started all six in-scope tasks on 2026-09-08, then deleted their old native goals and authorized fresh ARM64/Mac working-product goals on 2026-09-09. [START_AUTHORIZATION.md](START_AUTHORIZATION.md) supersedes the previous planning hold and proposed model mix. Each existing task verifies native state, creates one revised goal if none is unfinished, verifies its actual objective/status, then continues ready owned work. Do not invent native goal status; distinguish activation requested from confirmed active. Historical full-plan wording and blocked status must not be copied into the new goal.

| Work | Model | Reasoning |
| --- | --- | --- |
| Hub | GPT-5.6 Sol | High |
| Protocol, TypeScript SDK, Swift SDK, Home Assistant, Edge | GPT-5.6 Luna | Max |
| Hourly coordinator | GPT-6 Astra | Medium |

The user's “Luma 5.6” refers to the available GPT-5.6 Luna model in this task context. Apply overrides per product task; do not change global defaults. All six in-scope tasks may start now, with later-stage tasks performing independent preparation/corrections until their actual upstream runtime is ready. Keep one source writer per repository and only one heavy build or VM installation at a time, using the shared lock described in the authorization. Hub owns changes to the primary VM; helpers request bounded setup/handoffs.

Do not escalate models or duplicate workers without a concrete reason and current authorization. Read the current milestone and relevant evidence, not the entire archive on each continuation.

## Ownership

- Coordinator: master/specification, stage release, `TASKS.json`, shared environment and source-plan reconciliation.
- Hub task: `hub/` including collector, public API, bootstrap, aggregate installer, component inventory, shared fixtures/installed runner and integration ledger.
- Each companion task: its own root, payload, public interface, focused tests, package metadata, documentation and `docs/development/STATUS.json`.
- App: excluded. No App implementation or plan replacement.

The source roots are the existing `main` checkouts. No alternate worktree, branch, reset, clean, commit, push, CI or release is authorized. Preserve unrelated dirty files and source history. Shared resources require an explicit owner; never let a companion edit Hub while the Hub task writes it.

## Dependency handoff

Use this compact structure in the product status and task message:

```text
Stage / product milestone:
Result and source identity:
Evidence path and what it proves:
Needed input, if any:
Owner of that input:
Exact event that makes the next step ready:
Next ready owned action:
```

For the primary bootstrap, a Hub handoff gives the documented invocation, service address, accepted current profile, candidate/package identity, private location of pairing/CA inputs, selected helper/config paths and how to stop/restart the isolated runtime. It never sends secrets in chat. Later final-matrix handoffs include the existing runner's required private descriptors and installed-host evidence.

Do not make a full final-matrix descriptor a prerequisite for the first ordinary Hub journey unless the current code actually requires that boundary and cannot reuse its normal public interfaces. Keep the first bootstrap simple; strict final acceptance remains a later obligation.

When blocked by another product, record the dependency and continue ready independent owned work. Do not claim `complete`, rerun unchanged green tests or keep sending periodic messages. The coordinator resumes only affected tasks when the dependency changes. Use native goal blocked status only under its actual repeated-blocker rules, not because one task's next step requires normal coordination.

## Active priorities and platform pause

The 2026-09-09 owner amendment in [ACTIVE_SCOPE.md](ACTIVE_SCOPE.md) supersedes the previous strict stage release order wherever it would block ready working-product steps. Focus all six in-scope tasks on Debian ARM64 and Apple-silicon Mac. Pause every x86/amd64/Intel Mac build, VM, image, runtime, lifecycle and acceptance action; preserve existing state and deferred requirements. No cleanup or shutdown is implied.

1. Finish the actual ordinary Hub-only route on Debian ARM64.
2. Deliver the usable Mac Hub route and supported Swift consumer on existing Apple-silicon runtimes.
3. Finish optional active-target product paths in their actual roles; do minimum bootstrap/dependency/service work needed to use them.
4. Continue relevant reliability work. Named-source parity stays pending until its inputs exist, while independent usability advances.
5. Polish active-target distribution and lifecycle after the working paths. Cross-architecture and Azure gates remain deferred.

The coordinator continues the hourly heartbeat, routes changed dependencies and resumes useful owned work. Hub owns shared guest changes and is the only Hub writer. A shared runtime handoff names the exact target, action, private input locations and cleanup owner before execution. No new VMs or inferred checkpoint authorization. Model and heavy-build rules remain unchanged.

## Completion

Each fresh product goal covers its stated usable ARM64/Mac outcome, including the minimum setup, integration, restart and data-preservation behavior appropriate to that product. Complete it only when that outcome has actual supporting evidence. The canonical plan also retains later reliability, named-source parity, polished delivery and deferred architecture obligations; those are separate from the fresh working-product goal. A compilation, recorded prior pass, synthetic test, static Docker configuration or environment boot is never a substitute for the relevant acceptance evidence. Bounded synthetic usability evidence does not establish real collection parity or full product acceptance.

## Shared VM access

Use [VM_ACCESS.md](VM_ACCESS.md) for the prepared `debian13-arm64` and `macos13-arm64` guests, verified SSH/login instructions and private credential locations. Use the shared wrapper and lab directories; do not create per-product duplicate VMs or reuse retained baseline/quarantine guests as fresh development environments. Docker work belongs in the primary 32 GiB Debian guest when that development step is started.

The Colima retirement removed old test containers and image caches. Their historical receipts remain historical observations, not evidence of an available current runtime. Rebuild a missing candidate/tool when its owned stage needs it, preserving exact source/profile identities; an unchanged source commit does not make a deleted image usable.
