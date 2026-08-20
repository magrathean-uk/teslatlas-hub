# Teslatlas Hub: working Rust TeslaMate replacement for macOS

## Goal

Finish the existing Teslatlas Hub as a practical TeslaMate replacement written in Rust and working on macOS. Deliver a usable telemetry collector and local data hub, not another audit, release-certification exercise, or orchestration project.

There is no fixed execution time limit. Continue until the normal product path works, validation passes, or a concrete external blocker requires owner input.

## Primary user journey

An owner must be able to:

1. Build and launch Teslatlas Hub on a supported Mac.
2. Configure Tesla authentication and local storage without installing TeslaMate.
3. Optionally migrate one car, its history, and compatible credentials from a TeslaMate PostgreSQL database through read-only access.
4. Start the Hub and leave it running as a macOS user service.
5. Collect Owner API and streaming telemetry continuously without sending vehicle commands.
6. Persist cars, positions, drives, charging sessions, states, software updates, and settings correctly across restarts.
7. Pair and sync the stored history with the Teslatlas client over the local Hub API.
8. Inspect status and logs, stop or restart the service, back up the data, restore it, and run repair from the app or CLI.

## Product scope

- Use the current dependency-upgraded `hub/` tree as source authority.
- Finish and simplify the implementation already present. Do not rewrite working subsystems without a demonstrated need.
- Prioritize the real macOS path: first run, configuration, migration, authentication, collection, lifecycle reconstruction, local sync, restart, and recovery.
- Preserve a useful CLI even when the native control app is used.
- Keep the initial product boundary to macOS, Apple silicon, one vehicle, and the existing legacy Owner API plus streaming integration.
- TeslaMate migration is optional compatibility. A new installation must run without TeslaMate.
- Treat the supplied local PostgreSQL database as read-only. Its known older schema is useful for scale and rejection-path testing, not as false proof of supported-schema compatibility.

## Working definition

The Hub is working when all of these are true:

- `cargo build --locked --release` succeeds.
- `cargo fmt`, `cargo check`, `cargo test`, and `cargo clippy` pass for the current tree.
- The macOS app bundle builds and launches locally.
- A clean local configuration can initialize storage, start, report healthy status, stop, and restart without manual database surgery.
- The fake/local end-to-end collector path produces durable telemetry, completed drives and charges, and readable sync output.
- Compatible TeslaMate migration completes through bounded read-only PostgreSQL access; incompatible schemas fail clearly and leave both databases unchanged.
- Pairing and authenticated local sync work after a Hub restart.
- Backup, restore, and repair preserve a usable installation.
- No plaintext credential appears in logs, command output, persisted configuration, or test receipts.
- Any final live-account check uses credentials deliberately supplied by the owner, performs observation only, and never wakes or commands a vehicle.

## How to work

- Work directly in the active `hub/` checkout and preserve existing user changes.
- Use Sol, Terra, or Luna only for small independent implementation or review tasks that materially speed up the product work.
- Do not modify, redesign, debug, or perfect Agent Fleet. Fleet is not part of the product.
- Do not create process, policy, evidence, or review machinery unless it is required to make the macOS Hub work or to diagnose a real failure.
- Reproduce a failure, fix the smallest correct cause, run the relevant test, then continue through the user journey.
- Prefer existing maintained Rust crates and macOS facilities over custom infrastructure when they fit.
- Integrate useful already-completed fixes only after checking them against the current tree. Ignore the stale reviewed archive as code.
- One focused independent review near completion is enough. Fix concrete findings; do not reopen broad speculative audits.

## Not required for this goal

- Agent Fleet development.
- Licensing, SBOM, notices, legal review, or release paperwork.
- Developer ID signing, notarization, App Store work, GitHub automation, or CI design.
- Linux packaging or service management.
- Grafana, MQTT, official Fleet API, multi-car support, or feature parity with every TeslaMate integration.
- Exhaustive adversarial, SIGKILL, ENOSPC, power-loss, endurance, benchmark, or provider-policy matrices unless a normal product failure specifically requires one.
- Speculative hardening unrelated to the working macOS user journey.

## Safety boundaries

- Never write to the TeslaMate PostgreSQL source.
- Never use real Tesla credentials unless the owner explicitly supplies and authorizes them for the check.
- Never wake a vehicle or send a vehicle command.
- Never push, publish, deploy, or modify remote systems.
- Do not reset, clean, stash, or overwrite unrelated working-tree changes.

## Finish rule

Do not stop at a plan, candidate branch, narrow unit test, or review receipt. Finish the active macOS product path and report:

- what now works;
- exact local validation results;
- any live behavior actually observed;
- the smallest remaining external blockers, if any.
