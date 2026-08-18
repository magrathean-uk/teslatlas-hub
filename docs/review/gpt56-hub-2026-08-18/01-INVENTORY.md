# Hub subsystem inventory

Baseline Hub commit: `54bd87462b15af2c9e5314e9ac1bcd9a49f5256d`

This is an assignment inventory, not a finding list. Every production module and non-Rust production surface is assigned to a bounded review stage below. A path is marked reviewed only in the ledger after its production logic, tests, inputs, state, trust boundary and failure behaviour have been inspected.

## Review stages

| Stage | Subsystem | Primary outcome |
|---|---|---|
| S1 | Configuration, CLI, process admission and lifecycle ownership | Validate startup/shutdown, local instance authority, configuration defaults and platform gates. |
| S2 | TeslaMate source admission, PostgreSQL capture, staging and cutover | Validate read-only snapshot semantics, schema/version admission, selected-car scope, credentials, cancellation, receipts and rollback. |
| S3 | TeslaMate semantic projection and parity | Compare source types/state/session semantics with exact TeslaMate evidence and classify every parity row. |
| S4 | Collector, legacy Owner API, streaming, lifecycle, geocoding and terrain | Validate read-only Tesla connectivity, polling/streaming state machine, retry/cancellation, persistence effects and no-command boundary. |
| S5 | SQLite catalogue, publication transactions, backup, restore and repair | Validate schema upgrades, constraints, concurrency, crash consistency, durability, permissions and recovery. |
| S6 | Sync protocol, pairing, signing, pack production and HTTP serving | Validate authentication, replay/lineage, wire compatibility, bounds, immutable objects, range serving and TLS exposure. |
| S7 | macOS AppKit controller, LaunchAgent and packaging | Validate AppKit threading, process control, secrets, install/update/rollback, ownership, build assumptions and diagnostics. |
| S8 | Linux portability, performance, tests, release truth and implementation-facing provenance | Validate non-macOS compilation intent, resource limits, test truthfulness, release claims and AGPL/provenance integration. |

## Rust production modules

