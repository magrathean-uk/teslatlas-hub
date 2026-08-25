# Hub progress

Status: current Rust 1.98 source implements legacy Owner API and official Fleet
API setup, discovery, polling, regional refresh, multi-car collection, wake/control,
bounded provider-response retention, and opt-in TeslaMate write-back. The final
review fixed configured offline timeout use, durable stream audit receipts,
post-send Fleet refresh fencing, one-to-one VIN/EID rotation, recursive raw JSON
credential redaction, Fleet-only status, short-lived writer WAL cleanup, and the
China token endpoint. No known code blocker remains.

Current verification on 2026-08-25: 748 library + 39 CLI + 1 TLS tests passed
(788 total; 2 intentional fixture tests ignored); all-target Clippy with
`-D warnings`, release build, ShellCheck, Linux packaging tests, macOS packaging
checks, and 27 AppKit tests passed. Live EMEA Fleet authorization, vehicle
discovery, vehicle-data polling, partner registration, and virtual-key pairing
passed on macOS. The root package upgraded the running legacy per-user service,
the installed Hub now supervises the loopback Tesla command proxy as its child,
and the installed Mac app exposes all seven reviewed non-charging controls. One
additional UI climate-start and UI climate-stop were accepted; subsequent Fleet
telemetry reported climate on and then off. No charge command ran. `status`,
immutable preflight, and doctor passed
with a zero-byte WAL. The local TeslaMate PostgreSQL write-back dry-run reported
zero affected rows and left charge cost `0.10` unchanged.

Current artifact:

- macOS ARM64 app, embedded Hub SHA-256
  `f818ef855652e5383421e0643a38623662ff50ac697ccd4d8e08fb0fdb35658f`;
  embedded Tesla command proxy SHA-256
  `ee61a89137c8eb73db4db1d57f2e393a084221e51421b06087e1677a2c631cc2`;
  embedded service package SHA-256
  `d7a517c4732bc80d004a9ed249d8870572110fbe1aef8c63ddd64fa75990c658`;
  deep strict ad-hoc signature verification passed.
- Debian 13 amd64 package SHA-256
  `b4ba561c173ac6b4df759247a26d4d3ca28406c45b4eacb8709e7ef00e089ebc`.
- Debian 13 ARM64 package SHA-256
  `8f7d111c12f4f6a9b4c7a57f26f3ca476fd56a6b74966a52d2db1df53834181d`.

Both Linux packages were built natively from commit `e19ff55` on Debian 13,
installed, bootstrapped, checked, started on loopback, and stopped. Their build
roots, test installs, and ARM64 VM were deleted. TeslaMate stayed healthy.

The final stopped TeslaMate v4.1.1 migration read 105 migrations, 10,782,436
positions, 3,267 drives, and 1 car into about 1.86 GiB / 446 packs. Source
counts stayed unchanged. Hub refreshed the real legacy token, its encrypted
successor was handed back in one PostgreSQL transaction, and TeslaMate then
refreshed successfully itself. Hub, its data, build inputs, emulation packages,
and caches were removed from the VPS. TeslaMate and TeslaMateAPI are healthy;
VPS root has 28 GiB free.

Current work plan:

- Completed 2026-08-25: bundle and manage Tesla's official command proxy on
  macOS, expose confirmed non-charging controls in the Hub app, and test the
  installed app climate start/stop against the paired car.
- 2026-08-25: prepare a short physical driving-stream handover window.
- Completed 2026-08-25: rebuilt and installed source-identical Debian amd64 and
  ARM64 packages, ran package/service smoke checks, and deleted disposable data.
- 2026-08-25 through 2026-08-31: the active `Teslatlas Fleet endurance` daily
  check keeps Fleet Hub collection under read-only observation without vehicle
  commands or a second refresh owner.
- Produce a Developer ID-signed, notarized, provenance-bound macOS release using
  matching Apple credentials. The available 4AA Developer ID identities sign
  the exact app and package successfully, but the only usable local notary key
  belongs to NPS. Physical iOS sync remains outside Hub scope.

Driving-stream handover plan for 2026-08-25: keep VPS TeslaMate running until a
new short test window is explicitly approved; record TeslaMate and Hub database
watermarks; stop TeslaMate once; transfer the current legacy token pair to Hub;
observe one physical drive; verify stream samples and the closed drive; hand the
latest rotated pair back in one PostgreSQL transaction; stop legacy Hub; restart
and health-check TeslaMate. Any failed step aborts to TeslaMate restart. Fleet
Hub may continue polling separately because it owns a different Fleet token.

