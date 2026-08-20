# Hub progress

Status: active.

Current: verify backup, backup verification, restore, and repair.

Next: run the one final validation pass and practical macOS smoke.

Blocked: none.

## Completed

- Pairing and local sync — verified failure-safe invitation persistence, single-use TLS claim, authenticated vehicle/manifest/pack access, Range resume, restart persistence, and wrong-key rejection — 3 focused tests passed.

- Optional TeslaMate import — verified exact migration-set admission and final-snapshot publication; the local PostgreSQL copy stayed read-only at 99 migrations/2,644 drives and was correctly rejected as incompatible — 2 focused tests and one CLI negative smoke passed; the 632 KiB fixture was removed.

- Fake collection and restart durability — wired the observer acceptance path through native setup, then verified products, vehicle-data, streaming reconnect, encrypted credentials, and durable car/drive/charge/position/state/settings/update behavior across restart — 5 focused tests passed.

- macOS service control — added `service status`, `start`, `stop`, and `restart` with preflight and bounded launchctl state checks; focused launchctl/CLI tests passed and live read-only status reported stopped without changing the existing plist.

- Native clean setup — added `setup` with bounded private token files, products-only vehicle discovery, explicit multi-car selection, durable V2 publication, encrypted credential persistence, and general service preflight without TeslaMate — `cargo check --locked` and 5 focused tests passed.

- Product-fix merge — transport, migration, snapshot, pairing, credential handling, and durability changes merged to `main` — format, check, Clippy, release build, and 674 tests passed with 2 ignored.
- Workspace cleanup — 69 redundant Hub-only temporary clones, targets, and logs removed; sibling App artifacts untouched — zero matching Hub items remained in `/tmp`.
- Goal rewrite — direct sequential development, one checkout, one Cargo target, focused tests, one progress file, and cleanup before handoff.
