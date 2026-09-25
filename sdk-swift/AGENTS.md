# Teslatlas Swift SDK

This repository owns the public Swift client boundary.

Follow `../AGENTS.md` and `../docs/development/COORDINATION.md`, then this
product's `docs/development/PLAN.md` and `STATUS.json`. Model and effort defaults
are in `../AGENTS.md`. Work only on the assigned scope. App v7 (`../app`) consumes
this product; change the App only as App work. Viewer is excluded.

Run commands through `../scripts/dev/run.sh teslatlas-sdk-swift COMMAND...` so build output and
caches stay out of this tree (SwiftPM output is not routed by clean-development; pass `--scratch-path` outside the checkout).

- Follow Swift API Design Guidelines. Use the ecosystem calendar product version;
  keep SwiftPM/wire semantic versions separate.
- Use `PascalCase` public types, `camelCase` members, and lowercase-hyphenated documentation names.
- Keep `TeslatlasHubSDK` and `TeslatlasCommands` strictly derived from released public protocol artifacts.
- Keep `TeslatlasHubV1Compatibility` independent and limited to its hash-pinned deployed-Hub binding.
- Keep `TeslatlasCurrentHub` independently bound to the approved `hub-http-v1`
  profile and its explicit Hub product version binding; local tests do not imply
  installed acceptance.
- Never share models, routes, capabilities, or conformance claims across those contract boundaries implicitly.
- Keep credentials, endpoint identity, origins, query limits, and typed errors fail-closed.
- Do not add product UI, Rust FFI, Hub implementation source, proprietary Teslatlas source, hosted automation, or invented routes.

## Local execution

Run task-relevant disposable local checks and repair failures without repeated approval when the lane is open. Existing owner pauses, workspace authority, production and release gates remain in force.
