// SPDX-License-Identifier: AGPL-3.0-only

use std::{fs::File, io::BufReader};

use rusqlite::Connection;
use tempfile::tempdir;

use super::*;
use crate::{
    hub_pack::{
        ProjectionFixedNumericV2_2, ProjectionFloat64BitsV2_2, ProjectionPreferredRangeV2_2,
        ProjectionStateStatusV2_2, ProjectionUnitOfLengthV2_2, ProjectionUnitOfPressureV2_2,
        ProjectionUnitOfTemperatureV2_2,
    },
    protocol::MirrorTable,
    storage::db::HubStore,
    teslamate_stage::TeslaMateStageLimits,
};

fn stage_limits() -> TeslaMateStageLimits {
    TeslaMateStageLimits {
        max_rows: 32,
        max_stage_bytes: 2 * 1024 * 1024,
        minimum_free_bytes: 0,
    }
}

fn binding() -> ProjectionBinding {
    ProjectionBinding {
        installation_id: Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
        account_id: Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap(),
        vehicle_id: Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap(),
        generation: 1,
        selected_car_id: 1,
    }
}

fn snapshot_id() -> Uuid {
    Uuid::parse_str("44444444-4444-4444-8444-444444444444").unwrap()
}

fn sequence() -> SequenceRange {
    SequenceRange {
        from_exclusive: 7,
        to_inclusive: 7,
    }
}

fn seed_roots(stage: &mut TeslaMateStage) {
    let settings = TeslaMateSettingsPhysicalV2_2 {
        id: 1,
        unit_of_length: ProjectionUnitOfLengthV2_2::Kilometers,
        unit_of_temperature: ProjectionUnitOfTemperatureV2_2::Celsius,
        unit_of_pressure: ProjectionUnitOfPressureV2_2::Bar,
        preferred_range: ProjectionPreferredRangeV2_2::Rated,
        base_url: None,
        grafana_url: None,
        language: "en".into(),
        theme_mode: "system".into(),
        inserted_at_pg_us: 0,
        updated_at_pg_us: 0,
    };
    let car_settings = TeslaMateCarSettingsPhysicalV2_2 {
        id: 9,
        suspend_min: 21,
        suspend_after_idle_min: 15,
        req_not_unlocked: false,
        free_supercharging: false,
        use_streaming_api: true,
        enabled: true,
        lfp_battery: false,
    };
    let car = TeslaMateCarPhysicalV2_2 {
        id: 1,
        eid: 100,
        vid: 200,
        vin: Some("physical-vin".into()),
        name: Some("physical-car".into()),
        model: Some("3".into()),
        efficiency: Some(0.153),
        trim_badging: None,
        marketing_name: None,
        exterior_color: None,
        wheel_type: None,
        spoiler_type: None,
        display_priority: 1,
        inserted_at_pg_us: 0,
        updated_at_pg_us: 0,
        settings_id: car_settings.id,
    };
    stage
        .insert(TeslaMateStageTable::GlobalSettings, settings.id, &settings)
        .expect("settings root");
    stage
        .insert(
            TeslaMateStageTable::CarSettings,
            car_settings.id,
            &car_settings,
        )
        .expect("car settings root");
    stage
        .insert(TeslaMateStageTable::Cars, i64::from(car.id), &car)
        .expect("car root");
}

fn seed_updates(stage: &mut TeslaMateStage, ids: &[i32]) {
    for id in ids {
        let update = TeslaMateUpdatePhysicalV2_2 {
            id: *id,
            car_id: 1,
            start_date_pg_us: i64::from(*id) * 1_000_000,
            end_date_pg_us: None,
            version: Some(format!("2026.{id}")),
        };
        stage
            .insert(TeslaMateStageTable::Updates, i64::from(*id), &update)
            .expect("physical update");
    }
}

fn seed_states(
    stage: &mut TeslaMateStage,
    values: &[(i32, ProjectionStateStatusV2_2, i64, Option<i64>)],
) {
    for (id, state, start_date_pg_us, end_date_pg_us) in values {
        let state = TeslaMateStatePhysicalV2_2 {
            id: *id,
            car_id: 1,
            state: *state,
            start_date_pg_us: *start_date_pg_us,
            end_date_pg_us: *end_date_pg_us,
        };
        stage
            .insert(TeslaMateStageTable::States, i64::from(*id), &state)
            .expect("physical state");
    }
}

