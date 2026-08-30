// SPDX-License-Identifier: AGPL-3.0-only

fn mark_export_dirty_for_test(store: &HubStore, vehicle_id: Uuid) {
    let mut connection = store.open().expect("database");
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .expect("outbox transaction");
    mark_export_dirty_in_transaction(&transaction, vehicle_id).expect("mark export dirty");
    transaction.commit().expect("commit outbox mutation");
}

fn test_registered_vehicle(store: &HubStore) -> (SourceRecord, VehicleRecord) {
    let source = store
        .register_source(
            &SourceDescriptor::new("tesla_owner_api", "account-test"),
            1_000,
        )
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "vehicle-test"),
            1_001,
        )
        .expect("vehicle");
    (source, vehicle)
}

fn test_manifest() -> SyncManifest {
    let installation_id = Uuid::new_v4();
    let account_id = Uuid::new_v4();
    let vehicle_id = Uuid::new_v4();
    let digest = Sha256Digest::of_bytes(&[7_u8; 100]);
    let cursor = OpaqueCursor::issue(
        &CursorKey::from_bytes([7; 32]),
        CursorClaims {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            schema: SchemaVersion { major: 1, minor: 0 },
            installation_id,
            account_id,
            vehicle_id,
            generation: 1,
            sequence: 9,
        },
    )
    .expect("cursor");
    let pack = TransportPack {
        pack_id: Uuid::new_v4(),
        snapshot_id: Uuid::new_v4(),
        ordinal: 0,
        schema: SchemaVersion { major: 1, minor: 0 },
        format: PackFormat::SqliteTransport,
        compression: PackCompression::Zstd,
        relative_path: TransportPack::canonical_relative_path(digest),
        sha256: digest,
        compressed_bytes: 100,
        uncompressed_bytes: 100,
        row_count: 1,
        sequence: SequenceRange {
            from_exclusive: 9,
            to_inclusive: 9,
        },
        tables: vec![MirrorTable::Vehicle],
    };
    SyncManifest {
        protocol: ProtocolVersion { major: 1, minor: 0 },
        schema: SchemaVersion { major: 1, minor: 0 },
        installation_id,
        account_id,
        vehicle_id,
        generation: 1,
        snapshot_id: pack.snapshot_id,
        mode: TransferMode::FullSnapshot,
        base_sequence: 9,
        head_sequence: 9,
        chunk_count: 1,
        total_compressed_bytes: pack.compressed_bytes,
        total_uncompressed_bytes: pack.uncompressed_bytes,
        total_rows: pack.row_count,
        chunks: vec![pack],
        terminal_cursor: cursor,
    }
}

fn schema_22_test_manifest() -> SyncManifest {
    let mut manifest = test_manifest();
    manifest.schema = HUB_PROJECTION_SCHEMA_V3;
    manifest.chunks[0].schema = HUB_PROJECTION_SCHEMA_V3;
    manifest.chunks[0].format = PackFormat::HubProjectionSqlite;
    manifest.chunks[0].tables = vec![MirrorTable::Car];
    manifest.terminal_cursor = OpaqueCursor::issue(
        &CursorKey::from_bytes([7; 32]),
        CursorClaims {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            schema: HUB_PROJECTION_SCHEMA_V3,
            installation_id: manifest.installation_id,
            account_id: manifest.account_id,
            vehicle_id: manifest.vehicle_id,
            generation: manifest.generation,
            sequence: manifest.head_sequence,
        },
    )
    .expect("schema 2.2 cursor");
    manifest
        .validate()
        .expect("schema 2.2 remains protocol-valid");
    manifest
}

fn schema_22_successor_for_binding(
    binding: &ProjectionBinding,
) -> (SyncManifest, crate::updates_delivery::SignedNoOpState) {
    let mut manifest = schema_22_test_manifest();
    manifest.installation_id = binding.installation_id;
    manifest.account_id = binding.account_id;
    manifest.vehicle_id = binding.vehicle_id;
    manifest.generation = binding.generation;
    manifest.snapshot_id = Uuid::new_v4();
    manifest.base_sequence = 2;
    manifest.head_sequence = 2;
    manifest.chunks[0].snapshot_id = manifest.snapshot_id;
    manifest.chunks[0].sequence = SequenceRange {
        from_exclusive: 2,
        to_inclusive: 2,
    };
    manifest.terminal_cursor = OpaqueCursor::issue(
        &CursorKey::from_bytes([7; 32]),
        CursorClaims {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            schema: HUB_PROJECTION_SCHEMA_V3,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence: 2,
        },
    )
    .expect("schema 2.2 cursor");
    let noop = crate::updates_delivery::SignedNoOpState {
        schema: "teslatlas-hub-schema-22-noop-v1".into(),
        projection_schema: "2.2".into(),
        installation_id: binding.installation_id,
        account_id: binding.account_id,
        vehicle_id: binding.vehicle_id,
        generation: binding.generation,
        snapshot_id: manifest.snapshot_id,
        head_sequence: 2,
        pack_sha256: manifest.chunks[0].sha256.to_string(),
        terminal_cursor: manifest.terminal_cursor.clone(),
        source_witness: None,
    };
    (manifest, noop)
}

