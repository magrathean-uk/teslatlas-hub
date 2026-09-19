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
`NOT_ACCEPTED`. F0 passed independent review; F6 source work is ready while F4 runtime
work waits for the accepted F1/F3 Hub and contract artifacts. The exact current
`dfb2b05067a88ba02e6dbfbd6db614f8a99b12f4` source export and 31-file component
payload are now prepared for independent Hub catalog review; this source-only
handoff does not update the Hub catalog or replace the older runtime receipt.

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

## Current F6 source handoff

The tracked
[`f6-current-source-payload-handoff-2026-09-19-r1.json`](f6-current-source-payload-handoff-2026-09-19-r1.json)
binds the clean Git tree, deterministic archive, complete 102-file source manifest,
Hub-compatible 101-file source record, exact `hub-http-v1@1.0.0` profile and the
HACS-shaped 31-file `custom_components/teslatlas_hub` payload. The payload manifest
SHA-256 is `ebf7d09f88e2ce8f80ea71b8b2ae0fb7569ae3521dea038b643b6cfe626439ca`;
the current-source catalog selection receipt SHA-256 is
`99fdac66526bdec84d2429100f419eebedec588b0d32060c8f0f0a19bf43f2b3`.
Independent Sol/high review accepted the source/payload candidate at receipt SHA-256
`c228aa6a6df37e6a3fb0e2e61a432cec8b29e6178118fbdcfd7eb0b13a1c0613`
with no blocking findings and confirmed that the 102-file Git export becomes the
101-file Hub source record solely because the Hub walker excludes `AGENTS.md`.
The metadata-only receipt delta awaits same-reviewer confirmation before Hub catalog
admission. No manual, Container, HA OS, replacement/rollback, runtime, F4, F6 or F7
acceptance is claimed.

## Start and boundaries

The sent goal authorizes bounded source, Container/image, isolated runtime/guest,
test, unpublished package and validated source commit/push work. HACS submission,
release publication, CI and production remain unauthorized. Preserve the dirty `main`
tree, HA state and accepted receipt. Hub owns shared handoffs; HA owns component state.
Exclude App, Viewer, x86/amd64/Intel and Azure. Never reuse closed private inputs.