fn physical_drive() -> TeslaMateDrivePhysicalV2_2 {
    TeslaMateDrivePhysicalV2_2 {
        id: 30,
        car_id: 1,
        start_date_pg_us: 345_678,
        end_date_pg_us: Some(i64::MAX),
        start_position_id: Some(40),
        end_position_id: None,
        start_address_id: None,
        end_address_id: None,
        start_geofence_id: None,
        end_geofence_id: None,
        outside_temp_avg_e1: Some(ProjectionFixedNumericV2_2::NaN),
        inside_temp_avg_e1: None,
        speed_max: None,
        power_max: None,
        power_min: None,
        start_ideal_range_km_e2: None,
        end_ideal_range_km_e2: None,
        start_rated_range_km_e2: None,
        end_rated_range_km_e2: None,
        start_km: Some(ProjectionFloat64BitsV2_2((-0.0_f64).to_bits())),
        end_km: None,
        distance: None,
        duration_min: None,
        ascent: None,
        descent: None,
    }
}

fn physical_position() -> TeslaMatePositionPhysicalV2_2 {
    TeslaMatePositionPhysicalV2_2 {
        id: 40,
        car_id: 1,
        drive_id: Some(30),
        date_pg_us: 456_789,
        latitude_e6: ProjectionFixedNumericV2_2::NaN,
        longitude_e6: ProjectionFixedNumericV2_2::Finite(1_234_567),
        elevation: None,
        speed: None,
        power: None,
        odometer: Some(ProjectionFloat64BitsV2_2((-0.0_f64).to_bits())),
        ideal_battery_range_km_e2: None,
        est_battery_range_km_e2: None,
        rated_battery_range_km_e2: None,
        battery_level: None,
        usable_battery_level: None,
        battery_heater: None,
        battery_heater_on: None,
        battery_heater_no_power: None,
        outside_temp_e1: None,
        inside_temp_e1: None,
        fan_status: None,
        driver_temp_setting_e1: None,
        passenger_temp_setting_e1: None,
        is_climate_on: None,
        is_rear_defroster_on: None,
        is_front_defroster_on: None,
        tpms_pressure_fl_e1: None,
        tpms_pressure_fr_e1: None,
        tpms_pressure_rl_e1: None,
        tpms_pressure_rr_e1: None,
    }
}

fn seed_drive_and_position(stage: &mut TeslaMateStage) {
    let drive = physical_drive();
    let position = physical_position();
    stage
        .insert(TeslaMateStageTable::Drives, i64::from(drive.id), &drive)
        .expect("physical drive");
    stage
        .insert(
            TeslaMateStageTable::Positions,
            i64::from(position.id),
            &position,
        )
        .expect("physical position");
}

fn physical_charging_process(
    id: i32,
    cost_e2: ProjectionFixedNumericV2_2,
) -> TeslaMateChargingProcessPhysicalV2_2 {
    TeslaMateChargingProcessPhysicalV2_2 {
        id,
        car_id: 1,
        position_id: 40,
        address_id: None,
        geofence_id: None,
        start_date_pg_us: 567_890 + i64::from(id),
        end_date_pg_us: Some(i64::MAX),
        charge_energy_added_e2: Some(ProjectionFixedNumericV2_2::NaN),
        charge_energy_used_e2: None,
        start_ideal_range_km_e2: None,
        end_ideal_range_km_e2: None,
        start_rated_range_km_e2: None,
        end_rated_range_km_e2: None,
        start_battery_level: Some(10),
        end_battery_level: Some(20),
        duration_min: Some(30),
        outside_temp_avg_e1: Some(ProjectionFixedNumericV2_2::Finite(-1)),
        cost_e2: Some(cost_e2),
    }
}

fn seed_charging_processes(stage: &mut TeslaMateStage) {
    for process in [
        physical_charging_process(50, ProjectionFixedNumericV2_2::Finite(99_999_999_999_999)),
        physical_charging_process(51, ProjectionFixedNumericV2_2::Finite(-99_999_999_999_999)),
    ] {
        stage
            .insert(
                TeslaMateStageTable::ChargingProcesses,
                i64::from(process.id),
                &process,
            )
            .expect("physical charging process");
    }
}

fn physical_charge(id: i32, charging_process_id: i32) -> TeslaMateChargePhysicalV2_2 {
    TeslaMateChargePhysicalV2_2 {
        id,
        charging_process_id,
        date_pg_us: i64::from(id),
        battery_heater: None,
        battery_heater_on: Some(false),
        battery_heater_no_power: Some(true),
        battery_level: Some(10),
        usable_battery_level: Some(9),
        charge_energy_added_e2: ProjectionFixedNumericV2_2::NaN,
        charger_actual_current: Some(i16::MIN),
        charger_phases: None,
        charger_pilot_current: Some(i16::MAX),
        charger_power: i16::MIN,
        charger_voltage: None,
        conn_charge_cable: Some(String::new()),
        fast_charger_present: None,
        fast_charger_brand: None,
        fast_charger_type: None,
        ideal_battery_range_km_e2: ProjectionFixedNumericV2_2::Finite(-999_999),
        rated_battery_range_km_e2: Some(ProjectionFixedNumericV2_2::NaN),
        not_enough_power_to_heat: Some(false),
        outside_temp_e1: Some(ProjectionFixedNumericV2_2::Finite(-1)),
    }
}

