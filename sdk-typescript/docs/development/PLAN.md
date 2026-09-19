# TypeScript SDK full-product completion plan — 2026-09-19

Objective: complete the public TypeScript SDK as a reproducible, caller-owned Node and
browser package covering every documented supported contract and the final installed
Hub ecosystem.

Authority: [master plan](../../../docs/development/MASTER_PLAN.md),
[product specification](../../../docs/development/PRODUCT_SPEC.md),
[coordination](../../../docs/development/COORDINATION.md), and [STATUS.json](STATUS.json).

## Current position

G3/G4 remain accepted for the exact packed `@teslatlas/sdk@2026.36.2` candidate and
closed synthetic Mac Hub cohort. Preserve those receipts unchanged. They do not prove
the full public SDK surface, all documented profiles, a durable distribution workflow,
support floors, named-source semantics or a combined installed ecosystem.

`full_solution_state` is `NOT_ACCEPTED`; this goal is drafted and not started.

## Required completion

- **F0:** produce a support ledger for root, Node and browser exports; current-Hub and
  rich profiles `1.0.0`–`1.2.0`; typed results/errors; ETag/304, opaque pagination,
  replay/gap and uncertainty behavior; metadata/command APIs; cancellation/disposal;
  TLS, invitation, identity, credentials and CORS; examples, Docker tooling and every
  Node/npm/browser/toolchain claim. Implement and prove each claim or correct it.
- **F3:** build the package reproducibly from the exact Protocol inputs and install it
  into clean external Node and browser consumers. Exercise the full claimed public
  behavior against the exact F1 Hub, including failure and recovery paths, without a
  source-tree import. Bind generated validators, exported types and runtime code to the
  admitted Protocol bytes.
- **F5:** consume the fresh named-source/import and passive-capture Hub results to prove
  the SDK preserves real field, unit, null/zero, pagination and history semantics. This
  input-dependent gate is mandatory for the full solution, not optional follow-up.
- **F6:** produce a deterministic unpublished npm archive with ordered contents,
  checksums, licences, documentation, examples, exact toolchain/support policy and
  clean install/update/removal. The Hub six-source catalog must install and retain this
  exact package for consumers. No npm publication is required without separate owner
  authority.
- **F7:** run fresh Node and real-browser consumers from the F6 artifact in the final
  combined installed ecosystem, including TLS/CORS, pairing, reads/mutations that the
  selected profile supports, restart, credential rotation/reauthentication and cleanup.

The SDK is a developer package, never a daemon. Its Docker path proves the documented
ARM64 development/consumer workflow only. The support ledger must state exact runtime
and browser floors from evidence rather than silently narrowing the claim to the
already accepted Chrome/Node tuple.

## Work slices

1. **L1:** finish F0 and close package/API defects needed by F3.
2. **L2:** prove all declared ARM64/macOS browser and Node support floors against F1.
3. **L3:** finish deterministic distribution/docs and catalog integration for F6,
   consume F5 real-input semantics, then pass F7.

## Start and boundaries

This plan does not authorize source changes, generation, package creation, tests,
runtime, commit, push or publication. Preserve the dirty `main` tree and accepted G3/G4
artifacts. Exclude App, Viewer, x86/amd64/Intel and Azure. Never embed or retain Hub,
Tesla or pairing credentials in the package, examples or receipts.
