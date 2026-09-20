# Teslatlas edge

Follow `../AGENTS.md`, `../WORKSPACE_AUTHORITY.md` and
`../docs/development/COORDINATION.md`, then this product's `docs/development/PLAN.md`
and `STATUS.json`. Sol is the default implementation/review model; use the shared
role-based effort policy. Work only on the assigned scope; App and Viewer are excluded.

This repository owns a narrow user-operated Fleet Telemetry ingress.

- Use lowercase-hyphenated documentation names and `snake_case` Rust paths if Rust is selected.
- Preserve mTLS, bounded encrypted spooling, at-least-once delivery, and Hub-side deduplication.
- Edge must never hold Tesla account credentials or expose vehicle-command paths.
- Prefer outbound Hub-to-Edge connectivity.
- Do not turn Edge into a mandatory relay or consumer API.

## Local execution

Run task-relevant disposable local checks and repair failures without repeated approval when the lane is open. Existing owner pauses, workspace authority, production and release gates remain in force.