| Path | Responsibility and owned state/surface | External inputs or trust boundary | Assigned stage(s) |
|---|---|---|---|
| `src/lib.rs` | Crate module/export surface, unsafe-code prohibition, build/source constants and interactive legal notice. | Cargo metadata and downstream library callers. | S1, S8 |
| `src/main.rs` | CLI parsing and command dispatch; writable-store admission; macOS Serve supervisor; migration/install orchestration; signal handling; operator-visible output. | argv, stdin, config path, signals, filesystem, subprocess/service lifecycle. | S1, S2, S4, S5, S7, S8 |
| `src/config.rs` | Strict TOML model and validation for data path, bind/TLS, collector cadence, TeslaMate limits, geocoder, terrain and performance profile. | Operator-controlled TOML and environment-derived default paths. | S1, S2, S4, S6, S8 |
| `src/credentials.rs` | Redacted plaintext token/password types; managed and observer legacy-auth managers; encrypted-token persistence callbacks and sensitive-access guard. | Secret files/stdin, SQLite token records, token endpoint. | S2, S4, S6 |
| `src/crypto.rs` | Installs one Rustls `ring` provider for all TLS consumers. | Process-global Rustls provider selection. | S1, S2, S4, S6 |
| `src/hub_user_process.rs` | Arc-shared admitted-process capability over the retained local lifetime lock. | Live data-directory identity and user-session lifetime. | S1, S4, S6, S7 |
| `src/user_lifetime_lock.rs` | Unix descriptor-retained data-directory and lock-file identity, flock exclusion and revalidation. | Local filesystem names, inode replacement, modes, concurrent processes. | S1, S5, S7, S8 |
| `src/macos_launch_agent.rs` | macOS preflight, prepared installation and LaunchAgent/service lifecycle integration. | `/Library`, per-user LaunchAgents, config/data paths and `launchctl`. | S1, S7 |
| `src/teslamate.rs` | Credential-free PostgreSQL source URL and fixed required-table/session SQL contract. | Operator source URL. | S2 |
| `src/teslamate_schema.rs` | Pinned TeslaMate revision/migration set, source table/column/enum contracts and fail-closed schema validation. | PostgreSQL catalogue observations and TeslaMate migration history. | S2, S3 |
| `src/teslamate_reader.rs` | TLS/non-TLS PostgreSQL connection, read-only repeatable-read/exported snapshots, schema inspection, selected-car keyset/COPY readers, token ciphertext extraction and capture concurrency. | PostgreSQL wire data, native trust store, source credentials, hostile/large source rows. | S2, S3, S8 |
| `src/teslamate_stage.rs` | Private bounded SQLite capture stage, row/page insertion, accounting, sealing, reopening and discard. | Decoded PostgreSQL rows, disk space, interrupted capture and local filesystem. | S2, S5, S8 |
| `src/teslamate_direct.rs` | Direct exported-snapshot capture into bounded projection fragments; multi-lane source reads; source counts and logical/legacy fingerprints. | Exported PostgreSQL snapshot, disk capacity, pack writer and cancellation. | S2, S3, S6, S8 |
| `src/teslamate_fragments.rs` | Bounded full-snapshot pack fragmentation from a sealed stage, retry sizing, parent repetition, candidate ownership and cleanup. | Sealed stage rows, protocol limits, worker threads and filesystem objects. | S2, S3, S6, S8 |
| `src/teslamate_import.rs` | Stable source/vehicle identity, first-base versus successor publication, cutover/open-session reconciliation, projection-state capture, publication guards and rollback. | Operator-selected car/source key, direct/staged captures, live SQLite catalogue. | S2, S3, S5, S6 |
| `src/teslamate_projection.rs` | Typed TeslaMate physical/history models, validation and mapping to Hub projection rows and reports. | Decoded source values, nullability, timestamps, decimals and relationships. | S3 |
| `src/teslamate_projection_state.rs` | Bounded digest/state spool used to compare source projections and produce successor deltas/tombstones. | Source-projection rows, private spool filesystem and catalogue attachment/detachment. | S2, S3, S5, S6 |
| `src/teslamate_parity.rs` | Machine-readable selected-projection loss ledger and source-evidence fingerprinting. | Reviewed TeslaMate field/value domains and projection facts. | S3, S8 |
| `src/teslamate_credentials.rs` | Private TeslaMate encryption-key and Hub cursor-key creation/loading; cross-filesystem key/token replacement recovery. | Secret directory, key files, SQLite ciphertext pair, interruption and path attacks. | S2, S5, S6 |
| `src/teslamate_token.rs` | TeslaMate-compatible legacy token decrypt/encrypt implementation and ciphertext validation. | Source key bytes and encrypted/plain token material. | S2, S4, S8 |
| `src/updates_logical.rs` | Canonical logical-row encoding, decoding, summaries and hashes for TeslaMate `updates`. | Physical update rows and untrusted encoded streams. | S3, S6 |
| `src/updates_delivery.rs` | Schema-2.2 selected-car `updates` source-to-Hub publication, signed no-op state and receipts; pinned fixture proof path. | PostgreSQL snapshot facts, pack/no-op filesystem, signing cursor and optional external receipt tooling. | S2, S3, S5, S6, S8 |
| `src/collector.rs` | Supervised and bounded collection orchestration; request audit; source/vehicle identity; stream task ownership; lifecycle commit/publication; retries, fuses and terrain enrichment. | Tesla network, tokens, timers, cancellation, SQLite, machine sleep/network loss. | S4, S5, S6, S8 |
| `src/owner_api.rs` | Crate-private read-only legacy Owner API client for products, vehicle probe and `vehicle_data`; response bounds and auth facade. No vehicle command route is present. | HTTPS/loopback HTTP responses, bearer token and status codes. | S4, S8 |
| `src/legacy_auth.rs` | Tesla legacy refresh state machine, fuse, schedule, rotation journal boundary and bounded token response parsing. | Single-use refresh token, auth endpoint, clock, persistence callback and process interruption. | S4, S5, S8 |
| `src/tesla_stream.rs` | Legacy streaming endpoint selection, authenticated subscribe/unsubscribe, frame parsing, reconnect/backoff supervisor, bounded event queue and no-wake power gate. | WebSocket server, bearer token, frames, timers and cancellation. | S4, S8 |
| `src/lifecycle.rs` | Pure deterministic observation-to-vehicle-phase/open-session/drive/charge/state/update projection and restart state encoding. | Persisted observations and imported open-session seed. | S3, S4, S5 |
| `src/geocoder.rs` | Optional bounded Nominatim reverse geocoding, shared rate limiter, response parsing/cache and final egress admission check. | Coordinates, HTTPS provider, provider JSON and local cache. | S4, S5, S8 |
| `src/location.rs` | Pure WGS84 validation, distance and TeslaMate-style strict geofence matching. | Coordinates and geofence definitions. | S3, S4 |
| `src/terrain.rs` | Pure SRTM HGT tile naming, file/byte validation and cell decoding. | HGT names/files/bytes and coordinates. | S3, S4, S8 |
| `src/terrain_cache.rs` | Bounded restart-safe AWS/ESA terrain acquisition, archive extraction, cache publication, free-space checks and per-tile locks. | HTTPS archives, ZIP/GZIP content, filesystem and egress guard. | S4, S5, S8 |
| `src/db.rs` | Hub SQLite schema/version 49, migrations, catalogue/open-session/observation storage, publication gate, pairing/device tokens, request/refresh ledgers, manifests/packs, repair and backup primitives. | All durable Hub state, concurrent local processes, filesystem objects and protocol metadata. | S4, S5, S6, S8 |
| `src/data_recovery.rs` | Data-only backup, immutable verification and restore; excludes TLS, signing/cursor key, owner credentials and collector authority. | Backup directory trees, manifests, SQLite catalogue and immutable packs. | S5, S6, S8 |
| `src/protocol.rs` | Sync protocol 1.0, lineage 2.0, schemas 1.0/2.0/2.1/2.2, digests, cursors, manifests, limits and streaming pack verification. | Wire JSON, cursors, compressed SQLite bytes and client compatibility. | S6 |
| `src/manifest_signing.rs` | Ed25519 manifest signing key derived from cursor key and exact-byte signatures. | Cursor key and response bytes. | S6 |
| `src/http_range.rs` | Strict single-byte-range parsing and RFC content-range values for immutable pack downloads. | HTTP `Range` header and object length. | S6 |
| `src/transport.rs` | Generic schema-1.0 SQLite pack writer, validation, streaming compression and content-addressed publication. | Typed source rows, filesystem and protocol limits. | S5, S6, S8 |
| `src/hub_pack.rs` | Typed schema-2.x projection and delta SQLite layouts, source bindings, numeric/timestamp encodings, writer, verifier and signed manifest helpers. | Projected TeslaMate/collector rows, filesystem and wire-contract constants. | S3, S5, S6, S8 |
| `src/server.rs` | Axum HTTP routes, readiness/capabilities, pairing claim, bearer authentication, signed manifests/no-op, pack range serving and TLS/plaintext listener ownership. | Local/LAN HTTP requests, TLS identity, pairing secrets, bearer tokens and pack paths. | S1, S5, S6, S8 |
| `src/performance_profile.rs` | Host-local import COPY-lane reduction based on CPU/config; does not raise safety limits. Included as a private submodule of `teslamate_import.rs`. | Host CPU/filesystem measurements and config. | S2, S8 |
| `src/fake_tesla.rs` | Test-only fake Owner API/stream server and replacement-journey support; excluded from non-test library builds. | Synthetic test HTTP/WebSocket traffic. | S4, S8 |

