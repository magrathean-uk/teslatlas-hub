// SPDX-License-Identifier: AGPL-3.0-only

use std::{fs::File, path::Path};

use rusqlite::Connection;

use super::*;

#[test]
fn dense_stage_starts_at_the_first_bounded_retry_target() {
    let default = TeslaMateFragmentLimits::default();
    assert_eq!(
        initial_staged_fragment_limits(DENSE_STAGE_ROW_THRESHOLD - 1, default),
        default
    );
    assert_eq!(
        initial_staged_fragment_limits(DENSE_STAGE_ROW_THRESHOLD, default),
        next_fragment_limits(default).expect("default has a dense target")
    );
}

#[test]
fn position_projection_pool_returns_pages_in_source_order() {
    let mut pool = PositionProjectionPool::new(1, Arc::new(HashMap::new()));
    let mut pages = Vec::new();
    for ordinal in 0..4 {
        if let Some(page) = pool
            .submit(PositionProjectionJob {
                ordinal,
                rows: Vec::new(),
                car_id: 1,
            })
            .expect("submit empty position page")
        {
            pages.push(page);
        }
    }
    pages.extend(pool.finish().expect("finish position projection pool"));
    assert_eq!(
        pages.iter().map(|page| page.ordinal).collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
}
use crate::{
    hub_pack::{
        ProjectionBinding, ProjectionCar, ProjectionPackError, ProjectionPackOwnership,
        ProjectionPackRequest, ProjectionSnapshot, ProjectionUpdate, signed_full_snapshot_manifest,
    },
    protocol::CursorKey,
    teslamate_projection_state::{
        TeslaMateProjectionState, TeslaMateProjectionStateCapture, TeslaMateProjectionStateEntity,
        TeslaMateProjectionStateLimits,
    },
    teslamate_stage::TeslaMateStageLimits,
};

fn stage() -> (tempfile::TempDir, TeslaMateStage) {
    stage_with_sealed(true)
}

fn mutable_stage() -> (tempfile::TempDir, TeslaMateStage) {
    stage_with_sealed(false)
}

fn stage_with_sealed(seal: bool) -> (tempfile::TempDir, TeslaMateStage) {
    let temporary = crate::private_tempdir().unwrap();
    let mut stage = TeslaMateStage::create(
        temporary.path().join("imports"),
        TeslaMateStageLimits {
            max_rows: 2_000,
            max_stage_bytes: 4 * 1024 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .unwrap();
    let car = TeslaMateCar {
        id: 1,
        eid: 99,
        vid: Some(199),
        vin: Some("5YJTESTVIN1234567".into()),
        name: Some("Road car".into()),
        model: Some("Model 3".into()),
        trim_badging: None,
        marketing_name: None,
        exterior_color: None,
        wheel_type: None,
        spoiler_type: None,
        efficiency_wh_per_km: Some(0.145),
        settings: Default::default(),
    };
    let position = TeslaMatePosition {
        id: 20,
        car_id: 1,
        drive_id: Some(10),
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
    let drive = TeslaMateDrive {
        id: 10,
        car_id: 1,
        start_date_ms: 1_700_000_000_000,
        end_date_ms: Some(1_700_000_060_000),
        start_position_id: Some(20),
        end_position_id: Some(20),
        start_address_id: Some(100),
        end_address_id: Some(100),
        start_geofence_id: Some(200),
        end_geofence_id: Some(200),
        outside_temp_avg: Some(18.0),
        inside_temp_avg: Some(21.0),
        speed_max: Some(80),
        power_max: Some(36.0),
        power_min: Some(-7.0),
        start_ideal_range_km: Some(338.8),
        end_ideal_range_km: Some(334.8),
        start_rated_range_km: Some(400.0),
        end_rated_range_km: Some(390.0),
        start_km: None,
        end_km: None,
        distance_km: Some(12.0),
        duration_min: Some(10),
        ascent: Some(60),
        descent: Some(30),
    };
    let process = TeslaMateChargingProcess {
        id: 30,
        car_id: 1,
        position_id: Some(20),
        address_id: Some(100),
        geofence_id: Some(200),
        start_date_ms: 1_700_001_000_000,
        end_date_ms: Some(1_700_001_360_000),
        charge_energy_added: None,
        charge_energy_used_kwh: None,
        start_ideal_range_km: None,
        end_ideal_range_km: None,
        cost: None,
        start_battery_level: Some(50),
        end_battery_level: None,
        duration_min: Some(60),
        outside_temp_avg: Some(18.0),
        start_rated_range_km: None,
        end_rated_range_km: None,
    };
    let sample = TeslaMateCharge {
        id: 40,
        charging_process_id: 30,
        date_ms: 1_700_001_100_000,
        battery_heater: Some(false),
        battery_heater_on: Some(false),
        battery_heater_no_power: Some(false),
        battery_level: Some(80),
        usable_battery_level: Some(79),
        charge_energy_added_kwh: Some(20.0),
        charger_actual_current: Some(30.0),
        charger_phases: Some(1),
        charger_pilot_current: Some(32.0),
        charger_power_kw: Some(7.0),
        charger_voltage: Some(230.0),
        charge_cable: Some("Type 2".into()),
        fast_charger_present: Some(false),
        fast_charger_brand: None,
        fast_charger_type: None,
        ideal_range_km: Some(300.0),
        rated_range_km: Some(298.0),
        not_enough_power_to_heat: Some(false),
        outside_temp_c: Some(18.0),
    };
    stage
        .insert(TeslaMateStageTable::Cars, car.id, &car)
        .unwrap();
    stage
        .insert(TeslaMateStageTable::Positions, position.id, &position)
        .unwrap();
    stage
        .insert(TeslaMateStageTable::Drives, drive.id, &drive)
        .unwrap();
    stage
        .insert(TeslaMateStageTable::ChargingProcesses, process.id, &process)
        .unwrap();
    stage
        .insert(TeslaMateStageTable::Charges, sample.id, &sample)
        .unwrap();
    stage
        .insert(
            TeslaMateStageTable::Addresses,
            100,
            &TeslaMateAddress {
                id: 100,
                display_name: Some("Home, London".into()),
                name: Some("Home".into()),
            },
        )
        .unwrap();
    stage
        .insert(
            TeslaMateStageTable::Geofences,
            200,
            &TeslaMateGeofence {
                id: 200,
                name: "Home".into(),
                latitude: Some(51.0),
                longitude: Some(-0.1),
                radius_m: Some(100.0),
                billing_type: Some(crate::hub_pack::GeofenceBillingType::PerKwh),
                cost_per_unit: Some(0.30),
                session_fee: Some(2.0),
            },
        )
        .unwrap();
    stage
        .insert(
            TeslaMateStageTable::Updates,
            300,
            &TeslaMateUpdate {
                id: 300,
                car_id: 1,
                start_date_ms: 1_699_000_000_000,
                end_date_ms: Some(1_699_000_060_000),
                version: Some("2026.20.1".into()),
            },
        )
        .unwrap();
    if seal {
        stage.seal().unwrap();
    }
    (temporary, stage)
}

fn binding() -> ProjectionBinding {
    ProjectionBinding {
        installation_id: Uuid::from_u128(1),
        account_id: Uuid::from_u128(2),
        vehicle_id: Uuid::from_u128(3),
        generation: 1,
        selected_car_id: 1,
    }
}

fn update_rows_from_chunks(
    inspection_directory: &Path,
    chunks: &[BuiltProjectionPack],
) -> Vec<(i64, i64, i64, i64, String)> {
    let mut updates = Vec::new();
    for (index, chunk) in chunks.iter().enumerate() {
        let sqlite = zstd::stream::decode_all(File::open(&chunk.path).expect("open pack"))
            .expect("decode pack");
        let inspection_path = inspection_directory.join(format!("updates-{index}.sqlite"));
        std::fs::write(&inspection_path, sqlite).expect("write inspection database");
        let connection = Connection::open(inspection_path).expect("open inspection database");
        let mut statement = connection
            .prepare(
                "SELECT id, car_id, start_date_ms, end_date_ms, version
                 FROM updates ORDER BY id",
            )
            .expect("schema-2.1 updates table exists");
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .expect("query emitted updates")
            .collect::<Result<Vec<_>, _>>()
            .expect("read emitted updates");
        updates.extend(rows);
    }
    updates
}

fn capturable_snapshot(stage: &TeslaMateStage) -> (ProjectionSnapshot, Vec<ProjectionState>) {
    let car = stage
        .get::<TeslaMateCar>(TeslaMateStageTable::Cars, 1)
        .expect("car lookup")
        .expect("car");
    let position = stage
        .get::<TeslaMatePosition>(TeslaMateStageTable::Positions, 20)
        .expect("position lookup")
        .expect("position");
    let drive = stage
        .get::<TeslaMateDrive>(TeslaMateStageTable::Drives, 10)
        .expect("drive lookup")
        .expect("drive");
    let process = stage
        .get::<TeslaMateChargingProcess>(TeslaMateStageTable::ChargingProcesses, 30)
        .expect("process lookup")
        .expect("process");
    let sample = stage
        .get::<TeslaMateCharge>(TeslaMateStageTable::Charges, 40)
        .expect("sample lookup")
        .expect("sample");
    let address = stage
        .get::<TeslaMateAddress>(TeslaMateStageTable::Addresses, 100)
        .expect("address lookup")
        .expect("address");
    let geofence = stage
        .get::<TeslaMateGeofence>(TeslaMateStageTable::Geofences, 200)
        .expect("geofence lookup")
        .expect("geofence");
    let projected_car = project_car(&car, None).expect("project car");
    let projected_drive = project_drive(
        &drive,
        1,
        DriveRelations {
            start_position: Some(&position),
            end_position: Some(&position),
            start_address: Some(&address),
            end_address: Some(&address),
            start_geofence: Some(&geofence),
            end_geofence: Some(&geofence),
        },
    )
    .expect("project drive")
    .expect("completed drive");
    let projected_position = project_position(&position, 1, true)
        .expect("project position")
        .expect("included position");
    let projected_charge = project_charge(
        &process,
        1,
        Some(&position),
        Some(&address),
        Some(&geofence),
        &ChargeProjectionFacts::default(),
    )
    .expect("project charge");
    let projected_sample = project_charge_sample(&sample);
    (
        ProjectionSnapshot {
            cars: vec![projected_car],
            drives: vec![projected_drive],
            positions: vec![projected_position],
            charges: vec![projected_charge],
            charge_samples: vec![projected_sample],
        },
        vec![ProjectionState {
            id: 50,
            car_id: 1,
            state: "online".into(),
            start_date_ms: 1_700_002_000_000,
            end_date_ms: None,
        }],
    )
}

#[test]
fn dropped_candidate_sink_removes_unpublished_pack() {
    let temporary = crate::private_tempdir().unwrap();
    let writer = ProjectionPackWriter::new(temporary.path());
    let mut sink = PackSink::new(
        &writer,
        binding(),
        Uuid::from_u128(4),
        SequenceRange {
            from_exclusive: 0,
            to_inclusive: 1,
        },
        Vec::new(),
    );
    sink.write(ProjectionSnapshot {
        cars: vec![ProjectionCar {
            id: 1,
            name: "Corpus One".into(),
            model: "Model S".into(),
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
            settings: Default::default(),
        }],
        drives: Vec::new(),
        positions: Vec::new(),
        charges: Vec::new(),
        charge_samples: Vec::new(),
    })
    .unwrap();
    sink.finish().unwrap();
    assert_eq!(
        sink.chunks[0].ownership(),
        ProjectionPackOwnership::Created,
        "a fresh candidate sink must own its unpublished pack cleanup"
    );
    let candidate = sink.chunks[0].path.clone();
    assert!(candidate.is_file());
    drop(sink);
    assert!(!candidate.exists());
}

#[test]
fn reused_catalogued_pack_survives_sink_and_staged_candidate_cleanup() {
    let temporary = crate::private_tempdir().expect("temporary Hub store");
    let store = crate::db::HubStore::initialize(temporary.path()).expect("Hub store");
    let writer = ProjectionPackWriter::new(store.packs_dir());
    let (_stage_directory, stage) = stage();
    let (snapshot, _states) = capturable_snapshot(&stage);
    let request = ProjectionPackRequest {
        pack_id: Uuid::from_u128(0x77),
        snapshot_id: Uuid::from_u128(0x88),
        ordinal: 0,
        binding: binding(),
        sequence: SequenceRange {
            from_exclusive: 0,
            to_inclusive: 1,
        },
        snapshot: &snapshot,
    };

    let created = writer
        .write_full_snapshot(&request)
        .expect("first producer writes the deterministic pack");
    assert_eq!(created.ownership(), ProjectionPackOwnership::Created);
    let manifest = request
        .signed_manifest(&created, &CursorKey::from_bytes([45; 32]))
        .expect("sign deterministic first producer pack");
    store
        .publish_manifest(&manifest)
        .expect("catalogue first producer pack");
    let catalogued_path = created.path.clone();
    let digest = created.metadata.sha256;

    let clone = created.clone();
    assert_eq!(
        clone.ownership(),
        ProjectionPackOwnership::ReusedExisting,
        "cloning a created descriptor must not duplicate its deletion right"
    );

    let reused_for_sink = writer
        .write_full_snapshot(&request)
        .expect("second producer reuses existing immutable pack");
    assert_eq!(
        reused_for_sink.ownership(),
        ProjectionPackOwnership::ReusedExisting
    );
    let mut sink = PackSink::new(
        &writer,
        binding(),
        Uuid::from_u128(0x99),
        SequenceRange {
            from_exclusive: 0,
            to_inclusive: 1,
        },
        Vec::new(),
    );
    sink.chunks.push(reused_for_sink);
    drop(sink);
    assert!(
        catalogued_path.is_file(),
        "dropping a candidate that reused a catalogued file must not unlink it"
    );
    assert!(
        store
            .pack_sha256_is_retained(&digest.to_string())
            .expect("catalogue lookup after sink cleanup"),
        "the reused pack remains catalogued after PackSink cleanup"
    );

    let reused_for_staged = writer
        .write_full_snapshot(&request)
        .expect("another producer reuses existing immutable pack");
    assert_eq!(
        reused_for_staged.ownership(),
        ProjectionPackOwnership::ReusedExisting
    );
    let staged = StagedProjectionPacks::new(
        vec![reused_for_staged],
        ProjectionReport::default(),
        crate::protocol::Sha256Digest::of_bytes(b"reused-pack-cleanup"),
        Vec::new(),
    );
    drop(staged);
    assert!(
        catalogued_path.is_file(),
        "dropping staged reused chunks must not unlink a catalogued file"
    );
    store
        .catalogue_check()
        .expect("catalogue remains valid after reused-pack cleanup");
}

#[test]
fn legacy_bridge_capture_replays_the_physical_digest_without_writing_a_pack() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let (_stage_directory, stage) = stage();
    let (snapshot, states) = capturable_snapshot(&stage);
    let writer = ProjectionPackWriter::new(temporary.path());
    let mut historical = PackSink::new_with_schema_2_1(
        &writer,
        binding(),
        Uuid::from_u128(41),
        SequenceRange {
            from_exclusive: 0,
            to_inclusive: 1,
        },
        states.clone(),
        true,
    );
    historical
        .write(snapshot.clone())
        .expect("historical physical fragment");
    historical.finish().expect("historical pack build");
    let historical_fingerprint = historical
        .fingerprint()
        .expect("historical physical fingerprint");
    let historical_pack = historical.chunks[0].path.clone();
    drop(historical);
    assert!(
        !historical_pack.exists(),
        "the comparison fixture must not leave a candidate pack behind"
    );

    let state = TeslaMateProjectionState::create(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: 16,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("bridge state capture");
    let mut bridge = PackSink::new_with_schema_2_1(
        &writer,
        binding(),
        Uuid::from_u128(42),
        SequenceRange {
            from_exclusive: 0,
            to_inclusive: 1,
        },
        states,
        true,
    )
    .capture_only()
    .with_projection_state_capture(TeslaMateProjectionStateCapture::for_initial_base(state));
    bridge.write(snapshot).expect("packless bridge fragment");
    assert!(bridge.chunks.is_empty(), "bridge must not write a new pack");
    assert!(bridge.has_written_fragments());
    assert_eq!(
        bridge.fingerprint(),
        Some(historical_fingerprint),
        "bridge must replay the retired physical digest exactly"
    );
    let (_chunks, capture, _selected_car) = bridge.into_parts();
    let mut capture = capture.expect("bridge capture retained");
    capture.seal().expect("seal bridge state");
    assert_eq!(
        capture
            .page(None, 16)
            .expect("bridge state page")
            .rows
            .len(),
        6,
        "packless capture must still retain every projected fact"
    );
}

#[test]
fn successor_state_capture_records_updates_without_a_full_candidate_pack() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let (_stage_directory, stage) = stage();
    let (snapshot, states) = capturable_snapshot(&stage);
    let state = TeslaMateProjectionState::create(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: 16,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("successor state capture");
    let writer = ProjectionPackWriter::new(temporary.path());
    let mut successor = PackSink::new_with_schema_2_1(
        &writer,
        binding(),
        Uuid::from_u128(43),
        SequenceRange {
            from_exclusive: 1,
            to_inclusive: 2,
        },
        states,
        true,
    )
    .capture_state_only()
    .with_projection_state_capture(TeslaMateProjectionStateCapture::for_initial_base(state));
    successor
        .write_with_updates(
            snapshot,
            &[ProjectionUpdate {
                id: 300,
                car_id: 1,
                start_date_ms: 1_699_000_000_000,
                end_date_ms: 1_699_000_060_000,
                version: "2026.20.1".into(),
            }],
        )
        .expect("state-only successor accepts update history");
    assert!(
        successor.chunks.is_empty(),
        "successor comparison must not build a disposable full pack"
    );
    assert!(successor.has_written_fragments());
    let (chunks, capture, _selected_car) = successor.into_parts();
    assert!(chunks.is_empty());
    let mut capture = capture.expect("successor state retained");
    capture.seal().expect("seal successor state");
    assert_eq!(
        capture
            .page(None, 16)
            .expect("successor state page")
            .rows
            .len(),
        7,
        "successor state must include every projected fact and the update"
    );
}

#[test]
fn direct_pack_builds_complete_before_projection_state_writes_continue() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let (_stage_directory, stage) = stage();
    let (snapshot, states) = capturable_snapshot(&stage);
    let writer = ProjectionPackWriter::new(temporary.path());
    let mut direct = PackSink::new_with_schema_2_1(
        &writer,
        binding(),
        Uuid::from_u128(44),
        SequenceRange {
            from_exclusive: 0,
            to_inclusive: 1,
        },
        states,
        true,
    )
    .without_physical_fingerprint()
    .with_synchronous_pack_builds();
    direct.write(snapshot).expect("direct pack build");
    assert_eq!(direct.chunks.len(), 1);
    assert!(
        direct.build_queue.is_none(),
        "direct migration must not overlap a pack worker with state writes"
    );
}

#[test]
fn staged_fingerprint_binds_preprojection_source_evidence() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let writer = ProjectionPackWriter::new(temporary.path());
    let make_sink = |evidence: crate::protocol::Sha256Digest| {
        PackSink::new(
            &writer,
            binding(),
            Uuid::from_u128(0x5151),
            SequenceRange {
                from_exclusive: 0,
                to_inclusive: 1,
            },
            Vec::new(),
        )
        .with_source_evidence_fingerprint(evidence)
        .fingerprint()
        .expect("staged fingerprint")
    };

    let first = make_sink(crate::protocol::Sha256Digest::of_bytes(b"first evidence"));
    let repeated = make_sink(crate::protocol::Sha256Digest::of_bytes(b"first evidence"));
    let changed = make_sink(crate::protocol::Sha256Digest::of_bytes(b"changed evidence"));
    assert_eq!(first, repeated);
    assert_ne!(
        repeated, changed,
        "a source-only fact change must invalidate staged duplicate suppression"
    );
}

#[test]
fn legacy_bridge_refuses_update_rows_to_preserve_the_historical_layout() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let (_stage_directory, stage) = stage();
    let (snapshot, states) = capturable_snapshot(&stage);
    let writer = ProjectionPackWriter::new(temporary.path());
    let mut bridge = PackSink::new_with_schema_2_1(
        &writer,
        binding(),
        Uuid::from_u128(43),
        SequenceRange {
            from_exclusive: 0,
            to_inclusive: 1,
        },
        states,
        true,
    )
    .capture_only();

    let error = bridge
        .write_with_updates(
            snapshot,
            &[ProjectionUpdate {
                id: 300,
                car_id: 1,
                start_date_ms: 1_699_000_000_000,
                end_date_ms: 1_699_000_060_000,
                version: "2026.20.1".into(),
            }],
        )
        .expect_err("legacy bridge cannot add a new update table fact");
    assert!(matches!(
        error,
        TeslaMateFragmentError::LegacyBridgeUpdateHistory
    ));
    assert!(bridge.chunks.is_empty(), "bridge must not write a new pack");
    assert!(
        !bridge.has_written_fragments(),
        "a rejected update must not alter historical bridge accounting"
    );
}

#[test]
fn captures_each_projected_fact_once_after_verified_fragment_writes() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let (_stage_directory, stage) = stage();
    let (snapshot, states) = capturable_snapshot(&stage);
    let state = TeslaMateProjectionState::create(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: 16,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("state capture");
    let writer = ProjectionPackWriter::new(temporary.path());
    let mut sink = PackSink::new_with_schema_2_1(
        &writer,
        binding(),
        Uuid::from_u128(40),
        SequenceRange {
            from_exclusive: 0,
            to_inclusive: 1,
        },
        states,
        true,
    )
    .with_projection_state_capture(TeslaMateProjectionStateCapture::for_initial_base(state));
    sink.write(snapshot.clone()).expect("first fragment");
    sink.write(snapshot).expect("repeat parent fragment");
    let (_chunks, capture, _selected_car) = sink.into_parts();
    let mut capture = capture.expect("capture retained with candidate");
    capture.seal().expect("seal capture");
    let page = capture.page(None, 16).expect("captured state page");
    assert_eq!(page.rows.len(), 6, "repeated fragment parents deduplicate");
    assert_eq!(
        page.rows
            .iter()
            .find(|row| row.entity == TeslaMateProjectionStateEntity::ChargeSample)
            .expect("captured charge sample")
            .car_id,
        1,
        "charge samples take their source-car identity from their charge parent"
    );
    assert_eq!(
        page.rows.iter().map(|row| row.entity).collect::<Vec<_>>(),
        vec![
            TeslaMateProjectionStateEntity::Car,
            TeslaMateProjectionStateEntity::Drive,
            TeslaMateProjectionStateEntity::Position,
            TeslaMateProjectionStateEntity::Charge,
            TeslaMateProjectionStateEntity::ChargeSample,
            TeslaMateProjectionStateEntity::State,
        ]
    );
}

#[test]
fn state_capture_failure_removes_already_written_candidate_pack() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let (_stage_directory, stage) = stage();
    let (snapshot, states) = capturable_snapshot(&stage);
    let state = TeslaMateProjectionState::create(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: 1,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("state capture");
    let writer = ProjectionPackWriter::new(temporary.path());
    let mut sink = PackSink::new_with_schema_2_1(
        &writer,
        binding(),
        Uuid::from_u128(41),
        SequenceRange {
            from_exclusive: 0,
            to_inclusive: 1,
        },
        states,
        true,
    )
    .with_projection_state_capture(TeslaMateProjectionStateCapture::for_initial_base(state));
    sink.write(snapshot)
        .expect("pack build is deferred until queue finish");
    let error = sink
        .finish()
        .expect_err("state row ceiling rejects candidate after write");
    assert!(matches!(
        error,
        TeslaMateFragmentError::ProjectionState(TeslaMateProjectionStateError::RowLimitExceeded {
            maximum: 1
        })
    ));
    let candidate = sink.chunks[0].path.clone();
    assert!(
        candidate.is_file(),
        "pack existed before capture failure cleanup"
    );
    drop(sink);
    assert!(!candidate.exists(), "failed candidate pack is cleaned up");
}

#[test]
fn dropped_completed_candidate_retains_packs_for_gated_repair() {
    let temporary = crate::private_tempdir().unwrap();
    let (_stage_directory, stage) = stage();
    let candidate = write_staged_full_snapshot(
        &stage,
        &ProjectionPackWriter::new(temporary.path()),
        binding(),
        Uuid::from_u128(4),
        SequenceRange {
            from_exclusive: 0,
            to_inclusive: 1,
        },
    )
    .unwrap();
    let paths: Vec<_> = candidate
        .chunks
        .iter()
        .map(|chunk| chunk.path.clone())
        .collect();
    assert!(
        candidate
            .chunks
            .iter()
            .all(|chunk| chunk.ownership() == ProjectionPackOwnership::Created),
        "a fresh staged candidate must retain cleanup rights for every pack"
    );
    assert!(paths.iter().all(|path| path.is_file()));
    drop(candidate);
    assert!(paths.iter().all(|path| path.is_file()));
}

#[test]
fn adapts_a_snapshot_that_would_exceed_the_legacy_chunk_ceiling() {
    let temporary = crate::private_tempdir().unwrap();
    let (_stage_directory, mut stage) = mutable_stage();
    let base_position = TeslaMatePosition {
        id: 20,
        car_id: 1,
        drive_id: Some(10),
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
    for id in 21..=1060 {
        let mut position = base_position.clone();
        position.id = id;
        position.drive_id = None;
        position.date_ms += id;
        stage
            .insert(TeslaMateStageTable::Positions, id, &position)
            .unwrap();
    }
    stage.seal().unwrap();

    let mut capture_paths = Vec::new();
    let mut built = write_staged_full_snapshot_with_projection_state(
        &stage,
        &ProjectionPackWriter::new(temporary.path()),
        binding(),
        Uuid::from_u128(4),
        SequenceRange {
            from_exclusive: 0,
            to_inclusive: 1,
        },
        TeslaMateFragmentLimits {
            max_rows_per_fragment: 3,
            max_projected_json_bytes: 1024 * 1024,
        },
        || -> Result<TeslaMateProjectionStateCapture, TeslaMateFragmentError> {
            let state = TeslaMateProjectionState::create(
                temporary.path(),
                TeslaMateProjectionStateLimits {
                    max_rows: 2_000,
                    max_state_bytes: 4 * 1024 * 1024,
                    max_changed_payload_bytes: 4 * 1024 * 1024,
                    minimum_free_bytes: 0,
                },
            )
            .expect("fresh retry state capture");
            capture_paths.push(state.path_for_test().to_path_buf());
            Ok::<TeslaMateProjectionStateCapture, TeslaMateFragmentError>(
                TeslaMateProjectionStateCapture::for_initial_base(state),
            )
        },
    )
    .unwrap();

    assert!(built.chunks.len() < ProtocolLimits::default().max_chunks);
    assert!(
        capture_paths.len() > 1,
        "the original small fragment target must retry with a fresh capture"
    );
    assert!(
        capture_paths[..capture_paths.len() - 1]
            .iter()
            .all(|path| !path.exists()),
        "failed retry captures must remove only their own private spools"
    );
    let final_capture_path = capture_paths.last().unwrap().clone();
    assert!(final_capture_path.exists());
    let mut capture = built
        .projection_state
        .take()
        .expect("successful staged candidate retains its final capture");
    capture.seal().expect("seal final retry capture");
    assert!(capture.stats().row_count > 1_000);
    drop(capture);
    assert!(
        !final_capture_path.exists(),
        "dropping an unpublished staged candidate state removes only its final spool"
    );
}

#[test]
fn writes_fk_complete_fragments_and_a_signed_complete_manifest() {
    let temporary = crate::private_tempdir().unwrap();
    let (_stage_directory, stage) = stage();
    let snapshot_id = Uuid::from_u128(4);
    let sequence = SequenceRange {
        from_exclusive: 0,
        to_inclusive: 1,
    };
    let built = write_staged_full_snapshot_with_limits(
        &stage,
        &ProjectionPackWriter::new(temporary.path()),
        binding(),
        snapshot_id,
        sequence,
        TeslaMateFragmentLimits {
            max_rows_per_fragment: 3,
            max_projected_json_bytes: 1024 * 1024,
        },
    )
    .unwrap();
    assert!(built.chunks.len() >= 3);
    assert_eq!(built.report.completed_drives, 1);
    assert_eq!(built.report.projected_charge_samples, 1);
    let transport_rows = built
        .chunks
        .iter()
        .map(|chunk| chunk.metadata.row_count)
        .sum::<u64>();
    let manifest = signed_full_snapshot_manifest(
        &binding(),
        snapshot_id,
        sequence,
        &built.chunks,
        transport_rows,
        &CursorKey::from_bytes([7; 32]),
    )
    .unwrap();
    assert_eq!(manifest.chunk_count as usize, built.chunks.len());
    assert_eq!(manifest.total_rows, transport_rows);
    assert!(transport_rows > built.report.logical_row_count().unwrap());
    assert!(matches!(
        signed_full_snapshot_manifest(
            &binding(),
            snapshot_id,
            sequence,
            &built.chunks,
            built.report.logical_row_count().unwrap(),
            &CursorKey::from_bytes([7; 32]),
        ),
        Err(ProjectionPackError::Invalid(_))
    ));
}

#[test]
fn staged_thirty_nine_update_rows_are_exact_in_signed_schema_2_1_packs() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let (_stage_directory, mut stage) = mutable_stage();
    let mut expected = vec![(
        300,
        1,
        1_699_000_000_000,
        1_699_000_060_000,
        "2026.20.1".to_owned(),
    )];
    for id in 301_i64..=338 {
        let offset = id - 301;
        let start_date_ms = 1_800_000_000_000 + offset * 1_000;
        let end_date_ms = start_date_ms + 900;
        let version = format!("2026.44.{}", offset + 1);
        stage
            .insert(
                TeslaMateStageTable::Updates,
                id,
                &TeslaMateUpdate {
                    id,
                    car_id: 1,
                    start_date_ms,
                    end_date_ms: Some(end_date_ms),
                    version: Some(version.clone()),
                },
            )
            .expect("stage complete update");
        expected.push((id, 1, start_date_ms, end_date_ms, version));
    }
    stage.seal().expect("seal stage with 39 updates");
    let snapshot_id = Uuid::from_u128(44);
    let sequence = SequenceRange {
        from_exclusive: 0,
        to_inclusive: 1,
    };
    let built = write_staged_full_snapshot(
        &stage,
        &ProjectionPackWriter::new(temporary.path().join("packs")),
        binding(),
        snapshot_id,
        sequence,
    )
    .expect("stage with update history builds");

    assert_eq!(built.report.projected_updates, 39);
    assert_eq!(built.report.skipped_incomplete_updates, 0);
    assert!(
        built
            .chunks
            .iter()
            .all(|chunk| chunk.metadata.schema == crate::protocol::HUB_PROJECTION_SCHEMA_V2),
        "publishable update history requires schema 2.1 for every fragment"
    );
    assert_eq!(
        update_rows_from_chunks(temporary.path(), &built.chunks),
        expected,
        "source IDs, firmware versions, and complete date ranges survive staged pack emission"
    );

    let transport_rows = built
        .chunks
        .iter()
        .map(|chunk| chunk.metadata.row_count)
        .sum::<u64>();
    let manifest = signed_full_snapshot_manifest(
        &binding(),
        snapshot_id,
        sequence,
        &built.chunks,
        transport_rows,
        &CursorKey::from_bytes([41; 32]),
    )
    .expect("sign complete update-history manifest");
    manifest.validate().expect("signed manifest validates");
    assert_eq!(manifest.total_rows, transport_rows);
}

#[test]
fn staged_full_snapshot_uses_schema_2_1_with_an_empty_update_table() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let (_fixture_directory, fixture_stage) = stage();
    let car = fixture_stage
        .get::<TeslaMateCar>(TeslaMateStageTable::Cars, 1)
        .expect("read fixture car")
        .expect("fixture car");
    let mut empty_stage = TeslaMateStage::create(
        temporary.path().join("empty-stage"),
        TeslaMateStageLimits {
            max_rows: 4,
            max_stage_bytes: 64 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("create empty-update stage");
    empty_stage
        .insert(TeslaMateStageTable::Cars, car.id, &car)
        .expect("stage car without updates");
    empty_stage.seal().expect("seal empty-update stage");

    let snapshot_id = Uuid::from_u128(45);
    let sequence = SequenceRange {
        from_exclusive: 0,
        to_inclusive: 1,
    };
    let built = write_staged_full_snapshot(
        &empty_stage,
        &ProjectionPackWriter::new(temporary.path().join("packs")),
        binding(),
        snapshot_id,
        sequence,
    )
    .expect("empty update history still builds a complete source pack");

    assert_eq!(built.report.projected_updates, 0);
    assert_eq!(built.chunks.len(), 1);
    assert_eq!(
        built.chunks[0].metadata.schema,
        crate::protocol::HUB_PROJECTION_SCHEMA_V2,
        "ordinary staged full snapshots never silently fall back to schema 2.0"
    );
    assert!(
        update_rows_from_chunks(temporary.path(), &built.chunks).is_empty(),
        "schema 2.1 keeps the updates table even when no source rows exist"
    );

    let key = CursorKey::from_bytes([42; 32]);
    let manifest = signed_full_snapshot_manifest(
        &binding(),
        snapshot_id,
        sequence,
        &built.chunks,
        built.chunks[0].metadata.row_count,
        &key,
    )
    .expect("sign empty-update schema-2.1 manifest");
    manifest
        .validate_terminal_cursor(&key)
        .expect("empty update schema-2.1 manifest remains signed and valid");
}
