# Hub progress

Status: Mac full TeslaMate v4.1.1 migration passed; Debian ARM64 source-copy capacity rejection passed; driving remains deliberately deferred.

Current: TeslaMate remains stopped on the VPS. Mac completed the two-snapshot migration from the real v4.1.1 database and validated the resulting Hub store. The under-6 GiB Debian guest reached the real source through its read-only tunnel and cleanly stopped at its deliberate 64 MiB stage cap; a complete 10.7M-position import needs a larger Linux disk.

Next: full Linux migration only on Linux storage that can accommodate its observed 12 GiB temporary peak; real driving behavior only after a separate explicit test request.

Blocked: real wake and climate commands still require immediate explicit confirmation. The local TeslaMate PostgreSQL copy stays read-only and intentionally fails the 105-migration admission gate.

## Completed

- Live v4.1.1 migration and cleanup — with TeslaMate stopped, Mac migrated car 1 from exact TeslaMate v4.1.1 d6c43bc8c48784da8f0b701945b80b20911b3d1a through two read-only snapshots. Each snapshot staged 11,082,539 rows / 7,408,814,477 bytes; final import was imported, retained 1,116-byte access and 1,031-byte refresh ciphertexts, and doctor returned ok. The final temporary store was 3.2 GiB; peak staging/publication use was 12 GiB and took 853 seconds. The Mac test store was deleted. Migration now keeps all dangling historical TeslaMate sessions while omitting an ambiguous live tail, and accepts one normal newline on a supplied ENCRYPTION_KEY; both focused regressions passed.

- Debian ARM64 v4.1.1 migration boundary — one 5.5 GiB QEMU Debian 13 guest installed the Hub package, reached the real stopped v4.1.1 source through a loopback-only SSH tunnel, and copied until its deliberate 64 MiB stage cap returned stage database byte limit exceeded. The service remained inactive and the stage was empty afterward (660 KiB local state). The guest, package test data, private password file, tunnel, and 2.6 GiB host debug cache were deleted. No TeslaMate PostgreSQL write or vehicle command ran.

- Current Debian ARM64 package and live collection — one 5.5 GiB QEMU Debian 13 guest compiled `447c904` natively from the mounted Hub source and existing offline Cargo registry. The 5.3 MiB package (`523beee58b1e83790c82c0a4601a38741ce4321eb6695b8b3fcca57661652b18`) installed, bootstrapped, reported status, completed systemd start/restart/stop, and made a bounded default-streaming live observation with `is_climate_on=true`. Host format, 664 library + 34 CLI + 1 TLS tests (2 intentional fixtures ignored), and Clippy `-D warnings` passed. No vehicle command ran.

- Debian ARM64 acceptance — an earlier Debian 13 ARM64 QEMU guest built and package-tested the broader bootstrap, status, systemd lifecycle, backup, verification, restore, repair, and read-only migration-rejection surface. The dummy migration request safely refused the 17 GiB stage requirement before any PostgreSQL connection; no real Tesla or vehicle command ran.

- Linux portability — fixed Unix-sized Rustix mode/device handling, Linux service error formatting, root-owned packaged config admission, normal non-022 test umasks, Linux service CLI wording, and host-only byte-fixture execution. The package builder now removes only its exact temporary staging directory.

- Linux CLI delivery — added bootstrap, systemd `status|start|stop|restart`, explicit `control wake --confirm` and `control climate-start --confirm` with a local fake Owner API test, Debian package files, and Linux documentation. Host format, focused Owner API test, and binary check passed.

- Debian native portability — QEMU ARM64 compilation exposed platform-sized Rustix mode types and systemd test lifetimes; both are now portable. The native fake wake/climate test passed. Release package acceptance remains next.

- Linux runtime gates — lifted the existing admitted Unix runtime for setup, read-only migration, serving, and bounded observation; added the small systemd `status/start/stop/restart` adapter. Host format and all-target Rust check passed; Debian ARM64 compilation is deliberately deferred to the single QEMU guest because this host has no Linux C cross-compiler.

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
