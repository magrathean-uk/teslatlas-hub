# Working on Teslatlas Edge

Edge is an optional Fleet Telemetry ingress for a user-operated Hub. Keep it
narrow: receiver admission, encrypted bounded storage, and authenticated Hub
pull and acknowledgement. Account credentials, vehicle commands, consumer APIs
and mandatory hosted relays do not belong here.

## Boundaries

- Preserve receiver loopback admission, Hub mTLS plus bearer authentication,
  bounded encrypted spooling, at-least-once delivery and Hub-side deduplication.
- A Hub acknowledgement follows durable commit. Preserve occurrence-bound
  receipts, monotonic sequences and durable gap notices across restart.
- Never log VINs, coordinates, payloads, bearer values or private keys. Use
  synthetic inputs for tests and keep runtime state outside the source tree.
- Preserve dirty work. In the maintained workspace, stay on the existing
  checkout and branch; do not create branches, worktrees or stashes.
- GitHub is source storage. Do not add CI, hosted build/test automation,
  releases, tags, signing or binary publication. Pushes, public ingress and
  production operations need an explicit owner instruction.
- Change sibling products only when assigned. Use `codebase-memory-mcp` when
  structural lookup helps; do not recreate CodeGraph tooling.

## Development and checks

Carry authorized work through its relevant checks and resolve routine local
failures without repeated permission. Delegate bounded, independent work when
it saves time; give each writer distinct paths and keep one coordinator.

See [CONTRIBUTING.md](CONTRIBUTING.md) for build requirements and commands.
Run focused Cargo integration tests for changed behavior, then the relevant
interop or packaging checks. `cargo fmt --check` checks formatting without
rewriting files. The bridge test script downloads and builds its pinned Go
source; it is not a lightweight offline check.

For delivery, spool, credentials or installer changes, obtain independent
review and exercise failure/restart paths. Source review and unit tests do not
prove installed service behavior, real vehicle delivery or current Hub/App
acceptance. Report the exact source, artifact, environment, invocation and
remaining acceptance gap.

## Workspace context

When this checkout is inside the maintained Teslatlas workspace, the parent
`AGENTS.md` and `docs/development/MASTER_PLAN.md` govern programme scope and
status. Follow its existing execution wrapper and build lock when running
heavy work. Do not reproduce private lab details in public docs. Edge's old
`docs/development/PLAN.md`, `STATUS.json` and dated receipts are historical
context, not a second active plan. Read only the relevant contract or operation
guide for the task; keep documentation names lowercase and hyphenated.
