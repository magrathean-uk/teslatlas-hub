// SPDX-License-Identifier: AGPL-3.0-only

fn snapshot_v2_2() -> ProjectionSnapshotV2_2 {
    ProjectionSnapshotV2_2 {
        global_settings: vec![ProjectionGlobalSettingsV2_2 {
            id: i64::MIN,
            unit_of_length: ProjectionUnitOfLengthV2_2::Kilometers,
            unit_of_temperature: ProjectionUnitOfTemperatureV2_2::Celsius,
            unit_of_pressure: ProjectionUnitOfPressureV2_2::Bar,
            preferred_range: ProjectionPreferredRangeV2_2::Rated,
            base_url: Some("https://teslamate.example".into()),
            grafana_url: None,
            language: String::new(),
            theme_mode: "system".into(),
            inserted_at_pg_us: i64::MIN,
            updated_at_pg_us: i64::MAX,
        }],
        cars: vec![ProjectionCarV2_2 {
            id: 10,
            eid: 101,
            vid: 201,
            vin: Some("5YJTESTVIN1234567".into()),
            name: Some("Road car".into()),
            model: Some("Model 3".into()),
            // This remains source FLOAT8, deliberately not the legacy
            // normalized Wh-per-km compatibility value.
            efficiency: Some(-0.145),
            trim_badging: Some("74D".into()),
            marketing_name: Some("LR AWD".into()),
            exterior_color: Some("Pearl White".into()),
            wheel_type: Some("Apollo".into()),
            spoiler_type: Some("None".into()),
            display_priority: i16::MIN,
            inserted_at_pg_us: 1_700_000_000_000_000,
            updated_at_pg_us: 1_700_000_100_000_000,
            settings_id: 500,
        }],
        car_settings: vec![ProjectionCarSettingsV2_2 {
            id: 500,
            suspend_min: i32::MIN,
            suspend_after_idle_min: i32::MAX,
            req_not_unlocked: true,
            free_supercharging: false,
            use_streaming_api: true,
            enabled: true,
            lfp_battery: false,
        }],
        addresses: vec![
            ProjectionAddressV2_2 {
                id: 100,
                display_name: Some("Home, London".into()),
                latitude_e6: Some(ProjectionFixedNumericV2_2::Finite(51_500_123)),
                longitude_e6: Some(ProjectionFixedNumericV2_2::Finite(-123_456)),
                name: Some("Home".into()),
                house_number: Some("1".into()),
                road: Some("Strawberry Road".into()),
                neighbourhood: Some("Westminster".into()),
                city: Some("London".into()),
                county: Some("Greater London".into()),
                postcode: Some("SW1A 1AA".into()),
                state: Some("England".into()),
                state_district: Some("London".into()),
                country: Some("United Kingdom".into()),
                inserted_at_pg_us: 1_700_000_000_000_000,
                updated_at_pg_us: 1_700_000_100_000_000,
                osm_id: Some(-42),
                osm_type: Some("node".into()),
            },
            ProjectionAddressV2_2 {
                id: 101,
                display_name: Some("Work, London".into()),
                latitude_e6: Some(ProjectionFixedNumericV2_2::NaN),
                longitude_e6: None,
                name: Some("Work".into()),
                house_number: None,
                road: None,
                neighbourhood: None,
                city: None,
                county: None,
                postcode: None,
                state: None,
                state_district: None,
                country: None,
                inserted_at_pg_us: 1_700_000_200_000_000,
                updated_at_pg_us: 1_700_000_300_000_000,
                osm_id: None,
                osm_type: None,
            },
        ],
        geofences: vec![
            TeslaMateGeofencePhysicalV2_2 {
                id: 200,
                name: "Home".into(),
                latitude_e6: ProjectionFixedNumericV2_2::Finite(0),
                longitude_e6: ProjectionFixedNumericV2_2::NaN,
                radius: i16::MIN,
                billing_type: GeofenceBillingType::PerKwh,
                cost_per_unit_e4: Some(ProjectionFixedNumericV2_2::Finite(3_000)),
                session_fee_e2: Some(ProjectionFixedNumericV2_2::NaN),
                inserted_at_pg_us: 1_700_000_000_000_000,
                updated_at_pg_us: 1_700_000_100_000_000,
            }
            .into(),
            TeslaMateGeofencePhysicalV2_2 {
                id: 201,
                name: "Work".into(),
                latitude_e6: ProjectionFixedNumericV2_2::NaN,
                longitude_e6: ProjectionFixedNumericV2_2::Finite(-110_000),
                radius: i16::MAX,
                billing_type: GeofenceBillingType::PerMinute,
                cost_per_unit_e4: None,
                session_fee_e2: None,
                inserted_at_pg_us: 1_700_000_200_000_000,
                updated_at_pg_us: 1_700_000_300_000_000,
            }
            .into(),
        ],
        drives: vec![ProjectionDriveV2_2 {
            id: 20,
            car_id: 10,
            start_date_pg_us: i64::MAX,
            end_date_pg_us: Some(i64::MIN),
            start_position_id: Some(i32::MIN),
            end_position_id: Some(i32::MAX),
            start_address_id: Some(100),
            end_address_id: Some(101),
            start_geofence_id: Some(200),
            end_geofence_id: Some(201),
            outside_temp_avg_e1: Some(ProjectionFixedNumericV2_2::NaN),
            inside_temp_avg_e1: Some(ProjectionFixedNumericV2_2::Finite(-9_999)),
            speed_max: Some(i16::MIN),
            power_max: Some(i16::MAX),
            power_min: Some(i16::MIN),
            start_ideal_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(999_999)),
            end_ideal_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(-999_999)),
            start_rated_range_km_e2: Some(ProjectionFixedNumericV2_2::NaN),
            end_rated_range_km_e2: None,
            start_km: Some(ProjectionFloat64BitsV2_2((-0.0_f64).to_bits())),
            end_km: Some(ProjectionFloat64BitsV2_2(0x7ff8_0000_0000_0042)),
            distance: Some(ProjectionFloat64BitsV2_2(f64::INFINITY.to_bits())),
            duration_min: Some(i16::MIN),
            ascent: Some(i16::MAX),
            descent: Some(i16::MIN),
        }],
        positions: vec![ProjectionPositionV2_2 {
            id: 30,
            car_id: 10,
            drive_id: Some(20),
            date_pg_us: 1_700_000_030_123_456,
            latitude_e6: ProjectionFixedNumericV2_2::Finite(51_505_000),
            longitude_e6: ProjectionFixedNumericV2_2::Finite(-105_000),
            elevation: Some(i16::MIN),
            speed: Some(i16::MAX),
            power: Some(i16::MIN),
            odometer: Some(ProjectionFloat64BitsV2_2((-0.0_f64).to_bits())),
            ideal_battery_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(999_999)),
            est_battery_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(-999_999)),
            rated_battery_range_km_e2: Some(ProjectionFixedNumericV2_2::NaN),
            battery_level: Some(i16::MIN),
            usable_battery_level: Some(i16::MAX),
            battery_heater: Some(false),
            battery_heater_on: Some(true),
            battery_heater_no_power: None,
            outside_temp_e1: Some(ProjectionFixedNumericV2_2::NaN),
            inside_temp_e1: Some(ProjectionFixedNumericV2_2::Finite(-9_999)),
            fan_status: Some(i32::MIN),
            driver_temp_setting_e1: None,
            passenger_temp_setting_e1: Some(ProjectionFixedNumericV2_2::Finite(9_999)),
            is_climate_on: Some(true),
            is_rear_defroster_on: Some(false),
            is_front_defroster_on: None,
            tpms_pressure_fl_e1: Some(ProjectionFixedNumericV2_2::Finite(-9_999)),
            tpms_pressure_fr_e1: Some(ProjectionFixedNumericV2_2::NaN),
            tpms_pressure_rl_e1: None,
            tpms_pressure_rr_e1: Some(ProjectionFixedNumericV2_2::Finite(9_999)),
        }],
        charging_processes: vec![ProjectionChargingProcessV2_2 {
            id: 40,
            car_id: 10,
            position_id: 30,
            address_id: Some(100),
            geofence_id: Some(200),
            start_date_pg_us: i64::MIN,
            end_date_pg_us: Some(i64::MAX),
            charge_energy_added_e2: Some(ProjectionFixedNumericV2_2::NaN),
            charge_energy_used_e2: None,
            start_ideal_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(999_999)),
            end_ideal_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(-999_999)),
            start_rated_range_km_e2: Some(ProjectionFixedNumericV2_2::NaN),
            end_rated_range_km_e2: None,
            start_battery_level: Some(i16::MIN),
            end_battery_level: Some(i16::MAX),
            duration_min: Some(i16::MIN),
            outside_temp_avg_e1: Some(ProjectionFixedNumericV2_2::Finite(-9_999)),
            cost_e2: Some(ProjectionFixedNumericV2_2::Finite(999_999)),
        }],
        charges: vec![ProjectionChargeV2_2 {
            id: 50,
            charging_process_id: 40,
            date_pg_us: i64::MAX,
            battery_heater: Some(false),
            battery_heater_on: Some(true),
            battery_heater_no_power: None,
            battery_level: Some(i16::MIN),
            usable_battery_level: Some(i16::MAX),
            charge_energy_added_e2: ProjectionFixedNumericV2_2::NaN,
            charger_actual_current: Some(i16::MIN),
            charger_phases: Some(i16::MAX),
            charger_pilot_current: Some(i16::MIN),
            charger_power: i16::MAX,
            charger_voltage: Some(i16::MIN),
            conn_charge_cable: Some("Type 2".into()),
            fast_charger_present: Some(false),
            fast_charger_brand: Some("Tesla".into()),
            fast_charger_type: Some("Supercharger".into()),
            ideal_battery_range_km_e2: ProjectionFixedNumericV2_2::Finite(-999_999),
            rated_battery_range_km_e2: Some(ProjectionFixedNumericV2_2::NaN),
            not_enough_power_to_heat: Some(false),
            outside_temp_e1: Some(ProjectionFixedNumericV2_2::Finite(9_999)),
        }],
        states: vec![ProjectionStateV2_2 {
            id: i32::MIN,
            car_id: 10,
            state: ProjectionStateStatusV2_2::Online,
            start_date_pg_us: i64::MIN,
            end_date_pg_us: None,
        }],
        updates: vec![ProjectionUpdateV2_2 {
            id: i32::MAX,
            car_id: 10,
            start_date_pg_us: i64::MAX,
            end_date_pg_us: Some(i64::MIN),
            version: Some("2026.3".into()),
        }],
    }
}