fn deterministic_high_entropy_version(id: i32) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut state = (id as u64) ^ 0x9e37_79b9_7f4a_7c15;
    let mut value = String::with_capacity(255);
    for _ in 0..255 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let alphabet_index = usize::try_from(state & 63).expect("bounded alphabet index");
        value.push(char::from(ALPHABET[alphabet_index]));
    }
    value
}

fn seed_profile_limit_updates(stage: &mut TeslaMateStage, rows_per_chunk: i32) {
    const PAGE_ROWS: i32 = 2_000;
    let total_rows = rows_per_chunk * 2;
    let mut first_id = 1_i32;
    while first_id <= total_rows {
        let last_id = (first_id + PAGE_ROWS - 1).min(total_rows);
        let page = (first_id..=last_id).map(|id| {
            let version = if id <= rows_per_chunk {
                "x".repeat(255)
            } else {
                deterministic_high_entropy_version(id)
            };
            let update = TeslaMateUpdatePhysicalV2_2 {
                id,
                car_id: 1,
                start_date_pg_us: i64::from(id),
                end_date_pg_us: None,
                version: Some(version),
            };
            (i64::from(id), update)
        });
        stage
            .insert_page_parallel(TeslaMateStageTable::Updates, page)
            .expect("profile-limit update page");
        first_id = last_id + 1;
    }
}

fn chunk_limits() -> TeslaMatePhysicalFragmentLimits {
    TeslaMatePhysicalFragmentLimits {
        max_rows_per_chunk: 4,
        max_projected_json_bytes: 64 * 1024,
    }
}

fn decode_pack(chunk: &BuiltProjectionPack, destination: &std::path::Path) {
    let input = File::open(&chunk.path).expect("compressed pack");
    let mut decoder = zstd::stream::read::Decoder::new(BufReader::new(input)).expect("decoder");
    let mut output = File::create(destination).expect("decoded pack");
    std::io::copy(&mut decoder, &mut output).expect("decode pack");
}

#[test]
fn physical_writer_requires_an_explicit_sealed_physical_stage() {
    let temporary = tempdir().expect("temp dir");
    let writer = ProjectionPackWriter::new(temporary.path().join("packs"));
    let cursor_key = CursorKey::from_bytes([7; 32]);

    let physical = TeslaMateStage::create_physical_v3(
        temporary.path().join("physical-imports"),
        stage_limits(),
    )
    .expect("physical stage");
    let error = write_staged_physical_updates_snapshot_v3(
        &physical,
        &writer,
        binding(),
        snapshot_id(),
        sequence(),
        &cursor_key,
    )
    .expect_err("open stage");
    assert!(matches!(
        error,
        TeslaMatePhysicalFragmentError::StageNotSealed
    ));

    let mut compatibility =
        TeslaMateStage::create(temporary.path().join("compat-imports"), stage_limits())
            .expect("compatibility stage");
    compatibility.seal().expect("seal compatibility stage");
    let error = write_staged_physical_updates_snapshot_v3(
        &compatibility,
        &writer,
        binding(),
        snapshot_id(),
        sequence(),
        &cursor_key,
    )
    .expect_err("compatibility stage");
    assert!(matches!(
        error,
        TeslaMatePhysicalFragmentError::WrongStageFormat
    ));
}

#[test]
fn physical_writer_rejects_unsupported_relations_before_writing_a_chunk() {
    for (stage_table, expected_table) in [
        (TeslaMateStageTable::Addresses, "addresses"),
        (TeslaMateStageTable::Geofences, "geofences"),
    ] {
        let temporary = tempdir().expect("temp dir");
        let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
        let mut stage =
            TeslaMateStage::create_physical_v3(temporary.path().join("imports"), stage_limits())
                .expect("physical stage");
        seed_roots(&mut stage);
        stage
            .insert(stage_table, 20, &serde_json::json!({"id": 20}))
            .expect("unsupported physical relation marker");
        stage.seal().expect("seal physical stage");

        let writer = ProjectionPackWriter::new(store.packs_dir());
        let error = write_staged_physical_updates_snapshot_v3(
            &stage,
            &writer,
            binding(),
            snapshot_id(),
            sequence(),
            &CursorKey::from_bytes([10; 32]),
        )
        .expect_err("unsupported relation rows");
        assert!(matches!(
            error,
            TeslaMatePhysicalFragmentError::UnsupportedTableRows { table }
                if table == expected_table
        ));
        assert!(!store.packs_dir().join("sha256").exists());
    }
}

