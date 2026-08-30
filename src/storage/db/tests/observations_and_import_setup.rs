// SPDX-License-Identifier: AGPL-3.0-only

#[test]
fn appends_canonical_json_once_and_retries_idempotently() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store initializes");
    let (source, vehicle) = test_registered_vehicle(&store);
    let input = ObservationInput {
        source_id: source.source_id,
        vehicle_id: vehicle.vehicle_id,
        observed_at_ms: 10_000,
        payload: serde_json::json!({"speed": 0, "battery_level": 80}),
    };

    let first = store
        .append_observation(&input, 10_010)
        .expect("first observation");
    let retry = store
        .append_observation(&input, 99_999)
        .expect("idempotent retry");
    assert!(first.inserted);
    assert!(!retry.inserted);
    assert_eq!(retry.observation, first.observation);
    assert_eq!(first.observation.received_at_ms, 10_010);
    let canonical = serde_json::to_vec(&input.payload).expect("JSON serializes");
    assert_eq!(
        first.observation.payload_sha256,
        Sha256Digest::of_bytes(&canonical)
    );

    let connection = store.open().expect("open database");
    assert!(
        connection
            .execute(
                "UPDATE raw_observations SET payload_json = '{}' WHERE observation_id = ?1",
                params![first.observation.observation_id],
            )
            .is_err()
    );
    assert!(
        connection
            .execute(
                "DELETE FROM raw_observations WHERE observation_id = ?1",
                params![first.observation.observation_id],
            )
            .is_err()
    );

    let observations = store
        .observations_for_vehicle(vehicle.vehicle_id, ObservationQuery::from_start(10))
        .expect("read observations");
    assert_eq!(observations, vec![first.observation]);
}

#[test]
fn observations_are_time_ordered_and_query_is_bounded() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store initializes");
    let (source, vehicle) = test_registered_vehicle(&store);
    for (observed_at_ms, value) in [(3_000, 3), (1_000, 1), (2_000, 2)] {
        store
            .append_observation(
                &ObservationInput {
                    source_id: source.source_id,
                    vehicle_id: vehicle.vehicle_id,
                    observed_at_ms,
                    payload: serde_json::json!({"value": value}),
                },
                observed_at_ms + 1,
            )
            .expect("append observation");
    }
    let first_two = store
        .observations_for_vehicle(vehicle.vehicle_id, ObservationQuery::from_start(2))
        .expect("bounded page");
    assert_eq!(
        first_two
            .iter()
            .map(|row| row.observed_at_ms)
            .collect::<Vec<_>>(),
        vec![1_000, 2_000]
    );
    let filtered = store
        .observations_for_vehicle(
            vehicle.vehicle_id,
            ObservationQuery {
                from_observed_at_ms: Some(2_000),
                until_observed_at_ms: Some(3_000),
                limit: 10,
            },
        )
        .expect("time query");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].observed_at_ms, 2_000);

    let error = store
        .observations_for_vehicle(
            vehicle.vehicle_id,
            ObservationQuery::from_start(MAX_OBSERVATION_QUERY_LIMIT + 1),
        )
        .expect_err("over-large query rejected");
    assert!(matches!(
        error,
        StoreError::InvalidObservationQueryLimit { .. }
    ));
}