#[test]
fn initial_schema_22_finalizer_never_exposes_only_schema_21() {
    use crate::durability_fault::{DurabilityFaultPoint, inject};

    for inject_commit_fault in [true, false] {
        let temporary = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, legacy_manifest) = v2_base_manifest(&store);
        let run_id = store
            .begin_import_generation(
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                2_000,
            )
            .expect("staging generation");
        store
            .stage_import_generation_session(
                run_id,
                &crate::teslamate_projection::TeslaMateOpenSession {
                    car_id: binding.selected_car_id,
                    ..Default::default()
                },
            )
            .expect("staging session");
        let state = direct_test_projection_state(
            &store,
            run_id,
            &import_delta_test_car(binding.selected_car_id),
        );
        let (schema_22, noop) = schema_22_successor_for_binding(&binding);
        let gate = store.try_acquire_publication_gate().expect("gate");
        let result = if inject_commit_fault {
            let _fault = inject(DurabilityFaultPoint::CatalogueBeforeCommit);
            store.finalize_import_generation_with_projection_state_and_schema_22(
                &gate,
                run_id,
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                2_000,
                &legacy_manifest,
                Sha256Digest::of_bytes(b"atomic-schema-22"),
                &[],
                &binding,
                &state,
                &schema_22,
                &noop,
            )
        } else {
            store.finalize_import_generation_with_projection_state_and_schema_22(
                &gate,
                run_id,
                binding.account_id,
                vehicle.vehicle_id,
                binding.selected_car_id,
                2_000,
                &legacy_manifest,
                Sha256Digest::of_bytes(b"atomic-schema-22"),
                &[],
                &binding,
                &state,
                &schema_22,
                &noop,
            )
        };
        if inject_commit_fault {
            assert!(result.is_err(), "fault must abort both catalogue rows");
            assert!(
                store
                    .manifest_for_vehicle(vehicle.vehicle_id)
                    .expect("manifest lookup")
                    .is_none()
            );
        } else {
            result.expect("atomic first import");
            assert_eq!(
                store
                    .manifest_for_vehicle(vehicle.vehicle_id)
                    .expect("manifest lookup")
                    .expect("schema 2.2 head")
                    .schema,
                HUB_PROJECTION_SCHEMA_V3
            );
        }
    }
}

#[test]
fn initial_schema_22_finalizer_rejects_source_and_car_binding_mismatch() {
    for mismatch_source in [true, false] {
        let temporary = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, legacy_manifest) = v2_base_manifest(&store);
        let source_id = if mismatch_source {
            store
                .register_source(
                    &SourceDescriptor::new("teslamate_import", "mismatched-source"),
                    1_500,
                )
                .expect("mismatched source")
                .source_id
        } else {
            binding.account_id
        };
        let car_id = if mismatch_source {
            binding.selected_car_id
        } else {
            binding.selected_car_id + 1
        };
        let run_id = store
            .begin_import_generation(source_id, vehicle.vehicle_id, car_id, 2_000)
            .expect("mismatched staging generation");
        store
            .stage_import_generation_session(
                run_id,
                &crate::teslamate_projection::TeslaMateOpenSession {
                    car_id,
                    ..Default::default()
                },
            )
            .expect("mismatched staging session");
        let state = direct_test_projection_state(
            &store,
            run_id,
            &import_delta_test_car(binding.selected_car_id),
        );
        let (schema_22, noop) = schema_22_successor_for_binding(&binding);
        let gate = store.try_acquire_publication_gate().expect("gate");

        let error = store
            .finalize_import_generation_with_projection_state_and_schema_22(
                &gate,
                run_id,
                source_id,
                vehicle.vehicle_id,
                car_id,
                2_000,
                &legacy_manifest,
                Sha256Digest::of_bytes(b"mismatched-schema-22"),
                &[],
                &binding,
                &state,
                &schema_22,
                &noop,
            )
            .expect_err("source/car mismatch must fail closed");
        assert!(matches!(error, StoreError::LineageCatalogConflict));
        assert!(
            store
                .manifest_for_vehicle(vehicle.vehicle_id)
                .expect("manifest lookup")
                .is_none()
        );
        assert!(
            store
                .schema_22_noop_for_snapshot(vehicle.vehicle_id, schema_22.snapshot_id)
                .expect("no-op lookup")
                .is_none(),
            "binding mismatch must fail before immutable no-op publication"
        );
    }
}

