# Design: Edge carry-over and retirement

Status: G2-0 design, 2026-10-08. Not implemented. Edge (`teslatlas-edge@9aa14d9`, the preserved WIP patch commit; `f2498ea` is the last deployable state) is retired from active scope. This note lists what the Hub keeps from it and what is removed, so the retirement in G2-6 is a mechanical change.

## Keep

1. **Receiver envelope contract.** `profiles/edge-delivery-v2/2.0.0/receiver-envelope.schema.json` (required `version` const 1, `vin`, `txid`, `tx_type`, `received_at_ms`, `timestamp_ms`, `payload`; optional `device_client_version`, `firmware_version`; event-clock rule `timestamp_ms <= received_at_ms + 300000`) and the matching `vectors.json` entries (`identity`, `sequence`, `unicode_key_order`, `non_projection_envelopes`, `negative_cases`) move to `protocol/receiver/1.0.0/` as their own small profile. The rest of `edge-delivery-v2` (delivery, acks, dispositions, pull/ack OpenAPI) is deleted with the profile.
2. **Dispatcher patch.** `packaging/fleet-telemetry-bridge/0001-teslatlas-http-dispatcher.patch` stays; it is what emits the envelope (`datastore/teslatlas/teslatlas.go`, `marshalEnvelope`). It gains the E6 hunk from Edge's patch (`telemetry/record.go`: replace the DEBUG log that prints the raw payload with one that logs `record_type` only). This changes the patch bytes, so four pins move together: the patch, `fleet-telemetry-bridge-lock.json` `overlay.patch_sha256`, `scripts/build-fleet-telemetry-bridge.sh` `PATCH_SHA256`, and `fleet-telemetry-legal-lock.json` `bridge_lock_sha256`. `docs/legal/provenance.md` says the patch "adds only the bounded dispatcher"; that sentence is updated in the same change (it is not in the owner-controlled lock, but it is a legal document: owner sign-off recorded in the plan's Authorisations).
3. **Direct-route parser.** `src/collection/fleet_telemetry.rs` already matches the schema (`validate_envelope` :569-624) except that `validate_vin` upper-cases and the schema does not; the Hub keeps accepting upper-cased input but the conformance vector for a lowercase VIN is a negative case. Alert and error envelopes (`tx_type` `alerts`/`errors`) currently fail as `InvalidJson` (400) on the direct route because `WirePayload` is `Vehicle | Connectivity`; decision: accept them, record a `durable_non_projection_event` as the Edge consumer did (storage/db/edge_delivery.rs:8-19), and return 204, so a dispatcher never retries them forever.
4. **Per-VIN accumulator state.** The direct path keeps `field_watermarks`, `pack_voltage` and `pack_current` only in memory (SRV:384) and restarts with empty watermarks, so after a Hub restart a valid update can be classified `regressed` and dropped. Carry over the persistence: a table `fleet_telemetry_accumulator_states(vehicle_id PRIMARY KEY, state_version, state_json, updated_at_ms)` written in the same transaction as `persist_fleet_telemetry_snapshot` and restored by `fleet_telemetry_seed_for_vin`; the existing `encode_complete`/`restore_complete` (`fleet_telemetry.rs:191-261`) are reused. Test: a stream interrupted by a restart yields the same accepted set as an uninterrupted one.
5. **Test fixtures.** `src/collection/edge_delivery.rs` and `src/storage/db/tests/edge_delivery.rs` include the protocol examples by path (now `protocol/...` inside the workspace); the surviving envelope tests move to `fleet_telemetry/tests.rs` and read the receiver profile's examples.

## Remove (G2-6, after the carry-over above is merged)

| Item | Anchor | Lines |
| --- | --- | --- |
| Edge consumer loop | `src/collection/edge_delivery.rs` | 1,191 |
| Edge store | `src/storage/db/edge_delivery.rs` (`include!`d at `storage/db.rs:1267`) | 775 |
| Fault injection | `src/runtime/edge_test_fault.rs`, feature `edge-test-faults` (`Cargo.toml:28`), hooks in `collector.rs:109-121` and `geofences_and_terrain.rs:354-358` | 61 + hooks |
| Tests | `src/storage/db/tests/edge_delivery.rs`, `tests/edge_delivery_e2e.rs` | 1,120 + 2,122 |
| Tables | `edge_lineages`, `edge_applications`, `edge_sequence_dispositions` (+ index), `edge_accumulator_states`, `edge_pending_publications` (+ index), `edge_consumer_diagnostics`, their triggers and `car_id` column (MIG:1868-1970, 1996-2036) | migration under the store floor (G2-4) |
| Config | `[collector.edge]` (`runtime/config.rs:274-276, 431-540, 1126-1132, 1239-1241`), consumers in `diagnostics.rs`, `dispatch.rs`, `main/migration.rs`, `platform/macos_launch_agent.rs` | |
| Dev mode | `DevelopmentHubMode.edge` (Swift `DevelopmentHubRuntime.swift:65-69,132`; Rust `DevelopmentServeMode::Edge`, `cli.rs:34,44,53`) | |
| Dependency | `serde_jcs_legacy` (`Cargo.toml:55-56`; only user `edge_delivery.rs:797-801`) | |
| Tooling | `tools/source_run_receiver_edge_hub.py` (675), Edge lanes in `fixture.py`, `scripts/sync-ecosystem-versions.py` and `test_g3_admission.py` Edge pins | |

Audit items closed by this retirement without a fix: STO-01, X03, COL-01, STO-15 (Hub side); Edge C10/C11 remain historical findings in the archived repository.

## Protocol consequences

`edge-delivery-v2` disappears from `compatibility/hub.json` and from the TypeScript client's vendored copy (deleted anyway); the receiver profile is referenced by the dispatcher build only. No served API changes.

## Acceptance

- Receiver conformance: every positive example and `non_projection_envelopes` vector accepted (204), every negative vector rejected (400), on the direct route.
- Restart equivalence test (item 4).
- `cargo check --all-targets` with the feature list reduced; no `edge` string left outside `docs/development/archive/`.
- The Edge repository is archived read-only after the owner-requested push.
