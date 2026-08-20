# Goal: finish the Rust TeslaMate replacement for macOS

Status: defined only. Do not start until the owner says `start`.

## Result

Make `hub/` a working TeslaMate replacement for one vehicle on Apple-silicon macOS.

A clean install must work without TeslaMate. Importing an existing TeslaMate PostgreSQL database is optional and strictly read-only.

## Normal development

Work sequentially in the active `hub/main` checkout:

1. Read `PROGRESS.md` and select one unfinished product step.
2. Reproduce that step's real failure or missing behavior.
3. Make the smallest correct source change.
4. Run one focused check or test.
5. Fix it, update `PROGRESS.md`, and commit code plus progress together.
6. Continue to the next product step.

Use `PROGRESS.md` as the only ledger. Keep only current, next, blockers, and short completed results.

## Product order

1. Clean initialization and native setup without TeslaMate.
2. macOS user-service install, start, status, stop, and restart.
3. Fake/local Owner API and streaming collection without vehicle commands.
4. Durable cars, positions, drives, charges, states, settings, and updates across restart.
5. Optional bounded read-only TeslaMate import with clear schema rejection.
6. Pairing and authenticated local sync across restart.
7. Backup, verification, restore, and repair.
8. One final validation pass and one practical macOS smoke run.

## Hard resource limits

- Work only in `hub/`. Never edit or build `app/`.
- Use one checkout and one build cache: `hub/target`.
- Never create repository clones, worktrees, copied source trees, or extra Cargo target directories.
- Do not use Agent Fleet or subagents for normal development.
- Run one Cargo process at a time.
- Use focused tests while coding. Run the full test suite, Clippy, and release build once near completion, unless a broad change genuinely requires them sooner.
- Do not run `cargo clean`; keep and reuse the single build cache.
- Temporary Hub data outside the repository must use one small directory, stay below 1 GiB, and be deleted in the same step.
- Do not create evidence packs, review trees, archives, large logs, benchmarks, or duplicate progress documents.
- Use narrow `rg` searches and narrow file reads. Browse only for required current documentation.
- Keep conversation updates to completed milestones, failures, or real blockers. Do not stream internal analysis or long command output.
- Before handoff: remove all Hub temporary data, report `hub/target` size, and leave `hub/main` clean except known user files.

If a step would exceed these limits, stop and ask before spending the disk, CPU, or tokens.

## Scope exclusions

No Agent Fleet work, licensing work, release paperwork, Linux packaging, CI, notarization, multi-car support, speculative hardening, or repeated review cycles.

Improve working code; do not rewrite it without a reproduced product problem.

## Safety

- Never write to the local TeslaMate PostgreSQL database.
- Never use real Tesla credentials without explicit authorization.
- Never wake a vehicle or send a vehicle command.
- Never push, publish, deploy, or change remote systems.
- Never reset, clean, stash, or overwrite unrelated user work.

## Done

Done means the normal one-vehicle macOS journey works, the final Rust checks pass once, `PROGRESS.md` records the result, temporary Hub data is gone, and only exact external blockers remain.
