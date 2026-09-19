# Hub full-product completion plan — 2026-09-19

Objective: deliver a complete, independently usable Hub and the integrated six-product
Hub ecosystem. App v7 adoption readiness is an accepted historical milestone, not the
completion criterion.

Authority: [master plan](../../../docs/development/MASTER_PLAN.md),
[product specification](../../../docs/development/PRODUCT_SPEC.md),
[coordination](../../../docs/development/COORDINATION.md), and [STATUS.json](STATUS.json).

## Current position

G0–G7 and the bounded Home Assistant Container journey remain accepted exactly within
their recorded scopes. Their receipts, hashes and closed runtime identities are
immutable. They prove useful source-built synthetic journeys; they do not prove the
complete supported feature set, ordinary installation and lifecycle, minimum floors,
real-source parity, reproducible distribution or the final installed ecosystem.

`full_solution_state` remains `NOT_ACCEPTED`. This plan is drafted and not started.

## Full-product gates

- **F0 — feature and support ledger.** Inventory every user-visible and operator-facing
  claim in current Hub documentation, configuration, CLI, Mac UI, package/container
  recipes and public contracts. For every claim record its implementation owner,
  target/platform floor, distribution path, dependencies, tests and required runtime
  evidence. A claim must be implemented and proven or corrected before acceptance.
  The ledger includes setup and diagnostics, Legacy and Fleet collection adapters,
  pairing/trust, current/history and sync/export behavior, TeslaMate import, repair,
  retention, backup/recovery and all ordinary lifecycle commands.
- **F1 — standalone Hub.** Prove a fresh operator can build and use Hub without any
  companion on Debian 13 ARM64 and Apple-silicon macOS 13, plus the documented
  ARM64 container path. Cover first setup, restart/persistence, degraded and failed
  setup, TLS/device lifecycle, collection-adapter behavior, data operations and
  documented UI/CLI diagnostics. Vehicle-command logic may use local provider
  emulation only; no real vehicle action is permitted.
- **F2 — Edge path.** After F1, accept the pinned receiver/proxy and Edge delivery
  path under Edge's product plan. Hub must prove exact configuration, service order,
  commit/ACK/deduplication, restart and recovery boundaries without making Edge
  mandatory for standalone Hub.
- **F3 — Protocol and SDK consumers.** Admit the complete Protocol, TypeScript and
  Swift supported surfaces, packages, declared floors and recovery behavior against
  the exact Hub build. Keep product, HTTP-profile, Edge-profile and storage versions
  separate.
- **F4 — Home Assistant required.** Home Assistant is required for overall completion,
  though it may execute after the primary F1–F3 lanes. Accept its supported install,
  update, recovery and ordinary user journey against the exact Hub.
- **F5 — real input and parity.** A fresh owner-supplied, read-only named-source export
  and a fresh separately authorized passive capture are mandatory input-dependent
  gates. Prove field/unit/null/zero/history/import parity, passive ingestion,
  interruption/recovery and redaction. Never reuse historical private inputs and do
  not wake, command or mutate a vehicle or source.
- **F6 — reproducible distribution and usable documentation.** Prove source-built
  Debian ARM64 package, Apple-silicon package, ARM64 container and source handoffs,
  including install, upgrade, rollback, verified backup/restore and data-preserving
  removal. The shipped companion source catalog must admit exact immutable sources for
  the six active repositories only; ordinary install/update/status/rollback/removal
  must work without Viewer. An unavailable source or unsupported target must fail
  before activation. GitHub remains source storage; publication/release actions need
  their own authority.
- **F7 — combined installed end to end.** From clean supported hosts, run the complete
  installed ecosystem using F6 artifacts: Hub alone, each supported optional-component
  composition, the selected collection path, Protocol-bound TypeScript and Swift
  consumers, and required Home Assistant. Prove upgrade/restart/recovery and final
  cleanup with no hidden source-tree dependency.

## Hub-specific acceptance

F0 must map the exact documented Mac app and service, Debian systemd, Docker Compose,
Legacy/Fleet, migration/import, sync/export/repair/retention, pairing/API, backup and
recovery behavior. The Mac UI must complete setup, diagnostics, service control,
upgrade and data-preserving uninstall on its declared floor. Debian and container
paths must cover their documented capabilities and limitations rather than borrowing
Mac evidence.

External Fleet receiver and command-proxy dependencies are bounded dependencies, not
new products. Audit and pin their source/version, build, legal inputs, configuration,
credentials, service topology and Hub integration. Use source tests or local provider
emulation for command logic. Do not perform production, account or vehicle actions.

The companion bootstrap currently has no admitted ordinary source cohort. F6 must
populate and verify the exact six-repository catalog, exclude Viewer, and exercise
the real operator commands and rollback records. Hub remains usable when companions
are absent or one optional companion is unavailable.

## Work slices

L1–L3 are execution slices only; none is a completion substitute:

1. **L1:** close F0, then implement and prove Debian ARM64 F1/F2 installed lifecycles.
2. **L2:** prove Apple-silicon floors and native lifecycles for F1–F4.
3. **L3:** close F3/F4 packaging, the six-source catalog, F6 documentation and
   reproducibility, then run F5 when its fresh inputs exist and finish with F7.

## Start and boundaries

This draft does not authorize implementation, builds, tests, runtime, VM work,
catalog mutation, commit, push or publication. The future coordinator goal must be
explicitly sent before any execution begins.

Preserve the independent dirty `main` checkout and immutable G0–G7 evidence. Hub owns
shared runtimes, fixtures, installers and integration orchestration. Exclude App,
Viewer, x86/amd64/Intel and Azure. Do not inspect private closed runtime roots or reuse
credentials, endpoints, CAs, invitations, source exports, captures or receipts.