#[test]
fn physical_state_rows_require_matching_source_identity_and_selected_car() {
    for (stored_id, decoded_id, car_id, mismatch) in [
        (21_i64, 20_i32, 1_i16, "source"),
        (20_i64, 20_i32, 2_i16, "car"),
    ] {
        let temporary = tempdir().expect("temp dir");
        let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
        let mut stage =
            TeslaMateStage::create_physical_v3(temporary.path().join("imports"), stage_limits())
                .expect("physical stage");
        seed_roots(&mut stage);
        let state = TeslaMateStatePhysicalV2_2 {
            id: decoded_id,
            car_id,
            state: ProjectionStateStatusV2_2::Online,
            start_date_pg_us: 123_456,
            end_date_pg_us: None,
        };
        stage
            .insert(TeslaMateStageTable::States, stored_id, &state)
            .expect("physical state");
        stage.seal().expect("seal physical stage");

        let error = write_staged_physical_updates_snapshot_v3(
            &stage,
            &ProjectionPackWriter::new(store.packs_dir()),
            binding(),
            snapshot_id(),
            sequence(),
            &CursorKey::from_bytes([11; 32]),
        )
        .expect_err("invalid physical state");
        match mismatch {
            "source" => assert!(matches!(
                error,
                TeslaMatePhysicalFragmentError::SourceIdMismatch {
                    table: "states",
                    stored: 21,
                    decoded: 20
                }
            )),
            "car" => assert!(matches!(
                error,
                TeslaMatePhysicalFragmentError::SelectedCarMismatch
            )),
            _ => unreachable!(),
        }
        assert!(!store.packs_dir().join("sha256").exists());
        let connection = store.open().expect("catalogue");
        for table in ["sync_manifests", "sync_packs"] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("catalogue count");
            assert_eq!(count, 0);
        }
    }
}

#[test]
fn physical_relation_rows_require_matching_source_identity_and_selected_car() {
    for relation in [
        "drive-source",
        "position-car",
        "charging-process-source",
        "charging-process-car",
    ] {
        let temporary = tempdir().expect("temp dir");
        let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
        let mut stage =
            TeslaMateStage::create_physical_v3(temporary.path().join("imports"), stage_limits())
                .expect("physical stage");
        seed_roots(&mut stage);
        match relation {
            "drive-source" => {
                let drive = physical_drive();
                stage
                    .insert(TeslaMateStageTable::Drives, 31, &drive)
                    .expect("physical drive");
            }
            "position-car" => {
                let mut position = physical_position();
                position.car_id = 2;
                stage
                    .insert(
                        TeslaMateStageTable::Positions,
                        i64::from(position.id),
                        &position,
                    )
                    .expect("physical position");
            }
            "charging-process-source" => {
                let process =
                    physical_charging_process(50, ProjectionFixedNumericV2_2::Finite(100));
                stage
                    .insert(TeslaMateStageTable::ChargingProcesses, 51, &process)
                    .expect("physical charging process");
            }
            "charging-process-car" => {
                let mut process =
                    physical_charging_process(50, ProjectionFixedNumericV2_2::Finite(100));
                process.car_id = 2;
                stage
                    .insert(
                        TeslaMateStageTable::ChargingProcesses,
                        i64::from(process.id),
                        &process,
                    )
                    .expect("physical charging process");
            }
            _ => unreachable!(),
        }
        stage.seal().expect("seal physical stage");

        let error = write_staged_physical_updates_snapshot_v3(
            &stage,
            &ProjectionPackWriter::new(store.packs_dir()),
            binding(),
            snapshot_id(),
            sequence(),
            &CursorKey::from_bytes([12; 32]),
        )
        .expect_err("invalid physical relation");
        match relation {
            "drive-source" => assert!(matches!(
                error,
                TeslaMatePhysicalFragmentError::SourceIdMismatch {
                    table: "drives",
                    stored: 31,
                    decoded: 30
                }
            )),
            "position-car" => assert!(matches!(
                error,
                TeslaMatePhysicalFragmentError::SelectedCarMismatch
            )),
            "charging-process-source" => assert!(matches!(
                error,
                TeslaMatePhysicalFragmentError::SourceIdMismatch {
                    table: "charging_processes",
                    stored: 51,
                    decoded: 50
                }
            )),
            "charging-process-car" => assert!(matches!(
                error,
                TeslaMatePhysicalFragmentError::SelectedCarMismatch
            )),
            _ => unreachable!(),
        }
        assert!(!store.packs_dir().join("sha256").exists());
        let connection = store.open().expect("catalogue");
        for table in ["sync_manifests", "sync_packs"] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("catalogue count");
            assert_eq!(count, 0);
        }
    }
}