#[test]
fn processed_raw_observations_are_pruned_but_current_snapshot_and_dedup_survive() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store initializes");
    let (source, vehicle) = test_registered_vehicle(&store);
    let input = ObservationInput {
        source_id: source.source_id,
        vehicle_id: vehicle.vehicle_id,
        observed_at_ms: 10_000,
        payload: serde_json::json!({
            "record_type": "owner_api_vehicle_data_v1",
            "source_vehicle_id": "9",
            "source_vehicle_state": "online",
            "vehicle_data": {
                "drive_state": {"shift_state": "P", "speed": 0},
                "charge_state": {"charging_state": "Disconnected"},
                "vehicle_state": {"timestamp": 10_000}
            }
        }),
    };
    let first = store
        .accept_owner_observation_and_lifecycle(&input, 10_001, 1)
        .expect("accept current observation");
    assert!(first.append.inserted);
    assert_eq!(
        store
            .open()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM raw_observations", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
    let current = store
        .current_observations_for_vehicle(vehicle.vehicle_id)
        .expect("current observations");
    assert_eq!(current.len(), 1);
    assert_eq!(
        current[0].observation_id,
        first.append.observation.observation_id
    );
    assert_eq!(
        store
            .latest_current_observation_metadata_for_vehicle(vehicle.vehicle_id)
            .expect("current metadata")
            .expect("current metadata exists"),
        LatestObservationMetadata {
            observation_id: first.append.observation.observation_id,
            observed_at_ms: 10_000,
            received_at_ms: 10_001,
        }
    );

    let retry = store
        .accept_owner_observation_and_lifecycle(&input, 99_999, 1)
        .expect("deduplicated retry");
    assert!(!retry.append.inserted);
    assert_eq!(retry.append.observation, current[0]);
    assert_eq!(
        store
            .open()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM raw_observations", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );

    let mut newer = input.clone();
    newer.observed_at_ms = 20_000;
    newer.payload["vehicle_data"]["vehicle_state"]["timestamp"] = serde_json::json!(20_000);
    let second = store
        .accept_owner_observation_and_lifecycle(&newer, 20_001, 1)
        .expect("accept newer observation after pruning");
    assert!(second.append.inserted);
    assert!(
        second.append.observation.observation_id > first.append.observation.observation_id,
        "pruning must never allow SQLite row identifiers to be reused"
    );
    assert_eq!(
        store
            .current_observations_for_vehicle(vehicle.vehicle_id)
            .expect("new current observation")[0],
        second.append.observation
    );
}

#[test]
fn invalid_open_session_rolls_back_observations_without_resetting_state() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store initializes");
    let (source, vehicle) = test_registered_vehicle(&store);
    let input = ObservationInput {
        source_id: source.source_id,
        vehicle_id: vehicle.vehicle_id,
        observed_at_ms: 10_000,
        payload: serde_json::json!({
            "record_type": "owner_api_vehicle_data_v1",
            "source_vehicle_id": "9",
            "source_vehicle_state": "online",
            "vehicle_data": {
                "drive_state": {"shift_state": "P", "speed": 0},
                "charge_state": {"charging_state": "Disconnected"},
                "vehicle_state": {"timestamp": 10_000}
            }
        }),
    };
    let accepted = store
        .accept_owner_observation_and_lifecycle(&input, 10_001, 1)
        .expect("initial observation");
    let corrupt = b"not-json".to_vec();
    store
        .open()
        .expect("open")
        .execute(
            "UPDATE vehicle_lifecycle_state SET open_session_json = ?2
                 WHERE vehicle_id = ?1",
            params![vehicle.vehicle_id.to_string(), corrupt],
        )
        .expect("simulate corrupt durable lifecycle state");
    let preserved = store
        .load_lifecycle_state(vehicle.vehicle_id)
        .expect("load corrupt state")
        .expect("lifecycle state");

    let mut newer = input;
    newer.observed_at_ms = 20_000;
    newer.payload["vehicle_data"]["vehicle_state"]["timestamp"] = serde_json::json!(20_000);
    assert!(matches!(
        store.accept_owner_observation_and_lifecycle(&newer, 20_001, 1),
        Err(StoreError::InvalidLifecycleSession)
    ));
    assert!(matches!(
        store.accept_stream_observation_and_lifecycle(&newer, 20_001, 1),
        Err(StoreError::InvalidLifecycleSession)
    ));
    assert_eq!(
        store
            .load_lifecycle_state(vehicle.vehicle_id)
            .expect("load preserved state")
            .expect("preserved lifecycle state"),
        preserved
    );
    assert_eq!(
        store
            .current_observations_for_vehicle(vehicle.vehicle_id)
            .expect("preserved current observation"),
        vec![accepted.append.observation]
    );
    let raw_count: i64 = store
        .open()
        .expect("open")
        .query_row("SELECT COUNT(*) FROM raw_observations", [], |row| {
            row.get(0)
        })
        .expect("raw observation count");
    assert_eq!(raw_count, 0);
}

#[test]
fn rejects_wrong_source_non_object_and_oversized_observations() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store initializes");
    let (source, vehicle) = test_registered_vehicle(&store);
    let other_source = store
        .register_source(
            &SourceDescriptor::new("teslamate_import", "migration-a"),
            1_001,
        )
        .expect("second source");
    let mismatch = store
        .append_observation(
            &ObservationInput {
                source_id: other_source.source_id,
                vehicle_id: vehicle.vehicle_id,
                observed_at_ms: 2_000,
                payload: serde_json::json!({"status": "online"}),
            },
            2_001,
        )
        .expect_err("vehicle cannot be written by another source");
    assert!(matches!(mismatch, StoreError::VehicleSourceMismatch { .. }));

    let non_object = store
        .append_observation(
            &ObservationInput {
                source_id: source.source_id,
                vehicle_id: vehicle.vehicle_id,
                observed_at_ms: 2_000,
                payload: serde_json::json!(["a response batch is not one observation"]),
            },
            2_001,
        )
        .expect_err("array rejected");
    assert!(matches!(non_object, StoreError::ObservationMustBeObject));

    let oversized = store
        .append_observation(
            &ObservationInput {
                source_id: source.source_id,
                vehicle_id: vehicle.vehicle_id,
                observed_at_ms: 2_000,
                payload: serde_json::json!({"blob": "x".repeat(MAX_RAW_OBSERVATION_BYTES)}),
            },
            2_001,
        )
        .expect_err("oversized response rejected before database mutation");
    assert!(matches!(oversized, StoreError::ObservationTooLarge { .. }));
    assert!(
        store
            .observations_for_vehicle(vehicle.vehicle_id, ObservationQuery::from_start(10))
            .expect("read observation history")
            .is_empty()
    );
}

fn import_delta_test_car(car_id: i64) -> ProjectionCar {
    ProjectionCar {
        id: car_id,
        name: "Import delta fixture".into(),
        model: "Model 3".into(),
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
    }
}

fn import_delta_test_cursor_key() -> CursorKey {
    CursorKey::from_bytes([61; 32])
}