#[test]
fn imported_home_work_geofences_match_live_endpoints_after_restart() {
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);
    let imported = vec![
        crate::teslamate_projection::TeslaMateGeofence {
            id: 10,
            name: "Home".into(),
            latitude: Some(51.0000),
            longitude: Some(-0.1000),
            radius_m: Some(150.0),
            billing_type: Some(crate::hub_pack::GeofenceBillingType::PerKwh),
            cost_per_unit: Some(0.30),
            session_fee: Some(2.0),
        },
        crate::teslamate_projection::TeslaMateGeofence {
            id: 11,
            name: "Work".into(),
            latitude: Some(51.0010),
            longitude: Some(-0.1010),
            radius_m: Some(150.0),
            billing_type: Some(crate::hub_pack::GeofenceBillingType::PerMinute),
            cost_per_unit: Some(0.10),
            session_fee: Some(1.0),
        },
    ];
    assert_eq!(
        store
            .upsert_geofences(vehicle.vehicle_id, &imported)
            .unwrap(),
        2
    );
    assert_eq!(
        store
            .upsert_geofences(vehicle.vehicle_id, &imported)
            .unwrap(),
        0
    );

    let session = crate::lifecycle::OpenSessionState::new();
    let encoded = session.encode().expect("encode session");
    let drive = crate::hub_pack::ProjectionDrive {
        id: 1,
        car_id: 1,
        optimized_at_ms: None,
        start_date_ms: 1_000,
        end_date_ms: 2_000,
        distance_km: Some(1.0),
        duration_min: Some(1),
        efficiency: None,
        outside_temp_avg: None,
        inside_temp_avg: None,
        speed_max: Some(20),
        power_max: None,
        power_min: None,
        start_ideal_range_km: None,
        end_ideal_range_km: None,
        start_address: None,
        end_address: None,
        start_geofence: None,
        end_geofence: None,
        start_latitude: Some(51.0001),
        start_longitude: Some(-0.1001),
        end_latitude: Some(51.0011),
        end_longitude: Some(-0.1011),
        start_soc: Some(80),
        end_soc: Some(79),
        start_rated_range_km: None,
        end_rated_range_km: None,
        ascent: None,
        descent: None,
    };
    let charge = crate::hub_pack::ProjectionCharge {
        id: 2,
        car_id: 1,
        start_date_ms: 3_000,
        end_date_ms: Some(4_000),
        charge_energy_added: Some(1.0),
        charge_energy_used_kwh: None,
        start_ideal_range_km: None,
        end_ideal_range_km: None,
        cost: None,
        fast_charger_type: None,
        billing_type: None,
        cost_per_unit: None,
        session_fee: None,
        start_latitude: Some(47.5),
        start_longitude: Some(19.0),
        start_battery_level: Some(50),
        end_battery_level: Some(51),
        duration_min: Some(1),
        address: None,
        location_name: None,
        geofence: None,
        is_dc: Some(false),
        charge_rate_km_per_hour: None,
        max_charger_power_kw: Some(7.0),
        outside_temp_avg: None,
        start_rated_range_km: None,
        end_rated_range_km: None,
    };
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle.vehicle_id,
            car_id: 1,
            open_session_json: &encoded,
            last_observation_id: 1,
            quarantined: false,
            updated_at_ms: 4_000,
            delta: &crate::lifecycle::LifecycleDelta {
                drives: vec![drive],
                charges: vec![charge],
                charge_start_coordinates: vec![(2, 51.0001, -0.1001)],
                ..Default::default()
            },
        })
        .expect("live endpoint materialisation");

    let reopened = HubStore::initialize(temp.path()).expect("restart store");
    let connection = reopened.open().expect("open queue");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM address_enrichment_jobs WHERE vehicle_id = ?1",
                params![vehicle.vehicle_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        3
    );
    drop(connection);
    let first_job = reopened
        .claim_address_enrichment_job(5_000)
        .unwrap()
        .expect("pending start job");
    assert_eq!(first_job.target_type, "charge");
    assert_eq!(first_job.field, "address");
    reopened
        .complete_address_enrichment(&first_job, Some("Delayed response address"), 6_000)
        .unwrap();
    let retry_job = reopened
        .claim_address_enrichment_job(5_000)
        .unwrap()
        .expect("pending end job");
    reopened
        .retry_address_enrichment(&retry_job, "temporary transport", 5_000)
        .unwrap();
    let remaining_job = reopened
        .claim_address_enrichment_job(5_000)
        .unwrap()
        .expect("remaining endpoint job");
    reopened
        .complete_address_enrichment(&remaining_job, None, 6_000)
        .unwrap();
    drop(reopened);
    let resumed = HubStore::initialize(temp.path()).expect("resume store");
    assert!(
        resumed
            .claim_address_enrichment_job(14_999)
            .unwrap()
            .is_none()
    );
    assert!(
        resumed
            .claim_address_enrichment_job(15_000)
            .unwrap()
            .is_some()
    );
    let history = resumed
        .materialised_history(vehicle.vehicle_id)
        .expect("history");
    assert_eq!(
        history.charges[0].address.as_deref(),
        Some("Delayed response address")
    );
    let stored_charge = resumed
        .open()
        .unwrap()
        .query_row(
            "SELECT charge_json FROM materialised_charges WHERE vehicle_id = ?1",
            params![vehicle.vehicle_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    assert!(!stored_charge.contains("osm_type"));
    assert_eq!(history.drives[0].start_geofence.as_deref(), Some("Home"));
    assert_eq!(history.drives[0].end_geofence.as_deref(), Some("Work"));
    assert_eq!(history.charges[0].geofence.as_deref(), Some("Home"));
    assert_eq!(
        history.charges[0].billing_type,
        Some(crate::hub_pack::GeofenceBillingType::PerKwh)
    );
    assert_eq!(history.charges[0].cost_per_unit, Some(0.30));
    assert_eq!(history.charges[0].session_fee, Some(2.0));
    assert_eq!(history.charges[0].cost, Some(2.3));
}

#[test]
fn lifecycle_commit_recomputes_charge_energy_from_all_durable_samples() {
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);
    let start = 1_800_000_000_000;
    let sample = |observation_id, observed_at_ms, charge_state| crate::lifecycle::LifecycleSample {
        observation_id,
        observed_at_ms,
        vehicle_state: "online".to_owned(),
        payload: serde_json::json!({
            "record_type": "owner_api_vehicle_data_v1",
            "source_vehicle_id": "9",
            "vehicle_data": {"charge_state": charge_state},
        }),
    };
    let step = crate::lifecycle::apply_samples(
        crate::lifecycle::OpenSessionState::new(),
        1,
        &[
            sample(
                1,
                start,
                serde_json::json!({
                    "charging_state": "Charging",
                    "timestamp": start,
                    "charger_power": 1.0
                }),
            ),
            sample(
                2,
                start + 3_600_000,
                serde_json::json!({
                    "charging_state": "Charging",
                    "timestamp": start + 3_600_000,
                    "charger_power": 6.0,
                    "charger_phases": 1
                }),
            ),
            sample(
                3,
                start + 7_200_000,
                serde_json::json!({
                    "charging_state": "Complete",
                    "timestamp": start + 7_200_000,
                    "charger_power": 0.0
                }),
            ),
        ],
    )
    .expect("closed charge");
    let encoded = step.state.encode().expect("encode state");
    let mut delta = step.delta;
    delta.charges[0].charge_energy_used_kwh = Some(999.0);
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle.vehicle_id,
            car_id: 1,
            open_session_json: &encoded,
            last_observation_id: 3,
            quarantined: false,
            updated_at_ms: start + 7_200_000,
            delta: &delta,
        })
        .expect("commit lifecycle");
    let history = store
        .materialised_history(vehicle.vehicle_id)
        .expect("materialised history");
    assert_eq!(history.charges[0].charge_energy_used_kwh, Some(6.0));
}