## CLI surface

The binary exposes these commands at the baseline commit:

- all platforms: `legal`, `init`, `doctor`, `status`, `observation-watermark`, `verify-observation`, `serve`, `pair`/`create-pairing`, `repair`, `backup`, `verify-backup`, `restore-data`;
- macOS-only: `preflight`, `observe`, `install`, `migrate`.

Secret values are intended to arrive through protected files or stdin. The migration URL is credential-free; command arguments still expose non-secret source identity and secret-file paths and are reviewed as metadata leakage and process-boundary inputs.

## HTTP and protocol surface

Baseline routes:

| Route | Expected exposure at baseline |
|---|---|
| `GET /healthz` | Unauthenticated liveness; no-store. |
| `GET /readyz` | Unauthenticated redacted readiness; no-store. |
| `GET /.well-known/teslatlas-hub` | Capability and signing identity discovery. |
| `POST /v1/pairings/{pairing_id}/claim` | One-time pairing claim; enabled only on the TLS paired router. |
| `GET /v1/vehicles` | Device-authenticated on TLS paired router; unauthenticated only on loopback/development router. |
| `GET /v1/vehicles/{vehicle_id}/sync/manifest` | Same access model; signed on paired router. |
| `GET /v1/vehicles/{vehicle_id}/sync/noop` | Schema-2.2 signed no-op companion. |
| `GET /v1/packs/sha256/{object_name}` | Immutable content-addressed object with single-range support and catalogue authorisation. |

Protocol identities assigned to S6:

- `teslatlas-sync` protocol `1.0`;
- lineage envelope `2.0`;
- generic transport schema `1.0`;
- Hub projection schemas `2.0`, `2.1`, and full-snapshot-only `2.2`;
- SQLite application IDs `TSP1` for generic transport and `THP1` for typed Hub projection;
- cursor HMAC/authentication, manifest Ed25519 signature, content SHA-256, immutable pack path and delta-chain digest.

## Durable stores and schemas

