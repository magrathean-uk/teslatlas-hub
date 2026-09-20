// SPDX-License-Identifier: AGPL-3.0-only

fn edge_fixture() -> (tempfile::TempDir, HubStore, EdgeBinding) {
    let temporary = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temporary.path()).unwrap();
    let source = store
        .register_source(
            &SourceDescriptor::new("fleet_api_compat", "edge-test"),
            1_800_000_000_000,
        )
        .unwrap();
    let mut descriptor = VehicleDescriptor::new(source.source_id, "123456789")
        .with_tesla_identity(Some(123456789), None);
    descriptor.vin = Some("5YJ3E1EA7KF000001".into());
    let vehicle = store
        .register_vehicle(&descriptor, 1_800_000_000_000)
        .unwrap();
    (
        temporary,
        store,
        EdgeBinding {
            installation_id: "home-edge".into(),
            lineage: "spool-2026-09-05".into(),
            source_id: source.source_id,
            vehicle_id: vehicle.vehicle_id,
            vin: "5YJ3E1EA7KF000001".into(),
            car_id: 123456789,
        },
    )
}

fn projected_record(sequence: u64) -> VerifiedEdgeItem {
    VerifiedEdgeItem::Record(VerifiedEdgeRecord {
        spool_seq: sequence,
        stable_record_id: "8284fe7aea66b79f09cfa5b2fe3ca99fc79fdac24631e06b8365aa8a0c64e5c9".into(),
        legacy_record_id: if sequence == 10 {
            "ac89a19968e0d88fe632e2cf59046dd333213da1e4ecd34f97430341bc70a0bd"
        } else {
            "5fe1588aca9e75808ce9d1292f7b683e489d9724d5772fae5dfd941714fe0bd1"
        }.into(),
        payload_sha256: "8c1e0a6ea96f28999ea2c9c5113f13698a2e081dc829031696665326aeb5f147".into(),
        edge_received_at_ms: 1_800_000_000_100,
        envelope_json: include_bytes!("../../../../../teslatlas-protocol/profiles/edge-delivery-v2/2.0.0/examples/projected-envelope.json").to_vec(),
        non_projection_reason: None,
    })
}

fn projected_record_with(
    sequence: u64,
    id_byte: char,
    txid: &str,
    timestamp_ms: i64,
    data: Value,
) -> VerifiedEdgeItem {
    let envelope = serde_json::json!({
        "version": 1,
        "vin": "5YJ3E1EA7KF000001",
        "txid": txid,
        "tx_type": "V",
        "received_at_ms": 1_800_000_100_000_i64 + sequence as i64,
        "timestamp_ms": timestamp_ms,
        "payload": {
            "vin": "5YJ3E1EA7KF000001",
            "createdAt": "2027-01-15T08:00:00Z",
            "data": data,
        }
    });
    VerifiedEdgeItem::Record(VerifiedEdgeRecord {
        spool_seq: sequence,
        stable_record_id: id_byte.to_string().repeat(64),
        legacy_record_id: id_byte.to_string().repeat(64),
        payload_sha256: id_byte.to_string().repeat(64),
        edge_received_at_ms: 1_800_000_100_000_i64 + sequence as i64,
        envelope_json: serde_json::to_vec(&envelope).unwrap(),
        non_projection_reason: None,
    })
}

fn accept(
    store: &HubStore,
    binding: &EdgeBinding,
    item: &VerifiedEdgeItem,
) -> Result<EdgeAcceptance, StoreError> {
    store.accept_verified_edge_item(binding, item, 1_800_000_061_000, Duration::from_secs(900))
}

