# Remaining engineering

Every item is deliberately one-agent sized. Do not merge items or accept a
passing narrow test as proof for a larger row in the evidence matrix.

## Completed implementation slices

| Area | Evidence |
| --- | --- |
| Delta v2 backend, catalogue, server negotiation, and pack publication | Hub release check with `RUSTFLAGS='-D warnings' cargo check --release --all-targets`; passed with zero warnings. |
| Teslatlas delta v2 transport, staged apply, atomic activation, resume, and v1 fallback | `teslatlas-core/tests/hub_delta_e2e.rs`; real local Hub server and native Rust adapter; base plus delta passed, second sync fetched only the later delta, legacy no-capability path passed. |
| Import staging and multi-car result handling | Focused importer, direct, database, pack, and main test coverage; per-car partial failure and rerun semantics are implemented. |
| Open-session race reconciliation | Focused second-snapshot fixtures cover open child tails, close-between-snapshots, duplicate rows, restart, and rollback. |
| Durable export outbox | Collector/database focused coverage covers claim, sparse publication, exact completion, retry, and newer dirty mutations. |
| PerformanceProfile v1 | Implemented in dev73: measures available CPU parallelism plus filesystem capacity, safely reduces only direct-import COPY lanes, supports a deterministic lane override, never raises configured or hard safety limits, and logs a non-secret profile receipt. Runtime/profile proof remains open. |

## Remaining small steps

| ID | Small engineering step | Evidence gate |
| --- | --- | --- |
| E01 | Capture one current dev45 offline receipt and record request audit. | Redacted receipt proves real offline discovery and no wake request. |
| E02 | Run owner-authorized wake, wait 60 seconds, run one Hub collection. | New durable observation, timestamp, database/pack identity, and no-wake audit. |
| E05 | Make one exported PostgreSQL repeatable-read snapshot lease explicit. | Native source test proves lease retention and source write refusal. |
| E06 | Attach one bounded reader lane to that lease with fixed binary COPY. | Second native lane sees the same selected-car view and cannot interpolate SQL. |
| E07 | Complete each typed COPY relation: cars, drives, positions, charge processes, charges, addresses, geofences, states, updates. | Per-relation source count, decoder rejects, and projection checksum match. |
| E08 | Prove the implemented PerformanceProfile v1 and complete bounded lane/stage admission evidence. | macOS ARM64 and Debian ARM64 measurements, deterministic override, bounded-resource cases, and no source mutation. |
| E11 | Reconcile source/destination with set-based counts, digests, parent links, and open-session watermarks. | Zero unexplained differences on complete and pathological corpus. |
| E12 | Complete TeslaMate differential fixture runner by behavior family. | Pinned reference and Hub traces are compared automatically and filed as artifacts. |
| E13 | Inject crash, storage-full, corrupt pack, network loss, lease loss, and repeated observation failures. | Truthful recovery in 60 seconds, no lost acknowledged fact, no duplicate projection. |
| E14 | Implement or decide optional MQTT/updater scope. | Product decision record plus either compatible proof or signed exclusion. |
| E15 | Extend profiling only if needed after v1 evidence; memory-pressure and write-throughput profiling remain deferred. | Any extension changes only safe bounds and retains correctness; no extension is currently claimed. |
| E16 | Repeat fresh macOS arm64 proof against the current release artifact. | Current install, service, backup, restore, and restart matrix passes. |
| E17 | Repeat Debian arm64 fresh reinstall, free-space admission, and database validation. | Fresh VM/package proof records space, integrity, restart, and recovery. |
| E18 | Run the disposable full migration and cutover rehearsal. | Signed rehearsal and rollback report; source remains untouched. |

## Remaining proof boundary

Completed local implementation does not equal operational completion. Live wake,
stream, current macOS, Debian fresh reinstall/space/database, final migration
rehearsal, and optional backend integration decisions remain open.

## Related specifications

- [Roadmap 082-121](../../roadmap/000-map.md)
- [Migration mapping](../TESLAMATE_MIGRATION_MAPPING.md)
- [Performance baseline](../PERFORMANCE_BASELINE.md)
- [Evidence matrix](../COMPLETION_EVIDENCE_MATRIX.md)