#[test]
fn physical_charge_preflight_rejects_every_invalid_parent_before_writing() {
    for invalid in ["charge-source", "missing-parent", "parent-source"] {
        let temporary = tempdir().expect("temp dir");
        let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
        let mut stage =
            TeslaMateStage::create_physical_v3(temporary.path().join("imports"), stage_limits())
                .expect("physical stage");
        seed_roots(&mut stage);
        let charge = match invalid {
            "charge-source" => {
                let process =
                    physical_charging_process(50, ProjectionFixedNumericV2_2::Finite(100));
                stage
                    .insert(TeslaMateStageTable::ChargingProcesses, 50, &process)
                    .expect("physical charging process");
                physical_charge(60, 50)
            }
            "missing-parent" => physical_charge(60, 999),
            "parent-source" => {
                let process =
                    physical_charging_process(51, ProjectionFixedNumericV2_2::Finite(100));
                stage
                    .insert(TeslaMateStageTable::ChargingProcesses, 50, &process)
                    .expect("mismatched physical charging process");
                physical_charge(60, 50)
            }
            _ => unreachable!(),
        };
        let stored_charge_id = if invalid == "charge-source" { 61 } else { 60 };
        stage
            .insert(TeslaMateStageTable::Charges, stored_charge_id, &charge)
            .expect("physical charge");
        stage.seal().expect("seal physical stage");

        let error = write_staged_physical_updates_snapshot_v3(
            &stage,
            &ProjectionPackWriter::new(store.packs_dir()),
            binding(),
            snapshot_id(),
            sequence(),
            &CursorKey::from_bytes([13; 32]),
        )
        .expect_err("invalid charge parent");
        match invalid {
            "charge-source" => assert!(matches!(
                error,
                TeslaMatePhysicalFragmentError::SourceIdMismatch {
                    table: "charges",
                    stored: 61,
                    decoded: 60
                }
            )),
            "missing-parent" => assert!(matches!(
                error,
                TeslaMatePhysicalFragmentError::MissingChargingProcess {
                    charge_id: 60,
                    charging_process_id: 999
                }
            )),
            "parent-source" => assert!(matches!(
                error,
                TeslaMatePhysicalFragmentError::SourceIdMismatch {
                    table: "charging_processes",
                    stored: 50,
                    decoded: 51
                }
            )),
            _ => unreachable!(),
        }
        assert!(!store.packs_dir().join("sha256").exists());
        let connection = store.open().expect("catalogue");
        for table in ["sync_manifests", "sync_packs"] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("catalogue count");
            assert_eq!(count, 0);
        }
    }
}