fn request_v2_2<'a>(snapshot: &'a ProjectionSnapshotV2_2) -> ProjectionPackRequestV2_2<'a> {
    ProjectionPackRequestV2_2 {
        pack_id: Uuid::parse_str("45454545-4545-4545-8454-454545454545").unwrap(),
        snapshot_id: Uuid::parse_str("56565656-5656-4565-8565-565656565656").unwrap(),
        ordinal: 0,
        binding: binding(),
        sequence: SequenceRange {
            from_exclusive: 7,
            to_inclusive: 7,
        },
        snapshot,
    }
}

#[test]
fn schema_2_2_transport_table_vocabulary_maps_physical_charging_parent_and_child() {
    let mut process_only = snapshot_v2_2();
    process_only.drives.clear();
    process_only.positions.clear();
    process_only.charges.clear();
    process_only.states.clear();
    process_only.updates.clear();
    assert_eq!(
        tables_for_snapshot_v2_2(&process_only),
        vec![MirrorTable::Car, MirrorTable::Charge],
        "physical charging_processes map to the protocol parent vocabulary"
    );

    let mut charge_only = snapshot_v2_2();
    charge_only.drives.clear();
    charge_only.positions.clear();
    charge_only.charging_processes.clear();
    charge_only.states.clear();
    charge_only.updates.clear();
    assert_eq!(
        tables_for_snapshot_v2_2(&charge_only),
        vec![MirrorTable::Car, MirrorTable::ChargeSample],
        "physical charges map to the protocol child vocabulary independently"
    );

    let full = snapshot_v2_2();
    assert_eq!(
        tables_for_snapshot_v2_2(&full),
        vec![
            MirrorTable::Car,
            MirrorTable::Drive,
            MirrorTable::Charge,
            MirrorTable::Position,
            MirrorTable::ChargeSample,
            MirrorTable::State,
            MirrorTable::Update,
        ]
    );
}