Fleet endurance baseline on 2026-08-25: local Fleet Hub ready with one vehicle,
latest observation ID 910, database 3.6 MB, next token refresh scheduled for
2026-08-25 13:51 UTC, and token expiry at 14:21 UTC. The VPS TeslaMate and
TeslaMateAPI containers were both healthy. Recheck through 2026-08-31; do not
issue endurance vehicle commands. At 09:36 UTC a read-only check observed the
current record advance from 1013 to 1014 within five seconds, SQLite integrity
was `ok`, and the WAL was zero bytes. No refresh attempt exists yet because the
first scheduled refresh is still in the future.

Deliberate exclusions: TeslaFi import, `addresses.raw`, Grafana/MQTT/dashboard,
and native Fleet Telemetry ingestion. Fleet currently uses official REST
polling; legacy Owner API keeps TeslaMate-compatible vehicle streaming.

Blocked: no known code blocker. Real Developer ID notarization is blocked by a
local credential team mismatch: the application/installer identities and App
Store Connect notary key belong to different Apple teams. A disposable signing
proof passed for both exact artifacts with timestamps and hardened runtime, then
was deleted. No matching notary profile, API key, or app-specific password was
found locally. The local old-schema TeslaMate PostgreSQL copy remains an
intentional read-only negative fixture.

## Three-platform review, 2026-08-22

- macOS: migration stop/install/schema/restart rollback, bounded child output,
  service update, loaded-state upgrade rollback, and data-preserving privileged
  uninstall are implemented. Current ARM64 app assembly and validation passed.
- Debian amd64 and ARM64: upgrade rollback preserves active/enabled state;
  migration ownership and configured data paths are admitted; maintainer-script
  removal/reload behavior and generated shared-library dependencies are tested;
  package construction checks complete ELF class, data, ABI, machine, and
  interpreter identity.
- Shared Rust: paired-device expiry/revoke/rotate, deterministic refresh
  recovery, bounded TLS identity admission, direct-migration capacity admission,
  crash-safe reset, and current schema migration are implemented and tested.
- Artifact state: all three retained artifacts come from the repaired source.
  Independent macOS, Debian, and Rust reviews found no concrete blocker.

## Completed

- Final native Linux packages — final packaging rejects symlink binary inputs
  and includes `PROVENANCE.md`. Native Debian 13 amd64 and ARM64 builds used
  Rust 1.98; package regression, ELF/loader identity, fresh install, bootstrap,
  doctor, status, loopback systemd lifecycle, and package content checks passed.
  No Tesla credential, API, or command was used. VPS TeslaMate/TeslaMateAPI
  remained healthy; both build roots, test installs, and the ARM64 VM were
  removed.

- Build cleanup — removed the 4.0 GiB project target, 4,415 stale Hub test files
  totalling 1.68 GB from the macOS temp directory, and 31 MB of superseded
  per-user Hub binaries. Retained artifacts total 56 MB; the installed app and
  root service total 76 MB.

- Managed macOS command service and complete controls — pinned Tesla's official
  `vehicle-command` v0.4.1 source, packaged its loopback proxy beside Hub, and
  made Hub own proxy startup, readiness, exit, and shutdown. Fixed a real
  legacy-to-root package upgrade failure, installed the package, and verified
  the proxy was the Hub child on `127.0.0.1:4443`. Installed the Mac app in
  `/Applications`; Wake, Start/Stop Climate, Lock/Unlock, Flash, and Honk
  deliberately exclude charging. One UI start and stop each produced a command
  confirmation and Fleet telemetry changed true then false. A live proxy
  termination also made Hub fail closed; launchd restarted Hub and a new proxy,
  restored the loopback listener, and returned to ready Fleet collection.

- Fleet signed-command onboarding — published the EC public key at the Tesla
  well-known path without changing the root site, registered `magrathean.uk` as
  an EMEA Fleet partner, and opened Tesla's virtual-key pairing flow. The macOS
  Hub app now offers confirmed Start Climate and Stop Climate actions for one
  ready vehicle through the resident service; charging controls and iOS changes
  are deliberately absent. The release app built and signed, all 27 AppKit tests
  passed, and macOS package checks passed. After Tesla-app pairing approval,
  exactly one live climate-start and one climate-stop command succeeded, each
  wrote an audit receipt, and subsequent Fleet telemetry reported climate on
  then off. TeslaMate stayed healthy; no charge command ran.

- Official Fleet login — completed the EMEA authorization-code flow with the
  exact selected scopes, configured encrypted Fleet credentials through stdin,
  discovered the account vehicle, and stored live discovery plus vehicle-data
  observations. Added the self-hosted setup guide with exact regional endpoints
  and a no-temporary-file token exchange. Live testing found and fixed
  short-lived WAL residue, Fleet status reading the pruned raw table, writable
  status getters, and the Tesla.cn refresh endpoint. No wake or command ran.