fn import_delta_test_cursor(binding: &ProjectionBinding, sequence: u64) -> OpaqueCursor {
    OpaqueCursor::issue(
        &import_delta_test_cursor_key(),
        CursorClaims {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence,
        },
    )
    .expect("fixture cursor")
}

fn v2_base_manifest(store: &HubStore) -> (VehicleRecord, ProjectionBinding, SyncManifest) {
    let source = store
        .register_source(
            &SourceDescriptor::new("teslamate_import", "delta-fixture"),
            1_000,
        )
        .expect("fixture source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "10").with_tesla_identity(Some(70), None),
            1_001,
        )
        .expect("fixture vehicle");
    let binding = store
        .v2_projection_binding(vehicle.vehicle_id)
        .expect("fixture binding");
    let snapshot = ProjectionSnapshot {
        cars: vec![import_delta_test_car(binding.selected_car_id)],
        drives: Vec::new(),
        positions: Vec::new(),
        charges: Vec::new(),
        charge_samples: Vec::new(),
    };
    let base_sequence = 1;
    let base_snapshot_id = Uuid::new_v4();
    let request = ProjectionPackRequest {
        pack_id: Uuid::new_v4(),
        snapshot_id: base_snapshot_id,
        ordinal: 0,
        binding: binding.clone(),
        sequence: SequenceRange {
            from_exclusive: base_sequence,
            to_inclusive: base_sequence,
        },
        snapshot: &snapshot,
    };
    let pack = ProjectionPackWriter::new(store.packs_dir())
        .write_full_snapshot_with_states_and_updates(&request, &[], &[])
        .expect("fixture base pack");
    let manifest = request
        .signed_manifest_with_states_and_updates(&pack, &[], &[], &import_delta_test_cursor_key())
        .expect("fixture base manifest");
    (vehicle, binding, manifest)
}

fn imported_v2_base(store: &HubStore) -> (VehicleRecord, ProjectionBinding, LineageManifestV2) {
    let (vehicle, binding, manifest) = v2_base_manifest(store);
    store
        .finalize_import_snapshot_with_binding(
            &manifest,
            Sha256Digest::of_bytes(b"import-delta-fixture-base"),
            &[],
            &binding,
        )
        .expect("fixture base catalogue");
    let lineage = store
        .lineage_manifest_for_vehicle(vehicle.vehicle_id)
        .expect("fixture lineage lookup")
        .expect("fixture lineage catalogue");
    (vehicle, binding, lineage)
}

#[test]
fn imported_selection_returns_one_durable_eid_and_settings() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, _) = imported_v2_base(&store);
    let settings = ProjectionCarSettings {
        use_streaming_api: false,
        suspend_min: 9,
        ..ProjectionCarSettings::default()
    };
    store
        .upsert_car_settings(vehicle.vehicle_id, binding.selected_car_id, &settings)
        .expect("settings");

    assert_eq!(
        store.selected_tesla_eid().expect("selection"),
        Some((70, settings))
    );
}

#[test]
fn imported_selection_uses_materialised_settings_before_defaults() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, _) = imported_v2_base(&store);
    let settings = ProjectionCarSettings {
        use_streaming_api: false,
        suspend_after_idle_min: 19,
        ..ProjectionCarSettings::default()
    };
    let car = ProjectionCar {
        id: binding.selected_car_id,
        settings: settings.clone(),
        ..import_delta_test_car(binding.selected_car_id)
    };
    store
        .persist_materialised_car_if_absent(vehicle.vehicle_id, &car)
        .expect("imported car");

    assert_eq!(
        store.selected_tesla_eid().expect("selection"),
        Some((70, settings))
    );
}

