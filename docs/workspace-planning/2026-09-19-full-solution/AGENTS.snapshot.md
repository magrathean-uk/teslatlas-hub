# Teslatlas service workspace

Current owner scope: full solution, 2026-09-19. Read `WORKSPACE_AUTHORITY.md`, then
`docs/development/MASTER_PLAN.md` and your product's `docs/development/PLAN.md`.
The objective is a fully working Hub ecosystem across all six active products through F0–F7.
App v7 readiness is an accepted intermediate milestone, not completion. The full
goal is drafted but not started. Do not inspect, edit, build, test, clean or delegate work in `app/`.
Viewer is deferred until the other products work; it has no active task or dependency.
All x86/x86_64/amd64/Intel work remains paused.

## Models and ownership

- New coordinator: GPT-5.6 Sol / max. Implementation and technical review: Sol / high.
- Focused exploration: GPT-5.6 Luna / max. No fast mode. Do not silently substitute models.
- Preserve independent dirty `main` checkouts. One writer per repository; Hub owns shared runtimes.
- Legacy product tasks are archived. Use bounded subagents in the new coordinator task;
  do not resume old queues or create a fleet of persistent product chats.
- Planning reset is complete when the handoff files are verified. Development and lean VM
  reconstruction are authorized for the new coordinator launched with `HANDOFF_PROMPT.md`.

## Efficient execution

Use `rg` and bounded reads; use `codebase-memory-mcp` for structural lookup when available.
Do not search `vendor/`, `upstream/`, `repository-roots/`,
`fixtures/teslamate-postgres/`, or `TeslatlasCore.xcframework/`.
`repository-roots/` contains live Git storage: never delete it.
Crate documentation: rust-docs MCP; library documentation: context7 when relevant.
Use existing `scripts/dev/` wrappers after checking their current paths/toolchain.
Only one heavy build or VM installation at once; use the shared lab lock.
Run focused checks for changed behavior and required acceptance gates. Do not rerun
unchanged suites or poll inactive tasks. Source commit/push authority is limited to
the current upload request and the explicit full goal when the owner sends it.
No CI, release, tag, binary publication or production actions.
