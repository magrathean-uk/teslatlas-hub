# Alpha progress

Version: **1.0.0-alpha.1**

## Implemented

- One-car TeslaMate PostgreSQL history import.
- Parallel PostgreSQL reads and bounded multi-threaded projection/pack work.
- Opaque TeslaMate legacy-token ciphertext plus `ENCRYPTION_KEY` import.
- Fresh legacy access/refresh token fallback when the key is unavailable.
- In-memory decryption, scheduled refresh, and atomic encrypted token rotation.
- Legacy Owner API discovery/polling and Tesla WebSocket streaming.
- Drive, charge, state, position, climate, update, and settings persistence.
- SQLite Hub store, pairing, sync packs, backup, restore, and repair.
- AppKit control app for install, import, status, start, stop, restart, logs, and
  diagnostics.
- Per-user macOS LaunchAgent package and direct CLI operation.

## Verified before this release

- Rust: 602 passed, 0 failed, 2 intentionally ignored; warnings-denied
  all-target check and Clippy pass.
- AppKit: 4 passed, 0 failed; Xcode 27 release build is ARM64 with a macOS 12
  deployment target.
- Real one-car migration, legacy-token refresh/rotation, Owner API polling,
  WebSocket streaming, restart, and durable climate on/off observations.
- Current-tree secret scan and dependency licence inventory.

## Still needs owner testing

- Fresh App install/import flow from the published alpha artifact.
- A real drive and a real charging session, followed by lifecycle-row checks.
- Long-running collection on the owner's machine.
- Physical macOS 12 launch and UI check.
- Developer ID signing and notarization.

This alpha does not claim complete TeslaMate product parity. Grafana, MQTT,
multi-car collection, Fleet API, and Linux packaging are outside this release.

## Planned CLI system service

A login-independent CLI installation is feasible without changing collection
or token rotation. The safe design is a root-installed LaunchDaemon in the
system launchd domain with `UserName` set to a validated local owner. `sudo`
would install and control it; the Hub process itself must not run as root.

The existing encrypted SQLite tokens and mode-0600 encryption-key file work
without a GUI login. Remaining work is installer consolidation, ownership-safe
state migration, App control-path updates, rollback, and logout/reboot/refresh
testing. Estimate: one focused day for CLI only; one to two days including App
integration and real tests. Not implemented in this alpha.