fn test_projection_state(root: &Path, car: &ProjectionCar) -> TeslaMateProjectionState {
    let state = TeslaMateProjectionState::create(
        root,
        crate::teslamate_projection_state::TeslaMateProjectionStateLimits {
            max_rows: 10,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("projection state");
    let mut capture =
        crate::teslamate_projection_state::TeslaMateProjectionStateCapture::for_initial_base(state);
    capture.record_car(car).expect("capture car");
    capture.seal().expect("seal projection state");
    capture.into_state()
}

fn projection_state_with_digest_rows(
    root: &Path,
    selected_car_id: i64,
    rows: &[(TeslaMateProjectionStateEntity, i64)],
) -> TeslaMateProjectionState {
    let maximum_rows = u64::try_from(rows.len())
        .expect("test row count fits u64")
        .checked_add(1)
        .expect("test row count has room for car");
    let mut state = TeslaMateProjectionState::create(
        root,
        crate::teslamate_projection_state::TeslaMateProjectionStateLimits {
            max_rows: maximum_rows,
            max_state_bytes: 4 * 1024 * 1024,
            max_changed_payload_bytes: 4 * 1024 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("projection state");
    state
        .record(
            TeslaMateProjectionStateEntity::Car,
            selected_car_id,
            selected_car_id,
            &serde_json::json!({"id": selected_car_id, "entity": "car"}),
        )
        .expect("capture car");
    for (entity, id) in rows {
        state
            .record(
                *entity,
                *id,
                selected_car_id,
                &serde_json::json!({"id": id, "entity": entity.as_str()}),
            )
            .expect("capture digest row");
    }
    state.seal().expect("seal projection state");
    state
}

/// Direct-import finalizers deliberately reject the generic test spool.
/// Keep that generic helper above for the non-generation seams, and make
/// generation tests opt in to the same gated constructor as production.
fn create_direct_import_projection_state(
    store: &HubStore,
    run_id: Uuid,
    maximum_rows: u64,
) -> TeslaMateProjectionState {
    let publication_gate = store
        .try_acquire_publication_gate()
        .expect("direct projection-state publication gate");
    let state = store
        .create_import_projection_state(
            &publication_gate,
            run_id,
            crate::teslamate_projection_state::TeslaMateProjectionStateLimits {
                max_rows: maximum_rows,
                max_state_bytes: 4 * 1024 * 1024,
                max_changed_payload_bytes: 4 * 1024 * 1024,
                minimum_free_bytes: 0,
            },
            crate::teslamate_projection_state::DEFAULT_MAX_CHANGED_ROW_PAYLOAD_BYTES,
        )
        .expect("run-bound direct projection state");
    drop(publication_gate);
    state
}

fn direct_projection_state_with_digest_rows(
    store: &HubStore,
    run_id: Uuid,
    selected_car_id: i64,
    rows: &[(TeslaMateProjectionStateEntity, i64)],
) -> TeslaMateProjectionState {
    let maximum_rows = u64::try_from(rows.len())
        .expect("test row count fits u64")
        .checked_add(1)
        .expect("test row count has room for car");
    let mut state = create_direct_import_projection_state(store, run_id, maximum_rows);
    state
        .record(
            TeslaMateProjectionStateEntity::Car,
            selected_car_id,
            selected_car_id,
            &serde_json::json!({"id": selected_car_id, "entity": "car"}),
        )
        .expect("capture direct car");
    for (entity, id) in rows {
        state
            .record(
                *entity,
                *id,
                selected_car_id,
                &serde_json::json!({"id": id, "entity": entity.as_str()}),
            )
            .expect("capture direct digest row");
    }
    state.seal().expect("seal direct projection state");
    state
}

fn direct_test_projection_state(
    store: &HubStore,
    run_id: Uuid,
    car: &ProjectionCar,
) -> TeslaMateProjectionState {
    let state = create_direct_import_projection_state(store, run_id, 10);
    let mut capture =
        crate::teslamate_projection_state::TeslaMateProjectionStateCapture::for_initial_base(state);
    capture.record_car(car).expect("capture direct car");
    capture.seal().expect("seal direct projection state");
    capture.into_state()
}

fn begin_projection_state_recovery_generation(store: &HubStore) -> (Uuid, ProjectionBinding) {
    let (vehicle, binding, _) = v2_base_manifest(store);
    let run_id = store
        .begin_import_generation(
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            2_000,
        )
        .expect("staging projection-state generation");
    (run_id, binding)
}

#[cfg(unix)]
fn set_test_private_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .expect("set private test path mode");
}

#[cfg(unix)]
fn write_owned_test_v1_run(store: &HubStore, run_id: Uuid) -> (PathBuf, PathBuf) {
    if !store.private_import_spool_dir.exists() {
        fs::create_dir(&store.private_import_spool_dir).expect("create private import spool");
    }
    set_test_private_mode(&store.private_import_spool_dir, 0o700);
    let staging = store.private_import_spool_dir.join(".projection-state");
    let namespace = staging.join("v1");
    let run_directory = namespace.join(run_id.to_string());
    for directory in [&staging, &namespace, &run_directory] {
        if !directory.exists() {
            fs::create_dir(directory).expect("create owned v1 test directory");
        }
        set_test_private_mode(directory, 0o700);
    }
    let owner_marker = serde_json::json!({
        "schema": 1,
        "kind": "teslatlas-hub/teslamate-projection-state/v1",
        "runId": run_id.to_string(),
    });
    let owner_path = run_directory.join("owner.json");
    fs::write(
        &owner_path,
        serde_json::to_vec(&owner_marker).expect("encode owned v1 marker"),
    )
    .expect("write owned v1 marker");
    set_test_private_mode(&owner_path, 0o600);
    let spool_path = run_directory.join(format!("{}.sqlite", Uuid::new_v4()));
    fs::write(&spool_path, b"deliberately-not-a-sqlite-database")
        .expect("write owned v1 spool bytes");
    set_test_private_mode(&spool_path, 0o600);
    (run_directory, spool_path)
}

fn staging_generation_exists(store: &HubStore, run_id: Uuid) -> bool {
    store
            .open()
            .expect("open staging catalogue")
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM import_generations WHERE run_id = ?1 AND status = 'staging')",
                params![run_id.to_string()],
                |row| row.get(0),
            )
            .expect("read staging generation")
}

#[test]
fn recovery_reclaims_a_valid_owned_v1_spool_and_its_staging_generation() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (run_id, binding) = begin_projection_state_recovery_generation(&store);
    let publication_gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    let mut state = store
        .create_import_projection_state(
            &publication_gate,
            run_id,
            crate::teslamate_projection_state::TeslaMateProjectionStateLimits {
                max_rows: 10,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
            crate::teslamate_projection_state::DEFAULT_MAX_CHANGED_ROW_PAYLOAD_BYTES,
        )
        .expect("run-bound state");
    state
        .record_car(&import_delta_test_car(binding.selected_car_id))
        .expect("capture car");
    state.seal().expect("seal state");
    let spool_path = state.path_for_test().to_path_buf();
    let run_directory = spool_path
        .parent()
        .expect("spool run directory")
        .to_path_buf();
    state
        .abandon_for_recovery_test()
        .expect("simulate interrupted import");
    // Recovery owns file-system cleanup, not SQLite content validation.
    // A crash can leave a partially-written database, but the owned v1
    // marker still makes this exact staging run reclaimable.
    fs::write(
        &spool_path,
        b"not a SQLite database after interrupted write",
    )
    .expect("corrupt stale spool bytes");
    #[cfg(unix)]
    set_test_private_mode(&spool_path, 0o600);

    let connection = store.open().expect("catalogue connection");
    store
        .recover_stale_import_projection_state_spools(&publication_gate, &connection)
        .expect("recover owned stale run");
    assert!(
        !spool_path.exists() && !run_directory.exists(),
        "recovery removes exactly the proven owned v1 run"
    );
    assert!(
        !staging_generation_exists(&store, run_id),
        "recovery removes the matching staging row before broad abandoned-generation cleanup"
    );
}

#[test]
fn startup_recovery_refuses_to_scan_while_a_live_publication_gate_is_held() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (run_id, binding) = begin_projection_state_recovery_generation(&store);
    let publication_gate = store
        .try_acquire_publication_gate()
        .expect("live publication gate");
    let state = store
        .create_import_projection_state(
            &publication_gate,
            run_id,
            crate::teslamate_projection_state::TeslaMateProjectionStateLimits {
                max_rows: 10,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
            crate::teslamate_projection_state::DEFAULT_MAX_CHANGED_ROW_PAYLOAD_BYTES,
        )
        .expect("live run-bound state");
    let spool_path = state.path_for_test().to_path_buf();

    assert!(matches!(
        HubStore::initialize(temporary.path()),
        Err(StoreError::PublicationGateBusy)
    ));
    assert!(
        spool_path.exists(),
        "busy startup must not scan live spools"
    );
    assert!(
        staging_generation_exists(&store, run_id),
        "busy startup must not clear the live staging row"
    );
    drop(state);
    drop(publication_gate);
    let _ = binding;
}

#[test]
fn startup_recovery_preserves_a_flat_legacy_projection_state_spool() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let state = TeslaMateProjectionState::create(
        store.packs_dir(),
        crate::teslamate_projection_state::TeslaMateProjectionStateLimits {
            max_rows: 10,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("flat legacy spool");
    let legacy_spool_path = state.path_for_test().to_path_buf();
    state
        .abandon_for_recovery_test()
        .expect("leave legacy spool in place");
    drop(store);

    let _reopened = HubStore::initialize(temporary.path()).expect("restart succeeds");
    assert!(
        legacy_spool_path.exists(),
        "v1 recovery never guesses ownership of a flat legacy spool"
    );
}

#[cfg(unix)]
#[test]
fn startup_recovery_fails_closed_for_unsafe_v1_runs_without_deleting_any_sibling() {
    for unsafe_shape in ["owner", "unexpected", "mode", "symlink"] {
        let temporary = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (bad_run_id, _) = begin_projection_state_recovery_generation(&store);
        let valid_run_id = Uuid::new_v4();
        let (valid_directory, valid_spool) = write_owned_test_v1_run(&store, valid_run_id);
        let (bad_directory, bad_spool) = write_owned_test_v1_run(&store, bad_run_id);
        let sentinel = temporary.path().join(format!("{unsafe_shape}-sentinel"));

        match unsafe_shape {
            "owner" => {
                let owner = bad_directory.join("owner.json");
                fs::write(&owner, b"{\"schema\":999}").expect("malform owner marker");
                set_test_private_mode(&owner, 0o600);
            }
            "unexpected" => {
                let child = bad_directory.join("unrelated.txt");
                fs::write(&child, b"must not be reclaimed").expect("write unexpected child");
                set_test_private_mode(&child, 0o600);
            }
            "mode" => set_test_private_mode(&bad_directory, 0o755),
            "symlink" => {
                fs::write(&sentinel, b"outside the spool namespace")
                    .expect("write external sentinel");
                fs::remove_file(&bad_spool).expect("remove ordinary spool for symlink test");
                std::os::unix::fs::symlink(&sentinel, &bad_spool)
                    .expect("place symlink in owned-looking run");
            }
            _ => unreachable!("enumerated unsafe shape"),
        }

        assert!(matches!(
            HubStore::initialize(temporary.path()),
            Err(StoreError::TeslaMateProjectionState(_))
        ));
        assert!(
            valid_directory.exists() && valid_spool.exists(),
            "{unsafe_shape}: preflight failure must preserve a valid sibling"
        );
        assert!(
            bad_directory.exists(),
            "{unsafe_shape}: unsafe run itself must be left intact for inspection"
        );
        assert!(
            staging_generation_exists(&store, bad_run_id),
            "{unsafe_shape}: broad staging cleanup must not run after recovery rejects"
        );
        if unsafe_shape == "symlink" {
            assert_eq!(
                fs::read(&sentinel).expect("external sentinel survives"),
                b"outside the spool namespace"
            );
        }
    }
}

#[test]
fn run_bound_transfer_rejects_wrong_run_marker_mutation_and_attempt_substitution() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (run_id, binding) = begin_projection_state_recovery_generation(&store);
    let state = direct_projection_state_with_digest_rows(
        &store,
        run_id,
        binding.selected_car_id,
        &[(TeslaMateProjectionStateEntity::Position, 10)],
    );
    assert!(matches!(
        state.sealed_transfer_for_import_generation(Uuid::new_v4(), binding.selected_car_id),
        Err(TeslaMateProjectionStateError::ImportGenerationRunMismatch { .. })
    ));

    let transfer = state
        .sealed_transfer_for_import_generation(run_id, binding.selected_car_id)
        .expect("run-bound transfer descriptor");
    let replacement = direct_projection_state_with_digest_rows(
        &store,
        run_id,
        binding.selected_car_id,
        &[(TeslaMateProjectionStateEntity::Position, 99)],
    );
    assert_ne!(
        state.path_for_test(),
        replacement.path_for_test(),
        "a retry always owns a different attempt path"
    );
    fs::rename(replacement.path_for_test(), transfer.path())
        .expect("substitute another attempt at descriptor path");
    let connection = store.open().expect("catalogue connection");
    assert!(matches!(
        attach_teslamate_projection_state_transfer(&connection, &transfer),
        Err(StoreError::TeslaMateProjectionState(
            TeslaMateProjectionStateError::TransferDigestMismatch
        ))
    ));
    drop(connection);

    let marker = state
        .path_for_test()
        .parent()
        .expect("run directory")
        .join("owner.json");
    fs::write(&marker, b"{\"schema\":1}").expect("alter owner marker");
    #[cfg(unix)]
    set_test_private_mode(&marker, 0o600);
    assert!(matches!(
        state.sealed_transfer_for_import_generation(run_id, binding.selected_car_id),
        Err(TeslaMateProjectionStateError::InvalidOwnerMarker(_))
    ));
}

#[test]
fn retry_drop_removes_only_its_exact_attempt_and_keeps_a_live_sibling() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (run_id, binding) = begin_projection_state_recovery_generation(&store);
    let first = direct_projection_state_with_digest_rows(
        &store,
        run_id,
        binding.selected_car_id,
        &[(TeslaMateProjectionStateEntity::Position, 10)],
    );
    let first_path = first.path_for_test().to_path_buf();
    let run_directory = first_path.parent().expect("run directory").to_path_buf();
    let second = direct_projection_state_with_digest_rows(
        &store,
        run_id,
        binding.selected_car_id,
        &[(TeslaMateProjectionStateEntity::Position, 11)],
    );
    let second_path = second.path_for_test().to_path_buf();
    drop(first);
    assert!(
        !first_path.exists() && second_path.exists() && run_directory.exists(),
        "dropping a failed retry attempt cannot remove its replacement"
    );
    drop(second);
    assert!(
        !run_directory.exists(),
        "the final normal drop removes its now-empty owned run"
    );
}