#[test]
fn sealed_physical_stage_streams_relations_in_verified_contiguous_v3_chunks() {
    let temporary = tempdir().expect("temp dir");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let mut stage =
        TeslaMateStage::create_physical_v3(temporary.path().join("imports"), stage_limits())
            .expect("physical stage");
    seed_roots(&mut stage);
    seed_drive_and_position(&mut stage);
    seed_charging_processes(&mut stage);
    seed_states(
        &mut stage,
        &[
            (21, ProjectionStateStatusV2_2::Asleep, i64::MIN, None),
            (
                20,
                ProjectionStateStatusV2_2::Online,
                234_567,
                Some(i64::MAX),
            ),
        ],
    );
    seed_updates(&mut stage, &[11, 10]);
    stage.seal().expect("seal physical stage");

    let writer = ProjectionPackWriter::new(store.packs_dir());
    let cursor_key = CursorKey::from_bytes([8; 32]);
    let candidate = write_staged_physical_updates_snapshot_v3_with_limits(
        &stage,
        &writer,
        binding(),
        snapshot_id(),
        sequence(),
        &cursor_key,
        chunk_limits(),
    )
    .expect("physical candidate");

    assert_eq!(candidate.chunks.len(), 8);
    assert_eq!(candidate.manifest.chunks.len(), 8);
    assert_eq!(candidate.logical_source_rows, 11);
    assert_eq!(candidate.manifest.total_rows, 32);
    candidate
        .manifest
        .validate_terminal_cursor(&cursor_key)
        .expect("signed manifest cursor");
    let mut update_ids = Vec::new();
    let mut state_rows = Vec::new();
    let mut drive_rows = Vec::new();
    let mut position_rows = Vec::new();
    let mut charging_process_rows = Vec::new();
    for (ordinal, chunk) in candidate.chunks.iter().enumerate() {
        assert_eq!(chunk.metadata.ordinal, ordinal as u32);
        assert_eq!(
            chunk.metadata.pack_id,
            Uuid::new_v5(
                &snapshot_id(),
                format!("teslatlas-hub/schema-2.2/chunk/{ordinal}").as_bytes()
            )
        );
        assert_eq!(chunk.metadata.snapshot_id, snapshot_id());
        assert_eq!(chunk.metadata.sequence, sequence());
        assert!(chunk.metadata.compressed_bytes <= HUB_SYNC_PROFILE_MAX_PACK_BYTES);
        assert_eq!(
            chunk.metadata.tables,
            match ordinal {
                0 => vec![MirrorTable::Car, MirrorTable::Drive],
                1 => vec![MirrorTable::Car, MirrorTable::Position],
                2..=3 => vec![MirrorTable::Car, MirrorTable::Charge],
                4..=5 => vec![MirrorTable::Car, MirrorTable::State],
                _ => vec![MirrorTable::Car, MirrorTable::Update],
            }
        );
        chunk
            .metadata
            .verify_reader(
                File::open(&chunk.path).expect("pack file"),
                ProtocolLimits::default(),
            )
            .expect("verified compressed pack");

        let decoded = temporary.path().join(format!("chunk-{ordinal}.sqlite"));
        decode_pack(chunk, &decoded);
        let connection = Connection::open(decoded).expect("decoded SQLite");
        for table in ["global_settings", "cars", "car_settings"] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("root count");
            assert_eq!(count, 1, "{table} root must repeat in every chunk");
        }
        let mut statement = connection
            .prepare(
                "SELECT id, start_date_pg_us, end_date_pg_us,
                        outside_temp_avg_e1, outside_temp_avg_e1_is_nan,
                        start_km_f64_be
                 FROM drives ORDER BY id",
            )
            .expect("drive query");
        drive_rows.extend(
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i32>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<Vec<u8>>>(5)?,
                    ))
                })
                .expect("drives")
                .collect::<Result<Vec<_>, _>>()
                .expect("drive rows"),
        );
        let mut statement = connection
            .prepare(
                "SELECT id, drive_id, date_pg_us,
                        latitude_e6, latitude_e6_is_nan,
                        longitude_e6, longitude_e6_is_nan,
                        odometer_f64_be
                 FROM positions ORDER BY id",
            )
            .expect("position query");
        position_rows.extend(
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i32>(0)?,
                        row.get::<_, Option<i32>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, Option<Vec<u8>>>(7)?,
                    ))
                })
                .expect("positions")
                .collect::<Result<Vec<_>, _>>()
                .expect("position rows"),
        );
        let mut statement = connection
            .prepare(
                "SELECT id, position_id, start_date_pg_us, end_date_pg_us,
                        charge_energy_added_e2, charge_energy_added_e2_is_nan,
                        outside_temp_avg_e1, outside_temp_avg_e1_is_nan,
                        cost_e2, cost_e2_is_nan
                 FROM charging_processes ORDER BY id",
            )
            .expect("charging process query");
        charging_process_rows.extend(
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i32>(0)?,
                        row.get::<_, i32>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, Option<i64>>(8)?,
                        row.get::<_, i64>(9)?,
                    ))
                })
                .expect("charging processes")
                .collect::<Result<Vec<_>, _>>()
                .expect("charging process rows"),
        );
        let mut statement = connection
            .prepare("SELECT id, state, start_date_pg_us, end_date_pg_us FROM states ORDER BY id")
            .expect("state query");
        state_rows.extend(
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i32>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                })
                .expect("states")
                .collect::<Result<Vec<_>, _>>()
                .expect("state rows"),
        );
        let mut statement = connection
            .prepare("SELECT id FROM updates ORDER BY id")
            .expect("update query");
        update_ids.extend(
            statement
                .query_map([], |row| row.get::<_, i32>(0))
                .expect("updates")
                .collect::<Result<Vec<_>, _>>()
                .expect("update ids"),
        );
    }
    assert_eq!(
        state_rows,
        vec![
            (20, "online".to_owned(), 234_567, Some(i64::MAX)),
            (21, "asleep".to_owned(), i64::MIN, None),
        ]
    );
    assert_eq!(
        drive_rows,
        vec![(
            30,
            345_678,
            Some(i64::MAX),
            None,
            1,
            Some((-0.0_f64).to_bits().to_be_bytes().to_vec()),
        )]
    );
    assert_eq!(
        position_rows,
        vec![(
            40,
            Some(30),
            456_789,
            None,
            1,
            Some(1_234_567),
            0,
            Some((-0.0_f64).to_bits().to_be_bytes().to_vec()),
        )]
    );
    assert_eq!(
        charging_process_rows,
        vec![
            (
                50,
                40,
                567_940,
                Some(i64::MAX),
                None,
                1,
                Some(-1),
                0,
                Some(99_999_999_999_999),
                0,
            ),
            (
                51,
                40,
                567_941,
                Some(i64::MAX),
                None,
                1,
                Some(-1),
                0,
                Some(-99_999_999_999_999),
                0,
            ),
        ]
    );
    assert_eq!(update_ids, vec![10, 11]);

    let connection = store.open().expect("catalogue");
    for table in ["sync_manifests", "sync_packs"] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("catalogue count");
        assert_eq!(count, 0, "writer proof must not publish {table}");
    }
}

