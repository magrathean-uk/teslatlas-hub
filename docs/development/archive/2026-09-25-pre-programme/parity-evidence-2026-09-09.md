# TeslaMate parity evidence — 2026-09-09

## Result and boundary

This is a source-mapping evidence table for R1, not a completed TeslaMate
parity result. It names the reference, records the reviewed 17-domain ledger,
and separates it from the operational evidence that is still required. There
is no live TeslaMate database, source backup, imported dataset, passive
observation window, migration, or soak receipt behind this document. A
separate bounded synthetic data-only backup/verify/restore/continuation receipt
exists at `r1-synthetic-data-recovery-2026-09-09.json`; it is not a TeslaMate
source, import, live credential, or passive-collection result.

The named reference is the TeslaMate v4.2.0 source tag revision
`e8d24886f97f22469c2675f89be843f6d401c76a`, with 105 ordered migrations and
the pinned migration-set SHA-256
`ea850d1b038c4af950db32e7a0939aa5ebe8f1dcefe5e56dcd592f3451038868`. The
Hub adapter admits the v4.2-compatible physical schema, rather than claiming
that database inspection can identify the running TeslaMate application
version. It rejects unreviewed migration sets.

The source ledger is version 4. Its denominator is deliberately 17 reviewed
source fields or value domains, not all TeslaMate fields: 16 are preserved in
the schema-2.2 local pack and one is deliberately excluded. The code sets
`all_teslamate_fields_preserved` to `false`; this table must never be used to
claim full-field parity.

## Reviewed source field ledger

Every preserved row below is `PreservedInPack`, `CapturedTyped`, and `Bound`
by the source fingerprint. "Exact" means that the local schema-2.2 adapter
retains the listed source representation; it does not mean an actual source
row has been compared in this R1 run.

| TeslaMate source field or domain | Hub schema-2.2 path | Type, units, and NULL semantics | Conversion and disposition |
| --- | --- | --- | --- |
| `cars.display_priority` | `cars.display_priority` | PostgreSQL `smallint` to Rust `i16`; non-null | Exact source value; preserved. |
| `cars.inserted_at` | `cars.inserted_at_pg_us` | PostgreSQL binary `timestamp(0)` payload in signed `i64`; non-null | Raw PostgreSQL microseconds, not a wall-clock conversion; preserved. |
| `cars.updated_at` | `cars.updated_at_pg_us` | PostgreSQL binary `timestamp(0)` payload in signed `i64`; non-null | Raw PostgreSQL microseconds, not a wall-clock conversion; preserved. |
| `drives.start_km` | `drives.start_km` | Nullable `double precision` as `Option<ProjectionFloat64BitsV2_2>` | `NULL` remains absent; a present value retains its IEEE-754 bits, including distinct signed zero or NaN payloads; preserved. |
| `drives.end_km` | `drives.end_km` | Nullable `double precision` as `Option<ProjectionFloat64BitsV2_2>` | `NULL` remains absent; a present value retains its IEEE-754 bits, including distinct signed zero or NaN payloads; preserved. |
| `settings.unit_of_length` | `global_settings.unit_of_length` | Exact enum: `km` or `mi`; non-null | No unit conversion; preserved. |
| `settings.unit_of_temperature` | `global_settings.unit_of_temperature` | Exact enum: `C` or `F`; non-null | No unit conversion; preserved. |
| `settings.unit_of_pressure` | `global_settings.unit_of_pressure` | Exact enum: `bar` or `psi`; non-null | No unit conversion; preserved. |
| `settings.preferred_range` | `global_settings.preferred_range` | Exact enum: `ideal` or `rated`; non-null | No policy/default conversion; preserved. |
| `settings.base_url` | `global_settings.base_url` | Nullable text | `NULL` remains absent; present text stays opaque; preserved. |
| `settings.grafana_url` | `global_settings.grafana_url` | Nullable text | `NULL` remains absent; present text stays opaque; preserved. |
| `settings.language` | `global_settings.language` | Text; non-null | Exact source text; preserved. |
| `settings.theme_mode` | `global_settings.theme_mode` | Text; non-null | Exact source text; preserved. |
| `settings.inserted_at` | `global_settings.inserted_at_pg_us` | PostgreSQL binary `timestamp(0)` payload in signed `i64`; non-null | Raw PostgreSQL microseconds, not a wall-clock conversion; preserved. |
| `settings.updated_at` | `global_settings.updated_at_pg_us` | PostgreSQL binary `timestamp(0)` payload in signed `i64`; non-null | Raw PostgreSQL microseconds, not a wall-clock conversion; preserved. |
| `addresses.raw` | No Hub schema-2.2 field | Unread, unhashed provider payload | Deliberately excluded because it is not app-visible and can contain unrelated provider data. This is not an unsupported/lost field and does not support a full-parity claim. |
| `states.state` enum (`online`, `offline`, `asleep`) | `states.state` | Exact lower-case enum; non-null | The reviewed TeslaMate v4.2-compatible enum domain is preserved. |

## Source evidence run

The following local source checks passed against the current Hub workspace.
They did not open a PostgreSQL connection or start a collector.