#[test]
fn startup_recovery_reclaims_a_completed_run_orphaned_before_state_drop() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
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
            &TeslaMateOpenSession {
                car_id: binding.selected_car_id,
                ..Default::default()
            },
        )
        .expect("stage direct-import session");
    let state = direct_test_projection_state(
        &store,
        run_id,
        &import_delta_test_car(binding.selected_car_id),
    );
    let spool_path = state.path_for_test().to_path_buf();
    let run_directory = spool_path.parent().expect("run directory").to_path_buf();
    store
        .finalize_import_generation_with_projection_state(
            run_id,
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            2_000,
            &manifest,
            Sha256Digest::of_bytes(b"completed-run-orphan"),
            &[],
            &binding,
            &state,
            false,
        )
        .expect("complete direct base finalization");
    state
        .abandon_for_recovery_test()
        .expect("simulate termination after commit before state drop");
    drop(store);

    let reopened = HubStore::initialize(temporary.path()).expect("restart reclaims orphan");
    assert!(
        !spool_path.exists() && !run_directory.exists(),
        "a committed generation leaves no owned v1 spool after restart"
    );
    assert!(
        reopened
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("published lineage survives orphan cleanup")
            .is_some(),
        "startup reclamation never removes the completed catalogue result"
    );
}

fn persist_projection_state_rows(
    store: &HubStore,
    root: &Path,
    rows: &[(TeslaMateProjectionStateEntity, i64)],
) -> (VehicleRecord, ProjectionBinding) {
    let (vehicle, binding, manifest) = v2_base_manifest(store);
    let inventory = TeslaMateImportProjectionInventory {
        source_id: binding.account_id,
        selected_car_id: binding.selected_car_id,
        rows: Vec::new(),
    };
    let state = projection_state_with_digest_rows(root, binding.selected_car_id, rows);
    store
        .finalize_teslamate_import_snapshot_with_projection_state(
            &manifest,
            Sha256Digest::of_bytes(b"digest-cache-fixture"),
            &[],
            &binding,
            &inventory,
            &state,
        )
        .expect("persist fixture projection state");
    (vehicle, binding)
}

