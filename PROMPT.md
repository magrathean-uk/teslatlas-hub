# Goal: Linux TeslaMate replacement

## Result

Make `hub/` work as a one-vehicle Rust TeslaMate replacement on Debian ARM64.

The Linux product is CLI and systemd only. It needs clean bootstrap, service
status/start/stop/restart, setup, read-only TeslaMate migration, backup and
repair, a Debian package, and the existing local sync server. Wake and climate
start commands must be explicit CLI actions and hermetically tested. Driving
tests can follow later.

## Normal development

Work sequentially in `hub/main`:

1. Read `PROGRESS.md`; select one missing Linux product step.
2. Reproduce the missing behavior.
3. Make the smallest correct source change.
4. Run one focused test or check.
5. Update `PROGRESS.md` and commit the change.

`PROGRESS.md` is the only work ledger.

## Product order

1. Lift Linux platform gates for setup, migration, serving, and bounded observation.
2. Add a systemd service adapter and CLI service status viewer.
3. Add explicit wake and climate-start CLI actions with fake-Owner-API tests.
4. Add bootstrap and Debian ARM64 packaging.
5. Boot one Debian ARM64 QEMU guest, install the `.deb`, and test bootstrap,
   status, service lifecycle, migration rejection/read-only behavior, backup,
   restore, repair, wake, and climate-start against local fakes.
6. Remove QEMU/image/build data; retain only the packaged `.deb` and the
   normal `hub/target` cache.

## Resource limits

- Work only in `hub/`; never edit or build `app/`.
- Use one checkout and one host build cache: `hub/target`.
- Use one QEMU Debian ARM64 guest directory below `/tmp`, under 6 GiB, and
  delete it after final verification.
- Never create repository clones, worktrees, copied source trees, extra Cargo
  target directories, evidence archives, or benchmark data.
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

Done means the Linux ARM64 `.deb` installs in one local Debian QEMU guest; the
CLI bootstrap, status, systemd lifecycle, migration, backup/restore/repair,
and local fake wake/climate controls work; final Rust checks pass; QEMU data is
gone; and `PROGRESS.md` records exact remaining external proof.