#[test]
fn charge_chunks_repeat_the_parent_across_a_512_row_boundary() {
    const CHARGE_COUNT: i32 = 513;

    let temporary = tempdir().expect("temp dir");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let mut stage = TeslaMateStage::create_physical_v3(
        temporary.path().join("imports"),
        TeslaMateStageLimits {
            max_rows: 520,
            max_stage_bytes: 16 * 1024 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("physical stage");
    seed_roots(&mut stage);
    let process = physical_charging_process(50, ProjectionFixedNumericV2_2::Finite(100));
    stage
        .insert(TeslaMateStageTable::ChargingProcesses, 50, &process)
        .expect("physical charging process");
    stage
        .insert_page_parallel(
            TeslaMateStageTable::Charges,
            (1..=CHARGE_COUNT).map(|id| (i64::from(id), physical_charge(id, 50))),
        )
        .expect("physical charge page");
    stage.seal().expect("seal physical stage");

    let candidate = write_staged_physical_updates_snapshot_v3_with_limits(
        &stage,
        &ProjectionPackWriter::new(store.packs_dir()),
        binding(),
        snapshot_id(),
        sequence(),
        &CursorKey::from_bytes([14; 32]),
        TeslaMatePhysicalFragmentLimits {
            max_rows_per_chunk: 512,
            max_projected_json_bytes: 8 * 1024 * 1024,
        },
    )
    .expect("physical charge candidate");

    assert_eq!(candidate.logical_source_rows, 517);
    assert_eq!(candidate.chunks.len(), 2);
    assert_eq!(candidate.manifest.total_rows, 521);
    assert_eq!(candidate.chunks[0].metadata.row_count, 512);
    assert_eq!(candidate.chunks[1].metadata.row_count, 9);
    candidate
        .manifest
        .validate_terminal_cursor(&CursorKey::from_bytes([14; 32]))
        .expect("signed manifest cursor");

    let mut charge_ids = Vec::new();
    let mut child_counts = Vec::new();
    for (ordinal, chunk) in candidate.chunks.iter().enumerate() {
        assert_eq!(chunk.metadata.ordinal, ordinal as u32);
        assert_eq!(
            chunk.metadata.tables,
            vec![
                MirrorTable::Car,
                MirrorTable::Charge,
                MirrorTable::ChargeSample
            ]
        );
        assert!(chunk.metadata.compressed_bytes <= HUB_SYNC_PROFILE_MAX_PACK_BYTES);
        chunk
            .metadata
            .verify_reader(
                File::open(&chunk.path).expect("pack file"),
                ProtocolLimits::default(),
            )
            .expect("verified compressed pack");
        let decoded = temporary
            .path()
            .join(format!("charge-chunk-{ordinal}.sqlite"));
        decode_pack(chunk, &decoded);
        let connection = Connection::open(decoded).expect("decoded SQLite");
        let mut parent_statement = connection
            .prepare("SELECT id FROM charging_processes ORDER BY id")
            .expect("parent query");
        let parent_ids = parent_statement
            .query_map([], |row| row.get::<_, i32>(0))
            .expect("parents")
            .collect::<Result<Vec<_>, _>>()
            .expect("parent ids");
        assert_eq!(parent_ids, vec![50]);
        let mut child_statement = connection
            .prepare("SELECT id FROM charges ORDER BY id")
            .expect("charge query");
        let child_ids = child_statement
            .query_map([], |row| row.get::<_, i32>(0))
            .expect("charges")
            .collect::<Result<Vec<_>, _>>()
            .expect("charge ids");
        child_counts.push(child_ids.len());
        charge_ids.extend(child_ids);
    }
    assert_eq!(child_counts, vec![508, 5]);
    assert_eq!(charge_ids, (1..=CHARGE_COUNT).collect::<Vec<_>>());

    let connection = store.open().expect("catalogue");
    for table in ["sync_manifests", "sync_packs"] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("catalogue count");
        assert_eq!(count, 0);
    }
}

#[test]
fn charge_parent_and_child_must_fit_together_before_writing() {
    let temporary = tempdir().expect("temp dir");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let mut stage =
        TeslaMateStage::create_physical_v3(temporary.path().join("imports"), stage_limits())
            .expect("physical stage");
    seed_roots(&mut stage);
    let process = physical_charging_process(50, ProjectionFixedNumericV2_2::Finite(100));
    stage
        .insert(TeslaMateStageTable::ChargingProcesses, 50, &process)
        .expect("physical charging process");
    stage
        .insert(TeslaMateStageTable::Charges, 60, &physical_charge(60, 50))
        .expect("physical charge");
    stage.seal().expect("seal physical stage");

    let error = write_staged_physical_updates_snapshot_v3_with_limits(
        &stage,
        &ProjectionPackWriter::new(store.packs_dir()),
        binding(),
        snapshot_id(),
        sequence(),
        &CursorKey::from_bytes([15; 32]),
        TeslaMatePhysicalFragmentLimits {
            max_rows_per_chunk: 4,
            max_projected_json_bytes: 64 * 1024,
        },
    )
    .expect_err("roots, parent, and child exceed four rows");
    assert!(matches!(
        error,
        TeslaMatePhysicalFragmentError::ParentChildExceedsTarget
    ));
    assert!(!store.packs_dir().join("sha256").exists());
    let connection = store.open().expect("catalogue");
    for table in ["sync_manifests", "sync_packs"] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("catalogue count");
        assert_eq!(count, 0);
    }
}

