# Hub progress

Status: active — TeslaMate v4.1.1 parity fixes.

Current: charging calculations and derived vehicle data.

Next: current vehicle snapshot, bounded runtime storage, native controls, then one final validation pass.

Blocked: live Tesla Owner API collection and actual LaunchAgent start/restart were not run because real credentials and vehicle access were not authorized; the existing user plist was preserved.

## Completed

- TeslaMate v4.1.1 import — pinned the exact 105-migration tag at `d6c43bc8c48784da8f0b701945b80b20911b3d1a`, updated VIN/cost schema admission and pack contracts, and kept the local 99-migration database as a read-only negative fixture — 16 schema tests, 20 pack-contract tests, and the bounded preflight test passed.

- Final validation and smoke — format, all-target check, 681 tests passed with 2 ignored, Clippy `-D warnings`, and release build passed; the release binary initialized, reported status, passed doctor/repair, backed up, verified, restored, re-checked the restored store, and reported the real service stopped — the 1.8 MiB fixture was removed, no Hub `/tmp` artifacts remain, and the single `hub/target` is 5.2 GiB.

- Data recovery — verified immutable backup, backup verification, restore without source mutation, unsafe-destination refusal, and repair that preserves quarantine while deleting proven orphan packs — 2 focused tests passed.

- Pairing and local sync — verified failure-safe invitation persistence, single-use TLS claim, authenticated vehicle/manifest/pack access, Range resume, restart persistence, and wrong-key rejection — 3 focused tests passed.

- Optional TeslaMate import — verified exact migration-set admission and final-snapshot publication; the local PostgreSQL copy stayed read-only at 99 migrations/2,644 drives and was correctly rejected as incompatible — 2 focused tests and one CLI negative smoke passed; the 632 KiB fixture was removed.

- Fake collection and restart durability — wired the observer acceptance path through native setup, then verified products, vehicle-data, streaming reconnect, encrypted credentials, and durable car/drive/charge/position/state/settings/update behavior across restart — 5 focused tests passed.

- macOS service control — added `service status`, `start`, `stop`, and `restart` with preflight and bounded launchctl state checks; focused launchctl/CLI tests passed and live read-only status reported stopped without changing the existing plist.

- Native clean setup — added `setup` with bounded private token files, products-only vehicle discovery, explicit multi-car selection, durable V2 publication, encrypted credential persistence, and general service preflight without TeslaMate — `cargo check --locked` and 5 focused tests passed.

- Product-fix merge — transport, migration, snapshot, pairing, credential handling, and durability changes merged to `main` — format, check, Clippy, release build, and 674 tests passed with 2 ignored.
- Workspace cleanup — 69 redundant Hub-only temporary clones, targets, and logs removed; sibling App artifacts untouched — zero matching Hub items remained in `/tmp`.
- Goal rewrite — direct sequential development, one checkout, one Cargo target, focused tests, one progress file, and cleanup before handoff.