- macOS controller/package repair — fixed migration stop/confirm/restart,
  bounded concurrent child output, URL credential parsing, TOML path escaping,
  loaded-state upgrade rollback, data-preserving privileged uninstall, and a
  private process umask. Eleven focused AppKit tests and lightweight package
  checks passed without rebuilding the Rust binary or release artifact.

- Tesla stream wire compatibility — live VPS evidence showed Tesla returning a
  binary WebSocket JSON frame and no top-level `tag`, both accepted by
  TeslaMate v4.1.1 but rejected by Hub. Hub now decodes UTF-8 binary JSON,
  binds a missing tag to the active subscription, preserves rejection of an
  explicit foreign tag, and treats valid telemetry as stream health when no
  `control:hello` arrives. A local WebSocket regression test covers the exact
  binary/tagless/no-hello sequence and orderly unsubscribe; 25 native Debian
  stream tests passed. The amd64 package was rebuilt, ELF/package-verified,
  installed, and Hub restarted cleanly. It established a Tesla TLS stream
  connection. No raw or current stream row arrived while the parked vehicle
  was quiet. The one 5.6 GiB remote source/build directory was deleted
  immediately after copying the 6.5 MiB package back. Hub was later purged in
  full and TeslaMate restored healthy; VPS free space is 28 GiB.

- Native Debian amd64 package and direct cutover — built the 5.9 MiB amd64
  package natively on Debian 13, verified its ELF/package architecture, and
  installed it at `/usr/bin/teslatlas-hub`. During the test, the loopback-only
  systemd service was active while TeslaMate stayed stopped, preserving one
  legacy-token refresher. Its final repeatable-read, read-only TeslaMate v4.1.1 migration
  had 105 source migrations, 10,782,432 positions, and unchanged source
  counts; Hub finished at about 1.86 GiB with 447 packs. The single named
  `/var/tmp` build directory, private build inputs, and its stray `.zshenv`
  source line were removed. The package, unit, data, config, and service account
  were removed after testing; TeslaMate is healthy and VPS root has 28 GiB free.

- Native onboarding and recovery security — adapted Tesla Auth v0.15.0 PKCE,
  callback/state/issuer routing, private WebKit login, bounded no-redirect token
  exchange, and stdin-only Rust setup into the AppKit installer flow. Removed
  wake/climate CLI transport and its second credential-manager path. Plaintext
  sync is documented as a loopback trust boundary, not authentication. Backup
  v3 excludes both local keys; explicit AES-256-GCM credential export binds the
  installation ID and rejects wrong keys, tampering, or overwrite. Focused and
  full validation is recorded above.

- Rust 1.98 dependency and packaging refresh — updated the minimum toolchain to
  Rust 1.98 and all resolvable lockfile packages; `matchit` 0.8.4 remains Axum
  0.8.9's exact requirement. Mac format, all-target check, 702 tests with 2
  intentional ignores, Clippy `-D warnings`, release build, 4 macOS app tests,
  app assembly, and strict ad-hoc signature verification passed. A native
  Debian 13 ARM64 guest built and installed the 5.3 MiB package, then passed
  version, bootstrap, doctor, and systemd status smoke checks. Package SHA-256 is
  `1fee14f4d77839265df64869d29aeb5d4c3944baaf562289cc3c5a61dcacbda7`;
  the bounded QEMU guest was deleted.

- Compact direct catalogue — current direct imports retain one digest/state
  catalogue rather than a second `teslamate_import_projection_rows` copy.
  Legacy tombstone and collector-ID paths derive their compatible view from
  that state, while old stores keep their existing inventory. Focused direct
  base/state, direct successor, and staged-compatibility tests plus release
  `cargo check` passed.

- Compact direct v4.1.1 Mac measurement — the stopped VPS TeslaMate v4.1.1
  source (105 migrations; 10,782,430 positions) imported through one local-only
  SSH tunnel without a raw stage. It projected 11,100,209 rows, created one base
  and no deltas, used 446 packs, passed SQLite `integrity_check` and `doctor`,
  and left the source counts unchanged. Final disk was 1.88 GiB; sampled peak
  was 2.94 GiB. The legacy `teslamate_import_projection_*` inventory tables had
  zero rows; the compact state tables had 11,096,061 rows. TeslaMate was then
  restarted healthy. The exact test store and its tunnel were removed.

- Compact direct v4.1.1 Linux measurement — a disposable Debian 13 ARM64 QEMU
  guest built the current Hub source natively and read the same stopped source
  through its own loopback-only tunnel. Migration exited 0; `integrity_check`
  and `doctor` passed. Final data was 1,966,668 KiB with one base, no deltas,
  446 packs, 11,096,061 state rows, and zero legacy inventory rows. The source
  fingerprint remained 105 migrations, 10,782,430 positions, and 2,123,577,023
  bytes. TeslaMate restarted healthy. Guest, disks, private inputs, logs, and
  tunnel were deleted.

