# Active scope — App v7 Hub adoption readiness

Revision: 2026-09-18 reset. This replaces earlier active scope amendments.

Primary delivery order: **Hub → TypeScript SDK → Edge → Swift SDK**.
Protocol supplies the contract baseline from the start and runs in parallel only
when a concrete contract gap needs correction. Swift integration preparation may
advance alongside TypeScript/Edge because Swift is the App-facing adoption boundary.
Home Assistant remains a secondary product; it does not block starting App v7 adoption.

Targets: Debian 13 ARM64 and supported Apple-silicon macOS. Preserve declared OS
and toolchain floors. x86/amd64/Intel/Azure and a full architecture matrix are paused.
The owner deleted the VMs and now authorizes minimal replacements under
[ENVIRONMENT.md](ENVIRONMENT.md); waiting for the deleted guests to return is obsolete.

`app/` is untouched. Viewer has no active goal, work item, package dependency or
acceptance gate. It may be reconsidered last, after the other products work and
with a later explicit start. Preserve its source and historical evidence.

Near-term completion means the concrete adoption gates in [MASTER_PLAN.md](MASTER_PLAN.md)
pass with documented limits. Full parity, polished distribution and Home Assistant
completion remain distinct later obligations. A source test or old guest receipt
cannot establish current environment or end-to-end acceptance.