#[test]
fn edge_receipt_application_observation_lifecycle_accumulator_and_pending_publish_commit_together()
{
    let (_temporary, store, binding) = edge_fixture();
    let first = accept(&store, &binding, &projected_record(10)).unwrap();
    assert_eq!(first.category, "projected_telemetry");
    assert_eq!(first.ack_frontier, 10);
    assert!(first.observation_id.is_some());
    assert!(first.accumulator_state.is_some());

    let gap = VerifiedEdgeItem::Gap(VerifiedEdgeGap {
        spool_seq: 11,
        notice_id: "73002ffc20a769d62ab2800675b51e8fc3a895ff762b370e5edf11185b864ab2".into(),
        occurred_at_ms: 1_800_000_060_200,
        reason: "retention_expired".into(),
        evidence_sha256: "c2fceaa38e73c41b78386cad195a383e5ae4505db9f71c0e43e7f669a8380e53".into(),
    });
    assert_eq!(
        accept(&store, &binding, &gap).unwrap().category,
        "durable_gap"
    );
    let alert = VerifiedEdgeItem::Record(VerifiedEdgeRecord {
        spool_seq: 12,
        stable_record_id: "42424c8baa6532299915b8026625a0dc271769fd97e19647b6366df523866d1f".into(),
        legacy_record_id: "e9e9bee52f0bda000acdd63a1d79c245e72bdfeca9c3b9c4932c6f5ec0e8e618".into(),
        payload_sha256: "2b8aa76b27b1b29810087e1692c1efcfe3b04e340be83700464570f0924e8fd7".into(),
        edge_received_at_ms: 1_800_000_000_300,
        envelope_json: include_bytes!("../../../../../teslatlas-protocol/profiles/edge-delivery-v2/2.0.0/examples/alert-envelope.json").to_vec(),
        non_projection_reason: Some("alert_event_retained".into()),
    });
    assert_eq!(
        accept(&store, &binding, &alert).unwrap().category,
        "durable_non_projection_event"
    );
    assert_eq!(
        accept(&store, &binding, &projected_record(13))
            .unwrap()
            .category,
        "duplicate"
    );

    let counts = store
        .edge_ledger_counts("home-edge", "spool-2026-09-05")
        .unwrap();
    assert_eq!(counts.applications, 2);
    assert_eq!(counts.sequences, 4);
    assert_eq!(counts.pending_publications, 1);
    assert_eq!(counts.ack_frontier, Some(13));
    let observations = store
        .current_observations_for_vehicle(binding.vehicle_id)
        .unwrap();
    assert_eq!(
        observations.len(),
        1,
        "duplicate and non-projection event do not fabricate current observations"
    );
    assert_eq!(
        observations[0].payload["provider_raw_json"]["response"]["charge_state"]["usable_battery_level"],
        80,
        "stored current payload: {}",
        observations[0].payload,
    );
    assert!(
        store
            .load_lifecycle_state(binding.vehicle_id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn edge_acceptance_faults_roll_back_every_table_and_do_not_advance_ack() {
    for point in [
        StreamFaultPoint::RawInsert,
        StreamFaultPoint::LifecycleWrite,
        StreamFaultPoint::WatermarkUpdate,
        StreamFaultPoint::Commit,
    ] {
        let (_temporary, store, binding) = edge_fixture();
        store.inject_stream_fault(point);
        assert!(
            accept(&store, &binding, &projected_record(10)).is_err(),
            "{point:?}"
        );
        let counts = store
            .edge_ledger_counts("home-edge", "spool-2026-09-05")
            .unwrap();
        assert_eq!(
            counts,
            EdgeLedgerCounts {
                applications: 0,
                sequences: 0,
                pending_publications: 0,
                ack_frontier: None
            },
            "{point:?}"
        );
        assert!(
            store
                .observations_for_vehicle(binding.vehicle_id, ObservationQuery::from_start(10))
                .unwrap()
                .is_empty()
        );
        let accepted = accept(&store, &binding, &projected_record(10)).unwrap();
        assert_eq!(accepted.category, "projected_telemetry", "{point:?}");
        assert_eq!(accepted.ack_frontier, 10, "{point:?}");
        assert_eq!(
            store
                .edge_ledger_counts("home-edge", "spool-2026-09-05")
                .unwrap(),
            EdgeLedgerCounts {
                applications: 1,
                sequences: 1,
                pending_publications: 1,
                ack_frontier: Some(10),
            },
            "{point:?}"
        );
    }
}

#[test]
fn edge_sequence_payload_lineage_and_alias_conflicts_fail_without_advancing() {
    let (_temporary, store, binding) = edge_fixture();
    accept(&store, &binding, &projected_record(10)).unwrap();
    let mut sequence_conflict = projected_record(10);
    let VerifiedEdgeItem::Record(record) = &mut sequence_conflict else {
        unreachable!()
    };
    record.stable_record_id = "a".repeat(64);
    assert!(accept(&store, &binding, &sequence_conflict).is_err());
    let mut payload_conflict = projected_record(11);
    let VerifiedEdgeItem::Record(record) = &mut payload_conflict else {
        unreachable!()
    };
    record.payload_sha256 = "b".repeat(64);
    assert!(accept(&store, &binding, &payload_conflict).is_err());
    let mut sparse = projected_record(12);
    let VerifiedEdgeItem::Record(record) = &mut sparse else {
        unreachable!()
    };
    record.stable_record_id = "c".repeat(64);
    assert!(accept(&store, &binding, &sparse).is_err());
    let mut other = binding.clone();
    other.vin = "5YJ3E1EA7KF000002".into();
    assert!(accept(&store, &other, &projected_record(11)).is_err());
    assert_eq!(
        store
            .edge_ledger_counts("home-edge", "spool-2026-09-05")
            .unwrap()
            .ack_frontier,
        Some(10)
    );
}

#[test]
fn edge_binding_rejects_wrong_initial_and_mutated_source_car_identity() {
    let (_initial_temporary, initial_store, binding) = edge_fixture();
    let mut wrong_initial = binding.clone();
    wrong_initial.car_id += 1;
    assert!(
        accept(
            &initial_store,
            &wrong_initial,
            &projected_record_with(
                1,
                'a',
                "wrong-initial-car",
                1_800_000_001_000,
                serde_json::json!({"Soc": {"intValue": "80"}}),
            )
        )
        .is_err()
    );
    assert_eq!(
        initial_store
            .edge_ledger_counts("home-edge", "spool-2026-09-05")
            .unwrap(),
        EdgeLedgerCounts {
            applications: 0,
            sequences: 0,
            pending_publications: 0,
            ack_frontier: None,
        }
    );

    let (_temporary, store, binding) = edge_fixture();
    accept(
        &store,
        &binding,
        &projected_record_with(
            1,
            'b',
            "correct-car",
            1_800_000_001_000,
            serde_json::json!({"Soc": {"intValue": "80"}}),
        ),
    )
    .unwrap();
    let mut mutated = binding.clone();
    mutated.car_id += 1;
    assert!(
        accept(
            &store,
            &mutated,
            &projected_record_with(
                2,
                'c',
                "mutated-car",
                1_800_000_002_000,
                serde_json::json!({"InsideTemp": {"doubleValue": 21.5}}),
            )
        )
        .is_err()
    );
    let counts = store
        .edge_ledger_counts("home-edge", "spool-2026-09-05")
        .unwrap();
    assert_eq!(counts.applications, 1);
    assert_eq!(counts.sequences, 1);
    assert_eq!(counts.ack_frontier, Some(1));
    let current = store
        .current_observations_for_vehicle(binding.vehicle_id)
        .unwrap();
    let edge = current
        .iter()
        .find(|observation| observation.payload["record_type"] == EDGE_SOURCE_RECORD_TYPE)
        .unwrap();
    assert_eq!(edge.payload["source_vehicle_id"], "123456789");
    assert_eq!(
        edge.payload["provider_raw_json"]["response"]["climate_state"]["inside_temp"],
        Value::Null
    );
    assert_eq!(
        store
            .load_lifecycle_state(binding.vehicle_id)
            .unwrap()
            .unwrap()
            .car_id,
        123456789
    );
}

#[test]
fn schema_58_upgrade_preserves_old_lineage_and_enforces_new_immutable_car_identity() {
    let temporary = crate::private_tempdir().unwrap();
    let database = temporary.path().join("schema-58.sqlite");
    let connection = rusqlite::Connection::open(database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE vehicles (
                vehicle_id TEXT PRIMARY KEY NOT NULL,
                source_id TEXT NOT NULL,
                source_vehicle_key TEXT NOT NULL,
                vin TEXT,
                display_name TEXT,
                created_at_ms INTEGER NOT NULL,
                last_seen_at_ms INTEGER NOT NULL
             ) STRICT;
             INSERT INTO vehicles VALUES (
                'old-vehicle', 'old-source', '9', NULL, NULL, 1, 1
             );
             CREATE TABLE edge_lineages (
                installation_id TEXT NOT NULL,
                lineage TEXT NOT NULL,
                source_id TEXT NOT NULL,
                vehicle_id TEXT NOT NULL,
                vin TEXT NOT NULL,
                first_spool_seq INTEGER,
                ack_frontier INTEGER,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY(installation_id, lineage)
             ) STRICT;
             INSERT INTO edge_lineages VALUES (
                'old-edge', 'old-lineage', 'old-source', 'old-vehicle',
                '5YJ3E1EA7KF000001', 1, 1, 1, 1
             );
             PRAGMA user_version = 58;",
        )
        .unwrap();

    migrate(&connection).unwrap();
    assert_eq!(schema_version(&connection).unwrap(), 60);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('vehicles') WHERE name = 'retired_at_ms'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT retired_at_ms FROM vehicles WHERE vehicle_id = 'old-vehicle'",
                [],
                |row| row.get::<_, Option<i64>>(0),
            )
            .unwrap(),
        None
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT car_id FROM edge_lineages WHERE installation_id = 'old-edge'",
                [],
                |row| row.get::<_, Option<i64>>(0),
            )
            .unwrap(),
        None
    );
    assert!(
        connection
            .execute(
                "INSERT INTO edge_lineages(
                    installation_id, lineage, source_id, vehicle_id, vin,
                    first_spool_seq, ack_frontier, created_at_ms, updated_at_ms
                 ) VALUES ('missing-car', 'lineage', 'source', 'vehicle',
                    '5YJ3E1EA7KF000001', NULL, NULL, 1, 1)",
                [],
            )
            .is_err()
    );
    connection
        .execute(
            "INSERT INTO edge_lineages(
                installation_id, lineage, source_id, vehicle_id, vin, car_id,
                first_spool_seq, ack_frontier, created_at_ms, updated_at_ms
             ) VALUES ('new-edge', 'lineage', 'source', 'vehicle',
                '5YJ3E1EA7KF000001', 9, NULL, NULL, 1, 1)",
            [],
        )
        .unwrap();
    assert!(
        connection
            .execute(
                "UPDATE edge_lineages SET car_id = 10
                  WHERE installation_id = 'new-edge' AND lineage = 'lineage'",
                [],
            )
            .is_err()
    );
}