#[test]
fn lifecycle_state_intervals_upsert_and_survive_store_restart() {
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);
    let state = crate::lifecycle::OpenSessionState::new();
    let encoded = state.encode().expect("encode session");
    let first = crate::hub_pack::ProjectionState {
        id: 1,
        car_id: 1,
        state: "online".into(),
        start_date_ms: 1_000,
        end_date_ms: None,
    };
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle.vehicle_id,
            car_id: 1,
            open_session_json: &encoded,
            last_observation_id: 1,
            quarantined: false,
            updated_at_ms: 1_000,
            delta: &crate::lifecycle::LifecycleDelta {
                states: vec![first.clone()],
                ..Default::default()
            },
        })
        .expect("write open state");

    let closed = crate::hub_pack::ProjectionState {
        end_date_ms: Some(2_000),
        ..first
    };
    let next = crate::hub_pack::ProjectionState {
        id: 2,
        car_id: 1,
        state: "asleep".into(),
        start_date_ms: 2_000,
        end_date_ms: None,
    };
    let update = crate::hub_pack::ProjectionUpdate {
        id: 1,
        car_id: 1,
        start_date_ms: 1_500,
        end_date_ms: 2_500,
        version: "2026.2".into(),
    };
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle.vehicle_id,
            car_id: 1,
            open_session_json: &encoded,
            last_observation_id: 2,
            quarantined: false,
            updated_at_ms: 2_000,
            delta: &crate::lifecycle::LifecycleDelta {
                states: vec![closed, next],
                updates: vec![update],
                ..Default::default()
            },
        })
        .expect("close and open state");

    drop(store);
    let reopened = HubStore::initialize(temp.path()).expect("restart store");
    let history = reopened
        .materialised_history(vehicle.vehicle_id)
        .expect("state history");
    assert_eq!(history.states.len(), 2);
    assert_eq!(history.states[0].state, "online");
    assert_eq!(history.states[0].end_date_ms, Some(2_000));
    assert_eq!(history.states[1].state, "asleep");
    assert_eq!(history.states[1].end_date_ms, None);
    assert_eq!(history.updates.len(), 1);
    assert_eq!(history.updates[0].version, "2026.2");
}

#[test]
fn lifecycle_car_metadata_is_durable_and_preserves_imported_efficiency() {
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);
    let imported = crate::hub_pack::ProjectionCar {
        id: 1,
        name: "Imported car".into(),
        model: "3".into(),
        vin: Some("5YJIMPORTED123456".into()),
        source_eid: Some(88),
        source_vid: Some(99),
        trim_badging: Some("74D".into()),
        marketing_name: Some("LR AWD".into()),
        exterior_color: Some("Pearl White".into()),
        wheel_type: Some("Apollo".into()),
        spoiler_type: Some("None".into()),
        firmware_version: Some("2026.0".into()),
        efficiency_wh_per_km: Some(145.0),
        settings: Default::default(),
    };
    store
        .open()
        .expect("open")
        .execute(
            "INSERT INTO materialised_cars(vehicle_id, car_id, car_json) VALUES (?1, ?2, ?3)",
            params![
                vehicle.vehicle_id.to_string(),
                imported.id,
                serde_json::to_string(&imported).expect("serialize imported car")
            ],
        )
        .expect("seed imported car");

    let mut state = crate::lifecycle::OpenSessionState::new();
    state.last_observation_id = 1;
    state.car_metadata = Some(crate::hub_pack::ProjectionCarPatch {
        name: Some("Road car".into()),
        model: Some("3".into()),
        vin: Some("5YJNEWVIN1234567".into()),
        trim_badging: Some("74D".into()),
        marketing_name: Some("LR AWD".into()),
        exterior_color: Some("Pearl White".into()),
        wheel_type: Some("Apollo".into()),
        spoiler_type: Some("None".into()),
        firmware_version: Some("2026.1".into()),
    });
    let encoded = state.encode().expect("encode metadata state");
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle.vehicle_id,
            car_id: 1,
            open_session_json: &encoded,
            last_observation_id: 1,
            quarantined: false,
            updated_at_ms: 2_000,
            delta: &crate::lifecycle::LifecycleDelta::default(),
        })
        .expect("commit metadata");

    let car_mutations_before: i64 = store
        .open()
        .expect("mutation database")
        .query_row(
            "SELECT COUNT(*) FROM sync_mutations
                 WHERE vehicle_id = ?1 AND entity = 'car'",
            params![vehicle.vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("car mutation count");
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle.vehicle_id,
            car_id: 1,
            open_session_json: &encoded,
            last_observation_id: 1,
            quarantined: false,
            updated_at_ms: 2_001,
            delta: &crate::lifecycle::LifecycleDelta::default(),
        })
        .expect("repeat identical metadata");
    let car_mutations_after: i64 = store
        .open()
        .expect("repeat mutation database")
        .query_row(
            "SELECT COUNT(*) FROM sync_mutations
                 WHERE vehicle_id = ?1 AND entity = 'car'",
            params![vehicle.vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("repeat car mutation count");
    assert_eq!(
        car_mutations_after, car_mutations_before,
        "identical lifecycle metadata must not advance the sync journal"
    );

    let history = store
        .materialised_history(vehicle.vehicle_id)
        .expect("load metadata");
    let car = history.car.expect("materialised car");
    assert_eq!(car.name, "Road car");
    assert_eq!(car.model, "3");
    assert_eq!(car.vin.as_deref(), Some("5YJNEWVIN1234567"));
    assert_eq!(car.trim_badging.as_deref(), Some("74D"));
    assert_eq!(car.marketing_name.as_deref(), Some("LR AWD"));
    assert_eq!(car.exterior_color.as_deref(), Some("Pearl White"));
    assert_eq!(car.wheel_type.as_deref(), Some("Apollo"));
    assert_eq!(car.spoiler_type.as_deref(), Some("None"));
    assert_eq!(car.firmware_version.as_deref(), Some("2026.1"));
    assert_eq!(car.efficiency_wh_per_km, Some(145.0));

    state.last_observation_id = 2;
    state.car_metadata = Some(crate::hub_pack::ProjectionCarPatch {
        firmware_version: Some("2026.2".into()),
        ..Default::default()
    });
    let encoded = state.encode().expect("encode partial metadata state");
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle.vehicle_id,
            car_id: 1,
            open_session_json: &encoded,
            last_observation_id: 2,
            quarantined: false,
            updated_at_ms: 3_000,
            delta: &crate::lifecycle::LifecycleDelta::default(),
        })
        .expect("commit partial metadata");
    let car = store
        .materialised_history(vehicle.vehicle_id)
        .expect("reload metadata")
        .car
        .expect("materialised car after partial update");
    assert_eq!(car.name, "Road car");
    assert_eq!(car.vin.as_deref(), Some("5YJNEWVIN1234567"));
    assert_eq!(car.firmware_version.as_deref(), Some("2026.2"));
    assert_eq!(car.efficiency_wh_per_km, Some(145.0));
}

