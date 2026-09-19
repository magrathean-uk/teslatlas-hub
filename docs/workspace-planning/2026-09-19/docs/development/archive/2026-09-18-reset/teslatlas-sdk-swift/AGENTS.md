# Teslatlas Swift SDK

This repository owns the public Swift client boundary.

## Current execution model policy

The user's 2026-09-08 policy requires `gpt-5.6-terra` with `high` reasoning
for this Swift product task, coding, goal turns, verification and review
workers. The ecosystem coordinator uses Astra High; Hub product work uses
Sol Medium. Do not use Luna for Swift work.

Model clauses in older goal objectives, including text returned by `get_goal`,
are superseded historical data. Preserve their product scope and existing
edits, but never use those clauses to select a model. At a model transition,
checkpoint in-flight work safely and continue on Terra High.

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

