# Teslatlas Swift SDK

This repository owns the public Swift client boundary.

- Follow Swift API Design Guidelines and semantic versioning.
- Use `PascalCase` public types, `camelCase` members, and lowercase-hyphenated documentation names.
- Keep `TeslatlasHubSDK` and `TeslatlasCommands` strictly derived from released public protocol artifacts.
- Keep `TeslatlasHubV1Compatibility` independent and limited to its hash-pinned deployed-Hub binding.
- Never share models, routes, capabilities, or conformance claims across those contract boundaries implicitly.
- Keep credentials, endpoint identity, origins, query limits, and typed errors fail-closed.
- Do not add product UI, Rust FFI, Hub implementation source, proprietary Teslatlas source, hosted automation, or invented routes.