#[test]
fn repair_preserves_quarantined_sessions_and_removes_orphaned_packs() {
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);

    let connection = store.open().expect("open");
    connection
            .execute(
                "INSERT INTO vehicle_lifecycle_state(
                    vehicle_id, car_id, last_observation_id, open_session_json, quarantined, updated_at_ms
                 ) VALUES (?1, 1, 1, x'7b7d', 1, 1000)",
                params![vehicle.vehicle_id.to_string()],
            )
            .expect("insert quarantined");
    drop(connection);

    let orphaned_pack = store
        .packs_dir()
        .join("0000000000000000000000000000000000000000000000000000000000000000.sqlite.zst");
    std::fs::write(&orphaned_pack, b"orphaned bytes").expect("write pack");

    let report = store.repair().expect("repair");
    assert_eq!(report.status, "ok");
    assert_eq!(
        report.sqlite_integrity,
        catalogue_quick_check_label(&store.open().expect("integrity connection")).expect("pragma")
    );
    assert_eq!(report.sqlite_integrity, "ok");
    assert!(matches!(
        store.readiness_check(),
        Err(StoreError::QuarantinedLifecycle(1))
    ));
    assert_eq!(report.quarantined_sessions_preserved, 1);
    assert_eq!(report.orphaned_packs_removed, 1);
    assert_eq!(report.freed_bytes, 14);
    assert!(!orphaned_pack.exists());

    let connection = store.open().expect("open");
    let quarantined: i64 = connection
        .query_row(
            "SELECT quarantined FROM vehicle_lifecycle_state WHERE vehicle_id = ?1",
            params![vehicle.vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("query quarantined");
    assert_eq!(quarantined, 1);
}

#[test]
fn repair_does_not_report_ok_integrity_for_a_corrupt_catalogue() {
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let healthy = store.repair().expect("healthy repair");
    assert_eq!(
        healthy.sqlite_integrity,
        catalogue_quick_check_label(&store.open().expect("healthy connection"))
            .expect("healthy pragma")
    );

    std::fs::write(store.database_path(), b"not a sqlite catalogue").expect("overwrite catalogue");
    match store.repair() {
        Err(StoreError::Integrity(label)) => assert_ne!(label, "ok"),
        Err(StoreError::Open(_) | StoreError::Configure(_) | StoreError::Query(_)) => {}
        Ok(report) => panic!(
            "corrupt catalogue reported sqlite_integrity={}",
            report.sqlite_integrity
        ),
        Err(other) => panic!("unexpected repair error: {other}"),
    }
}

#[test]
fn car_settings_are_idempotent_and_survive_reopen() {
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);
    let settings = ProjectionCarSettings {
        enabled: false,
        use_streaming_api: false,
        suspend_after_idle_min: 4,
        suspend_min: 9,
        suspend_min_resolved: true,
        req_not_unlocked: true,
        free_supercharging: true,
        lfp_battery: true,
    };
    store
        .upsert_car_settings(vehicle.vehicle_id, 1, &settings)
        .expect("first settings write");
    store
        .upsert_car_settings(vehicle.vehicle_id, 1, &settings)
        .expect("idempotent settings write");
    assert_eq!(
        store.load_car_settings(vehicle.vehicle_id).unwrap(),
        settings
    );
    let settings_mutations: i64 = store
        .open()
        .expect("mutation database")
        .query_row(
            "SELECT COUNT(*) FROM sync_mutations
                 WHERE vehicle_id = ?1 AND entity = 'car_setting'",
            params![vehicle.vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("settings mutation count");
    assert_eq!(
        settings_mutations, 1,
        "an identical settings write must not advance the sync journal"
    );
    drop(store);
    let reopened = HubStore::initialize(temp.path()).expect("reopen");
    assert_eq!(
        reopened.load_car_settings(vehicle.vehicle_id).unwrap(),
        settings
    );
}

#[test]
fn unresolved_live_default_resolves_once_and_explicit_value_wins() {
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);
    let live = ProjectionCarSettings::new_live();
    store
        .upsert_car_settings(vehicle.vehicle_id, 1, &live)
        .expect("live settings");
    assert!(
        store
            .resolve_car_suspend_min(vehicle.vehicle_id, Some("3"), Some("74D"), None)
            .expect("resolve model 3")
    );
    let resolved = store.load_car_settings(vehicle.vehicle_id).unwrap();
    assert_eq!(resolved.suspend_min, 12);
    assert!(resolved.suspend_min_resolved);
    drop(store);
    let reopened = HubStore::initialize(temp.path()).expect("restart");
    assert!(
        !reopened
            .resolve_car_suspend_min(vehicle.vehicle_id, Some("Y"), None, None)
            .expect("metadata must not rewrite")
    );
    assert_eq!(
        reopened
            .load_car_settings(vehicle.vehicle_id)
            .unwrap()
            .suspend_min,
        12
    );

    let explicit_source = reopened
        .register_source(
            &SourceDescriptor::new("tesla_owner_api", "explicit-test"),
            2_000,
        )
        .expect("explicit source");
    let explicit_vehicle = reopened
        .register_vehicle(
            &VehicleDescriptor::new(explicit_source.source_id, "explicit-vehicle"),
            2_001,
        )
        .expect("explicit vehicle");
    let explicit = ProjectionCarSettings {
        suspend_min: 7,
        suspend_min_resolved: true,
        ..ProjectionCarSettings::default()
    };
    reopened
        .upsert_car_settings(explicit_vehicle.vehicle_id, 1, &explicit)
        .expect("explicit settings");
    assert_eq!(
        reopened
            .load_car_settings(explicit_vehicle.vehicle_id)
            .unwrap()
            .suspend_min,
        7
    );
    assert!(
        !reopened
            .resolve_car_suspend_min(explicit_vehicle.vehicle_id, Some("3"), None, None)
            .expect("explicit value must stay authoritative")
    );
}

#[test]
fn stream_watermark_is_strictly_increasing_and_survives_reopen() {
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);

    assert!(
        store
            .accept_stream_timestamp(vehicle.vehicle_id, 1_000)
            .expect("first watermark")
    );
    assert!(
        !store
            .accept_stream_timestamp(vehicle.vehicle_id, 1_000)
            .expect("duplicate watermark")
    );
    assert!(
        !store
            .accept_stream_timestamp(vehicle.vehicle_id, 999)
            .expect("older watermark")
    );
    assert!(
        store
            .accept_stream_timestamp(vehicle.vehicle_id, 1_001)
            .expect("newer watermark")
    );

    drop(store);
    let reopened = HubStore::initialize(temp.path()).expect("reopen");
    assert!(
        !reopened
            .accept_stream_timestamp(vehicle.vehicle_id, 1_000)
            .expect("old frame after restart")
    );
    assert!(
        reopened
            .accept_stream_timestamp(vehicle.vehicle_id, 1_002)
            .expect("new frame after restart")
    );
}