/// Mirrors the released client verifier's `PRAGMA table_list` and
/// `table_xinfo` contract. This deliberately checks more than table
/// names: a schema-2.0 pack must not inherit any 2.1 columns or types.
fn assert_schema_2_0_client_layout(connection: &Connection) {
    let mut table_statement = connection.prepare("PRAGMA table_list").unwrap();
    let table_flags = table_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let expected: &[(&str, bool, &[(&str, &str, bool, bool)])] = &[
        (
            "hub_pack_metadata",
            false,
            &[("key", "TEXT", true, true), ("value", "TEXT", true, false)],
        ),
        (
            "cars",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("name", "TEXT", true, false),
                ("model", "TEXT", true, false),
                ("vin", "TEXT", false, false),
                ("firmware_version", "TEXT", false, false),
                ("efficiency_wh_per_km", "REAL", false, false),
            ],
        ),
        (
            "drives",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("car_id", "INTEGER", true, false),
                ("optimized_at_ms", "INTEGER", false, false),
                ("start_date_ms", "INTEGER", true, false),
                ("end_date_ms", "INTEGER", true, false),
                ("distance_km", "REAL", false, false),
                ("duration_min", "INTEGER", false, false),
                ("efficiency", "REAL", false, false),
                ("outside_temp_avg", "REAL", false, false),
                ("speed_max", "INTEGER", false, false),
                ("start_address", "TEXT", false, false),
                ("end_address", "TEXT", false, false),
                ("start_geofence", "TEXT", false, false),
                ("end_geofence", "TEXT", false, false),
                ("start_latitude", "REAL", false, false),
                ("start_longitude", "REAL", false, false),
                ("end_latitude", "REAL", false, false),
                ("end_longitude", "REAL", false, false),
                ("start_soc", "INTEGER", false, false),
                ("end_soc", "INTEGER", false, false),
                ("start_rated_range_km", "REAL", false, false),
                ("end_rated_range_km", "REAL", false, false),
            ],
        ),
        (
            "charges",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("car_id", "INTEGER", true, false),
                ("start_date_ms", "INTEGER", true, false),
                ("end_date_ms", "INTEGER", false, false),
                ("charge_energy_added", "REAL", false, false),
                ("start_battery_level", "INTEGER", false, false),
                ("end_battery_level", "INTEGER", false, false),
                ("duration_min", "INTEGER", false, false),
                ("address", "TEXT", false, false),
                ("location_name", "TEXT", false, false),
                ("geofence", "TEXT", false, false),
                ("is_dc", "INTEGER", false, false),
                ("charge_rate_km_per_hour", "REAL", false, false),
                ("max_charger_power_kw", "REAL", false, false),
                ("outside_temp_avg", "REAL", false, false),
                ("start_rated_range_km", "REAL", false, false),
                ("end_rated_range_km", "REAL", false, false),
            ],
        ),
        (
            "positions",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("drive_id", "INTEGER", true, false),
                ("car_id", "INTEGER", true, false),
                ("date_ms", "INTEGER", true, false),
                ("latitude", "REAL", true, false),
                ("longitude", "REAL", true, false),
                ("speed", "INTEGER", false, false),
                ("power", "INTEGER", false, false),
                ("battery_level", "INTEGER", false, false),
                ("usable_battery_level", "INTEGER", false, false),
                ("elevation", "INTEGER", false, false),
                ("odometer", "REAL", false, false),
                ("ideal_battery_range_km", "REAL", false, false),
                ("rated_battery_range_km", "REAL", false, false),
                ("is_climate_on", "INTEGER", false, false),
                ("inside_temp", "REAL", false, false),
                ("outside_temp", "REAL", false, false),
            ],
        ),
        (
            "charge_samples",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("charge_process_id", "INTEGER", true, false),
                ("timestamp_ms", "INTEGER", true, false),
                ("battery_level", "INTEGER", false, false),
                ("usable_battery_level", "INTEGER", false, false),
                ("charge_energy_added_kwh", "REAL", false, false),
                ("charger_power_kw", "REAL", false, false),
                ("charger_voltage", "REAL", false, false),
                ("charger_actual_current", "REAL", false, false),
                ("charger_pilot_current", "REAL", false, false),
                ("charger_phases", "INTEGER", false, false),
                ("ideal_range_km", "REAL", false, false),
                ("rated_range_km", "REAL", false, false),
                ("outside_temp_c", "REAL", false, false),
                ("battery_heater_on", "INTEGER", false, false),
                ("battery_heater", "INTEGER", false, false),
                ("battery_heater_no_power", "INTEGER", false, false),
                ("not_enough_power_to_heat", "INTEGER", false, false),
                ("fast_charger_present", "INTEGER", false, false),
                ("fast_charger_brand", "TEXT", false, false),
                ("fast_charger_type", "TEXT", false, false),
                ("charge_cable", "TEXT", false, false),
            ],
        ),
    ];
    for (table, without_rowid, expected_columns) in expected {
        let (_, actual_without_rowid, strict) = table_flags
            .iter()
            .find(|(name, _, _)| name == table)
            .unwrap_or_else(|| panic!("missing {table} table"));
        assert_eq!(*actual_without_rowid, i64::from(*without_rowid), "{table}");
        assert_eq!(*strict, 1, "{table}");

        let mut column_statement = connection
            .prepare(&format!("PRAGMA table_xinfo('{table}')"))
            .unwrap();
        let actual_columns = column_statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let expected_columns = expected_columns
            .iter()
            .map(|(name, declared_type, not_null, primary_key)| {
                (
                    (*name).to_owned(),
                    (*declared_type).to_owned(),
                    i64::from(*not_null),
                    i64::from(*primary_key),
                    0,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(actual_columns, expected_columns, "{table}");
    }
}

#[test]
fn writes_a_checked_typed_projection_pack() {
    use std::os::unix::fs::PermissionsExt;

    let temporary = crate::private_tempdir().unwrap();
    let source = snapshot();
    let built = ProjectionPackWriter::new(temporary.path().join("packs"))
        .write_full_snapshot(&request(&source))
        .unwrap();
    assert_eq!(built.metadata.schema, HUB_PROJECTION_SCHEMA_V1);
    assert_eq!(built.metadata.format, PackFormat::HubProjectionSqlite);
    assert_eq!(built.metadata.row_count, 5);
    assert_eq!(
        fs::metadata(&built.path).unwrap().permissions().mode() & 0o777,
        SHARED_IMMUTABLE_PACK_MODE,
        "a collector-created immutable pack remains readable by the API data group"
    );
    assert!(
        fs::read_dir(temporary.path().join("packs/.staging"))
            .unwrap()
            .next()
            .is_none(),
        "completed publication removes its now-group-readable staging alias"
    );
    built
        .metadata
        .verify_reader(File::open(&built.path).unwrap(), ProtocolLimits::default())
        .unwrap();

    let sqlite = zstd::stream::decode_all(File::open(&built.path).unwrap()).unwrap();
    let inspect = temporary.path().join("inspect.sqlite");
    fs::write(&inspect, sqlite).unwrap();
    let connection = Connection::open(inspect).unwrap();
    let application_id: u32 = connection
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .unwrap();
    assert_eq!(application_id, SQLITE_HUB_PROJECTION_APPLICATION_ID);
    for table in ["cars", "drives", "positions", "charges", "charge_samples"] {
        let rows: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 1, "{table}");
    }
    let selected_car_id: String = connection
        .query_row(
            "SELECT value FROM hub_pack_metadata WHERE key = 'selected_car_id'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(selected_car_id, "10");
}

#[test]
fn compression_uses_bounded_parallel_workers_and_is_deterministic() {
    assert_eq!(compression_worker_count_for(0), 1);
    assert_eq!(compression_worker_count_for(1), 1);
    assert_eq!(compression_worker_count_for(2), 2);
    assert_eq!(
        compression_worker_count_for(64),
        MAX_COMPRESSION_WORKERS as u32
    );

    let temporary = crate::private_tempdir().unwrap();
    let source = temporary.path().join("source.bin");
    let first = temporary.path().join("first.zst");
    let second = temporary.path().join("second.zst");
    let mut input = Vec::with_capacity(2 * 1024 * 1024);
    for index in 0..(2 * 1024 * 1024) {
        input.push((index as u8).wrapping_mul(31));
    }
    fs::write(&source, input).unwrap();
    File::create(&first).unwrap();
    File::create(&second).unwrap();

    let first_digest = compress_file_with_workers(&source, &first, 2).unwrap();
    let second_digest = compress_file_with_workers(&source, &second, 2).unwrap();
    assert_eq!(first_digest, second_digest);
    assert_eq!(fs::read(first).unwrap(), fs::read(second).unwrap());
}

#[test]
fn signs_and_catalogues_a_typed_snapshot() {
    let temporary = crate::private_tempdir().unwrap();
    let source = snapshot();
    let request = request(&source);
    let built = ProjectionPackWriter::new(temporary.path().join("packs"))
        .write_full_snapshot(&request)
        .unwrap();
    let key = CursorKey::from_bytes([9; 32]);
    let manifest = request.signed_manifest(&built, &key).unwrap();
    manifest.validate_terminal_cursor(&key).unwrap();

    fs::set_permissions(
        temporary.path().join("packs"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let store = crate::db::HubStore::initialize(temporary.path()).unwrap();
    store.publish_manifest(&manifest).unwrap();
    assert_eq!(
        store
            .manifest_for_vehicle(request.binding.vehicle_id)
            .unwrap()
            .unwrap(),
        manifest
    );
    assert_eq!(
        store
            .pack_for_digest(built.metadata.sha256)
            .unwrap()
            .unwrap()
            .path,
        built.path
    );
}

#[test]
fn signs_several_parent_complete_snapshot_chunks() {
    let temporary = crate::private_tempdir().unwrap();
    let source = snapshot();
    let first_request = request(&source);
    let first = ProjectionPackWriter::new(temporary.path().join("packs"))
        .write_full_snapshot(&first_request)
        .unwrap();

    let mut second_snapshot = snapshot();
    second_snapshot.positions.clear();
    second_snapshot.charge_samples.clear();
    let mut second_request = request(&second_snapshot);
    second_request.pack_id = Uuid::new_v4();
    second_request.ordinal = 1;
    let second = ProjectionPackWriter::new(temporary.path().join("packs"))
        .write_full_snapshot(&second_request)
        .unwrap();

    let key = CursorKey::from_bytes([3; 32]);
    let transport_rows = first
        .metadata
        .row_count
        .checked_add(second.metadata.row_count)
        .unwrap();
    let manifest = signed_full_snapshot_manifest(
        &first_request.binding,
        first_request.snapshot_id,
        first_request.sequence,
        &[first.clone(), second.clone()],
        transport_rows,
        &key,
    )
    .unwrap();
    assert_eq!(manifest.chunk_count, 2);
    assert_eq!(manifest.chunks[0].ordinal, 0);
    assert_eq!(manifest.chunks[1].ordinal, 1);
    assert_eq!(manifest.total_rows, transport_rows);
    manifest.validate_terminal_cursor(&key).unwrap();

    assert!(matches!(
        signed_full_snapshot_manifest(
            &first_request.binding,
            first_request.snapshot_id,
            first_request.sequence,
            &[first, second],
            first_request.snapshot.row_count().unwrap(),
            &key,
        ),
        Err(ProjectionPackError::Invalid(_))
    ));
}

#[test]
fn rejects_a_position_without_its_drive_or_valid_coordinates() {
    let mut source = snapshot();
    source.positions[0].drive_id = Some(999);
    let temporary = crate::private_tempdir().unwrap();
    assert!(matches!(
        ProjectionPackWriter::new(temporary.path().join("packs"))
            .write_full_snapshot(&request(&source)),
        Err(ProjectionPackError::Invalid(_))
    ));

    let mut source = snapshot();
    source.positions[0].latitude = 0.0;
    source.positions[0].longitude = 0.0;
    assert!(matches!(
        ProjectionPackWriter::new(temporary.path().join("packs"))
            .write_full_snapshot(&request(&source)),
        Err(ProjectionPackError::Invalid(_))
    ));
}

#[test]
fn schema_2_0_rejects_positions_outside_the_legacy_layout() {
    let temporary = crate::private_tempdir().unwrap();
    let mut standalone = snapshot();
    standalone.positions[0].drive_id = None;
    assert!(matches!(
        ProjectionPackWriter::new(temporary.path().join("standalone"))
            .write_full_snapshot(&request(&standalone)),
        Err(ProjectionPackError::Invalid(message))
            if message == "schema 2.0 position.drive_id must be present"
    ));

    let mut fractional_power = snapshot();
    fractional_power.positions[0].power = Some(3.5);
    assert!(matches!(
        ProjectionPackWriter::new(temporary.path().join("fractional"))
            .write_full_snapshot(&request(&fractional_power)),
        Err(ProjectionPackError::Invalid(message))
            if message == "schema 2.0 position.power must be an integer"
    ));
}

#[test]
fn schema_2_1_state_pack_preserves_ordered_rows_and_open_end_date() {
    let temporary = crate::private_tempdir().unwrap();
    let source = snapshot();
    let states = vec![
        ProjectionState {
            id: 12,
            car_id: 10,
            state: "asleep".into(),
            start_date_ms: 1_700_000_200_000,
            end_date_ms: Some(1_700_000_300_000),
        },
        ProjectionState {
            id: 11,
            car_id: 10,
            state: "online".into(),
            start_date_ms: 1_700_000_100_000,
            end_date_ms: None,
        },
    ];
    let updates = vec![ProjectionUpdate {
        id: 21,
        car_id: 10,
        start_date_ms: 1_700_000_400_000,
        end_date_ms: 1_700_000_500_000,
        version: "2026.2".into(),
    }];
    let built = ProjectionPackWriter::new(temporary.path().join("packs"))
        .write_full_snapshot_with_states_and_updates(&request(&source), &states, &updates)
        .unwrap();
    assert_eq!(built.metadata.schema, HUB_PROJECTION_SCHEMA_V2);

    let sqlite = zstd::stream::decode_all(File::open(&built.path).unwrap()).unwrap();
    let inspect = temporary.path().join("inspect.sqlite");
    fs::write(&inspect, sqlite).unwrap();
    let connection = Connection::open(inspect).unwrap();
    let setting_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM car_settings", [], |row| row.get(0))
        .unwrap();
    assert_eq!(setting_count, 1);
    let rows: Vec<(i64, String, Option<i64>)> = connection
        .prepare("SELECT id, state, end_date_ms FROM states ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(
        rows,
        vec![
            (11, "online".into(), None),
            (12, "asleep".into(), Some(1_700_000_300_000)),
        ]
    );
    let update: (i64, String) = connection
        .query_row("SELECT id, version FROM updates", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(update, (21, "2026.2".into()));
}

#[test]
fn schema_2_2_full_snapshot_has_exact_physical_rows_and_full_only_metadata() {
    let temporary = crate::private_tempdir().unwrap();
    let source = snapshot_v2_2();
    let request = request_v2_2(&source);
    let built = ProjectionPackWriter::new(temporary.path().join("packs"))
        .write_full_snapshot_2_2(&request)
        .expect("locally validate schema 2.2 full pack");
    assert_eq!(built.metadata.schema, HUB_PROJECTION_SCHEMA_V3);
    assert_eq!(built.metadata.row_count, 13);
    built
        .metadata
        .verify_reader(File::open(&built.path).unwrap(), ProtocolLimits::default())
        .expect("schema 2.2 SQLite header matches the protocol identity");

    let sqlite = zstd::stream::decode_all(File::open(&built.path).unwrap()).unwrap();
    let inspect = temporary.path().join("inspect-2-2.sqlite");
    fs::write(&inspect, sqlite).unwrap();
    let connection = Connection::open(inspect).unwrap();
    let application_id: u32 = connection
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .unwrap();
    let user_version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(application_id, SQLITE_HUB_PROJECTION_APPLICATION_ID);
    assert_eq!(user_version, HUB_PROJECTION_SCHEMA_V3.sqlite_user_version());
    let cars: Vec<(
        i64,
        i64,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<Vec<u8>>,
        i64,
        i64,
        i64,
    )> = connection
        .prepare(
            "SELECT id, eid, vid, vin, name, model, efficiency, display_priority,
                    inserted_at_pg_us, settings_id
             FROM cars ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        cars,
        vec![(
            10,
            101,
            201,
            Some("5YJTESTVIN1234567".into()),
            Some("Road car".into()),
            Some("Model 3".into()),
            Some((-0.145_f64).to_bits().to_be_bytes().to_vec()),
            i64::from(i16::MIN),
            1_700_000_000_000_000,
            500,
        ),]
    );
    let car_settings: (i64, i64, i64, i64, i64, i64, i64, i64) = connection
        .query_row(
            "SELECT id, suspend_min, suspend_after_idle_min, req_not_unlocked,
                    free_supercharging, use_streaming_api, enabled, lfp_battery
             FROM car_settings WHERE id = 500",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        car_settings,
        (500, i64::from(i32::MIN), i64::from(i32::MAX), 1, 0, 1, 1, 0)
    );
    let global_settings: (
        i64,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        i64,
        i64,
    ) = connection
        .query_row(
            "SELECT id, unit_of_length, unit_of_temperature, unit_of_pressure,
                    preferred_range, base_url, grafana_url, language, theme_mode,
                    inserted_at_pg_us, updated_at_pg_us
             FROM global_settings",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        global_settings,
        (
            i64::MIN,
            "km".into(),
            "C".into(),
            "bar".into(),
            "rated".into(),
            Some("https://teslamate.example".into()),
            None,
            String::new(),
            "system".into(),
            i64::MIN,
            i64::MAX,
        )
    );
    let state: (i64, i64, String, i64, Option<i64>) = connection
        .query_row(
            "SELECT id, car_id, state, start_date_pg_us, end_date_pg_us FROM states",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        state,
        (i64::from(i32::MIN), 10, "online".into(), i64::MIN, None)
    );
    let update: (i64, i64, i64, Option<i64>, Option<String>) = connection
        .query_row(
            "SELECT id, car_id, start_date_pg_us, end_date_pg_us, version FROM updates",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        update,
        (
            i64::from(i32::MAX),
            10,
            i64::MAX,
            Some(i64::MIN),
            Some("2026.3".into()),
        )
    );
    connection
        .execute_batch("PRAGMA foreign_keys = ON")
        .unwrap();
    for statement in [
        "UPDATE cars SET id = 32768 WHERE id = 10",
        "UPDATE cars SET display_priority = -32769 WHERE id = 10",
        "UPDATE cars SET inserted_at_pg_us = 1 WHERE id = 10",
        "UPDATE cars SET updated_at_pg_us = -9223372036854775807 WHERE id = 10",
        "UPDATE cars SET model = replace(hex(zeroblob(256)), '00', 'x') WHERE id = 10",
        "UPDATE cars SET efficiency = x'00000000000000' WHERE id = 10",
        "UPDATE cars SET settings_id = 999 WHERE id = 10",
        "UPDATE car_settings SET suspend_min = 2147483648 WHERE id = 500",
        "UPDATE global_settings SET unit_of_length = 'kilometres'",
        "UPDATE global_settings SET unit_of_temperature = 'K'",
        "UPDATE global_settings SET unit_of_pressure = 'kpa'",
        "UPDATE global_settings SET preferred_range = 'full'",
        "UPDATE global_settings SET base_url = replace(hex(zeroblob(256)), '00', 'x')",
        "UPDATE global_settings SET language = replace(hex(zeroblob(16385)), '00', 'x')",
        "UPDATE global_settings SET inserted_at_pg_us = 1",
        "UPDATE global_settings SET updated_at_pg_us = -9223372036854775807",
        "UPDATE states SET id = 2147483648 WHERE id = -2147483648",
        "UPDATE states SET car_id = 32768 WHERE id = -2147483648",
        "UPDATE states SET state = 'driving' WHERE id = -2147483648",
        "UPDATE states SET start_date_pg_us = -9223372036854775807 WHERE id = -2147483648",
        "UPDATE states SET start_date_pg_us = -211813488000000001 WHERE id = -2147483648",
        "UPDATE states SET start_date_pg_us = 9223371331200000000 WHERE id = -2147483648",
        "UPDATE updates SET id = -2147483649 WHERE id = 2147483647",
        "UPDATE updates SET car_id = -32769 WHERE id = 2147483647",
        "UPDATE updates SET end_date_pg_us = 9223372036854775806 WHERE id = 2147483647",
        "UPDATE updates SET version = replace(hex(zeroblob(256)), '00', 'x') WHERE id = 2147483647",
    ] {
        assert!(connection.execute(statement, []).is_err(), "{statement}");
    }
    connection
        .execute(
            "UPDATE states SET start_date_pg_us = -211813488000000000, end_date_pg_us = 9223371331199999999 WHERE id = -2147483648",
            [],
        )
        .expect("finite PostgreSQL timestamp bounds are physical source values");
    connection
        .execute(
            "UPDATE updates SET start_date_pg_us = (-9223372036854775807 - 1), end_date_pg_us = 9223372036854775807 WHERE id = 2147483647",
            [],
        )
        .expect("PostgreSQL infinity timestamp sentinels are physical source values");
    connection
        .execute(
            "UPDATE updates SET end_date_pg_us = NULL, version = '' WHERE id = 2147483647",
            [],
        )
        .expect("nullable update end/version and an empty varchar are physical source values");
    let address_columns: Vec<String> = connection
        .prepare("PRAGMA table_xinfo('addresses')")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        address_columns,
        vec![
            "id",
            "display_name",
            "latitude_e6",
            "latitude_e6_is_nan",
            "longitude_e6",
            "longitude_e6_is_nan",
            "name",
            "house_number",
            "road",
            "neighbourhood",
            "city",
            "county",
            "postcode",
            "state",
            "state_district",
            "country",
            "inserted_at_pg_us",
            "updated_at_pg_us",
            "osm_id",
            "osm_type",
        ]
    );
    assert!(
        !address_columns.contains(&"raw".to_owned()),
        "the sensitive source payload has no schema/output representation"
    );
    let metadata: Vec<(String, String)> = connection
        .prepare(
            "SELECT key, value FROM hub_pack_metadata
             WHERE key IN (
               'ledger_state', 'ledger_slice', 'mapped_fields', 'unreconciled_fields',
               'source_revision', 'migration_set_sha256', 'car_settings_slice_sha256',
               'settings_slice_sha256',
               'cars_efficiency_encoding', 'cars_slice_sha256', 'address_slice_sha256',
               'drives_float_encoding', 'drives_slice_sha256', 'fixed_numeric_encoding',
               'geofence_slice_sha256', 'positions_odometer_encoding',
               'positions_relation_scope', 'positions_slice_sha256',
               'charging_boolean_encoding', 'charges_relation_scope',
               'charging_processes_slice_sha256', 'charges_slice_sha256',
               'postgres_timestamp_encoding',
               'postgres_timestamp_0_encoding',
               'states_slice_sha256', 'updates_slice_sha256',
               'reconciliation', 'schema_support', 'publication_scope'
             )
             ORDER BY key",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut expected_metadata = vec![
            ("address_slice_sha256".into(), thp2_2_address_slice_sha256()),
            (
                "car_settings_slice_sha256".into(),
                thp2_2_car_settings_slice_sha256()
            ),
            (
                "settings_slice_sha256".into(),
                thp2_2_global_settings_slice_sha256()
            ),
            (
                "cars_efficiency_encoding".into(),
                THP2_2_CARS_EFFICIENCY_ENCODING.into()
            ),
            ("cars_slice_sha256".into(), thp2_2_cars_slice_sha256()),
            (
                "charging_boolean_encoding".into(),
                THP2_2_CHARGING_TRI_STATE_BOOL_ENCODING.into()
            ),
            (
                "charges_relation_scope".into(),
                THP2_2_CHARGES_RELATION_SCOPE.into()
            ),
            (
                "charging_processes_slice_sha256".into(),
                thp2_2_charging_processes_slice_sha256()
            ),
            ("charges_slice_sha256".into(), thp2_2_charges_slice_sha256()),
            (
                "drives_float_encoding".into(),
                THP2_2_DRIVES_FLOAT_ENCODING.into()
            ),
            ("drives_slice_sha256".into(), thp2_2_drives_slice_sha256()),
            (
                "fixed_numeric_encoding".into(),
                THP2_2_FIXED_NUMERIC_ENCODING.into()
            ),
            (
                "geofence_slice_sha256".into(),
                thp2_2_geofence_slice_sha256()
            ),
            (
                "ledger_slice".into(),
                "settings+car_settings+cars+drives+positions+charging_processes+charges+addresses+geofences+states+updates".into()
            ),
            ("ledger_state".into(), "draft_blocked".into()),
            ("mapped_fields".into(), "168".into()),
            (
                "migration_set_sha256".into(),
                TESLAMATE_V4_MIGRATION_SET_SHA256.into(),
            ),
            (
                "positions_odometer_encoding".into(),
                THP2_2_POSITIONS_ODOMETER_ENCODING.into()
            ),
            (
                "positions_relation_scope".into(),
                THP2_2_POSITIONS_RELATION_SCOPE.into()
            ),
            (
                "positions_slice_sha256".into(),
                thp2_2_positions_slice_sha256()
            ),
            (
                "postgres_timestamp_0_encoding".into(),
                THP2_2_POSTGRES_TIMESTAMP_0_ENCODING.into()
            ),
            (
                "postgres_timestamp_encoding".into(),
                THP2_2_POSTGRES_TIMESTAMP_ENCODING.into()
            ),
            ("publication_scope".into(), "local_validation_only".into()),
            ("reconciliation".into(), "not_run".into()),
            ("schema_support".into(), "full_snapshot_only".into()),
            (
                "source_revision".into(),
                TESLAMATE_V4_SOURCE_REVISION.into()
            ),
            ("states_slice_sha256".into(), thp2_2_states_slice_sha256()),
            ("unreconciled_fields".into(), "1".into()),
            ("updates_slice_sha256".into(), thp2_2_updates_slice_sha256()),
        ];
    expected_metadata.sort_unstable();
    assert_eq!(metadata, expected_metadata);
    #[derive(Debug, PartialEq)]
    struct DriveRow {
        id: i64,
        car_id: i64,
        start_date_pg_us: i64,
        end_date_pg_us: Option<i64>,
        start_position_id: Option<i64>,
        end_position_id: Option<i64>,
        start_address_id: Option<i64>,
        end_address_id: Option<i64>,
        start_geofence_id: Option<i64>,
        end_geofence_id: Option<i64>,
        outside_temp_avg_e1: Option<i64>,
        outside_temp_avg_e1_is_nan: i64,
        inside_temp_avg_e1: Option<i64>,
        inside_temp_avg_e1_is_nan: i64,
        speed_max: Option<i64>,
        power_max: Option<i64>,
        power_min: Option<i64>,
        start_ideal_range_km_e2: Option<i64>,
        start_ideal_range_km_e2_is_nan: i64,
        end_ideal_range_km_e2: Option<i64>,
        end_ideal_range_km_e2_is_nan: i64,
        start_rated_range_km_e2: Option<i64>,
        start_rated_range_km_e2_is_nan: i64,
        end_rated_range_km_e2: Option<i64>,
        end_rated_range_km_e2_is_nan: i64,
        start_km_f64_be: Option<Vec<u8>>,
        end_km_f64_be: Option<Vec<u8>>,
        distance_f64_be: Option<Vec<u8>>,
        duration_min: Option<i64>,
        ascent: Option<i64>,
        descent: Option<i64>,
    }
    let drive = connection
        .query_row(
            "SELECT id, car_id, start_date_pg_us, end_date_pg_us, start_position_id,
                    end_position_id, start_address_id, end_address_id, start_geofence_id,
                    end_geofence_id, outside_temp_avg_e1, outside_temp_avg_e1_is_nan,
                    inside_temp_avg_e1, inside_temp_avg_e1_is_nan, speed_max, power_max,
                    power_min, start_ideal_range_km_e2, start_ideal_range_km_e2_is_nan,
                    end_ideal_range_km_e2, end_ideal_range_km_e2_is_nan,
                    start_rated_range_km_e2, start_rated_range_km_e2_is_nan,
                    end_rated_range_km_e2, end_rated_range_km_e2_is_nan, start_km_f64_be,
                    end_km_f64_be, distance_f64_be, duration_min, ascent, descent
             FROM drives WHERE id = 20",
            [],
            |row| {
                Ok(DriveRow {
                    id: row.get(0)?,
                    car_id: row.get(1)?,
                    start_date_pg_us: row.get(2)?,
                    end_date_pg_us: row.get(3)?,
                    start_position_id: row.get(4)?,
                    end_position_id: row.get(5)?,
                    start_address_id: row.get(6)?,
                    end_address_id: row.get(7)?,
                    start_geofence_id: row.get(8)?,
                    end_geofence_id: row.get(9)?,
                    outside_temp_avg_e1: row.get(10)?,
                    outside_temp_avg_e1_is_nan: row.get(11)?,
                    inside_temp_avg_e1: row.get(12)?,
                    inside_temp_avg_e1_is_nan: row.get(13)?,
                    speed_max: row.get(14)?,
                    power_max: row.get(15)?,
                    power_min: row.get(16)?,
                    start_ideal_range_km_e2: row.get(17)?,
                    start_ideal_range_km_e2_is_nan: row.get(18)?,
                    end_ideal_range_km_e2: row.get(19)?,
                    end_ideal_range_km_e2_is_nan: row.get(20)?,
                    start_rated_range_km_e2: row.get(21)?,
                    start_rated_range_km_e2_is_nan: row.get(22)?,
                    end_rated_range_km_e2: row.get(23)?,
                    end_rated_range_km_e2_is_nan: row.get(24)?,
                    start_km_f64_be: row.get(25)?,
                    end_km_f64_be: row.get(26)?,
                    distance_f64_be: row.get(27)?,
                    duration_min: row.get(28)?,
                    ascent: row.get(29)?,
                    descent: row.get(30)?,
                })
            },
        )
        .unwrap();
    assert_eq!(
        drive,
        DriveRow {
            id: 20,
            car_id: 10,
            start_date_pg_us: i64::MAX,
            end_date_pg_us: Some(i64::MIN),
            start_position_id: Some(i64::from(i32::MIN)),
            end_position_id: Some(i64::from(i32::MAX)),
            start_address_id: Some(100),
            end_address_id: Some(101),
            start_geofence_id: Some(200),
            end_geofence_id: Some(201),
            outside_temp_avg_e1: None,
            outside_temp_avg_e1_is_nan: 1,
            inside_temp_avg_e1: Some(-9_999),
            inside_temp_avg_e1_is_nan: 0,
            speed_max: Some(i64::from(i16::MIN)),
            power_max: Some(i64::from(i16::MAX)),
            power_min: Some(i64::from(i16::MIN)),
            start_ideal_range_km_e2: Some(999_999),
            start_ideal_range_km_e2_is_nan: 0,
            end_ideal_range_km_e2: Some(-999_999),
            end_ideal_range_km_e2_is_nan: 0,
            start_rated_range_km_e2: None,
            start_rated_range_km_e2_is_nan: 1,
            end_rated_range_km_e2: None,
            end_rated_range_km_e2_is_nan: 0,
            start_km_f64_be: Some((-0.0_f64).to_bits().to_be_bytes().to_vec()),
            end_km_f64_be: Some(0x7ff8_0000_0000_0042_u64.to_be_bytes().to_vec()),
            distance_f64_be: Some(f64::INFINITY.to_bits().to_be_bytes().to_vec()),
            duration_min: Some(i64::from(i16::MIN)),
            ascent: Some(i64::from(i16::MAX)),
            descent: Some(i64::from(i16::MIN)),
        }
    );
    let quoted_row = |query: &str| {
        connection
            .query_row(query, [], |row| {
                (0..row.as_ref().column_count())
                    .map(|index| row.get(index))
                    .collect::<rusqlite::Result<Vec<String>>>()
            })
            .unwrap()
    };
    assert_eq!(
        quoted_row(
            "SELECT quote(id), quote(car_id), quote(position_id), quote(address_id),
                    quote(geofence_id), quote(start_date_pg_us), quote(end_date_pg_us),
                    quote(charge_energy_added_e2), quote(charge_energy_added_e2_is_nan),
                    quote(charge_energy_used_e2), quote(charge_energy_used_e2_is_nan),
                    quote(start_ideal_range_km_e2), quote(start_ideal_range_km_e2_is_nan),
                    quote(end_ideal_range_km_e2), quote(end_ideal_range_km_e2_is_nan),
                    quote(start_rated_range_km_e2), quote(start_rated_range_km_e2_is_nan),
                    quote(end_rated_range_km_e2), quote(end_rated_range_km_e2_is_nan),
                    quote(start_battery_level), quote(end_battery_level), quote(duration_min),
                    quote(outside_temp_avg_e1), quote(outside_temp_avg_e1_is_nan),
                    quote(cost_e2), quote(cost_e2_is_nan)
             FROM charging_processes WHERE id = 40",
        ),
        vec![
            "40",
            "10",
            "30",
            "100",
            "200",
            "-9223372036854775808",
            "9223372036854775807",
            "NULL",
            "1",
            "NULL",
            "0",
            "999999",
            "0",
            "-999999",
            "0",
            "NULL",
            "1",
            "NULL",
            "0",
            "-32768",
            "32767",
            "-32768",
            "-9999",
            "0",
            "999999",
            "0",
        ]
    );
    assert_eq!(
        quoted_row(
            "SELECT quote(id), quote(charging_process_id), quote(date_pg_us),
                    quote(battery_heater), quote(battery_heater_on),
                    quote(battery_heater_no_power), quote(battery_level),
                    quote(usable_battery_level), quote(charge_energy_added_e2),
                    quote(charge_energy_added_e2_is_nan), quote(charger_actual_current),
                    quote(charger_phases), quote(charger_pilot_current), quote(charger_power),
                    quote(charger_voltage), quote(conn_charge_cable),
                    quote(fast_charger_present), quote(fast_charger_brand),
                    quote(fast_charger_type), quote(ideal_battery_range_km_e2),
                    quote(ideal_battery_range_km_e2_is_nan), quote(rated_battery_range_km_e2),
                    quote(rated_battery_range_km_e2_is_nan), quote(not_enough_power_to_heat),
                    quote(outside_temp_e1), quote(outside_temp_e1_is_nan)
             FROM charges WHERE id = 50",
        ),
        vec![
            "50",
            "40",
            "9223372036854775807",
            "0",
            "1",
            "NULL",
            "-32768",
            "32767",
            "NULL",
            "1",
            "-32768",
            "32767",
            "-32768",
            "32767",
            "-32768",
            "'Type 2'",
            "0",
            "'Tesla'",
            "'Supercharger'",
            "-999999",
            "0",
            "NULL",
            "1",
            "0",
            "9999",
            "0",
        ]
    );
    for statement in [
        "UPDATE drives SET id = 2147483648 WHERE id = 20",
        "UPDATE drives SET car_id = 32768 WHERE id = 20",
        "UPDATE drives SET start_date_pg_us = -9223372036854775807 WHERE id = 20",
        "UPDATE drives SET end_date_pg_us = 9223371331200000000 WHERE id = 20",
        "UPDATE drives SET start_position_id = 2147483648 WHERE id = 20",
        "UPDATE drives SET outside_temp_avg_e1 = 10000, outside_temp_avg_e1_is_nan = 0 WHERE id = 20",
        "UPDATE drives SET inside_temp_avg_e1 = 1, inside_temp_avg_e1_is_nan = 1 WHERE id = 20",
        "UPDATE drives SET start_ideal_range_km_e2_is_nan = 2 WHERE id = 20",
        "UPDATE drives SET start_km_f64_be = x'00000000000000' WHERE id = 20",
        "UPDATE drives SET duration_min = 32768 WHERE id = 20",
    ] {
        assert!(
            connection.execute(statement, []).is_err(),
            "{statement} must violate the exact physical drives DDL"
        );
    }
    connection
        .execute(
            "UPDATE drives SET end_date_pg_us = NULL, start_position_id = -2147483648,
                    end_position_id = 2147483647, start_address_id = -2147483648,
                    end_address_id = 2147483647, start_geofence_id = -2147483648,
                    end_geofence_id = 2147483647 WHERE id = 20",
            [],
        )
        .expect("open and signed raw drive values have no invented semantic policy");
    let charging_process_refs: (Option<i64>, Option<i64>) = connection
        .query_row(
            "SELECT address_id, geofence_id FROM charging_processes WHERE id = 40",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(charging_process_refs, (Some(100), Some(200)));
    #[derive(Debug, PartialEq)]
    struct AddressRow {
        id: i64,
        display_name: Option<String>,
        latitude_e6: Option<i64>,
        latitude_e6_is_nan: i64,
        longitude_e6: Option<i64>,
        longitude_e6_is_nan: i64,
        name: Option<String>,
        house_number: Option<String>,
        road: Option<String>,
        neighbourhood: Option<String>,
        city: Option<String>,
        county: Option<String>,
        postcode: Option<String>,
        state: Option<String>,
        state_district: Option<String>,
        country: Option<String>,
        inserted_at_pg_us: i64,
        updated_at_pg_us: i64,
        osm_id: Option<i64>,
        osm_type: Option<String>,
    }
    let addresses: Vec<AddressRow> = connection
        .prepare(
            "SELECT id, display_name, latitude_e6, latitude_e6_is_nan, longitude_e6,
                    longitude_e6_is_nan, name, house_number, road,
                    neighbourhood, city, county, postcode, state, state_district, country,
                    inserted_at_pg_us, updated_at_pg_us, osm_id, osm_type
             FROM addresses ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok(AddressRow {
                id: row.get(0)?,
                display_name: row.get(1)?,
                latitude_e6: row.get(2)?,
                latitude_e6_is_nan: row.get(3)?,
                longitude_e6: row.get(4)?,
                longitude_e6_is_nan: row.get(5)?,
                name: row.get(6)?,
                house_number: row.get(7)?,
                road: row.get(8)?,
                neighbourhood: row.get(9)?,
                city: row.get(10)?,
                county: row.get(11)?,
                postcode: row.get(12)?,
                state: row.get(13)?,
                state_district: row.get(14)?,
                country: row.get(15)?,
                inserted_at_pg_us: row.get(16)?,
                updated_at_pg_us: row.get(17)?,
                osm_id: row.get(18)?,
                osm_type: row.get(19)?,
            })
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        addresses,
        vec![
            AddressRow {
                id: 100,
                display_name: Some("Home, London".into()),
                latitude_e6: Some(51_500_123),
                latitude_e6_is_nan: 0,
                longitude_e6: Some(-123_456),
                longitude_e6_is_nan: 0,
                name: Some("Home".into()),
                house_number: Some("1".into()),
                road: Some("Strawberry Road".into()),
                neighbourhood: Some("Westminster".into()),
                city: Some("London".into()),
                county: Some("Greater London".into()),
                postcode: Some("SW1A 1AA".into()),
                state: Some("England".into()),
                state_district: Some("London".into()),
                country: Some("United Kingdom".into()),
                inserted_at_pg_us: 1_700_000_000_000_000,
                updated_at_pg_us: 1_700_000_100_000_000,
                osm_id: Some(-42),
                osm_type: Some("node".into()),
            },
            AddressRow {
                id: 101,
                display_name: Some("Work, London".into()),
                latitude_e6: None,
                latitude_e6_is_nan: 1,
                longitude_e6: None,
                longitude_e6_is_nan: 0,
                name: Some("Work".into()),
                house_number: None,
                road: None,
                neighbourhood: None,
                city: None,
                county: None,
                postcode: None,
                state: None,
                state_district: None,
                country: None,
                inserted_at_pg_us: 1_700_000_200_000_000,
                updated_at_pg_us: 1_700_000_300_000_000,
                osm_id: None,
                osm_type: None,
            },
        ]
    );
    assert!(
        connection
            .execute(
                "UPDATE addresses SET latitude_e6 = 100000000 WHERE id = 100",
                [],
            )
            .is_err()
    );
    assert!(
        connection
            .execute(
                "UPDATE addresses SET longitude_e6 = -1000000000 WHERE id = 100",
                [],
            )
            .is_err()
    );
    assert!(
        connection
            .execute(
                "UPDATE addresses SET latitude_e6 = 1, latitude_e6_is_nan = 1 WHERE id = 100",
                [],
            )
            .is_err(),
        "finite numeric and NaN tag must not be conflated"
    );
    assert!(
        connection
            .execute(
                "UPDATE addresses SET longitude_e6_is_nan = 2 WHERE id = 100",
                [],
            )
            .is_err(),
        "numeric NaN tag must be binary"
    );
    assert!(
        connection
            .execute("UPDATE addresses SET id = 2147483648 WHERE id = 100", [],)
            .is_err()
    );
    assert!(
        connection
            .execute(
                "UPDATE addresses SET inserted_at_pg_us = 1 WHERE id = 100",
                [],
            )
            .is_err()
    );
    assert!(
        connection
            .execute(
                "UPDATE addresses SET updated_at_pg_us = -9223372036854775807 WHERE id = 100",
                [],
            )
            .is_err()
    );
    #[derive(Debug, PartialEq)]
    struct GeofenceRow {
        id: i64,
        name: String,
        latitude_e6: Option<i64>,
        latitude_e6_is_nan: i64,
        longitude_e6: Option<i64>,
        longitude_e6_is_nan: i64,
        radius: i64,
        billing_type: String,
        cost_per_unit_e4: Option<i64>,
        cost_per_unit_e4_is_nan: i64,
        session_fee_e2: Option<i64>,
        session_fee_e2_is_nan: i64,
        inserted_at_pg_us: i64,
        updated_at_pg_us: i64,
    }
    let geofences: Vec<GeofenceRow> = connection
        .prepare(
            "SELECT id, name, latitude_e6, latitude_e6_is_nan, longitude_e6,
                    longitude_e6_is_nan, radius, billing_type, cost_per_unit_e4,
                    cost_per_unit_e4_is_nan, session_fee_e2, session_fee_e2_is_nan,
                    inserted_at_pg_us, updated_at_pg_us
             FROM geofences ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok(GeofenceRow {
                id: row.get(0)?,
                name: row.get(1)?,
                latitude_e6: row.get(2)?,
                latitude_e6_is_nan: row.get(3)?,
                longitude_e6: row.get(4)?,
                longitude_e6_is_nan: row.get(5)?,
                radius: row.get(6)?,
                billing_type: row.get(7)?,
                cost_per_unit_e4: row.get(8)?,
                cost_per_unit_e4_is_nan: row.get(9)?,
                session_fee_e2: row.get(10)?,
                session_fee_e2_is_nan: row.get(11)?,
                inserted_at_pg_us: row.get(12)?,
                updated_at_pg_us: row.get(13)?,
            })
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        geofences,
        vec![
            GeofenceRow {
                id: 200,
                name: "Home".into(),
                latitude_e6: Some(0),
                latitude_e6_is_nan: 0,
                longitude_e6: None,
                longitude_e6_is_nan: 1,
                radius: i64::from(i16::MIN),
                billing_type: "per_kwh".into(),
                cost_per_unit_e4: Some(3_000),
                cost_per_unit_e4_is_nan: 0,
                session_fee_e2: None,
                session_fee_e2_is_nan: 1,
                inserted_at_pg_us: 1_700_000_000_000_000,
                updated_at_pg_us: 1_700_000_100_000_000,
            },
            GeofenceRow {
                id: 201,
                name: "Work".into(),
                latitude_e6: None,
                latitude_e6_is_nan: 1,
                longitude_e6: Some(-110_000),
                longitude_e6_is_nan: 0,
                radius: i64::from(i16::MAX),
                billing_type: "per_minute".into(),
                cost_per_unit_e4: None,
                cost_per_unit_e4_is_nan: 0,
                session_fee_e2: None,
                session_fee_e2_is_nan: 0,
                inserted_at_pg_us: 1_700_000_200_000_000,
                updated_at_pg_us: 1_700_000_300_000_000,
            },
        ]
    );
    assert!(
        connection
            .execute(
                "UPDATE geofences SET latitude_e6 = NULL, latitude_e6_is_nan = 0 WHERE id = 200",
                [],
            )
            .is_err(),
        "required source numeric must be finite or NaN"
    );
    assert!(
        connection
            .execute(
                "UPDATE geofences SET cost_per_unit_e4 = 1, cost_per_unit_e4_is_nan = 1 WHERE id = 200",
                [],
            )
            .is_err(),
        "optional source numeric finite and NaN states must stay distinct"
    );
    for statement in [
        "UPDATE geofences SET id = 2147483648 WHERE id = 200",
        "UPDATE geofences SET name = replace(hex(zeroblob(256)), '00', 'x') WHERE id = 200",
        "UPDATE geofences SET latitude_e6 = 100000000 WHERE id = 200",
        "UPDATE geofences SET cost_per_unit_e4 = 1000000 WHERE id = 200",
        "UPDATE geofences SET inserted_at_pg_us = 1 WHERE id = 200",
        "UPDATE geofences SET updated_at_pg_us = -9223372036854775807 WHERE id = 200",
    ] {
        assert!(
            connection.execute(statement, []).is_err(),
            "{statement} must violate the physical geofence DDL"
        );
    }
    let settings_rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM car_settings", [], |row| row.get(0))
        .unwrap();
    assert_eq!(settings_rows, 1);
    let geofence_columns: Vec<String> = connection
        .prepare("PRAGMA table_xinfo('geofences')")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for lossy_column in [
        "latitude",
        "longitude",
        "radius_m",
        "cost_per_unit",
        "session_fee",
    ] {
        assert!(
            !geofence_columns.contains(&lossy_column.to_owned()),
            "schema 2.2 geofence must retain exact fixed-scale source values"
        );
    }
    let foreign_key_rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(foreign_key_rows, 0);
    let drive_columns: Vec<String> = connection
        .prepare("PRAGMA table_xinfo('drives')")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(!drive_columns.contains(&"start_address".into()));
    assert!(!drive_columns.contains(&"end_address".into()));
    assert!(!drive_columns.contains(&"start_geofence".into()));
    assert!(!drive_columns.contains(&"end_geofence".into()));
}