| Store/object | Owner | Review concerns |
|---|---|---|
| `<data_dir>/hub.sqlite` | `db.rs` | SQLite application ID `TAHU`, schema version 49, migration ordering, foreign keys, WAL/checkpoint, publication transactions, request/credential ledgers, pairing and lifecycle state. |
| `<data_dir>/packs/sha256/*.sqlite.zst` | `transport.rs`, `hub_pack.rs`, `db.rs` | Temporary-file safety, verification before publication, inode/mode, catalogue authorisation, retirement and cleanup. |
| `<data_dir>/packs/noop/...` | `updates_delivery.rs`, `db.rs` | Manifest-last pairing, signed-byte identity and atomic replacement. |
| `<data_dir>/import-spool/...` | `teslamate_projection_state.rs`, `db.rs` | Private ownership/mode, stale-generation recovery, capacity and attach/detach consistency. |
| import stage `.staging/*.sqlite` | `teslamate_stage.rs` | Private creation, open/sealed state, accounting, integrity and cancellation cleanup. |
| `secrets/teslamate-encryption.key` and previous generation | `teslamate_credentials.rs` | 0600/0700, symlink/hard-link defence, fsync and cross-store recovery. |
| `secrets/hub-cursor.key` | `teslamate_credentials.rs` | create-once race, permissions, signing/cursor derivation and backup exclusion. |
| data-backup generation | `data_recovery.rs` | immutable envelope, member hashes/modes, no traversal, no credential/authority claim and atomic publication. |
| PostgreSQL source transaction | `teslamate_reader.rs`, `teslamate_direct.rs` | `READ ONLY`, `REPEATABLE READ`, UTC, exported snapshot lanes, schema/migration pin and selected-car scoping. |

## Background tasks and lifecycle ownership

Assigned task/process surfaces include:

- macOS Serve supervisor owning collector and HTTP server tasks, readiness hand-off and bounded stop/abort;
- Axum listener task owned by `OwnedServer` with graceful-drain and hard-stop bounds;
- supervised collector loop plus independent durable heartbeat lease;
- per-vehicle stream supervisors and bounded stream event channels;
- coordinated asynchronous legacy-refresh worker/ticket;
- terrain/geocoder optional outbound work and per-tile async locks;
- PostgreSQL connection tasks, exported-snapshot lanes and `JoinSet`/channel capture workers;
- projection fragment worker threads and bounded queues;
- AppKit background `Process` invocations with main-queue UI completion;
- LaunchAgent process lifetime across login, logout, reboot, start, stop and restart.

Each is assigned to S1/S2/S4/S7 as applicable. Detached-task behaviour, cancellation, shutdown ordering, blocking work and unbounded queues are explicit review items.

## macOS controller and package inventory

AppKit production files:

| Path | Responsibility | Assigned stage |
|---|---|---|
| `macos/TeslatlasHubApp/TeslatlasHubApp/main.swift` | NSApplication entry. | S7 |
| `.../AppDelegate.swift` | App/window lifecycle. | S7 |
| `.../HubController.swift` | Embedded CLI/package execution, launchctl service control, status/config/import/logs and snapshot model. | S7 |
| `.../MainWindowController.swift` | Main controls and state rendering. | S7 |
| `.../ImportSheetController.swift` | TeslaMate source/car/password/key input workflow. | S2, S7 |
| `.../DiagnosticsWindowController.swift` | Diagnostic presentation. | S7 |
| `.../LogsWindowController.swift` | Bounded log presentation. | S7 |
| `.../ServiceDetailsWindowController.swift` | Service/config details. | S7 |
| `.../Info.plist` | Bundle metadata. | S7, S8 |
| `macos/TeslatlasHubApp/project.yml` | XcodeGen source: macOS 12, arm64, Swift language setting 5.0, unsigned build defaults. | S7, S8 |