#[test]
fn verify_no_wake_applies_the_captured_receipt_watermark() {
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let correlation_id = Uuid::new_v4();

    let old = store
        .begin_outbound_request(&OutboundRequestStart {
            correlation_id,
            vehicle_tesla_id: Some(505),
            transport: OutboundRequestTransport::OwnerApi,
            operation: OutboundRequestOperation::VehicleProbe,
            safety_class: OutboundRequestSafetyClass::DirectWakeCommand,
            precondition: OutboundRequestPrecondition::NotRequired,
        })
        .expect("old receipt");
    store
        .complete_outbound_request(
            old,
            &OutboundRequestCompletion {
                outcome: OutboundRequestOutcome::Success,
                http_status: None,
                retry_after_seconds: None,
            },
        )
        .expect("complete old receipt");
    let watermark = store
        .outbound_request_watermark()
        .expect("watermark")
        .receipt_id;

    let current = store
        .begin_outbound_request(&OutboundRequestStart {
            correlation_id,
            vehicle_tesla_id: Some(505),
            transport: OutboundRequestTransport::OwnerApi,
            operation: OutboundRequestOperation::Products,
            safety_class: OutboundRequestSafetyClass::NonWakeEndpoint,
            precondition: OutboundRequestPrecondition::NotRequired,
        })
        .expect("current receipt");
    store
        .complete_outbound_request(
            current,
            &OutboundRequestCompletion {
                outcome: OutboundRequestOutcome::Success,
                http_status: None,
                retry_after_seconds: None,
            },
        )
        .expect("complete current receipt");

    let verification = store
        .verify_no_wake_after(watermark, correlation_id, None)
        .expect("verify watermark window");
    assert_eq!(verification.matching_receipts, 1);
    assert_eq!(verification.direct_wake_receipts, 0);
    assert_eq!(verification.unresolved_receipts, 0);
    assert!(verification.verified());
}

#[test]
fn sync_mutations_are_durable_monotonic_and_coalescible() {
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);
    let car = crate::hub_pack::ProjectionCar {
        id: 1,
        name: "Test car".into(),
        model: "3".into(),
        vin: None,
        source_eid: None,
        source_vid: None,
        trim_badging: None,
        marketing_name: None,
        exterior_color: None,
        wheel_type: None,
        spoiler_type: None,
        firmware_version: None,
        efficiency_wh_per_km: None,
        settings: ProjectionCarSettings::default(),
    };
    store
        .persist_materialised_car_if_absent(vehicle.vehicle_id, &car)
        .expect("car");
    store
        .upsert_car_settings(vehicle.vehicle_id, 1, &ProjectionCarSettings::default())
        .expect("settings one");
    store
        .upsert_car_settings(
            vehicle.vehicle_id,
            1,
            &ProjectionCarSettings {
                enabled: false,
                ..ProjectionCarSettings::default()
            },
        )
        .expect("settings two");

    let connection = store.open().expect("open");
    let revisions: Vec<i64> = connection
        .prepare(
            "SELECT revision FROM sync_mutations
                 WHERE vehicle_id = ?1 ORDER BY revision",
        )
        .expect("journal query")
        .query_map(params![vehicle.vehicle_id.to_string()], |row| row.get(0))
        .expect("journal rows")
        .map(|row| row.expect("revision"))
        .collect();
    assert_eq!(revisions, vec![1, 2, 3]);
    drop(connection);

    let claim = store
        .claim_sync_mutations(vehicle.vehicle_id, 2_000, 100)
        .expect("claim")
        .expect("pending mutations");
    assert_eq!((claim.from_revision, claim.to_revision), (1, 3));
    let delta = store
        .projection_delta_for_mutations(
            &claim,
            store
                .v2_projection_binding(vehicle.vehicle_id)
                .expect("binding"),
            SequenceRange {
                from_exclusive: 0,
                to_inclusive: 3,
            },
            Sha256Digest::of_bytes(b"parent"),
        )
        .expect("typed delta");
    assert_eq!(delta.cars.len(), 1);
    assert_eq!(delta.car_settings.len(), 0);
    assert_eq!(delta.cars.len() + delta.car_settings.len(), 1);
    store.release_sync_mutations(&claim).expect("release");
}