fn unchanged_direct_successor_projection_state(
    store: &HubStore,
    run_id: Uuid,
    vehicle_id: Uuid,
    binding: &ProjectionBinding,
    car: &ProjectionCar,
) -> TeslaMateProjectionState {
    let prior = store
        .teslamate_import_projection_state_lookup(
            vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("verified direct prior state");
    let state = create_direct_import_projection_state(store, run_id, 10);
    let mut capture =
        crate::teslamate_projection_state::TeslaMateProjectionStateCapture::for_successor(
            state,
            Box::new(prior),
        );
    capture
        .record_car(car)
        .expect("capture unchanged direct car");
    capture.seal().expect("seal direct successor state");
    capture.into_state()
}

#[test]
fn projection_state_lookup_allows_live_lineage_after_the_last_import() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding) = persist_projection_state_rows(
        &store,
        temporary.path(),
        &[(TeslaMateProjectionStateEntity::Position, 10)],
    );
    let imported_head = store
        .teslamate_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("import projection state")
        .header()
        .head_sequence;
    let (claim, delta) = claimed_collector_delta(&store, vehicle.vehicle_id, &binding);
    store
        .commit_v2_delta_claim(
            &claim,
            &delta,
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, delta.to_sequence),
        )
        .expect("publish live Hub delta");

    let lookup = store
        .teslamate_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("a later live head must not invalidate the prior import state");
    assert_eq!(lookup.header().head_sequence, imported_head);
    assert!(delta.to_sequence > imported_head);
}

