# Source layout

The Rust workspace is grouped by product domain. New implementation belongs in
the narrowest owning domain; `src/lib.rs` is the only Rust file permitted at the
`src/` root.

## Find implementation

| Domain | Path | Responsibility |
|---|---|---|
| API | `src/api/` | HTTP service, provider clients, transport, and sync protocol |
| Application | `src/application/` | CLI entry point, service control, migration, and pairing commands |
| Authentication | `src/auth/` | Credential custody, encryption, recovery, and refresh inputs |
| Collection | `src/collection/` | Discovery, polling, streaming, Fleet Telemetry, and scheduling |
| Geography | `src/geo/` | Location enrichment, terrain, geofences, and GPX export |
| Import | `src/import/teslamate/` | TeslaMate capture, schema compatibility, projection, and write-back |
| Platform | `src/platform/` | macOS launchd, Linux systemd, and process ownership |
| Runtime | `src/runtime/` | Configuration, diagnostics, and vehicle lifecycle projection |
| Storage | `src/storage/` | SQLite catalogue, recovery, transactions, and durable models |
| Sync | `src/sync/` | Immutable packs, manifests, delivery, and logical updates |

The crate keeps compatibility exports such as `teslatlas_hub::db` and
`teslatlas_hub::collector`. New code should use domain paths such as
`teslatlas_hub::storage::db` and `teslatlas_hub::collection::collector`.

## Add or change code

1. Put the file below its owning domain using lowercase `snake_case`.
2. Add the module to that domain's `mod.rs`.
3. Keep tests beside their implementation in a `tests.rs` file or a named
   `tests/` fragment when a suite has several concerns.
4. Split a source unit before it exceeds 3,000 lines.
5. Run the repository layout, provenance, formatting, Clippy, and test gates.

```sh
python3 scripts/verify-repository-layout.py
python3 scripts/verify-provenance.py
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
```

The layout gate rejects flat source modules, nonconforming Rust names,
tool-specific repository metadata, missing domains, and source units above the
line ceiling.

## Understand split modules

Large storage, collection, sync-pack, reader, lifecycle, and CLI modules use
named source fragments. Each fragment is included into its owning Rust module,
so the refactor changes physical navigation without changing privacy or public
behaviour. Fragment names describe the responsibility they contain; generic
numbered names are forbidden by convention.

Tests follow the same rule. Shared fixtures remain in the parent test module,
and named fragments group database migration, pairing, projection, scheduler,
terrain, pack-contract, and delta cases.