- Direct v4.1.1 Mac measurement before catalogue compaction — with the source
  stopped, one read-only direct import projected 11,085,583 rows. It used no
  raw-history stage, finished at 3.17 GiB, and had a sampled 4.17 GiB peak
  (including its temporary comparison spool). The disposable store and tunnel
  were removed. The current compact-catalogue change was measured separately.

- Direct bounded migration — the CLI now uses the existing repeatable-read
  PostgreSQL-to-pack importer rather than `imports/.staging`. The initial pass
  builds packs inline; the final pass captures comparison state only and emits
  sparse deltas. Legacy ciphertext is read from that exported snapshot. Capacity
  admission reserves one active fragment plus the free-space floor instead of
  a 16 GiB cap. 667 library tests passed (2 ignored); Mac and Linux live space
  receipts passed.

- Historical staged-path measurement — with TeslaMate stopped, Mac migrated car
  1 from exact TeslaMate v4.1.1 d6c43bc8c48784da8f0b701945b80b20911b3d1a through
  two read-only snapshots. Each snapshot staged 11,082,539 rows / 7,408,814,477
  bytes; final store was 3.2 GiB; peak staging/publication use was 12 GiB and
  took 853 seconds. The test store was deleted. This measures the retired raw
  stage path, not the direct migration now in the CLI.

- Debian ARM64 v4.1.1 migration boundary — one 5.5 GiB QEMU Debian 13 guest installed the Hub package, reached the real stopped v4.1.1 source through a loopback-only SSH tunnel, and copied until its deliberate 64 MiB stage cap returned stage database byte limit exceeded. The service remained inactive and the stage was empty afterward (660 KiB local state). The guest, package test data, private password file, tunnel, and 2.6 GiB host debug cache were deleted. No TeslaMate PostgreSQL write or vehicle command ran.

- Earlier Debian ARM64 package and live collection — one 5.5 GiB QEMU Debian 13 guest compiled `447c904` natively from the mounted Hub source and existing offline Cargo registry. The 5.3 MiB package (`523beee58b1e83790c82c0a4601a38741ce4321eb6695b8b3fcca57661652b18`) installed, bootstrapped, reported status, completed systemd start/restart/stop, and made a bounded default-streaming live observation with `is_climate_on=true`. Host format, 664 library + 34 CLI + 1 TLS tests (2 intentional fixtures ignored), and Clippy `-D warnings` passed. No vehicle command ran.

- Debian ARM64 acceptance — an earlier Debian 13 ARM64 QEMU guest built and package-tested the broader bootstrap, status, systemd lifecycle, backup, verification, restore, repair, and read-only migration-rejection surface. The dummy migration request safely refused the 17 GiB stage requirement before any PostgreSQL connection; no real Tesla or vehicle command ran.

- Linux portability — fixed Unix-sized Rustix mode/device handling, Linux service error formatting, root-owned packaged config admission, normal non-022 test umasks, Linux service CLI wording, and host-only byte-fixture execution. The package builder now removes only its exact temporary staging directory.

- Linux CLI delivery (historical) — added bootstrap, systemd
  `status|start|stop|restart`, Debian package files, and Linux documentation.
  The earlier wake/climate commands and command-only credential path are now
  intentionally removed for TeslaMate parity and single refresh ownership.

- Debian native portability — QEMU ARM64 compilation exposed platform-sized
  Rustix mode types and systemd test lifetimes; both are portable. Later native
  package acceptance passed; its historical fake wake/climate surface has since
  been removed.

- Linux runtime gates — lifted the existing admitted Unix runtime for setup, read-only migration, serving, and bounded observation; added the small systemd `status/start/stop/restart` adapter. Host format and all-target Rust check passed; Debian ARM64 compilation and package smoke were later completed in one bounded native QEMU guest.

- Final integration — fixed monotonic observation IDs after bounded raw-row pruning, preventing later Owner/stream telemetry from being skipped after SQLite row reuse; corrected historical migration fixtures and current-snapshot assertions — collector 60/60 and all 8 affected upgrade tests passed.

- v4.1.1 surface closeout — matched Nominatim address aliases including Australian territories and corrected the stale 17-item parity ledger: schema 2.2 preserves 16 reviewed domains, excludes only provider raw JSON, and loses none — 3 focused tests passed.

- Charge and geofence parity — charge cost edits now accept total, per-kWh, or per-minute input; geofence changes relabel bounded historical drive/charge pages, optionally calculate missing matching charge costs, and enforce TeslaMate's sub-5-km radius — 2 focused tests passed.

- Credential continuity (historical, superseded in part) — `control sign-out`
  stops the LaunchAgent, refuses a concurrently running direct Hub, and deletes
  the token row and both key generations. The former backup-v2 key restoration
  was replaced by current data-only backup v3 plus separate encrypted recovery.

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