#[test]
fn projection_state_digest_cache_reuses_one_verified_range() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding) = persist_projection_state_rows(
        &store,
        temporary.path(),
        &[
            (TeslaMateProjectionStateEntity::Position, 10),
            (TeslaMateProjectionStateEntity::Position, 20),
            (TeslaMateProjectionStateEntity::Position, 30),
        ],
    );
    let mut lookup = store
        .teslamate_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("verified lookup");

    assert!(
        lookup
            .digest_store(TeslaMateProjectionStateEntity::Position, 10)
            .expect("first cached digest")
            .is_some()
    );
    assert_eq!(lookup.digest_cache_loads, 1);
    assert!(
        lookup
            .digest_store(TeslaMateProjectionStateEntity::Position, 20)
            .expect("second cached digest")
            .is_some()
    );
    assert!(
        lookup
            .digest_store(TeslaMateProjectionStateEntity::Position, 20)
            .expect("repeated cached digest")
            .is_some()
    );
    assert!(
        lookup
            .digest_store(TeslaMateProjectionStateEntity::Position, 30)
            .expect("third cached digest")
            .is_some()
    );
    assert_eq!(
        lookup.digest_cache_loads, 1,
        "one entity/id range must satisfy later in-range lookups"
    );
}

#[test]
fn projection_state_digest_cache_preserves_gaps_and_exhausted_absence() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding) = persist_projection_state_rows(
        &store,
        temporary.path(),
        &[
            (TeslaMateProjectionStateEntity::Position, 10),
            (TeslaMateProjectionStateEntity::Position, 20),
        ],
    );
    let mut lookup = store
        .teslamate_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("verified lookup");

    assert!(
        lookup
            .digest_store(TeslaMateProjectionStateEntity::Position, 10)
            .expect("cached digest")
            .is_some()
    );
    assert_eq!(
        lookup
            .digest_store(TeslaMateProjectionStateEntity::Position, 15)
            .expect("gap lookup"),
        None,
        "a cached range may not treat a gap as unchanged"
    );
    assert_eq!(
        lookup
            .digest_store(TeslaMateProjectionStateEntity::Position, 21)
            .expect("exhausted tail lookup"),
        None,
        "an exhausted range must preserve a missing tail row"
    );
    assert_eq!(
        lookup.digest_cache_loads, 1,
        "gaps and an exhausted tail are exact cached absences"
    );
    assert_eq!(
        lookup
            .digest_store(TeslaMateProjectionStateEntity::Position, 9)
            .expect("backward missing lookup"),
        None,
    );
    assert_eq!(
        lookup.digest_cache_loads, 2,
        "an earlier ID is outside the cached lower bound and must reload"
    );
}

