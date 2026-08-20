# Goal: working Rust TeslaMate replacement on macOS

Status: defined only. Do not start automatically.

## Outcome

Finish `hub/` as a usable TeslaMate replacement written in Rust for one vehicle on Apple-silicon macOS. A new installation must work without TeslaMate. Optional TeslaMate migration must be read-only.

There is no time limit. When started, continue step by step until the product path works or a real external blocker needs owner input.

## Source and scope

- Work only in the active `hub/main` checkout.
- Do not edit or build the sibling `app/` project.
- Improve the existing implementation; do not rewrite working code without a reproduced problem.
- Product path: build, configure, initialize, migrate optionally, run as a macOS user service, collect telemetry, persist lifecycle history, restart, pair, sync, back up, restore, and repair.
- Use the local PostgreSQL `teslamate` database only as a read-only older-schema and scale fixture. Never modify it.
- Exclude Agent Fleet work, release paperwork, licensing work, Linux packaging, CI, notarization, multi-car support, and speculative hardening.

## One progress document

Use `PROGRESS.md` as the only work ledger. Do not create review forests, evidence packs, duplicated plans, or per-agent reports.

Keep only: status, one current step, one next step, exact blockers, and a short completed list stating each change and its focused validation. Update it with the code before each commit.

## Sequential development loop

1. Read `PROGRESS.md` and choose the next incomplete product step.
2. Inspect only the relevant files and reproduce the actual failure or missing behavior.
3. Make the smallest correct edit directly in `hub/`.
4. Run the smallest relevant check or focused test.
5. Fix until that step passes.
6. Update `PROGRESS.md` with the change and result.
7. Commit the code and progress update together.
8. Continue to the next step.

Do not stop for another broad audit, architecture exercise, candidate tournament, or review cycle between normal product steps.

## Product order

1. Release build and clean first-run initialization.
2. Configuration and macOS user-service start, status, stop, and restart.
3. Fake/local Owner API and streaming collection without vehicle commands.
4. Durable cars, positions, drives, charges, states, settings, and updates across restart.
5. Optional bounded read-only TeslaMate migration with clear incompatible-schema rejection.
6. Pairing and authenticated local sync across restart.
7. Backup, verify, restore, and repair.
8. One final full validation and one practical macOS smoke run.

## Resource rules

- Primary agent codes directly. Use a subagent only for one small independent question that avoids duplicate work.
- One checkout. One Cargo target directory: `hub/target`.
- Never create per-agent, per-review, per-test, or per-commit clones or target directories.
- Do not run the full test suite after every edit. Use focused tests during development.
- Run `fmt`, full `check`, full `test`, `clippy`, and release build once near completion, or after a genuinely broad dependency/API change.
- Use `rg` and narrow file reads. Do not dump whole repositories or long command output into the conversation.
- Browse only when current external documentation is required.
- Keep user updates to completed milestones, failures, or blockers.
- Do not generate large logs, archives, fixtures, benchmarks, or evidence unless the product failure requires them.
- Any exceptional temporary Hub checkout or artifact must be removed immediately after use.
- Before every handoff: zero Hub temp clones/targets in `/tmp`, report `hub/target` size, and leave `hub/main` clean except known user files.

## Safety

- Never write to the TeslaMate PostgreSQL source.
- Never use real Tesla credentials without explicit owner authorization.
- Never wake a vehicle or send a vehicle command.
- Never push, publish, deploy, or change remote systems.
- Never reset, clean, stash, or overwrite unrelated user work.

## Done

The goal is complete only when the normal macOS journey works, final Rust gates pass, practical local behavior is recorded in `PROGRESS.md`, temporary Hub artifacts are cleaned, and only exact external blockers remain.
