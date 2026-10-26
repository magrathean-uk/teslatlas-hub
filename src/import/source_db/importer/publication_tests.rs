// SPDX-License-Identifier: AGPL-3.0-only

#[test]
fn migration_final_snapshot_after_initial_base_is_servable() {
    let data = crate::private_tempdir().expect("Hub data directory");
    let imports = crate::private_tempdir().expect("staging directory");
    let store = HubStore::initialize(data.path()).expect("Hub store");
    let request = SourceDbImportRequest {
        new_import_source: false,
        source_key: "migration-final-snapshot".into(),
        scope: SourceDbImportScope::Selected(1),
        imported_at_ms: 1_700_000_000_000,
    };
    let cursor_key = CursorKey::from_bytes([73; 32]);
    let mut stage = SourceDbStage::create(
        imports.path().join("imports"),
        SourceDbStageLimits {
            max_rows: 16,
            max_stage_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("create initial migration stage");
    let car = history().cars.remove(0);
    stage
        .insert(SourceDbStageTable::Cars, car.id, &car)
        .expect("stage initial selected car");
    stage.seal().expect("seal initial migration stage");
    let initial = publish_staged_history(&store, &cursor_key, &request, &stage)
        .expect("initial migration base");

    let mut final_stage = SourceDbStage::create(
        imports.path().join("imports"),
        SourceDbStageLimits {
            max_rows: 16,
            max_stage_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("create final migration stage");
    let mut final_history = history();
    final_history.drives.push(completed_drive(173));
    let final_car = final_history.cars.remove(0);
    final_stage
        .insert(SourceDbStageTable::Cars, final_car.id, &final_car)
        .expect("stage final selected car");
    for drive in final_history.drives {
        final_stage
            .insert(SourceDbStageTable::Drives, drive.id, &drive)
            .expect("stage final drive");
    }
    final_stage.seal().expect("seal final migration stage");
    let mut active_drive = completed_drive(900);
    active_drive.end_date_ms = None;
    let final_session = SourceDbOpenSession {
        car_id: 1,
        drive: Some(active_drive),
        ..SourceDbOpenSession::default()
    };
    let final_report = publish_staged_history_with_session(
        &store,
        &cursor_key,
        &request,
        &final_stage,
        &final_session,
    )
    .expect("changed final migration snapshot");
    assert_eq!(final_report.snapshot_id, initial.snapshot_id);
    assert!(final_report.sequence > initial.sequence);
    let persisted_session = store
        .load_imported_open_session(initial.source_id, initial.vehicle_id)
        .expect("load final imported session")
        .expect("final imported session");
    assert_eq!(
        persisted_session.drive.as_ref().map(|row| row.id),
        Some(900)
    );
    assert_eq!(
        persisted_session
            .drive
            .as_ref()
            .and_then(|row| row.end_date_ms),
        None
    );
    assert!(persisted_session.drive_positions.is_empty());
    assert!(persisted_session.charge_samples.is_empty());

    let lineage = store
        .lineage_manifest_for_vehicle(initial.vehicle_id)
        .expect("servable lineage lookup")
        .expect("servable lineage");
    lineage.validate().expect("Serve-compatible lineage");
    assert_eq!(lineage.deltas.len(), 1);
    let delta = &lineage.deltas[0].pack;
    let delta_path = store
        .pack_for_digest(delta.sha256)
        .expect("servable delta lookup")
        .expect("servable delta pack")
        .path;
    delta
        .verify_reader(
            File::open(delta_path).expect("open servable delta"),
            ProtocolLimits::default(),
        )
        .expect("Serve-compatible delta bytes");
}

#[test]
fn staged_v21_base_hands_off_to_a_direct_successor_capture() {
    let data = crate::private_tempdir().expect("Hub data directory");
    let imports = crate::private_tempdir().expect("staging directory");
    let store = HubStore::initialize(data.path()).expect("Hub store");
    let request = SourceDbImportRequest {
        new_import_source: false,
        source_key: "staged-direct-handoff".into(),
        scope: SourceDbImportScope::Selected(1),
        imported_at_ms: 1_700_000_000_000,
    };
    let cursor_key = CursorKey::from_bytes([72; 32]);
    let mut stage = SourceDbStage::create(
        imports.path().join("imports"),
        SourceDbStageLimits {
            max_rows: 16,
            max_stage_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("create sealed source stage");
    let car = history().cars.remove(0);
    stage
        .insert(SourceDbStageTable::Cars, car.id, &car)
        .expect("stage selected car");
    stage.seal().expect("seal staged source");
    let base = publish_staged_history(&store, &cursor_key, &request, &stage)
        .expect("publish staged schema-2.1 base");
    let binding = store
        .v2_projection_binding(base.vehicle_id)
        .expect("staged immutable binding");
    let publication_gate = store
        .try_acquire_publication_gate()
        .expect("acquire direct handoff gate");
    let run_id = store
        .begin_import_generation(base.source_id, base.vehicle_id, 1, 1_700_000_060_000)
        .expect("begin direct successor generation");
    let mut capture = direct_projection_state_capture(
        &store,
        &publication_gate,
        run_id,
        base.vehicle_id,
        base.source_id,
        1,
        true,
        direct_state_test_limits(),
    )
    .expect("staged base provides the direct successor's durable prior state");
    assert_eq!(
        capture.mode(),
        crate::import_projection_state::SourceDbProjectionStateCaptureMode::Successor
    );

    let mut changed = history();
    changed.drives.push(completed_drive(78));
    let projected = project_vehicle(&changed, 1).expect("project direct successor source");
    let selected_car = projected.snapshot.cars[0].clone();
    record_projected_direct_state(&mut capture, &projected, 1);
    let state = capture.seal().expect("seal direct successor capture");
    assert_eq!(state.changed_row_count, 1);
    let mut batches = Vec::new();
    direct_delta_rows_from_capture(&mut capture, &binding, &selected_car, 1, |batch| {
        batches.push(batch);
        Ok(())
    })
    .expect("direct successor can emit a sparse typed delta after staged handoff");
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].drives, projected.snapshot.drives);

    drop(capture);
    store
        .abort_import_generation(run_id)
        .expect("discard direct handoff test generation");
}

#[test]
fn staged_publication_adapts_before_the_protocol_chunk_ceiling() {
    let data = crate::private_tempdir().unwrap();
    let imports = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(data.path()).unwrap();
    let mut stage = SourceDbStage::create(
        imports.path().join("imports"),
        SourceDbStageLimits {
            max_rows: 2_000,
            max_stage_bytes: 4 * 1024 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .unwrap();
    let car = history().cars.remove(0);
    stage
        .insert(SourceDbStageTable::Cars, car.id, &car)
        .unwrap();
    let base_position = SourceDbPosition {
        id: 20,
        car_id: 1,
        drive_id: None,
        date_ms: 1_700_000_030_000,
        latitude: 51.5,
        longitude: -0.1,
        elevation: None,
        speed: Some(50),
        power: Some(10.0),
        odometer: None,
        ideal_battery_range_km: None,
        est_battery_range_km: None,
        rated_battery_range_km: Some(390.0),
        battery_level: Some(78),
        usable_battery_level: Some(77),
        fan_status: None,
        driver_temp_setting: None,
        passenger_temp_setting: None,
        is_climate_on: Some(false),
        is_rear_defroster_on: None,
        is_front_defroster_on: None,
        outside_temp: Some(18.0),
        inside_temp: Some(20.0),
        battery_heater: None,
        battery_heater_on: None,
        battery_heater_no_power: None,
        tpms_pressure_fl: None,
        tpms_pressure_fr: None,
        tpms_pressure_rl: None,
        tpms_pressure_rr: None,
    };
    for id in 20..=1060 {
        let mut position = base_position.clone();
        position.id = id;
        position.date_ms += id;
        stage
            .insert(SourceDbStageTable::Positions, id, &position)
            .unwrap();
    }
    stage.seal().unwrap();
    let open_session = SourceDbOpenSession {
        car_id: 1,
        ..SourceDbOpenSession::default()
    };

    let report = publish_staged_history_with_limits(
        &store,
        &CursorKey::from_bytes([12; 32]),
        &SourceDbImportRequest {
            new_import_source: false,
            source_key: "home-source".into(),
            scope: SourceDbImportScope::Selected(1),
            imported_at_ms: 1_700_000_000_000,
        },
        &stage,
        &open_session,
        SourceDbFragmentLimits {
            max_rows_per_fragment: 3,
            max_projected_json_bytes: 1024 * 1024,
        },
    )
    .unwrap();
    let manifest = store
        .manifest_for_vehicle(report.vehicle_id)
        .unwrap()
        .expect("published staged manifest");
    assert_eq!(report.projection.projected_positions, 1_041);
    assert!(manifest.chunk_count < 512);
    assert!(manifest.total_rows > 1_000);
}

#[tokio::test]
#[ignore = "needs PostgreSQL: set TESLATLAS_HUB_TEST_POSTGRES_URL (run with --ignored)"]
async fn native_complete_corpus_publishes_a_durable_manifest_when_configured() {
    let url = std::env::var("TESLATLAS_HUB_TEST_POSTGRES_URL")
        .expect("set TESLATLAS_HUB_TEST_POSTGRES_URL to run this ignored test");
    let source = ReadOnlySource::parse(&url).expect("credential-free source URL");
    let password = SourceDbPassword::from_bytes(b"fixture-password").expect("fixture password");
    let data = crate::private_tempdir().expect("Hub data directory");
    let store = HubStore::initialize(data.path()).expect("Hub store");
    let report = import_from_postgres(
        &store,
        &source,
        &password,
        &CursorKey::from_bytes([11; 32]),
        &SourceDbImportRequest {
            new_import_source: false,
            source_key: "native-corpus".into(),
            scope: SourceDbImportScope::Selected(1),
            imported_at_ms: 1_767_225_600_000,
        },
        SourceDbReadLimits {
            maximum_rows: 32,
            parallel_copy_lanes: 3,
            ..SourceDbReadLimits::default()
        },
    )
    .await
    .expect("full native publication");

    // car + drive + 3 positions + charging process + charge + state +
    // update + 2 addresses + geofence
    assert_eq!(report.projected_rows, 13);
    let manifest = store
        .manifest_for_vehicle(report.vehicle_id)
        .expect("manifest query")
        .expect("published manifest");
    assert_eq!(manifest.snapshot_id, report.snapshot_id);
    assert_eq!(manifest.total_rows, 13);
    assert!(!manifest.chunks.is_empty());
    assert_eq!(store.repair().expect("Hub repair check").status, "ok");

    let backup_parent = crate::private_tempdir().expect("backup parent");
    let backup_root = backup_parent.path().join("restored-hub");
    store.backup_to(&backup_root).expect("complete backup");
    let restored = HubStore::initialize(&backup_root).expect("restored Hub store");
    restored.quick_check().expect("restored integrity");
    let restored_manifest = restored
        .manifest_for_vehicle(report.vehicle_id)
        .expect("restored manifest query")
        .expect("restored manifest");
    assert_eq!(restored_manifest, manifest);
    for chunk in restored_manifest.chunks {
        assert!(
            restored
                .pack_for_digest(chunk.sha256)
                .expect("restored pack lookup")
                .expect("restored pack")
                .path
                .is_file()
        );
    }
}

#[path = "capacity_fallback_tests.rs"]
mod capacity_fallback;
