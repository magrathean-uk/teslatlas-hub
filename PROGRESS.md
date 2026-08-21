# Hub progress

Status: active — Debian ARM64 CLI and systemd support.

Current: lift Linux platform gates for the existing one-car Hub.

Next: systemd CLI, explicit wake/climate commands, bootstrap, `.deb`, then one QEMU acceptance run.

Blocked: real Tesla Owner API collection and vehicle commands need separate explicit credentials/confirmation. The local TeslaMate PostgreSQL copy stays read-only and intentionally fails the 105-migration admission gate.

## Completed

- Final integration — fixed monotonic observation IDs after bounded raw-row pruning, preventing later Owner/stream telemetry from being skipped after SQLite row reuse; corrected historical migration fixtures and current-snapshot assertions — collector 60/60 and all 8 affected upgrade tests passed.

- v4.1.1 surface closeout — matched Nominatim address aliases including Australian territories and corrected the stale 17-item parity ledger: schema 2.2 preserves 16 reviewed domains, excludes only provider raw JSON, and loses none — 3 focused tests passed.

- Charge and geofence parity — charge cost edits now accept total, per-kWh, or per-minute input; geofence changes relabel bounded historical drive/charge pages, optionally calculate missing matching charge costs, and enforce TeslaMate's sub-5-km radius — 2 focused tests passed.

- Credential continuity — `control sign-out` now stops the LaunchAgent, refuses a concurrently running direct Hub, deletes the token row and both key generations, while backup v2 verifies and restores the encryption and cursor keys with the service stopped — 4 focused tests passed.

- Native TeslaMate controls — added live pause/resume and settings pickup within 30 seconds, geofence create/list/delete, completed-charge cost replacement, bounded TeslaMate-compatible GPX export, and rated-range charge-consensus efficiency recalculation — 5 focused tests passed.

- Bounded terrain storage — added a 512 MiB default cache quota, serialized cache admission, oldest-tile eviction, corrupt-tile removal, bounded provenance reads, and release of unused per-tile locks — 3 focused tests passed.

- Current vehicle and raw-storage parity — added authenticated `GET /v1/vehicles/{vehicle_id}/current` with the TeslaMate v4.1.1 live-summary surface, durable Owner/stream overlay, current geofence and restart state; processed raw observations are transactionally pruned while three bounded current snapshots preserve display and deduplication — 3 focused tests passed.

- TeslaMate v4.1.1 derived data — charge integration now uses the current row, falls back to charger power when phase inference is unavailable, normalizes nonpositive phases, and recomputes from all durable samples before publication; Model 3 MY2022+ trim `50` is now `RWD` — 5 focused tests passed. TeslaMate v4.1.1 itself no longer writes the legacy per-drive efficiency column, so live `NULL` remains compatible.

- TeslaMate v4.1.1 import — pinned the exact 105-migration tag at `d6c43bc8c48784da8f0b701945b80b20911b3d1a`, updated VIN/cost schema admission and pack contracts, and kept the local 99-migration database as a read-only negative fixture — 16 schema tests, 20 pack-contract tests, and the bounded preflight test passed.

- Final validation and smoke — format, all-target check, 697 tests passed with 2 ignored, Clippy `-D warnings`, and release build passed; the release binary initialized, reported status, passed doctor/repair, backed up, verified, restored, and re-checked the restored store — the 1.8 MiB fixture was removed, no Hub `/tmp` artifacts remain, and the retained release-only `hub/target` is 611 MiB after deleting 20.1 GiB of debug/test artifacts.

- Data recovery — verified immutable backup, backup verification, restore without source mutation, unsafe-destination refusal, and repair that preserves quarantine while deleting proven orphan packs — 2 focused tests passed.

- Pairing and local sync — verified failure-safe invitation persistence, single-use TLS claim, authenticated vehicle/manifest/pack access, Range resume, restart persistence, and wrong-key rejection — 3 focused tests passed.

- Optional TeslaMate import — verified exact migration-set admission and final-snapshot publication; the local PostgreSQL copy stayed read-only at 99 migrations/2,644 drives and was correctly rejected as incompatible — 2 focused tests and one CLI negative smoke passed; the 632 KiB fixture was removed.

- Fake collection and restart durability — wired the observer acceptance path through native setup, then verified products, vehicle-data, streaming reconnect, encrypted credentials, and durable car/drive/charge/position/state/settings/update behavior across restart — 5 focused tests passed.

- macOS service control — added `service status`, `start`, `stop`, and `restart` with preflight and bounded launchctl state checks; focused launchctl/CLI tests passed and live read-only status reported stopped without changing the existing plist.

- Native clean setup — added `setup` with bounded private token files, products-only vehicle discovery, explicit multi-car selection, durable V2 publication, encrypted credential persistence, and general service preflight without TeslaMate — `cargo check --locked` and 5 focused tests passed.

- Product-fix merge — transport, migration, snapshot, pairing, credential handling, and durability changes merged to `main` — format, check, Clippy, release build, and 674 tests passed with 2 ignored.
- Workspace cleanup — 69 redundant Hub-only temporary clones, targets, and logs removed; sibling App artifacts untouched — zero matching Hub items remained in `/tmp`.
- Goal rewrite — direct sequential development, one checkout, one Cargo target, focused tests, one progress file, and cleanup before handoff.