#[test]
fn valid_unsupported_vehicle_payload_is_durable_and_does_not_block_next_record() {
    let (_temporary, store, binding) = edge_fixture();
    let unsupported_envelope = serde_json::json!({
        "version": 1,
        "vin": "5YJ3E1EA7KF000001",
        "txid": "future-valid-payload",
        "tx_type": "V",
        "received_at_ms": 1_800_000_001_100_i64,
        "timestamp_ms": 1_800_000_001_000_i64,
        "payload": {"futureObject": {"futureValue": "retained-by-identity"}},
    });
    let unsupported = VerifiedEdgeItem::Record(VerifiedEdgeRecord {
        spool_seq: 1,
        stable_record_id: "d".repeat(64),
        legacy_record_id: "d".repeat(64),
        payload_sha256: "d".repeat(64),
        edge_received_at_ms: 1_800_000_001_100,
        envelope_json: serde_json::to_vec(&unsupported_envelope).unwrap(),
        non_projection_reason: None,
    });
    let disposition = accept(&store, &binding, &unsupported).unwrap();
    assert_eq!(disposition.category, "durable_non_projection_event");
    assert!(disposition.observation_id.is_none());
    assert!(
        store
            .current_observations_for_vehicle(binding.vehicle_id)
            .unwrap()
            .is_empty()
    );

    let unsupported_connectivity_envelope = serde_json::json!({
        "version": 1,
        "vin": "5YJ3E1EA7KF000001",
        "txid": "future-valid-connectivity-payload",
        "tx_type": "connectivity",
        "received_at_ms": 1_800_000_001_200_i64,
        "timestamp_ms": 1_800_000_001_100_i64,
        "payload": {"futureConnectivity": {"phase": "bounded-public-event"}},
    });
    let unsupported_connectivity = VerifiedEdgeItem::Record(VerifiedEdgeRecord {
        spool_seq: 2,
        stable_record_id: "c".repeat(64),
        legacy_record_id: "c".repeat(64),
        payload_sha256: "c".repeat(64),
        edge_received_at_ms: 1_800_000_001_200,
        envelope_json: serde_json::to_vec(&unsupported_connectivity_envelope).unwrap(),
        non_projection_reason: None,
    });
    let connectivity_disposition = accept(&store, &binding, &unsupported_connectivity).unwrap();
    assert_eq!(
        connectivity_disposition.category,
        "durable_non_projection_event"
    );
    assert!(connectivity_disposition.observation_id.is_none());

    let supported = projected_record_with(
        3,
        'e',
        "supported-after-future",
        1_800_000_002_000,
        serde_json::json!({"Soc": {"intValue": "81"}}),
    );
    assert_eq!(
        accept(&store, &binding, &supported).unwrap().category,
        "projected_telemetry"
    );
    let counts = store
        .edge_ledger_counts("home-edge", "spool-2026-09-05")
        .unwrap();
    assert_eq!(counts.applications, 3);
    assert_eq!(counts.sequences, 3);
    assert_eq!(counts.ack_frontier, Some(3));
    let current = store
        .current_observations_for_vehicle(binding.vehicle_id)
        .unwrap();
    assert_eq!(
        current[0].payload["provider_raw_json"]["response"]["charge_state"]["usable_battery_level"],
        81
    );
}