| Command | Result | What it establishes | What it does not establish |
| --- | --- | --- | --- |
| `cargo test --locked --lib reviewed_loss_ledger_has_an_exact_fail_closed_denominator` | 1 passed | The public ledger is exactly 17 domains: 16 preserved, 1 deliberately excluded, 0 fingerprint-only, 0 unsupported/lost; full-field parity fails closed. | A real source has not been read. |
| `cargo test --locked --lib evidence_digest_binds_each_drive_endpoint_and_state_text` | 1 passed | Drive odometer endpoints and state text alter the pre-projection evidence digest. | A source digest has not been captured. |
| `cargo test --locked --lib teslamate_check_snapshot_json_covers_connection_and_redacts_vin` | 1 passed | The redacted check report carries connection/read-only diagnostics and does not emit a VIN value. | No `teslamate-check` command has run against a real source. |
| `cargo test --locked --lib live_source_witness_is_fixed_read_only_and_never_reads_private_tokens` | 1 passed | The live-source witness is fixed and read-only, and it avoids reading token ciphertexts. | It is not a live database receipt. |
| `cargo test --locked --lib v2_2_physical_conversion_preserves` | 7 passed | Physical car, settings, drives, positions, charging, address, state, and update conversions preserve their reviewed source representations. | It does not compare any real IDs, nulls, units, or time bounds. |

The relevant implementation path is
`src/import/teslamate/schema.rs` → `reader.rs` → `projection.rs` →
`parity.rs` → `sync/hub_pack/model.rs`. The read-only path uses a fixed
repeatable-read session; the `teslamate-check` CLI exposes a redacted,
non-mutating compatibility check before a migration is considered.

## Synthetic recovery source evidence

The following fresh local tests exercise the R1 recovery primitives. They use
temporary synthetic stores rather than a service, a named TeslaMate source, or
a passive vehicle. They establish that a bounded operational lane is
implementable. The distinct direct-command synthetic receipt is linked in the
operational ledger below; neither source is a named-source acceptance result.

| Command | Result | Source-level result | Operational limit |
| --- | --- | --- | --- |
| `cargo test --locked --lib storage::data_recovery::tests` | 13 passed | Creates, verifies, and restores data-only generations; preserves installation identity and lineage; revokes pairing/collector authority; rejects unsafe trees and tampering; and exercises supported historical schema restoration. | The public CLI commands were not run against a durable R1 fixture or an installed service. |
| `cargo test --locked --bin teslatlas-hub observation_commands_parse_their_machine_readable_inputs` | 1 passed | The binary CLI accepts the machine-readable watermark and verification command inputs. | Parsing is not a durable observation receipt. |
| `cargo test --locked --lib watermark_and_verification_use_only_durable_observation_metadata` | 1 passed | Watermark/verification derives from durable observation metadata. | No passive window or live provenance was read. |
| `cargo test --locked --lib stream_watermark_is_strictly_increasing_and_survives_reopen` | 1 passed | Stream watermarks are monotonic across a store reopen. | This is not a collection restart/reconnect or retention/soak result. |

## R1 operational parity ledger

These are the remaining evidence obligations from H7. “Not run” means neither
pass nor failure has been inferred. Existing bounded synthetic Hub/Edge/HA and
Viewer receipts are useful component evidence but are not substituted here for
TeslaMate collection or parity.

| H7 requirement | Current evidence | Status | Required next receipt |
| --- | --- | --- | --- |
| Read-only named-source admission | Source contract and CLI are source-tested. | Not run against a source. | A redacted `teslamate-check` receipt from a named, owner-authorized source, including schema/migration, selected-car inventory, read-only transaction, and no-source-mutation evidence. |
| Backup and source comparison | No source backup or source data has been accessed. | Not run. | Preserved backup identity plus bounded before/after comparison of IDs, NULLs, units, time bounds, and content for the selected source scope. |
| Complete field/value and derived-domain comparison | The 17-domain mapping above is static only. Drives, charges, positions, geocoding, update/state events, and derived values have no live comparison. | Not run. | Named-source comparison covering source and Hub counts, IDs, NULL/zero/absent/inapplicable semantics, normalized values, units, and time ranges. |
| Open sessions and migration cutover | Import boundary types validate open drive, charge, and state shape, but there is no migration run. | Not run. | Stopped, read-only migration receipt with open-session handling, successor delta, source-to-Hub identity, and original-volume preservation. |
| Passive collection | No real vehicle or passive provenance was accessed. | Not run. | Starting/ending durable watermarks, collection mode/provenance, agreed window, and `verify-observation` output without a wake or command. |
| Gaps, outage/reconnect, duplicates, ordering, and retention | Selected synthetic companion outage/duplicate observations exist outside this table; none are named TeslaMate collection evidence. | Not run for parity. | A bounded passive or approved synthetic R1 receipt that identifies the observation window, gap/reconnect boundaries, duplicate disposition, ordering, and retention result. |
| Backup, restore, and continuation | The source suite passes 13 recovery tests. `r1-synthetic-data-recovery-2026-09-09.json` additionally records one fresh direct-command synthetic data-only backup/verify/restore and post-watermark continuation, plus a fail-closed truncated-copy rejection. | Passed for the bounded synthetic data-only lane; not a named-source/import/passive/installed-service result. | A named-source or separately authorized operational receipt verifying the selected source scope, source-to-Hub identity, pairing/TLS behavior, newer continuity, and original-volume preservation before collector authority returns. |
| Soak and clean shutdown | No sustained R1 collection receipt exists. | Not run. | Redacted durable receipt for restart, reconnect, storage growth/retention, duplicate delivery, and clean shutdown. |

## Guardrails for the next step

Use a fresh, owner-authorized source and an explicit read-only PostgreSQL
transaction. Do not refresh a copied production token, enable Tesla egress for
history-only import, wake a vehicle, mutate a TeslaMate service, or treat an
absent event as a pass. A future real-source result must cite this table,
identify its source snapshot without exposing credentials or vehicle data, and
leave every unobserved operational row marked untested.