#[test]
fn native_controls_update_settings_geofences_charge_cost_and_gpx_pages() {
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);
    let mut settings = ProjectionCarSettings {
        enabled: false,
        ..ProjectionCarSettings::default()
    };
    store
        .upsert_car_settings(vehicle.vehicle_id, 1, &settings)
        .expect("pause collection");
    assert!(!store.load_car_settings(vehicle.vehicle_id).unwrap().enabled);
    settings.suspend_min = 12;
    store
        .replace_car_settings(vehicle.vehicle_id, &settings)
        .expect("owner suspend override");
    assert_eq!(
        store
            .load_car_settings(vehicle.vehicle_id)
            .unwrap()
            .suspend_min,
        12
    );
    let pending: i64 = store
        .open()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM export_outbox WHERE vehicle_id = ?1",
            params![vehicle.vehicle_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending, 1);

    let geofence = store
        .save_geofence(
            vehicle.vehicle_id,
            None,
            crate::teslamate_projection::TeslaMateGeofence {
                id: 0,
                name: " Home ".into(),
                latitude: Some(47.5),
                longitude: Some(19.0),
                radius_m: Some(20.0),
                billing_type: Some(crate::hub_pack::GeofenceBillingType::PerKwh),
                cost_per_unit: Some(0.25),
                session_fee: Some(1.0),
            },
        )
        .expect("create geofence");
    assert_eq!((geofence.id, geofence.name.as_str()), (1, "Home"));
    let mut replacement = geofence.clone();
    replacement.cost_per_unit = None;
    store
        .save_geofence(vehicle.vehicle_id, Some(geofence.id), replacement)
        .expect("replace geofence");
    assert_eq!(store.geofences(vehicle.vehicle_id).unwrap().len(), 1);
    assert_eq!(
        store.geofences(vehicle.vehicle_id).unwrap()[0].cost_per_unit,
        None
    );
    store
        .delete_geofence(vehicle.vehicle_id, geofence.id)
        .expect("delete geofence");
    assert!(store.geofences(vehicle.vehicle_id).unwrap().is_empty());

    let drive = ProjectionDrive {
        id: 7,
        car_id: 1,
        optimized_at_ms: None,
        start_date_ms: 1_700_000_000_000,
        end_date_ms: 1_700_000_001_000,
        distance_km: Some(1.0),
        duration_min: Some(1),
        efficiency: None,
        outside_temp_avg: None,
        inside_temp_avg: None,
        speed_max: None,
        power_max: None,
        power_min: None,
        start_ideal_range_km: None,
        end_ideal_range_km: None,
        start_address: None,
        end_address: None,
        start_geofence: None,
        end_geofence: None,
        start_latitude: Some(47.5),
        start_longitude: Some(19.0),
        end_latitude: Some(47.6),
        end_longitude: Some(19.1),
        start_soc: None,
        end_soc: None,
        start_rated_range_km: None,
        end_rated_range_km: None,
        ascent: None,
        descent: None,
    };
    let position = |id, date_ms| crate::hub_pack::ProjectionPosition {
        id,
        drive_id: Some(7),
        car_id: 1,
        date_ms,
        latitude: 47.5,
        longitude: 19.0,
        speed: None,
        power: None,
        battery_level: None,
        usable_battery_level: None,
        elevation: None,
        odometer: None,
        ideal_battery_range_km: None,
        est_battery_range_km: None,
        rated_battery_range_km: None,
        fan_status: None,
        driver_temp_setting: None,
        passenger_temp_setting: None,
        is_climate_on: None,
        is_rear_defroster_on: None,
        is_front_defroster_on: None,
        inside_temp: None,
        outside_temp: None,
        battery_heater: None,
        battery_heater_on: None,
        battery_heater_no_power: None,
        tpms_pressure_fl: None,
        tpms_pressure_fr: None,
        tpms_pressure_rl: None,
        tpms_pressure_rr: None,
    };
    let charge = crate::hub_pack::ProjectionCharge {
        id: 9,
        car_id: 1,
        start_date_ms: 1_700_000_000_000,
        end_date_ms: Some(1_700_000_001_000),
        charge_energy_added: Some(10.0),
        charge_energy_used_kwh: Some(11.0),
        start_ideal_range_km: None,
        end_ideal_range_km: None,
        cost: None,
        fast_charger_type: None,
        billing_type: None,
        cost_per_unit: None,
        session_fee: None,
        start_latitude: Some(47.5),
        start_longitude: Some(19.0),
        start_battery_level: None,
        end_battery_level: None,
        duration_min: Some(10),
        address: None,
        location_name: None,
        geofence: None,
        is_dc: None,
        charge_rate_km_per_hour: None,
        max_charger_power_kw: None,
        outside_temp_avg: None,
        start_rated_range_km: None,
        end_rated_range_km: None,
    };
    let connection = store.open().unwrap();
    connection
        .execute(
            "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
                 VALUES (?1, 7, 1, ?2)",
            params![
                vehicle.vehicle_id.to_string(),
                serde_json::to_string(&drive).unwrap()
            ],
        )
        .unwrap();
    for row in [
        position(2, 1_700_000_000_200),
        position(1, 1_700_000_000_100),
    ] {
        connection
            .execute(
                "INSERT INTO materialised_positions(
                        vehicle_id, position_id, drive_id, car_id, position_json
                     ) VALUES (?1, ?2, 7, 1, ?3)",
                params![
                    vehicle.vehicle_id.to_string(),
                    row.id,
                    serde_json::to_string(&row).unwrap()
                ],
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
                 VALUES (?1, 9, 1, ?2)",
            params![
                vehicle.vehicle_id.to_string(),
                serde_json::to_string(&charge).unwrap()
            ],
        )
        .unwrap();
    drop(connection);

    let page = store
        .drive_positions_page(vehicle.vehicle_id, 7, None, 1)
        .expect("first GPX page");
    assert_eq!(page[0].id, 1);
    let next = store
        .drive_positions_page(
            vehicle.vehicle_id,
            7,
            Some((page[0].date_ms, page[0].id)),
            1,
        )
        .expect("second GPX page");
    assert_eq!(next[0].id, 2);
    let mut gpx = Vec::new();
    crate::gpx::export_drive_gpx(&store, vehicle.vehicle_id, 7, &mut gpx).expect("drive GPX");
    let gpx = String::from_utf8(gpx).expect("GPX UTF-8");
    assert!(gpx.contains("<name>2023-11-14T22:13:20Z</name>"));
    assert!(gpx.find("2023-11-14T22:13:20.1Z") < gpx.find("2023-11-14T22:13:20.2Z"));
    let active_geofence = store
        .save_geofence(
            vehicle.vehicle_id,
            None,
            crate::teslamate_projection::TeslaMateGeofence {
                id: 0,
                name: "Tariff".into(),
                latitude: Some(47.5),
                longitude: Some(19.0),
                radius_m: Some(100.0),
                billing_type: Some(crate::hub_pack::GeofenceBillingType::PerKwh),
                cost_per_unit: Some(0.25),
                session_fee: Some(1.0),
            },
        )
        .expect("save active geofence");
    assert_eq!(
        store
            .materialised_drive_for_vehicle(vehicle.vehicle_id, 7)
            .unwrap()
            .unwrap()
            .start_geofence
            .as_deref(),
        Some("Tariff")
    );
    assert_eq!(
        store
            .recalculate_missing_charge_costs(vehicle.vehicle_id, active_geofence.id)
            .expect("recalculate matching charge"),
        1
    );
    assert_eq!(
        store
            .set_charge_cost(vehicle.vehicle_id, 9, 4.25)
            .expect("set charge cost")
            .cost,
        Some(4.25)
    );
    assert_eq!(
        store
            .set_charge_cost_rate(
                vehicle.vehicle_id,
                9,
                0.5,
                crate::hub_pack::GeofenceBillingType::PerKwh,
            )
            .expect("set per-kWh cost")
            .cost,
        Some(5.5)
    );
    assert_eq!(
        store
            .set_charge_cost_rate(
                vehicle.vehicle_id,
                9,
                0.25,
                crate::hub_pack::GeofenceBillingType::PerMinute,
            )
            .expect("set per-minute cost")
            .cost,
        Some(2.5)
    );
    store
        .delete_geofence(vehicle.vehicle_id, active_geofence.id)
        .expect("delete active geofence");
    assert_eq!(
        store
            .materialised_drive_for_vehicle(vehicle.vehicle_id, 7)
            .unwrap()
            .unwrap()
            .start_geofence,
        None
    );
}

