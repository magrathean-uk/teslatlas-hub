# Teslatlas Swift SDK

This package provides four public Swift products across separate wire contracts:

- `TeslatlasHubSDK` and `TeslatlasCommands` implement the public protocol profile.
- `TeslatlasHubV1Compatibility` supports the deployed Hub v1 compatibility surface.
- `TeslatlasCurrentHub` supports the approved current-Hub profile.

Do not share models, routes, capabilities, or conformance claims across these
contracts without an explicit reviewed binding. Keep compatibility bindings
independent from protocol authority.

## Development

Follow Swift API Design Guidelines. Keep public types in `PascalCase`, members
in `camelCase`, and documentation filenames lowercase with hyphens.

Read [docs/development.md](docs/development.md) for commands and test selection. Use
an external SwiftPM scratch path, including for `swift package dump-package`.
Run the smallest relevant check first, then the fixture suite when changes cross
products, transports or bindings. Keep live, matrix and Apple consumer journeys
opt-in; ordinary package validation must retain their documented exclusions.

Complete authorized safe local work and focused checks without repeated approval.
Delegate independent bounded work when useful, with one writer per file and a clear
stop condition. Preserve existing changes. In the coordinated workspace, follow
its root guidance, single-main-branch policy, build lock and current
`docs/development/MASTER_PLAN.md`; old SDK plan files are historical evidence.
Use `codebase-memory-mcp` when graph-assisted code lookup is needed, not CodeGraph.

## Boundaries

Treat credentials, pairing invitations, endpoint identity, origins, query
bounds, response limits, cursors, and typed errors as security boundaries. Keep
validation fail-closed. Do not log credentials or invitation data. A cursor is
opaque and must remain bound to its issuing Hub, vehicle, and time window.

Do not add product UI, Rust FFI, Hub implementation code, proprietary
Teslatlas source, hosted automation, or inferred routes to this repository.

Do not issue live vehicle commands or expose public ingress.

GitHub is source storage only. Do not add CI workflows, releases, tags,
package publication, signing, or deployment without explicit owner direction.


## Evidence

Record the source revision, command, platform and result. Distinguish fixture
checks, external consumer builds, installed-Hub journeys and physical-device
acceptance. Do not claim broader support from a narrower check. Preserve
[LICENSE](LICENSE), binding attribution and fixture authority records.
