# Teslatlas edge

This repository owns a narrow user-operated Fleet Telemetry ingress.

- Use lowercase-hyphenated documentation names and `snake_case` Rust paths if Rust is selected.
- Preserve mTLS, bounded encrypted spooling, at-least-once delivery, and Hub-side deduplication.
- Edge must never hold Tesla account credentials or expose vehicle-command paths.
- Prefer outbound Hub-to-Edge connectivity.
- Do not turn Edge into a mandatory relay or consumer API.

## Local execution

Run task-relevant disposable local checks and repair failures without repeated approval when the lane is open. Existing owner pauses, workspace authority, production and release gates remain in force.

