# Goal: Rust TeslaMate replacement

## Result

Make `hub/` work as a one-vehicle Rust TeslaMate replacement on macOS and
Debian ARM64.

The product needs clean bootstrap, service status/start/stop/restart, setup,
read-only TeslaMate migration, backup and repair, a Debian package, and the
existing local sync server. Wake and climate-start commands must be explicit
CLI actions and hermetically tested. Driving tests can follow later.

## Normal development

Work sequentially in `hub/main`:

1. Read `PROGRESS.md`; select one missing Linux product step.
2. Reproduce the missing behavior.
3. Make the smallest correct source change.
4. Run one focused test or check.
5. Update `PROGRESS.md` and commit the change.

`PROGRESS.md` is the only work ledger.

## Product order

1. Keep migration as one PostgreSQL stream into Hub packs: no full raw-history
   SQLite stage. The first pass writes the base; the stopped cutover pass writes
   only sparse deltas from compact comparison state.
2. Lift Linux platform gates for setup, migration, serving, and bounded observation.
3. Add a systemd service adapter and CLI service status viewer.
4. Add explicit wake and climate-start CLI actions with fake-Owner-API tests.
5. Add bootstrap and Debian ARM64 packaging.
6. Boot one Debian ARM64 QEMU guest, install the `.deb`, and test bootstrap,
   status, service lifecycle, migration rejection/read-only behavior, backup,
   restore, repair, wake, and climate-start against local fakes.
7. Remove QEMU/image/build data; retain only the packaged `.deb` and the
   normal `hub/target` cache.

## Resource limits

- Work only in `hub/`; never edit or build `app/`.
- Use one checkout and one host build cache: `hub/target`.
- Use one QEMU Debian ARM64 guest directory below `/tmp`, under 6 GiB, and
  delete it after final verification.
- Never create repository clones, worktrees, copied source trees, extra Cargo
  target directories, evidence archives, or benchmark data.
- Migration may use final packs, one active fragment's temporary files, and a
  compact comparison spool. It must never reserve or create a whole-history
  raw stage just because a configured cap permits one.
- Run one Cargo process at a time. Focused tests while coding; one full suite,
  Clippy, and release build near handoff.
- Do not install host services or change host configuration.

## Safety

- TeslaMate PostgreSQL access remains read-only.
- Do not use or print real Tesla credentials.
- Implement and fake-test wake/climate commands first. A real vehicle command
  needs a separate explicit confirmation immediately before it is sent.
- Never push, publish, deploy, or change remote systems.
- Preserve unrelated user work.

## Done

Done means macOS and the Linux ARM64 `.deb` have a working one-vehicle Hub;
the CLI bootstrap, status, service lifecycle, direct read-only migration,
backup/restore/repair, and local fake wake/climate controls work; final Rust
checks pass; temporary test data is gone; and `PROGRESS.md` records exact
remaining external proof.
