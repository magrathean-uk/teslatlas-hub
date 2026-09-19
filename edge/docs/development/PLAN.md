# Edge full-product completion plan — 2026-09-19

Objective: complete Edge and its pinned Fleet Telemetry receiver as an optional,
recoverable installed component of the Hub ecosystem on every active documented
ARM64/Apple-silicon path.

Authority: [master plan](../../../docs/development/MASTER_PLAN.md),
[product specification](../../../docs/development/PRODUCT_SPEC.md),
[coordination](../../../docs/development/COORDINATION.md), and [STATUS.json](STATUS.json).

## Current position

G3/G5 remain accepted for the exact synthetic Debian ARM64 delivery lane. Preserve
their receipts and closed cohort. They do not prove native/container packaging,
minimum floors, complete spool migration/recovery, passive input, or final installed
integration. `full_solution_state` is `NOT_ACCEPTED`; this plan is not started.

## Required completion

- **F0:** inventory every Edge/receiver claim: receiver mTLS and bearer isolation,
  decoded envelope validation, bounded encrypted spool, stable identities, v1/v2 ACK
  behavior, gaps/dispositions, quotas/retention, retry/deduplication, spool-format
  migration and guards, doctor/status, TLS and credential rotation, native services,
  Docker Compose, backup/restore/rollback and removal. Resolve exact active ARM64 Linux
  and Apple-silicon macOS floors and correct any unsupported claim.
- **F2:** against the exact F1 Hub, prove receiver-first startup, loopback admission,
  durable write before acceptance, Hub commit before occurrence-bound ACK, outage and
  restart recovery, no duplicate projection, gap handling, backpressure/degraded
  states, rotation and bounded failure. Edge must remain optional to standalone Hub.
- **F5:** with a fresh authorized passive capture, prove real receiver mapping,
  timestamps/units/nulls, unsupported durable dispositions, interruption/replay and
  redaction. The capture is mandatory external input. It must be read-only/passive and
  must not cause account, command or vehicle action.
- **F6:** prove source-built Debian 13 ARM64 packages/systemd, Apple-silicon macOS 13+
  packages/LaunchAgents and the Linux ARM64 Compose path. Each must cover install,
  configuration, upgrade, spool-format transition, verified backup/restore, rollback
  and spool-preserving removal. The Hub catalog must install/update/status/rollback/
  remove the exact Edge source without Viewer.
- **F7:** run the F6 Edge/receiver artifacts in the final installed ecosystem and show
  durable forwarding, recovery and cleanup alongside the SDKs and required HA lane.

The external receiver/proxy is a bounded audited dependency. Pin its source/version,
toolchain, binary hash, legal inputs, configuration schema, TLS/bearer files, users,
listeners and service order. Do not expand into unrelated upstream development.

## Work slices

1. **L1:** complete F0 and Debian ARM64 installed F2 lifecycle after F1 Hub exists.
2. **L2:** close Apple-silicon package/floor and ARM64 container claims.
3. **L3:** complete catalog/reproducibility/docs for F6, run mandatory F5 when fresh
   input exists, then contribute the final F7 receipt.

## Start and boundaries

No implementation, build, package, guest, receiver, event, test, commit, push or
publication is authorized by this draft. Preserve the dirty `main` tree and accepted
receipts. Hub owns shared runtimes; Edge owns its package, receiver binding and spool.
Exclude App, Viewer, x86/amd64/Intel and Azure. Never reuse old receiver traffic,
credentials, CAs, roots or closed cohorts; never expose the receiver through a generic
TLS-terminating proxy.
