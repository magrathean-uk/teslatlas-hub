# Teslatlas TypeScript SDK

Current coordination authority: `../docs/development/COORDINATION.md` (2026-09-18).
Sol 5.6/max coordinator, Sol 5.6/high implementation/review, Luna exploration; no fast mode.
Legacy chats are archived. Use this product's current PLAN; no App or Viewer work.

This repository owns public browser and Node.js transport bindings.

- Use `camelCase` TypeScript names and lowercase-hyphenated documentation names.
- Derive public types from released protocol artifacts.
- Keep credential storage caller-owned and browser-safe.
- Preserve cursors, ETags, SSE replay, typed errors, and non-idempotent command safety.
- Do not add viewer UI, Hub implementation, or server-held credentials.

## Local execution

Run task-relevant disposable local checks and repair failures without repeated approval when the lane is open. Existing owner pauses, workspace authority, production and release gates remain in force.
