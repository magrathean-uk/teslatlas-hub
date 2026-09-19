# Home Assistant full-product completion plan — 2026-09-19

Objective: complete the Home Assistant integration as a required part of the overall
Hub ecosystem, with supported installation, lifecycle, recovery and ordinary user
operation against the final Hub.

Authority: [master plan](../../../docs/development/MASTER_PLAN.md),
[product specification](../../../docs/development/PRODUCT_SPEC.md),
[coordination](../../../docs/development/COORDINATION.md), and [STATUS.json](STATUS.json).

## Current position

The bounded Debian 13 ARM64 Home Assistant Container 2026.8.3 journey remains accepted
and closed. Its receipt is immutable. It does not prove full manual/package lifecycle,
headed UI, replacement/image upgrade and rollback, real-source semantics, supported
distribution or the final installed ecosystem. `full_solution_state` is
`NOT_ACCEPTED`; this plan is drafted and not started.

## Required completion

- **F0:** inventory all integration claims: config and reauth flows, identity and TLS,
  polling cadence/concurrency/backoff, devices/entities and translations, dynamic
  vehicles, zero/unknown/unavailable semantics, diagnostics/redaction, migration,
  reload/reconfigure/disable/unload, manual install, Container, existing selected
  HA OS/VM targets, component/image replacement, backup/rollback and HACS-shaped
  distribution. Record exact Home Assistant/Python/Docker/runtime floors and either
  prove or correct each supported claim.
- **F4:** this gate is required for overall completion. Against the exact F1/F3 Hub,
  prove the normal headed user journey and API behavior in the supported Debian ARM64
  Container plus the documented manual installation into a selected existing supported
  runtime. Cover pairing/replay rejection, polling and changed observations,
  reauthentication, outage/restart, reload/reconfigure, registry stability, migration,
  replacement/image upgrade, invalid-candidate rollback, backup/restore and removal.
  A Mac may consume or stage to an explicitly selected runtime; it does not become a
  native HA daemon.
- **F5:** use the fresh named-source/import and passive-capture Hub evidence to validate
  every mapped entity's units, numeric zero, null/unknown/unavailable transitions,
  dynamic vehicle behavior, stale data and recovery. This input-dependent evidence is
  mandatory, with identifiers and locations redacted.
- **F6:** produce a deterministic installable component archive/source handoff with
  exact profile, manifest, translations, licences, checksums and operator docs. Prove
  manual and Container install/update/status/rollback/removal and Hub-catalog staging
  without Viewer. HACS submission/publication remains a separate external action unless
  explicitly authorized; any supported-distribution claim selected in F0 must still be
  fully evidenced.
- **F7:** run the F6 component in the final combined installed ecosystem, preserving
  config entry, devices, entities, unrelated HA state and credentials across ecosystem
  restart/upgrade/recovery, then perform owned cleanup.

HA may execute after primary Hub/Edge/SDK work, but F7 cannot be accepted without F4.

## Work slices

1. **L1:** complete F0 and any component gaps independent of a runtime.
2. **L2:** prove supported runtime floors and full F4 lifecycle against F1/F3.
3. **L3:** complete F5 semantics and F6 package/catalog/docs, then pass F7.

## Start and boundaries

No source change, Container/image work, runtime, guest, test, package, commit, push,
HACS submission or publication is authorized by this plan. Preserve the dirty `main`
tree, HA state and accepted receipt. Hub owns shared handoffs; HA owns component state.
Exclude App, Viewer, x86/amd64/Intel and Azure. Never reuse closed private inputs.