#[test]
fn corrupt_complete_accumulator_still_blocks_without_advancing() {
    let (_temporary, store, binding) = edge_fixture();
    accept(
        &store,
        &binding,
        &projected_record_with(
            1,
            'a',
            "initial",
            1_800_000_001_000,
            serde_json::json!({"Soc": {"intValue": "80"}}),
        ),
    )
    .unwrap();
    store
        .open()
        .unwrap()
        .execute(
            "UPDATE edge_accumulator_states SET state_json = ?1",
            params![b"{}".as_slice()],
        )
        .unwrap();
    assert!(
        accept(
            &store,
            &binding,
            &projected_record_with(
                2,
                'b',
                "after-corruption",
                1_800_000_002_000,
                serde_json::json!({"InsideTemp": {"doubleValue": 21.5}}),
            )
        )
        .is_err()
    );
    assert_eq!(
        store
            .edge_ledger_counts("home-edge", "spool-2026-09-05")
            .unwrap()
            .ack_frontier,
        Some(1)
    );
}

#[test]
fn complete_accumulator_for_another_vin_blocks_without_any_acceptance_effect() {
    let (_temporary, store, binding) = edge_fixture();
    accept(
        &store,
        &binding,
        &projected_record_with(
            1,
            'a',
            "initial-before-wrong-state",
            1_800_000_001_000,
            serde_json::json!({"Soc": {"intValue": "80"}}),
        ),
    )
    .unwrap();

    let connection = store.open().unwrap();
    let encoded: Vec<u8> = connection
        .query_row(
            "SELECT state_json FROM edge_accumulator_states
              WHERE installation_id = 'home-edge' AND lineage = 'spool-2026-09-05'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut wrong_state: Value = serde_json::from_slice(&encoded).unwrap();
    wrong_state["vin"] = Value::String("5YJ3E1EA7KF000002".into());
    wrong_state["owner_data"]["vin"] = Value::String("5YJ3E1EA7KF000002".into());
    let wrong_state = serde_json::to_vec(&wrong_state).unwrap();
    connection
        .execute(
            "UPDATE edge_accumulator_states SET state_json = ?1
              WHERE installation_id = 'home-edge' AND lineage = 'spool-2026-09-05'",
            params![wrong_state],
        )
        .unwrap();
    drop(connection);

    let counts_before = store
        .edge_ledger_counts("home-edge", "spool-2026-09-05")
        .unwrap();
    let history_before = store
        .observations_for_vehicle(binding.vehicle_id, ObservationQuery::from_start(10))
        .unwrap();
    let current_before = store
        .current_observations_for_vehicle(binding.vehicle_id)
        .unwrap();
    let lifecycle_before = store.load_lifecycle_state(binding.vehicle_id).unwrap();

    assert!(
        accept(
            &store,
            &binding,
            &projected_record_with(
                2,
                'b',
                "supported-after-wrong-state",
                1_800_000_002_000,
                serde_json::json!({"InsideTemp": {"doubleValue": 21.5}}),
            )
        )
        .is_err()
    );
    assert_eq!(
        store
            .edge_ledger_counts("home-edge", "spool-2026-09-05")
            .unwrap(),
        counts_before
    );
    assert_eq!(
        store
            .observations_for_vehicle(binding.vehicle_id, ObservationQuery::from_start(10))
            .unwrap(),
        history_before
    );
    assert_eq!(
        store
            .current_observations_for_vehicle(binding.vehicle_id)
            .unwrap(),
        current_before
    );
    assert_eq!(
        store.load_lifecycle_state(binding.vehicle_id).unwrap(),
        lifecycle_before
    );
    let connection = store.open().unwrap();
    let persisted: Vec<u8> = connection
        .query_row(
            "SELECT state_json FROM edge_accumulator_states
              WHERE installation_id = 'home-edge' AND lineage = 'spool-2026-09-05'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(persisted, wrong_state);
}

#[test]
fn complete_accumulator_state_preserves_split_pack_components_and_watermarks() {
    let mut accumulator = FleetTelemetryAccumulator::empty("5YJ3E1EA7KF000001").unwrap();
    let first = serde_json::json!({
        "version":1,"vin":"5YJ3E1EA7KF000001","txid":"voltage","tx_type":"V",
        "received_at_ms":1800000000100_i64,"timestamp_ms":1800000000000_i64,
        "payload":{"vin":"5YJ3E1EA7KF000001","createdAt":"2027-01-15T08:00:00Z","data":{"PackVoltage":{"doubleValue":400.0}}}
    });
    accumulator
        .apply_json(&serde_json::to_vec(&first).unwrap())
        .unwrap();
    let encoded = accumulator.encode_complete().unwrap();
    let mut restored = FleetTelemetryAccumulator::restore_complete(&encoded).unwrap();
    let second = serde_json::json!({
        "version":1,"vin":"5YJ3E1EA7KF000001","txid":"current","tx_type":"V",
        "received_at_ms":1800000000200_i64,"timestamp_ms":1800000000100_i64,
        "payload":{"vin":"5YJ3E1EA7KF000001","createdAt":"2027-01-15T08:00:00.100Z","data":{"PackCurrent":{"doubleValue":10.0}}}
    });
    let snapshot = restored
        .apply_json(&serde_json::to_vec(&second).unwrap())
        .unwrap();
    assert_eq!(snapshot.owner_data["drive_state"]["power"], -4.0);
    let mut regressed = first;
    regressed["txid"] = Value::String("old-voltage".into());
    let snapshot = restored
        .apply_json(&serde_json::to_vec(&regressed).unwrap())
        .unwrap();
    assert!(
        snapshot
            .regressed_fields
            .contains(&"PackVoltage".to_owned())
    );
    assert_eq!(snapshot.owner_data["drive_state"]["power"], -4.0);
}

#[test]
fn delayed_distinct_field_updates_merged_current_state_across_store_restart() {
    let (temporary, store, binding) = edge_fixture();
    let soc = projected_record_with(
        1,
        'a',
        "soc-newer",
        1_800_000_002_000,
        serde_json::json!({"Soc": {"intValue": "80"}}),
    );
    accept(&store, &binding, &soc).unwrap();
    drop(store);

    let reopened = HubStore::initialize(temporary.path()).unwrap();
    let inside_temperature = projected_record_with(
        2,
        'b',
        "inside-temperature-delayed",
        1_800_000_001_000,
        serde_json::json!({"InsideTemp": {"doubleValue": 21.5}}),
    );
    let accepted = accept(&reopened, &binding, &inside_temperature).unwrap();
    assert_eq!(accepted.category, "projected_telemetry");

    let current = reopened
        .current_observations_for_vehicle(binding.vehicle_id)
        .unwrap();
    let edge = current
        .iter()
        .find(|observation| observation.payload["record_type"] == EDGE_SOURCE_RECORD_TYPE)
        .unwrap();
    assert_eq!(
        edge.payload["provider_raw_json"]["response"]["charge_state"]["usable_battery_level"],
        80
    );
    assert_eq!(
        edge.payload["provider_raw_json"]["response"]["climate_state"]["inside_temp"],
        21.5
    );
    assert_eq!(edge.observed_at_ms, 1_800_000_002_000);
    assert_eq!(
        reopened
            .edge_ledger_counts("home-edge", "spool-2026-09-05")
            .unwrap()
            .ack_frontier,
        Some(2)
    );
}

#[test]
fn uninterrupted_and_restarted_sparse_delivery_have_exact_complete_state_parity() {
    fn run(restart_after_each: bool) -> (tempfile::TempDir, HubStore, EdgeBinding) {
        let (temporary, mut store, binding) = edge_fixture();
        let records = [
            projected_record_with(
                1,
                '1',
                "parity-soc",
                1_800_000_002_000,
                serde_json::json!({"Soc": {"intValue": "80"}}),
            ),
            projected_record_with(
                2,
                '2',
                "parity-delayed-temperature",
                1_800_000_001_000,
                serde_json::json!({"InsideTemp": {"doubleValue": 22.75}}),
            ),
            projected_record_with(
                3,
                '3',
                "parity-voltage",
                1_800_000_003_000,
                serde_json::json!({"PackVoltage": {"doubleValue": 400.0}}),
            ),
            projected_record_with(
                4,
                '4',
                "parity-stale-soc",
                1_800_000_001_500,
                serde_json::json!({"Soc": {"intValue": "70"}}),
            ),
            projected_record_with(
                5,
                '5',
                "parity-current",
                1_800_000_003_100,
                serde_json::json!({"PackCurrent": {"doubleValue": 10.0}}),
            ),
        ];
        for record in records {
            accept(&store, &binding, &record).unwrap();
            if restart_after_each {
                drop(store);
                store = HubStore::initialize(temporary.path()).unwrap();
            }
        }
        (temporary, store, binding)
    }

    fn complete_snapshot(store: &HubStore, binding: &EdgeBinding) -> Value {
        let history = store
            .observations_for_vehicle(binding.vehicle_id, ObservationQuery::from_start(100))
            .unwrap();
        let current = store
            .current_observations_for_vehicle(binding.vehicle_id)
            .unwrap();
        let lifecycle = store.load_lifecycle_state(binding.vehicle_id).unwrap();
        let connection = store.open().unwrap();
        let accumulator: Vec<u8> = connection
            .query_row(
                "SELECT state_json FROM edge_accumulator_states
                  WHERE installation_id = 'home-edge' AND lineage = 'spool-2026-09-05'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let applications = {
            let mut statement = connection
                .prepare(
                    "SELECT stable_record_id, payload_sha256, disposition,
                            first_spool_seq, observation_id, applied_at_ms
                       FROM edge_applications
                      WHERE installation_id = 'home-edge' AND lineage = 'spool-2026-09-05'
                      ORDER BY first_spool_seq",
                )
                .unwrap();
            statement
                .query_map([], |row| {
                    Ok(serde_json::json!([
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ]))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        let dispositions = {
            let mut statement = connection
                .prepare(
                    "SELECT spool_seq, item_kind, item_id, legacy_record_id,
                            payload_sha256, category, reason, committed_at_ms
                       FROM edge_sequence_dispositions
                      WHERE installation_id = 'home-edge' AND lineage = 'spool-2026-09-05'
                      ORDER BY spool_seq",
                )
                .unwrap();
            statement
                .query_map([], |row| {
                    Ok(serde_json::json!([
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, i64>(7)?,
                    ]))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        let pending = {
            let mut statement = connection
                .prepare(
                    "SELECT stable_record_id, vehicle_id, status, attempts,
                            next_attempt_ms, last_error, created_at_ms, completed_at_ms
                       FROM edge_pending_publications
                      WHERE installation_id = 'home-edge' AND lineage = 'spool-2026-09-05'
                      ORDER BY stable_record_id",
                )
                .unwrap();
            statement
                .query_map([], |row| {
                    let vehicle_id = row.get::<_, String>(1)?;
                    assert_eq!(vehicle_id, binding.vehicle_id.to_string());
                    Ok(serde_json::json!([
                        row.get::<_, String>(0)?,
                        "bound_vehicle",
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                    ]))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        let materialised_states = {
            let mut statement = connection
                .prepare(
                    "SELECT state_id, car_id, state_json
                       FROM materialised_states WHERE vehicle_id = ?1 ORDER BY state_id",
                )
                .unwrap();
            statement
                .query_map(params![binding.vehicle_id.to_string()], |row| {
                    Ok(serde_json::json!([
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        serde_json::from_str::<Value>(&row.get::<_, String>(2)?).unwrap(),
                    ]))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        let open_rows = {
            let mut statement = connection
                .prepare(
                    "SELECT source_id, source_table, source_row_id, vehicle_id, car_id,
                            domain, parent_source_row_id, row_json
                       FROM lifecycle_open_rows WHERE vehicle_id = ?1
                      ORDER BY source_table, source_row_id",
                )
                .unwrap();
            statement
                .query_map(params![binding.vehicle_id.to_string()], |row| {
                    assert_eq!(row.get::<_, String>(0)?, binding.source_id.to_string());
                    assert_eq!(row.get::<_, String>(3)?, binding.vehicle_id.to_string());
                    Ok(serde_json::json!([
                        "bound_source",
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        "bound_vehicle",
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                        serde_json::from_str::<Value>(&row.get::<_, String>(7)?).unwrap(),
                    ]))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        let sync_mutations = {
            let mut statement = connection
                .prepare(
                    "SELECT revision, entity, entity_id, car_id, operation,
                            payload_json, published, claimed_until_ms
                       FROM sync_mutations WHERE vehicle_id = ?1 ORDER BY revision",
                )
                .unwrap();
            statement
                .query_map(params![binding.vehicle_id.to_string()], |row| {
                    Ok(serde_json::json!([
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        serde_json::from_str::<Value>(&row.get::<_, String>(5)?).unwrap(),
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                    ]))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert!(history.iter().all(|row| {
            row.source_id == binding.source_id && row.vehicle_id == binding.vehicle_id
        }));
        assert!(current.iter().all(|row| {
            row.source_id == binding.source_id && row.vehicle_id == binding.vehicle_id
        }));
        assert!(
            lifecycle
                .as_ref()
                .is_none_or(|row| row.vehicle_id == binding.vehicle_id)
        );
        serde_json::json!({
            "history": history.iter().map(|row| serde_json::json!({
                "observation_id": row.observation_id,
                "source_id": "bound_source",
                "vehicle_id": "bound_vehicle",
                "observed_at_ms": row.observed_at_ms,
                "received_at_ms": row.received_at_ms,
                "payload_sha256": row.payload_sha256.to_string(),
                "payload": row.payload,
            })).collect::<Vec<_>>(),
            "current": current.iter().map(|row| serde_json::json!({
                "observation_id": row.observation_id,
                "source_id": "bound_source",
                "vehicle_id": "bound_vehicle",
                "observed_at_ms": row.observed_at_ms,
                "received_at_ms": row.received_at_ms,
                "payload_sha256": row.payload_sha256.to_string(),
                "payload": row.payload,
            })).collect::<Vec<_>>(),
            "lifecycle": lifecycle.map(|row| serde_json::json!({
                "vehicle_id": "bound_vehicle",
                "car_id": row.car_id,
                "last_observation_id": row.last_observation_id,
                "open_session_json": row.open_session_json,
                "quarantined": row.quarantined,
                "updated_at_ms": row.updated_at_ms,
            })),
            "accumulator": serde_json::from_slice::<Value>(&accumulator).unwrap(),
            "applications": applications,
            "dispositions": dispositions,
            "pending": pending,
            "materialised_states": materialised_states,
            "open_rows": open_rows,
            "sync_mutations": sync_mutations,
            "counts": {
                "applications": store.edge_ledger_counts("home-edge", "spool-2026-09-05").unwrap().applications,
                "sequences": store.edge_ledger_counts("home-edge", "spool-2026-09-05").unwrap().sequences,
                "pending_publications": store.edge_ledger_counts("home-edge", "spool-2026-09-05").unwrap().pending_publications,
                "ack_frontier": store.edge_ledger_counts("home-edge", "spool-2026-09-05").unwrap().ack_frontier,
            },
        })
    }

    let (_continuous_temporary, continuous, continuous_binding) = run(false);
    let (_restarted_temporary, restarted, restarted_binding) = run(true);
    let continuous = complete_snapshot(&continuous, &continuous_binding);
    let restarted = complete_snapshot(&restarted, &restarted_binding);
    assert_eq!(restarted, continuous);
    assert_eq!(restarted["counts"]["ack_frontier"], 5);
    assert_eq!(
        restarted["current"][0]["payload"]["provider_raw_json"]["response"]["charge_state"]["usable_battery_level"],
        80,
        "stale same-field Soc must not overwrite the newer value"
    );
    assert_eq!(
        restarted["current"][0]["payload"]["provider_raw_json"]["response"]["climate_state"]["inside_temp"],
        22.75
    );
    assert_eq!(
        restarted["current"][0]["payload"]["provider_raw_json"]["response"]["drive_state"]["power"],
        -4.0
    );
    assert_eq!(
        restarted["accumulator"]["field_watermarks"]["soc"],
        1_800_000_002_000_i64
    );
    assert_eq!(
        restarted["accumulator"]["field_watermarks"]["insidetemp"],
        1_800_000_001_000_i64
    );
    assert_eq!(
        restarted["accumulator"]["pack_voltage"],
        serde_json::json!({"value": 400.0, "timestamp_ms": 1_800_000_003_000_i64})
    );
    assert_eq!(
        restarted["accumulator"]["pack_current"],
        serde_json::json!({"value": 10.0, "timestamp_ms": 1_800_000_003_100_i64})
    );
}

#[test]
fn pending_publication_and_redacted_diagnostic_survive_store_restart() {
    let (temporary, store, binding) = edge_fixture();
    accept(&store, &binding, &projected_record(10)).unwrap();
    store
        .fail_edge_publications(&binding, 1_800_000_061_100, "publication_failed")
        .unwrap();
    store
        .set_edge_diagnostic(
            &binding,
            "pending_publication",
            "derived publication pending retry",
            1_800_000_061_100,
        )
        .unwrap();
    drop(store);

    let reopened = HubStore::initialize(temporary.path()).unwrap();
    let diagnostic = reopened
        .edge_diagnostic(&binding.installation_id, &binding.lineage)
        .unwrap()
        .unwrap();
    assert_eq!(diagnostic.state, "pending_publication");
    assert_eq!(diagnostic.detail, "derived publication pending retry");
    assert_eq!(diagnostic.updated_at_ms, 1_800_000_061_100);
    assert!(reopened.edge_has_pending_publications(&binding).unwrap());
    assert_eq!(
        reopened
            .edge_ledger_counts(&binding.installation_id, &binding.lineage)
            .unwrap()
            .pending_publications,
        1
    );
    assert_eq!(
        reopened
            .complete_edge_publications(&binding, 1_800_000_061_200)
            .unwrap(),
        1
    );
    assert!(!reopened.edge_has_pending_publications(&binding).unwrap());
    assert_eq!(
        reopened
            .edge_ledger_counts(&binding.installation_id, &binding.lineage)
            .unwrap()
            .pending_publications,
        0
    );
}
