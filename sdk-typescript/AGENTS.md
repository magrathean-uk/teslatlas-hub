# Teslatlas TypeScript SDK

Follow `../AGENTS.md`, `../WORKSPACE_AUTHORITY.md` and
`../docs/development/COORDINATION.md`, then this product's `docs/development/PLAN.md`
and `STATUS.json`. Sol is the default implementation/review model; use the shared
role-based effort policy. Work only on the assigned scope; App and Viewer are excluded.

This repository owns public browser and Node.js transport bindings.

- Use `camelCase` TypeScript names and lowercase-hyphenated documentation names.
- Derive public types from released protocol artifacts.
- Keep credential storage caller-owned and browser-safe.
- Preserve cursors, ETags, SSE replay, typed errors, and non-idempotent command safety.
- Do not add viewer UI, Hub implementation, or server-held credentials.

## Local execution

Run task-relevant disposable local checks and repair failures without repeated approval when the lane is open. Existing owner pauses, workspace authority, production and release gates remain in force.