#[test]
fn projection_state_digest_cache_reloads_for_entity_changes_and_backtracking() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding) = persist_projection_state_rows(
        &store,
        temporary.path(),
        &[
            (TeslaMateProjectionStateEntity::Drive, 1),
            (TeslaMateProjectionStateEntity::Position, 10),
            (TeslaMateProjectionStateEntity::Position, 20),
        ],
    );
    let mut lookup = store
        .teslamate_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("verified lookup");

    assert!(
        lookup
            .digest_store(TeslaMateProjectionStateEntity::Position, 10)
            .expect("position digest")
            .is_some()
    );
    assert!(
        lookup
            .digest_store(TeslaMateProjectionStateEntity::Drive, 1)
            .expect("drive digest")
            .is_some()
    );
    assert!(
        lookup
            .digest_store(TeslaMateProjectionStateEntity::Position, 20)
            .expect("position digest after entity change")
            .is_some()
    );
    assert_eq!(
        lookup.digest_cache_loads, 2,
        "a different entity may not invalidate or reuse the position range"
    );
    assert!(
        lookup
            .digest_store(TeslaMateProjectionStateEntity::Position, 9)
            .expect("backtracked position digest")
            .is_none()
    );
    assert_eq!(
        lookup.digest_cache_loads, 3,
        "an earlier ID must replace only its entity range, preserving exact absence"
    );
}

#[test]
fn projection_state_digest_cache_is_bounded_and_leaves_tombstone_paging_exact() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let cache_limit = i64::try_from(TESLAMATE_IMPORT_PROJECTION_STATE_DIGEST_CACHE_ROWS)
        .expect("cache limit fits i64");
    let mut rows = Vec::with_capacity(TESLAMATE_IMPORT_PROJECTION_STATE_DIGEST_CACHE_ROWS + 2);
    rows.push((TeslaMateProjectionStateEntity::Drive, 1));
    rows.extend((1..=cache_limit + 1).map(|id| (TeslaMateProjectionStateEntity::Position, id)));
    let (vehicle, binding) = persist_projection_state_rows(&store, temporary.path(), &rows);
    let mut lookup = store
        .teslamate_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("verified lookup");

    assert!(
        lookup
            .digest_store(TeslaMateProjectionStateEntity::Position, 1)
            .expect("first bounded digest")
            .is_some()
    );
    let cache = lookup
        .digest_caches
        .iter()
        .find(|cache| cache.entity == TeslaMateProjectionStateEntity::Position)
        .expect("position cache loaded");
    assert_eq!(
        cache.rows.len(),
        TESLAMATE_IMPORT_PROJECTION_STATE_DIGEST_CACHE_ROWS
    );
    assert!(!cache.exhausted, "a full cache page cannot claim a tail");
    assert!(
        lookup
            .digest_store(TeslaMateProjectionStateEntity::Position, cache_limit + 1)
            .expect("next range digest")
            .is_some()
    );
    assert_eq!(lookup.digest_cache_loads, 2);
    assert!(
        lookup
            .digest_caches
            .iter()
            .all(|cache| cache.rows.len() <= TESLAMATE_IMPORT_PROJECTION_STATE_DIGEST_CACHE_ROWS)
    );
    assert!(lookup.digest_caches.len() <= TeslaMateProjectionStateEntity::ALL.len());

    let first = lookup
        .page_after_store(None, 2)
        .expect("first tombstone page");
    assert_eq!(first.rows.len(), 2);
    assert_eq!(first.rows[0].entity, TeslaMateProjectionStateEntity::Car);
    assert_eq!(first.rows[1].entity, TeslaMateProjectionStateEntity::Drive);
    let second = lookup
        .page_after_store(first.next_after, 2)
        .expect("second tombstone page");
    assert_eq!(second.rows.len(), 2);
    assert!(
        second
            .rows
            .iter()
            .all(|row| row.entity == TeslaMateProjectionStateEntity::Position)
    );
}

#[test]
fn projection_state_digest_cache_rejects_mismatched_durable_rows() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding) = persist_projection_state_rows(
        &store,
        temporary.path(),
        &[
            (TeslaMateProjectionStateEntity::Position, 77),
            (TeslaMateProjectionStateEntity::Position, 78),
        ],
    );
    let connection = store.open().expect("fixture catalogue");
    connection
        .execute(
            "UPDATE teslamate_import_projection_state_rows
                    SET car_id = ?1
                  WHERE vehicle_id = ?2 AND entity_ordinal = ?3 AND entity_id = ?4",
            params![
                binding.selected_car_id + 1,
                vehicle.vehicle_id.to_string(),
                i64::from(TeslaMateProjectionStateEntity::Position.ordinal()),
                77_i64,
            ],
        )
        .expect("inject wrong car row");
    connection
        .execute_batch("PRAGMA ignore_check_constraints = ON")
        .expect("allow malformed test row");
    connection
        .execute(
            "UPDATE teslamate_import_projection_state_rows
                    SET entity = 'drive'
                  WHERE vehicle_id = ?1 AND entity_ordinal = ?2 AND entity_id = ?3",
            params![
                vehicle.vehicle_id.to_string(),
                i64::from(TeslaMateProjectionStateEntity::Position.ordinal()),
                78_i64,
            ],
        )
        .expect("inject mismatched entity row");
    connection
        .execute_batch("PRAGMA ignore_check_constraints = OFF")
        .expect("restore constraints");
    drop(connection);

    let mut lookup = store
        .teslamate_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("open lookup over corrupted rows");
    for id in [77_i64, 78_i64] {
        assert!(matches!(
            lookup.digest_store(TeslaMateProjectionStateEntity::Position, id),
            Err(StoreError::LineageCatalogConflict)
        ));
    }
}
