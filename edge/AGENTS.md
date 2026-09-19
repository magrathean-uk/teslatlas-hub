# Teslatlas edge

Current coordination authority: `../docs/development/COORDINATION.md` (2026-09-18).
Sol 5.6/max coordinator, Sol 5.6/high implementation/review, Luna exploration; no fast mode.
Legacy chats are archived. Use this product's current PLAN; no App or Viewer work.

This repository owns a narrow user-operated Fleet Telemetry ingress.

- Use lowercase-hyphenated documentation names and `snake_case` Rust paths if Rust is selected.
- Preserve mTLS, bounded encrypted spooling, at-least-once delivery, and Hub-side deduplication.
- Edge must never hold Tesla account credentials or expose vehicle-command paths.
- Prefer outbound Hub-to-Edge connectivity.
- Do not turn Edge into a mandatory relay or consumer API.

## Local execution

Run task-relevant disposable local checks and repair failures without repeated approval when the lane is open. Existing owner pauses, workspace authority, production and release gates remain in force.
