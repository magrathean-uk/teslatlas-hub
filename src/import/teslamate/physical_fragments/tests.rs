// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    fs::File,
    io::{BufReader, Write},
};

use rusqlite::Connection;
use tempfile::tempdir;

use super::*;
use crate::{
    hub_pack::{
        GeofenceBillingType, ProjectionFixedNumericV2_2, ProjectionFloat64BitsV2_2,
        ProjectionPreferredRangeV2_2, ProjectionStateStatusV2_2, ProjectionUnitOfLengthV2_2,
        ProjectionUnitOfPressureV2_2, ProjectionUnitOfTemperatureV2_2,
    },
    protocol::MirrorTable,
    storage::db::{HubStore, SourceDescriptor, VehicleDescriptor},
    teslamate_stage::TeslaMateStageLimits,
};

pub(crate) fn stage_limits() -> TeslaMateStageLimits {
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

pub(crate) fn seed_roots(stage: &mut TeslaMateStage) {
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

pub(crate) fn seed_updates(stage: &mut TeslaMateStage, ids: &[i32]) {
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

fn physical_address(id: i32) -> TeslaMateAddressPhysicalV2_2 {
    TeslaMateAddressPhysicalV2_2 {
        id,
        display_name: Some(String::new()),
        latitude_e6: Some(ProjectionFixedNumericV2_2::NaN),
        longitude_e6: Some(ProjectionFixedNumericV2_2::Finite(-1)),
        name: Some(format!("address-{id}")),
        house_number: None,
        road: None,
        neighbourhood: None,
        city: None,
        county: None,
        postcode: None,
        state: None,
        state_district: None,
        country: None,
        inserted_at_pg_us: i64::MIN,
        updated_at_pg_us: i64::MAX,
        osm_id: Some(i64::MIN),
        osm_type: Some(String::new()),
    }
}

fn physical_geofence(id: i32) -> TeslaMateGeofencePhysicalV2_2 {
    TeslaMateGeofencePhysicalV2_2 {
        id,
        name: format!("geofence-{id}"),
        latitude_e6: ProjectionFixedNumericV2_2::NaN,
        longitude_e6: ProjectionFixedNumericV2_2::Finite(-1),
        radius: i16::MIN,
        billing_type: GeofenceBillingType::PerKwh,
        cost_per_unit_e4: Some(ProjectionFixedNumericV2_2::Finite(999_999_999)),
        session_fee_e2: Some(ProjectionFixedNumericV2_2::Finite(-99_999_999_999_999)),
        inserted_at_pg_us: i64::MIN,
        updated_at_pg_us: i64::MAX,
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

fn two_chunk_physical_candidate(
    temporary: &std::path::Path,
    store: &HubStore,
    candidate_snapshot_id: Uuid,
    candidate_binding: ProjectionBinding,
) -> StagedPhysicalProjectionV3 {
    std::fs::create_dir_all(temporary).expect("candidate fixture root");
    let mut stage = TeslaMateStage::create_physical_v3(
        temporary.join(format!("stage-{candidate_snapshot_id}")),
        stage_limits(),
    )
    .expect("physical stage");
    seed_roots(&mut stage);
    seed_updates(&mut stage, &[10, 11]);
    stage.seal().expect("sealed physical stage");
    write_staged_physical_updates_snapshot_v3_with_limits(
        &stage,
        &ProjectionPackWriter::new(store.packs_dir()),
        candidate_binding,
        candidate_snapshot_id,
        sequence(),
        &CursorKey::from_bytes([19; 32]),
        chunk_limits(),
    )
    .expect("two-chunk physical candidate")
}

pub(crate) fn registered_admission_binding(store: &HubStore) -> ProjectionBinding {
    let source = store
        .register_source(
            &SourceDescriptor::new("teslamate", "physical-v3-admission"),
            1_000,
        )
        .expect("physical source");
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "1"), 1_000)
        .expect("physical vehicle");
    store
        .v2_projection_binding(vehicle.vehicle_id)
        .expect("physical projection binding")
}

pub(crate) fn public_admission_candidate_fixture(
    temporary: &std::path::Path,
    store: &HubStore,
    cursor_key: &CursorKey,
    update_count: u32,
) -> (ProjectionBinding, StagedPhysicalProjectionV3) {
    public_admission_candidate_fixture_for_source(
        temporary,
        store,
        cursor_key,
        update_count,
        "physical-v3-admission",
    )
}

pub(crate) fn public_admission_candidate_fixture_for_source(
    temporary: &std::path::Path,
    store: &HubStore,
    cursor_key: &CursorKey,
    update_count: u32,
    source_key: &str,
) -> (ProjectionBinding, StagedPhysicalProjectionV3) {
    public_admission_candidate_fixture_for_source_and_sequence(
        temporary,
        store,
        cursor_key,
        update_count,
        source_key,
        sequence().to_inclusive,
    )
}

pub(crate) fn public_admission_candidate_fixture_for_source_and_sequence(
    temporary: &std::path::Path,
    store: &HubStore,
    cursor_key: &CursorKey,
    update_count: u32,
    source_key: &str,
    head_sequence: u64,
) -> (ProjectionBinding, StagedPhysicalProjectionV3) {
    assert!(update_count > 0);
    std::fs::create_dir_all(temporary).expect("public admission fixture root");
    let source = store
        .register_source(&SourceDescriptor::new("teslamate", source_key), 1_000)
        .expect("public physical source");
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "1"), 1_000)
        .expect("public physical vehicle");
    let binding = store
        .v2_projection_binding(vehicle.vehicle_id)
        .expect("public physical projection binding");
    let mut stage = TeslaMateStage::create_physical_v3(
        temporary.join(format!("public-physical-stage-{source_key}")),
        TeslaMateStageLimits {
            max_rows: u64::from(update_count) + 3,
            max_stage_bytes: 32 * 1024 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("public physical stage");
    seed_roots(&mut stage);
    let ids =
        (10..10 + i32::try_from(update_count).expect("bounded update count")).collect::<Vec<_>>();
    seed_updates(&mut stage, &ids);
    stage.seal().expect("sealed public physical stage");
    let candidate = write_staged_physical_updates_snapshot_v3_with_limits(
        &stage,
        &ProjectionPackWriter::new(store.packs_dir()),
        binding.clone(),
        Uuid::new_v4(),
        SequenceRange {
            from_exclusive: head_sequence,
            to_inclusive: head_sequence,
        },
        cursor_key,
        TeslaMatePhysicalFragmentLimits {
            max_rows_per_chunk: 4,
            max_projected_json_bytes: 64 * 1024,
        },
    )
    .expect("public physical candidate");
    assert_eq!(candidate.chunks.len(), update_count as usize);
    (binding, candidate)
}

fn public_admission_candidate_for_binding(
    temporary: &std::path::Path,
    store: &HubStore,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    head_sequence: u64,
    cursor_byte: u8,
) -> StagedPhysicalProjectionV3 {
    std::fs::create_dir_all(temporary).expect("bound admission fixture root");
    let mut stage = TeslaMateStage::create_physical_v3(
        temporary.join(format!("physical-stage-{snapshot_id}")),
        TeslaMateStageLimits {
            max_rows: 5,
            max_stage_bytes: 32 * 1024 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("bound physical stage");
    seed_roots(&mut stage);
    seed_updates(&mut stage, &[10, 11]);
    stage.seal().expect("sealed bound physical stage");
    let cursor_key = CursorKey::from_bytes([cursor_byte; 32]);
    write_staged_physical_updates_snapshot_v3_with_limits(
        &stage,
        &ProjectionPackWriter::new(store.packs_dir()),
        binding,
        snapshot_id,
        SequenceRange {
            from_exclusive: head_sequence,
            to_inclusive: head_sequence,
        },
        &cursor_key,
        TeslaMatePhysicalFragmentLimits {
            max_rows_per_chunk: 4,
            max_projected_json_bytes: 64 * 1024,
        },
    )
    .expect("bound physical candidate")
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
fn physical_relation_preflight_rejects_unreferenced_and_tampered_rows_before_writing() {
    for invalid in [
        "address-source",
        "address-unreferenced",
        "geofence-source",
        "geofence-unreferenced",
    ] {
        let temporary = tempdir().expect("temp dir");
        let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
        let mut stage =
            TeslaMateStage::create_physical_v3(temporary.path().join("imports"), stage_limits())
                .expect("physical stage");
        seed_roots(&mut stage);
        match invalid {
            "address-source" => stage
                .insert(TeslaMateStageTable::Addresses, 70, &physical_address(71))
                .expect("tampered physical address"),
            "address-unreferenced" => stage
                .insert(TeslaMateStageTable::Addresses, 70, &physical_address(70))
                .expect("unreferenced physical address"),
            "geofence-source" => stage
                .insert(TeslaMateStageTable::Geofences, 80, &physical_geofence(81))
                .expect("tampered physical geofence"),
            "geofence-unreferenced" => stage
                .insert(TeslaMateStageTable::Geofences, 80, &physical_geofence(80))
                .expect("unreferenced physical geofence"),
            _ => unreachable!(),
        }
        stage.seal().expect("seal physical stage");

        let error = write_staged_physical_updates_snapshot_v3(
            &stage,
            &ProjectionPackWriter::new(store.packs_dir()),
            binding(),
            snapshot_id(),
            sequence(),
            &CursorKey::from_bytes([10; 32]),
        )
        .expect_err("invalid physical relation row");
        match invalid {
            "address-source" => assert!(matches!(
                error,
                TeslaMatePhysicalFragmentError::SourceIdMismatch {
                    table: "addresses",
                    stored: 70,
                    decoded: 71
                }
            )),
            "address-unreferenced" => assert!(matches!(
                error,
                TeslaMatePhysicalFragmentError::UnreferencedRelation {
                    table: "addresses",
                    source_id: 70
                }
            )),
            "geofence-source" => assert!(matches!(
                error,
                TeslaMatePhysicalFragmentError::SourceIdMismatch {
                    table: "geofences",
                    stored: 80,
                    decoded: 81
                }
            )),
            "geofence-unreferenced" => assert!(matches!(
                error,
                TeslaMatePhysicalFragmentError::UnreferencedRelation {
                    table: "geofences",
                    source_id: 80
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
        assert!(
            chunk.metadata.compressed_bytes
                <= ProtocolLimits::hub_sync_v1_1_3_schema_2_2().max_compressed_pack_bytes
        );
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
fn referenced_relations_emit_once_with_their_deterministic_first_referrer() {
    let temporary = tempdir().expect("temp dir");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let mut stage =
        TeslaMateStage::create_physical_v3(temporary.path().join("imports"), stage_limits())
            .expect("physical stage");
    seed_roots(&mut stage);
    let mut drive = physical_drive();
    drive.start_address_id = Some(70);
    drive.end_address_id = Some(999);
    drive.start_geofence_id = Some(80);
    drive.end_geofence_id = Some(999);
    stage
        .insert(TeslaMateStageTable::Drives, i64::from(drive.id), &drive)
        .expect("physical drive");
    let mut shared_process = physical_charging_process(50, ProjectionFixedNumericV2_2::Finite(100));
    shared_process.address_id = Some(70);
    shared_process.geofence_id = Some(80);
    stage
        .insert(
            TeslaMateStageTable::ChargingProcesses,
            i64::from(shared_process.id),
            &shared_process,
        )
        .expect("shared-ref physical process");
    let mut owned_process = physical_charging_process(51, ProjectionFixedNumericV2_2::Finite(200));
    owned_process.address_id = Some(71);
    owned_process.geofence_id = Some(81);
    stage
        .insert(
            TeslaMateStageTable::ChargingProcesses,
            i64::from(owned_process.id),
            &owned_process,
        )
        .expect("owned-ref physical process");
    stage
        .insert(TeslaMateStageTable::Charges, 60, &physical_charge(60, 51))
        .expect("physical charge");
    for id in [70, 71] {
        stage
            .insert(
                TeslaMateStageTable::Addresses,
                id.into(),
                &physical_address(id),
            )
            .expect("physical address");
    }
    for id in [80, 81] {
        stage
            .insert(
                TeslaMateStageTable::Geofences,
                id.into(),
                &physical_geofence(id),
            )
            .expect("physical geofence");
    }
    stage.seal().expect("seal physical stage");

    let candidate = write_staged_physical_updates_snapshot_v3_with_limits(
        &stage,
        &ProjectionPackWriter::new(store.packs_dir()),
        binding(),
        snapshot_id(),
        sequence(),
        &CursorKey::from_bytes([16; 32]),
        TeslaMatePhysicalFragmentLimits {
            max_rows_per_chunk: 6,
            max_projected_json_bytes: 1024 * 1024,
        },
    )
    .expect("physical relation candidate");
    assert_eq!(candidate.logical_source_rows, 11);
    assert_eq!(candidate.chunks.len(), 4);
    assert_eq!(candidate.manifest.total_rows, 21);

    let mut address_ids = Vec::new();
    let mut geofence_ids = Vec::new();
    for (ordinal, chunk) in candidate.chunks.iter().enumerate() {
        let decoded = temporary
            .path()
            .join(format!("relation-chunk-{ordinal}.sqlite"));
        decode_pack(chunk, &decoded);
        let connection = Connection::open(decoded).expect("decoded SQLite");
        let mut address_statement = connection
            .prepare("SELECT id FROM addresses ORDER BY id")
            .expect("address query");
        let mut addresses = address_statement
            .query_map([], |row| row.get::<_, i32>(0))
            .expect("addresses")
            .collect::<Result<Vec<_>, _>>()
            .expect("address ids");
        let mut geofence_statement = connection
            .prepare("SELECT id FROM geofences ORDER BY id")
            .expect("geofence query");
        let mut geofences = geofence_statement
            .query_map([], |row| row.get::<_, i32>(0))
            .expect("geofences")
            .collect::<Result<Vec<_>, _>>()
            .expect("geofence ids");
        for id in &addresses {
            let referring: i64 = connection
                .query_row(
                    "SELECT
                        (SELECT COUNT(*) FROM drives
                         WHERE start_address_id = ?1 OR end_address_id = ?1)
                        +
                        (SELECT COUNT(*) FROM charging_processes WHERE address_id = ?1)",
                    [id],
                    |row| row.get(0),
                )
                .expect("same-pack address referrer");
            assert!(referring >= 1);
        }
        for id in &geofences {
            let referring: i64 = connection
                .query_row(
                    "SELECT
                        (SELECT COUNT(*) FROM drives
                         WHERE start_geofence_id = ?1 OR end_geofence_id = ?1)
                        +
                        (SELECT COUNT(*) FROM charging_processes WHERE geofence_id = ?1)",
                    [id],
                    |row| row.get(0),
                )
                .expect("same-pack geofence referrer");
            assert!(referring >= 1);
        }
        address_ids.append(&mut addresses);
        geofence_ids.append(&mut geofences);
    }
    assert_eq!(address_ids, vec![70, 71]);
    assert_eq!(geofence_ids, vec![80, 81]);
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
    let mut process = physical_charging_process(50, ProjectionFixedNumericV2_2::Finite(100));
    process.address_id = Some(70);
    process.geofence_id = Some(80);
    stage
        .insert(TeslaMateStageTable::ChargingProcesses, 50, &process)
        .expect("physical charging process");
    stage
        .insert(TeslaMateStageTable::Addresses, 70, &physical_address(70))
        .expect("physical address");
    stage
        .insert(TeslaMateStageTable::Geofences, 80, &physical_geofence(80))
        .expect("physical geofence");
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

    assert_eq!(candidate.logical_source_rows, 519);
    assert_eq!(candidate.chunks.len(), 2);
    assert_eq!(candidate.manifest.total_rows, 523);
    assert_eq!(candidate.chunks[0].metadata.row_count, 512);
    assert_eq!(candidate.chunks[1].metadata.row_count, 11);
    candidate
        .manifest
        .validate_terminal_cursor(&CursorKey::from_bytes([14; 32]))
        .expect("signed manifest cursor");

    let mut charge_ids = Vec::new();
    let mut child_counts = Vec::new();
    let mut address_counts = Vec::new();
    let mut geofence_counts = Vec::new();
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
        assert!(
            chunk.metadata.compressed_bytes
                <= ProtocolLimits::hub_sync_v1_1_3_schema_2_2().max_compressed_pack_bytes
        );
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
        address_counts.push(
            connection
                .query_row("SELECT COUNT(*) FROM addresses", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("address count"),
        );
        geofence_counts.push(
            connection
                .query_row("SELECT COUNT(*) FROM geofences", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("geofence count"),
        );
    }
    assert_eq!(child_counts, vec![506, 7]);
    assert_eq!(address_counts, vec![1, 0]);
    assert_eq!(geofence_counts, vec![1, 0]);
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
fn parent_relation_group_must_fit_atomically_before_writing() {
    let temporary = tempdir().expect("temp dir");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let mut stage =
        TeslaMateStage::create_physical_v3(temporary.path().join("imports"), stage_limits())
            .expect("physical stage");
    seed_roots(&mut stage);
    let mut drive = physical_drive();
    drive.start_address_id = Some(70);
    drive.start_geofence_id = Some(80);
    stage
        .insert(TeslaMateStageTable::Drives, i64::from(drive.id), &drive)
        .expect("physical drive");
    stage
        .insert(TeslaMateStageTable::Addresses, 70, &physical_address(70))
        .expect("physical address");
    stage
        .insert(TeslaMateStageTable::Geofences, 80, &physical_geofence(80))
        .expect("physical geofence");
    stage.seal().expect("seal physical stage");

    let error = write_staged_physical_updates_snapshot_v3_with_limits(
        &stage,
        &ProjectionPackWriter::new(store.packs_dir()),
        binding(),
        snapshot_id(),
        sequence(),
        &CursorKey::from_bytes([17; 32]),
        TeslaMatePhysicalFragmentLimits {
            max_rows_per_chunk: 5,
            max_projected_json_bytes: 64 * 1024,
        },
    )
    .expect_err("roots, drive, and owned relations exceed five rows");
    assert!(matches!(
        error,
        TeslaMatePhysicalFragmentError::ParentRelationsExceedTarget
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
    let mut process = physical_charging_process(50, ProjectionFixedNumericV2_2::Finite(100));
    process.address_id = Some(70);
    stage
        .insert(TeslaMateStageTable::ChargingProcesses, 50, &process)
        .expect("physical charging process");
    stage
        .insert(TeslaMateStageTable::Addresses, 70, &physical_address(70))
        .expect("physical address");
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
fn physical_v3_admission_survives_restart_and_transfers_pack_ownership() {
    let temporary = tempdir().expect("temp dir");
    let root = temporary.path().join("hub");
    let store = HubStore::initialize(&root).expect("store");
    let gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    let expected_binding = registered_admission_binding(&store);
    let candidate = two_chunk_physical_candidate(
        temporary.path(),
        &store,
        snapshot_id(),
        expected_binding.clone(),
    );
    let paths = candidate
        .chunks
        .iter()
        .map(|chunk| chunk.path.clone())
        .collect::<Vec<_>>();
    let admission = store
        .stage_pending_physical_v3_admission(&gate, candidate)
        .expect("physical admission");
    assert_eq!(admission.chunk_count, 2);
    assert_eq!(admission.vehicle_id, expected_binding.vehicle_id);
    assert_eq!(admission.snapshot_id, snapshot_id());
    assert!(admission.receipt_id.starts_with("pv3_"));
    assert!(paths.iter().all(|path| path.is_file()));
    assert!(
        store
            .manifest_for_vehicle(expected_binding.vehicle_id)
            .expect("generic current manifest")
            .is_none(),
        "pending physical admission must not become a generic current manifest"
    );
    for pack in &admission.manifest.chunks {
        assert!(
            store
                .pack_for_digest(pack.sha256)
                .expect("generic pack lookup")
                .is_none(),
            "pending physical object must not reach the generic pack GET lookup"
        );
    }
    drop(gate);
    let backup_root = temporary.path().join("backup");
    store
        .backup_to(&backup_root)
        .expect("pending physical admission backup");
    drop(store);

    let reopened = HubStore::initialize(&root).expect("reopened store");
    assert_eq!(
        reopened
            .pending_physical_v3_admission_for_vehicle(expected_binding.vehicle_id)
            .expect("admission lookup")
            .expect("admission survives restart"),
        admission
    );
    assert!(paths.iter().all(|path| path.is_file()));
    drop(reopened);
    let mut tampered = std::fs::OpenOptions::new()
        .write(true)
        .open(&paths[0])
        .expect("retained physical pack");
    tampered
        .write_all(&[0])
        .expect("same-length retained pack tamper");
    drop(tampered);
    let tampered_restart = HubStore::initialize(&root).expect("tampered restart");
    assert!(matches!(
        tampered_restart.pending_physical_v3_admission_for_vehicle(expected_binding.vehicle_id),
        Err(crate::storage::db::StoreError::PhysicalV3AdmissionConflict)
    ));
    let restored = HubStore::initialize(&backup_root).expect("restored backup");
    assert_eq!(
        restored
            .pending_physical_v3_admission_for_vehicle(expected_binding.vehicle_id)
            .expect("restored admission lookup")
            .expect("pending admission survives backup"),
        admission
    );
    assert!(
        restored
            .manifest_for_vehicle(expected_binding.vehicle_id)
            .expect("restored generic manifest")
            .is_none()
    );
}

#[test]
fn physical_v3_admission_faults_cleanup_only_unretained_objects() {
    use crate::durability_fault::{DurabilityFaultPoint, inject};

    let temporary = tempdir().expect("temp dir");
    let root = temporary.path().join("hub");
    let store = HubStore::initialize(&root).expect("store");
    let gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    let expected_binding = registered_admission_binding(&store);
    let candidate = two_chunk_physical_candidate(
        temporary.path(),
        &store,
        snapshot_id(),
        expected_binding.clone(),
    );
    let paths = candidate
        .chunks
        .iter()
        .map(|chunk| chunk.path.clone())
        .collect::<Vec<_>>();
    let _fault = inject(DurabilityFaultPoint::CatalogueBeforeCommit);
    assert!(matches!(
        store.stage_pending_physical_v3_admission(&gate, candidate),
        Err(crate::storage::db::StoreError::CatalogueDurability(_))
    ));
    drop(_fault);
    assert!(paths.iter().all(|path| !path.exists()));
    assert!(
        store
            .pending_physical_v3_admission_for_vehicle(expected_binding.vehicle_id)
            .expect("absent admission")
            .is_none()
    );
    let connection = store.open().expect("catalogue");
    for table in [
        "pending_physical_v3_admissions",
        "pending_physical_v3_packs",
        "sync_manifests",
        "sync_packs",
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("catalogue count");
        assert_eq!(count, 0, "pre-commit failure must leave {table} empty");
    }
    drop(connection);
    drop(gate);

    let committed_root = temporary.path().join("committed-hub");
    let committed_store = HubStore::initialize(&committed_root).expect("committed store");
    let committed_gate = committed_store
        .try_acquire_publication_gate()
        .expect("committed publication gate");
    let committed_binding = registered_admission_binding(&committed_store);
    let committed_candidate = two_chunk_physical_candidate(
        &temporary.path().join("committed-candidate"),
        &committed_store,
        snapshot_id(),
        committed_binding.clone(),
    );
    let committed_paths = committed_candidate
        .chunks
        .iter()
        .map(|chunk| chunk.path.clone())
        .collect::<Vec<_>>();
    let _after_fault = inject(DurabilityFaultPoint::CatalogueAfterCommit);
    let committed = committed_store
        .stage_pending_physical_v3_admission(&committed_gate, committed_candidate)
        .expect("post-commit fault reconciles exact admission");
    drop(_after_fault);
    assert_eq!(
        committed_store
            .pending_physical_v3_admission_for_vehicle(committed_binding.vehicle_id)
            .expect("committed admission")
            .expect("post-commit marker"),
        committed
    );
    assert!(committed_paths.iter().all(|path| path.is_file()));
}

#[test]
fn physical_v3_admission_rejects_tamper_and_a_second_head() {
    let temporary = tempdir().expect("temp dir");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    let expected_binding = registered_admission_binding(&store);

    let tampered_snapshot = Uuid::from_u128(0x55555555_5555_4555_8555_555555555555);
    let tampered = two_chunk_physical_candidate(
        temporary.path(),
        &store,
        tampered_snapshot,
        expected_binding.clone(),
    );
    let tampered_paths = tampered
        .chunks
        .iter()
        .map(|chunk| chunk.path.clone())
        .collect::<Vec<_>>();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&tampered_paths[0])
        .expect("tampered pack")
        .write_all(b"tamper")
        .expect("append tamper");
    assert!(matches!(
        store.stage_pending_physical_v3_admission(&gate, tampered),
        Err(crate::storage::db::StoreError::UnpublishedPackDigestMismatch(_))
            | Err(crate::storage::db::StoreError::PhysicalV3Pack(_))
    ));
    assert!(
        store
            .pending_physical_v3_admission_for_vehicle(expected_binding.vehicle_id)
            .expect("no tampered admission")
            .is_none()
    );
    assert!(
        !tampered_paths[1].exists(),
        "other unretained chunks are cleaned even when the tampered object needs operator repair"
    );
    for path in tampered_paths {
        let _ = std::fs::remove_file(path);
    }

    let first = two_chunk_physical_candidate(
        temporary.path(),
        &store,
        snapshot_id(),
        expected_binding.clone(),
    );
    let admitted = store
        .stage_pending_physical_v3_admission(&gate, first)
        .expect("first admitted head");
    let second_snapshot = Uuid::from_u128(0x66666666_6666_4666_8666_666666666666);
    let second = two_chunk_physical_candidate(
        temporary.path(),
        &store,
        second_snapshot,
        expected_binding.clone(),
    );
    let second_paths = second
        .chunks
        .iter()
        .map(|chunk| chunk.path.clone())
        .collect::<Vec<_>>();
    assert!(matches!(
        store.stage_pending_physical_v3_admission(&gate, second),
        Err(crate::storage::db::StoreError::PhysicalV3SecondHeadUnsupported(vehicle_id))
            if vehicle_id == expected_binding.vehicle_id
    ));
    assert!(second_paths.iter().all(|path| !path.exists()));
    assert_eq!(
        store
            .pending_physical_v3_admission_for_vehicle(expected_binding.vehicle_id)
            .expect("current admission")
            .expect("first head retained"),
        admitted
    );
}

#[test]
fn pending_physical_v3_admission_rejects_foreign_or_inactive_identity() {
    let temporary = tempdir().expect("temp dir");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    let expected_binding = registered_admission_binding(&store);

    let mut foreign_binding = expected_binding.clone();
    foreign_binding.installation_id = Uuid::from_u128(0x77777777_7777_4777_8777_777777777777);
    let foreign = two_chunk_physical_candidate(
        temporary.path(),
        &store,
        Uuid::from_u128(0x77777777_7777_4777_8777_777777777778),
        foreign_binding,
    );
    let foreign_paths = foreign
        .chunks
        .iter()
        .map(|chunk| chunk.path.clone())
        .collect::<Vec<_>>();
    assert!(matches!(
        store.stage_pending_physical_v3_admission(&gate, foreign),
        Err(crate::storage::db::StoreError::PhysicalV3AdmissionInvalid)
    ));
    assert!(foreign_paths.iter().all(|path| !path.exists()));

    let inactive = two_chunk_physical_candidate(
        temporary.path(),
        &store,
        Uuid::from_u128(0x88888888_8888_4888_8888_888888888888),
        expected_binding.clone(),
    );
    let inactive_paths = inactive
        .chunks
        .iter()
        .map(|chunk| chunk.path.clone())
        .collect::<Vec<_>>();
    assert!(
        store
            .retire_vehicle(expected_binding.vehicle_id, 2_000)
            .expect("retire vehicle")
    );
    assert!(matches!(
        store.stage_pending_physical_v3_admission(&gate, inactive),
        Err(crate::storage::db::StoreError::PhysicalV3AdmissionInvalid)
    ));
    assert!(inactive_paths.iter().all(|path| !path.exists()));
}

#[test]
fn pending_physical_v3_lookup_rechecks_active_projection_binding() {
    let temporary = tempdir().expect("temp dir");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    let expected_binding = registered_admission_binding(&store);
    let candidate = two_chunk_physical_candidate(
        temporary.path(),
        &store,
        snapshot_id(),
        expected_binding.clone(),
    );
    let admitted = store
        .stage_pending_physical_v3_admission(&gate, candidate)
        .expect("physical admission");

    assert!(
        store
            .retire_vehicle(expected_binding.vehicle_id, 2_000)
            .expect("retire vehicle")
    );
    assert!(matches!(
        store.pending_physical_v3_admission_for_vehicle(expected_binding.vehicle_id),
        Err(crate::storage::db::StoreError::PhysicalV3AdmissionInvalid)
    ));
    assert!(
        store
            .reactivate_vehicle(expected_binding.vehicle_id)
            .expect("reactivate vehicle")
    );
    assert_eq!(
        store
            .pending_physical_v3_admission_for_vehicle(expected_binding.vehicle_id)
            .expect("reactivated lookup")
            .expect("retained admission"),
        admitted
    );

    let connection = store.open().expect("catalogue");
    connection
        .execute(
            "UPDATE sources SET generation = generation + 1
              WHERE source_id = (
                    SELECT source_id FROM vehicles WHERE vehicle_id = ?1
              )",
            [expected_binding.vehicle_id.to_string()],
        )
        .expect("advance source generation");
    drop(connection);
    assert!(matches!(
        store.pending_physical_v3_admission_for_vehicle(expected_binding.vehicle_id),
        Err(crate::storage::db::StoreError::PhysicalV3AdmissionInvalid)
    ));
}

#[test]
fn physical_v3_rotation_retains_prior_across_restart_and_backup_but_stays_unservable() {
    let temporary = tempdir().expect("temp dir");
    let root = temporary.path().join("hub");
    let store = HubStore::initialize(&root).expect("store");
    let gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    let binding = registered_admission_binding(&store);
    let first = public_admission_candidate_for_binding(
        &temporary.path().join("first"),
        &store,
        binding.clone(),
        Uuid::from_u128(0x91919191_9191_4191_8191_919191919191),
        1,
        91,
    );
    let first_paths = first
        .chunks
        .iter()
        .map(|chunk| chunk.path.clone())
        .collect::<Vec<_>>();
    let prior = store
        .stage_pending_physical_v3_admission(&gate, first)
        .expect("first head");
    let second = public_admission_candidate_for_binding(
        &temporary.path().join("second"),
        &store,
        binding.clone(),
        Uuid::from_u128(0x92929292_9292_4292_8292_929292929292),
        2,
        92,
    );
    let second_paths = second
        .chunks
        .iter()
        .map(|chunk| chunk.path.clone())
        .collect::<Vec<_>>();
    let retained_at_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("current time")
            .as_millis(),
    )
    .expect("current milliseconds");
    let current = store
        .rotate_pending_physical_v3_admission_at(&gate, second, retained_at_ms)
        .expect("rotate physical head");
    assert_eq!(current.head_sequence, 2);
    assert_eq!(current.vehicle_id, prior.vehicle_id);
    let retained = store
        .retained_physical_v3_admission_for_receipt_at(
            binding.vehicle_id,
            &prior.receipt_id,
            retained_at_ms,
            true,
        )
        .expect("retained lookup")
        .expect("prior retained");
    assert_eq!(retained.admission, prior);
    assert_eq!(retained.retained_at_ms, retained_at_ms);
    assert_eq!(
        retained.expires_at_ms,
        retained_at_ms + crate::storage::db::RETIRED_LINEAGE_PACK_RETENTION_MS
    );
    assert!(matches!(
        store.pending_physical_v3_control_admission_for_vehicle(binding.vehicle_id),
        Err(crate::storage::db::StoreError::PhysicalV3SecondHeadUnsupported(vehicle_id))
            if vehicle_id == binding.vehicle_id
    ));
    for pack in &current.manifest.chunks {
        assert!(
            store
                .pending_physical_v3_pack_for_digest(pack.sha256)
                .expect("blocked pack lookup")
                .is_none()
        );
    }
    assert_eq!(
        store
            .runtime_inventory()
            .expect("runtime inventory")
            .referenced_packs,
        4
    );
    let third = public_admission_candidate_for_binding(
        &temporary.path().join("third"),
        &store,
        binding.clone(),
        Uuid::from_u128(0x93939393_9393_4393_8393_939393939393),
        3,
        93,
    );
    let third_paths = third
        .chunks
        .iter()
        .map(|chunk| chunk.path.clone())
        .collect::<Vec<_>>();
    assert!(matches!(
        store.rotate_pending_physical_v3_admission_at(&gate, third, retained_at_ms + 1),
        Err(crate::storage::db::StoreError::PhysicalV3SecondHeadUnsupported(vehicle_id))
            if vehicle_id == binding.vehicle_id
    ));
    assert!(third_paths.iter().all(|path| !path.exists()));
    drop(gate);

    let backup_root = temporary.path().join("backup");
    store.backup_to(&backup_root).expect("rotation backup");
    drop(store);
    let reopened = HubStore::initialize(&root).expect("reopened store");
    assert_eq!(
        reopened
            .pending_physical_v3_admission_for_vehicle(binding.vehicle_id)
            .expect("current after restart")
            .expect("current retained"),
        current
    );
    assert_eq!(
        reopened
            .retained_physical_v3_admission_for_receipt_at(
                binding.vehicle_id,
                &prior.receipt_id,
                retained_at_ms,
                true,
            )
            .expect("prior after restart")
            .expect("prior retained after restart")
            .admission,
        prior
    );
    assert!(first_paths.iter().all(|path| path.is_file()));
    assert!(second_paths.iter().all(|path| path.is_file()));

    let restored = HubStore::initialize(&backup_root).expect("restored rotation backup");
    assert_eq!(
        restored
            .retained_physical_v3_admission_for_receipt_at(
                binding.vehicle_id,
                &prior.receipt_id,
                retained_at_ms,
                true,
            )
            .expect("restored prior lookup")
            .expect("restored prior")
            .admission,
        prior
    );
}

#[test]
fn physical_v3_rotation_faults_are_atomic_and_expired_prior_objects_are_repaired() {
    use crate::durability_fault::{DurabilityFaultPoint, inject};

    let temporary = tempdir().expect("temp dir");
    let root = temporary.path().join("hub");
    let store = HubStore::initialize(&root).expect("store");
    let gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    let binding = registered_admission_binding(&store);
    let first = public_admission_candidate_for_binding(
        &temporary.path().join("first"),
        &store,
        binding.clone(),
        Uuid::from_u128(0xa1a1a1a1_a1a1_41a1_81a1_a1a1a1a1a1a1),
        1,
        101,
    );
    let first_paths = first
        .chunks
        .iter()
        .map(|chunk| chunk.path.clone())
        .collect::<Vec<_>>();
    let prior = store
        .stage_pending_physical_v3_admission(&gate, first)
        .expect("first head");
    let failed = public_admission_candidate_for_binding(
        &temporary.path().join("failed"),
        &store,
        binding.clone(),
        Uuid::from_u128(0xa2a2a2a2_a2a2_42a2_82a2_a2a2a2a2a2a2),
        2,
        102,
    );
    let failed_paths = failed
        .chunks
        .iter()
        .map(|chunk| chunk.path.clone())
        .collect::<Vec<_>>();
    let fault = inject(DurabilityFaultPoint::CatalogueBeforeCommit);
    assert!(matches!(
        store.rotate_pending_physical_v3_admission_at(&gate, failed, 1),
        Err(crate::storage::db::StoreError::CatalogueDurability(_))
    ));
    drop(fault);
    assert!(failed_paths.iter().all(|path| !path.exists()));
    assert_eq!(
        store
            .pending_physical_v3_admission_for_vehicle(binding.vehicle_id)
            .expect("current after failed rotation")
            .expect("prior remains current"),
        prior
    );
    assert!(
        store
            .retained_physical_v3_admission_for_receipt_at(
                binding.vehicle_id,
                &prior.receipt_id,
                1,
                true,
            )
            .expect("no retained row after failed rotation")
            .is_none()
    );

    let committed = public_admission_candidate_for_binding(
        &temporary.path().join("committed"),
        &store,
        binding.clone(),
        Uuid::from_u128(0xa3a3a3a3_a3a3_43a3_83a3_a3a3a3a3a3a3),
        3,
        103,
    );
    let committed_paths = committed
        .chunks
        .iter()
        .map(|chunk| chunk.path.clone())
        .collect::<Vec<_>>();
    let after_fault = inject(DurabilityFaultPoint::CatalogueAfterCommit);
    let current = store
        .rotate_pending_physical_v3_admission_at(&gate, committed, 1)
        .expect("post-commit rotation reconciles");
    drop(after_fault);
    assert_eq!(current.head_sequence, 3);
    assert!(committed_paths.iter().all(|path| path.is_file()));
    drop(gate);

    store.repair().expect("repair expired physical retention");
    assert!(first_paths.iter().all(|path| !path.exists()));
    assert!(committed_paths.iter().all(|path| path.is_file()));
    let connection = store.open().expect("catalogue after repair");
    let retained_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM retained_physical_v3_admissions",
            [],
            |row| row.get(0),
        )
        .expect("retained rows after repair");
    assert_eq!(retained_rows, 0);
    assert!(matches!(
        store.pending_physical_v3_control_admission_for_vehicle(binding.vehicle_id),
        Err(crate::storage::db::StoreError::PhysicalV3SecondHeadUnsupported(vehicle_id))
            if vehicle_id == binding.vehicle_id
    ));
}

#[test]
fn unpublished_physical_candidate_uses_only_the_schema_2_2_profile_ceiling() {
    assert_eq!(ProtocolLimits::default().max_chunks, 512);
    let physical = ProtocolLimits::hub_sync_v1_1_3_schema_2_2();
    assert_eq!(physical.max_chunks, 1_771);
    assert_eq!(physical.max_compressed_pack_bytes, 16 * 1024 * 1024);
}