#[test]
fn rated_range_charge_consensus_updates_live_car_efficiency() {
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);
    let car = ProjectionCar {
        id: 1,
        name: "Efficiency car".into(),
        model: "3".into(),
        vin: None,
        source_eid: None,
        source_vid: None,
        trim_badging: None,
        marketing_name: None,
        exterior_color: None,
        wheel_type: None,
        spoiler_type: None,
        firmware_version: None,
        efficiency_wh_per_km: None,
        settings: ProjectionCarSettings::default(),
    };
    store
        .persist_materialised_car_if_absent(vehicle.vehicle_id, &car)
        .expect("materialised car");
    let charge = |id| crate::hub_pack::ProjectionCharge {
        id,
        car_id: 1,
        start_date_ms: 1_700_000_000_000 + id * 100_000,
        end_date_ms: Some(1_700_000_050_000 + id * 100_000),
        charge_energy_added: Some(10.0),
        charge_energy_used_kwh: Some(11.0),
        start_ideal_range_km: Some(100.0),
        end_ideal_range_km: Some(140.0),
        cost: None,
        fast_charger_type: None,
        billing_type: None,
        cost_per_unit: None,
        session_fee: None,
        start_latitude: None,
        start_longitude: None,
        start_battery_level: Some(40),
        end_battery_level: Some(80),
        duration_min: Some(30),
        address: None,
        location_name: None,
        geofence: None,
        is_dc: None,
        charge_rate_km_per_hour: None,
        max_charger_power_kw: None,
        outside_temp_avg: None,
        start_rated_range_km: Some(100.0),
        end_rated_range_km: Some(150.0),
    };
    let mut connection = store.open().expect("open");
    for id in 1..=8 {
        let row = charge(id);
        connection
            .execute(
                "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
                     VALUES (?1, ?2, 1, ?3)",
                params![
                    vehicle.vehicle_id.to_string(),
                    id,
                    serde_json::to_string(&row).unwrap()
                ],
            )
            .expect("charge");
    }
    let transaction = connection.transaction().expect("transaction");
    recompute_car_efficiency(&transaction, vehicle.vehicle_id, 1).expect("efficiency");
    transaction.commit().expect("commit");

    assert_eq!(
        store
            .materialised_car_for_vehicle(vehicle.vehicle_id)
            .unwrap()
            .unwrap()
            .efficiency_wh_per_km,
        Some(0.2)
    );
}
