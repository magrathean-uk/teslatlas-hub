> OWNER RESTART — 2026-09-18: The owner explicitly resumed development on the six in-scope products and ecosystem orchestration. Do not use fast mode. Resume Hub with Sol/high and Protocol, TypeScript SDK, Swift SDK, Home Assistant and Edge with Luna/max from preserved checkpoints. Viewer remains excluded and all x86/amd64/Intel work remains paused. The App keeps its separately authorized scope.

> APP-ONLY RESTART EXCEPTION — 2026-09-12T16:53:09.669326+00:00: The App task reports the owner directly authorized continued App work. App v6, its localization workers and its own heartbeat may resume under that task. Hub, Protocol, TypeScript SDK, Swift SDK, Home Assistant, Edge and ecosystem coordination remain paused until explicit owner restart. Viewer remains excluded; x86 remains paused.

> OWNER PAUSE — 2026-09-12T16:51:03.456724+00:00: The owner paused this work and all development on this project. Stop development, reviews, tests, builds, runtime preparation, delegation and polling until explicit owner restart. This supersedes earlier restart and standing execution authority, including r11. Preserve dirty source, evidence and goals. Hourly coordination is PAUSED. Viewer remains excluded; x86 remains paused.

> OWNER SCOPE — 2026-09-12: Viewer is excluded from all active development, goals, plans, packaging and acceptance dependencies. Preserve its existing source and historical evidence; do not schedule or resume Viewer work. Six products remain: Hub, Protocol, TypeScript SDK, Swift SDK, Home Assistant and Edge.

> OWNER RESTART — 2026-09-12: The owner explicitly said restart on all. Development and hourly coordination resume from preserved checkpoints for Debian ARM64 and Apple-silicon Mac. The 2026-09-10 global pause is lifted; x86 remains paused. Hub Sol/high, companions Luna/max. Preserve existing goals, dirty checkouts and completed evidence; revalidate current runtime state and use fresh expiring inputs.

# Teslatlas service workspace

Active roots: `app/` (iOS + `teslatlas-core`), `hub/`, `teslatlas-protocol/`,
`teslatlas-sdk-typescript/`, `teslatlas-sdk-swift/`, `teslatlas-viewer/`,
`teslatlas-home-assistant/`, and `teslatlas-edge/`. Authority: `WORKSPACE_AUTHORITY.md`.

Current programme authority: `docs/development/MASTER_PLAN.md` and
`docs/development/PRODUCT_SPEC.md`; each product uses `docs/development/PLAN.md`.
Current task state is in `docs/development/TASKS.json`. The owner requires a
working products on Debian ARM64 and Apple-silicon Mac first; all x86 development and acceptance work is paused. See `docs/development/ACTIVE_SCOPE.md`. Minimum setup needed for use comes first; polished installers and final platform gates come later. Missing named-source parity input must not stall independent ARM64/Mac work. The owner has started development: Hub uses Sol/High; in-scope companions use Luna/Max. See `docs/development/START_AUTHORIZATION.md`. Historical
compatibility evidence remains in `hub/docs/compatibility/execution-state.json`. Keep the existing independent
`main` checkouts and preserve unrelated edits. GitHub is source storage only;
no CI, releases, or artifact uploads without an explicit request.

## Token budget

- Skills: `teslatlas-core-nav`, `teslatlas-ffi`, `teslatlas-cargo` (on demand).
- Graph: `Users-bolyki-dev-source-teslatlas-service-app-teslatlas-core`, `Users-bolyki-dev-source-teslatlas-service-hub`.
- Crate docs: MCP `rust-docs`. Apple/general docs: MCP `context7`.
- LSP: rust-analyzer, SourceKit-LSP, clangd via `.grok/lsp.json`.
- Do not search `vendor/`, `upstream/`, `repository-roots/`, `fixtures/teslamate-postgres/`, `TeslatlasCore.xcframework/`.
- `app/CONTEXT.md` is vocabulary. Load it only when the task is domain terms, not for every Rust edit.

## Local execution

Run task-relevant disposable local checks and repair failures without repeated approval when the lane is open. Existing owner pauses, workspace authority, production and release gates remain in force.