#[test]
fn failed_later_chunk_removes_created_objects_and_never_touches_the_catalogue() {
    let temporary = tempdir().expect("temp dir");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let mut stage =
        TeslaMateStage::create_physical_v3(temporary.path().join("imports"), stage_limits())
            .expect("physical stage");
    seed_roots(&mut stage);
    let process = physical_charging_process(50, ProjectionFixedNumericV2_2::Finite(100));
    stage
        .insert(TeslaMateStageTable::ChargingProcesses, 50, &process)
        .expect("physical charging process");
    for id in [60, 61] {
        stage
            .insert(
                TeslaMateStageTable::Charges,
                id.into(),
                &physical_charge(id, 50),
            )
            .expect("physical charge");
    }
    stage.seal().expect("seal physical stage");

    let writer = ProjectionPackWriter::new(store.packs_dir());
    let error = write_staged_physical_updates_snapshot_v3_with_test_failure(
        &stage,
        &writer,
        binding(),
        snapshot_id(),
        sequence(),
        &CursorKey::from_bytes([9; 32]),
        TeslaMatePhysicalFragmentLimits {
            max_rows_per_chunk: 5,
            max_projected_json_bytes: 64 * 1024,
        },
        1,
    )
    .expect_err("injected second chunk failure");
    assert!(matches!(
        error,
        TeslaMatePhysicalFragmentError::InjectedTestFailure(1)
    ));

    let content = store.packs_dir().join("sha256");
    let remaining = std::fs::read_dir(content)
        .expect("content directory")
        .collect::<Result<Vec<_>, _>>()
        .expect("content entries");
    assert!(remaining.is_empty());
    let connection = store.open().expect("catalogue");
    for table in ["sync_manifests", "sync_packs"] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("catalogue count");
        assert_eq!(count, 0);
    }
}

#[test]
fn compressed_profile_limit_removes_the_oversize_pack_and_prior_chunk() {
    const ROWS_PER_CHUNK: i32 = 110_000;

    let temporary = tempdir().expect("temp dir");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let mut stage = TeslaMateStage::create_physical_v3(
        temporary.path().join("imports"),
        TeslaMateStageLimits {
            max_rows: u64::try_from(ROWS_PER_CHUNK * 2 + 3).expect("bounded rows"),
            max_stage_bytes: 128 * 1024 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("physical stage");
    seed_roots(&mut stage);
    seed_profile_limit_updates(&mut stage, ROWS_PER_CHUNK);
    stage.seal().expect("seal physical stage");
    let stats = stage.stats().expect("stage stats");
    let stage_file_bytes = std::fs::metadata(stage.path())
        .expect("stage metadata")
        .len();
    eprintln!(
        "physical profile-limit fixture: rows={}, payload_bytes={}, file_bytes={}, cap_bytes={}",
        stats.row_count, stats.payload_bytes, stage_file_bytes, stats.limits.max_stage_bytes
    );
    assert!(stage_file_bytes <= stats.limits.max_stage_bytes);

    let error = write_staged_physical_updates_snapshot_v3_with_limits(
        &stage,
        &ProjectionPackWriter::new(store.packs_dir()),
        binding(),
        snapshot_id(),
        sequence(),
        &CursorKey::from_bytes([12; 32]),
        TeslaMatePhysicalFragmentLimits {
            max_rows_per_chunk: u64::try_from(ROWS_PER_CHUNK + 3).expect("bounded rows"),
            max_projected_json_bytes: 128 * 1024 * 1024,
        },
    )
    .expect_err("second chunk must exceed the compressed profile bound");
    assert!(matches!(
        error,
        TeslaMatePhysicalFragmentError::PackExceedsProfileLimit
    ));

    let content = store.packs_dir().join("sha256");
    let remaining = std::fs::read_dir(content)
        .expect("content directory from attempted chunks")
        .collect::<Result<Vec<_>, _>>()
        .expect("content entries");
    assert!(
        remaining.is_empty(),
        "oversize pack or prior chunk survived: {remaining:?}"
    );
    let connection = store.open().expect("catalogue");
    for table in ["sync_manifests", "sync_packs"] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("catalogue count");
        assert_eq!(count, 0);
    }
}

#[test]
fn unpublished_physical_candidate_keeps_the_internal_chunk_ceiling() {
    assert_eq!(PHYSICAL_CANDIDATE_MAX_CHUNKS, 512);
    assert_eq!(
        PHYSICAL_CANDIDATE_MAX_CHUNKS,
        ProtocolLimits::default().max_chunks
    );
}