Tests: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubControllerTests.swift` is assigned to S7/S8.

Packaging/install surfaces:

- `scripts/build-macos-app.sh` — Rust release build, service package, XcodeGen/Xcode build, resource/legal embedding, architecture/deployment checks and ad-hoc signing;
- `scripts/build-macos-service-package.sh` — arm64 payload/package construction and expansion verification;
- `packaging/macos-service/com.teslatlas.hub.plist.in` — installed LaunchAgent template;
- `packaging/macos-service/scripts/common.sh`, `preinstall`, `postinstall` — privileged package scripts and per-user installation/update handling;
- `packaging/com.teslatlas.hub.plist.in` — repository LaunchAgent template used by the non-package path.

All are assigned to S7; non-macOS assumptions are also assigned to S8.

## Tests and fixtures

- Rust unit tests are embedded throughout production modules and must be reviewed with the code they exercise.
- `tests/tls_import_e2e.rs` is the sole standalone Rust integration-test file and is assigned to S2/S6/S8.
- `src/fake_tesla.rs` is test-only support assigned to S4/S8.
- `fixtures/delta-v2/manifest.json`, `lineage_manifest_v2.json`, `SHA256SUMS` and three compressed SQLite packs are protocol golden fixtures assigned to S6/S8.
- `fixtures/teslamate-corpus/v1/updates-lossless-selected-car.sql` is a synthetic PostgreSQL COPY fixture assigned to S2/S3/S8.
- Binary fixture contents require repository tests or external decompression/SQLite inspection; the GitHub text connector alone cannot prove their internal schema or checksum validity.

## Platform and feature branches

Cargo declares no crate feature flags. Relevant conditional compilation is platform/test based:

- `macos_launch_agent` and macOS Serve/Observe/Install/Migrate/Preflight paths: `target_os = "macos"`;
- local lifetime lock and admitted-process wrapper: Unix;
- `fake_tesla` and numerous fault/characterisation seams: tests only;
- geocoder/terrain convenience methods without a real admitted-user egress guard: non-macOS or tests;
- PostgreSQL and SQLite code otherwise compile as general Unix-oriented Rust but use `std::os::unix`, Unix modes and `rustix` broadly.

Linux is therefore not merely an installer gap: the Rust service has an intended non-macOS path, but filesystem, service-management, permission, TLS trust-store and signal assumptions require S8 validation.

## Third-party dependency classes

The lockfile resolves the exact dependency graph; the principal implementation-facing classes are:

- async/runtime/network: Tokio, Axum, axum-server, Reqwest, Tokio Tungstenite, tower-http;
- databases/types: bundled Rusqlite/SQLite, Tokio PostgreSQL, Rust Decimal, `time`;
- TLS/crypto: Rustls with `ring`, Tokio PostgreSQL Rustls, AES-GCM, Ed25519 Dalek, SHA-256, `zeroize`, `rcgen`;
- storage/archive: zstd, flate2, zip, rustix;
- data/CLI: Serde/JSON/TOML, URL, UUID, Clap, QR code;
- tests: tempfile, tower, http-body-util.

No vendored source tree was found. Dependency audit execution is unavailable here; `Cargo.lock`, `THIRD_PARTY_NOTICES.md`, `DEPENDENCY_LICENSE_REVIEW.md` and `cargo deny check` are assigned to S8.

## Generated, binary and release material

- Generated: `Cargo.lock`; Xcode project derived from `project.yml`; build products under `target/`/`dist/`; packages and ordinary database/compressed outputs.
- Deliberately committed binary fixtures: three `fixtures/delta-v2/v1/packs/sha256/*.sqlite.zst` files.
- No committed `.xcodeproj`, built executable, app bundle or installer package was found.
- Release/legal/provenance documents constraining implementation claims include `README.md`, `PROGRESS.md`, `SECURITY.md`, `LICENSE`, `NOTICE`, `THIRD_PARTY_NOTICES.md`, `PROVENANCE.md`, `DEPENDENCY_LICENSE_REVIEW.md`, `BRANDING.md`, `TRADEMARKS.md`, `PRIVACY.md`, `LEGAL.md`, `AGPL_COMPLIANCE.md`, and matching public compliance documents under `docs/`.

## Initial claim-to-code locations

The product claims in scope are assigned as follows, without yet concluding correctness:

| Claim | Primary code evidence | Stage |
|---|---|---|
| Native Rust self-hosted telemetry service | Cargo package, CLI, server, collector, DB. | S1, S4, S5, S6 |
| macOS now, Linux intended | AppKit/packaging plus broad Unix/non-macOS code paths. | S7, S8 |
| Alpha with AppKit, CLI and bootstrap/install | Cargo version, CLI, AppKit, package/build scripts and LaunchAgent. | S1, S7, S8 |
| One selected vehicle | migration scope, status/preflight, source/vehicle binding and collector identity. | S2, S3, S4, S6 |
| Read-only TeslaMate PostgreSQL migration | source/session SQL, reader/direct/stage/import and credentials. | S2 |
| Legacy Owner API polling and streaming | owner client, stream supervisor and collector. | S4 |
| Own SQLite store, pairing, signed sync, backup/restore/repair | DB, protocol/server/signing/packs and data recovery. | S5, S6 |
| Separate from proprietary client | crate/API boundary and legal/provenance material; app inspected only if protocol evidence requires it. | S6, S8 |
| `AGPL-3.0-only` | Cargo metadata, legal notice, licence/notices and packaged resources. | S8 |
