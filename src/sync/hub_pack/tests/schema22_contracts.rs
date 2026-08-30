// SPDX-License-Identifier: AGPL-3.0-only

#[test]
fn schema_2_2_address_contract_hash_and_ddl_are_pinned() {
    assert_eq!(
        Sha256Digest::of_bytes(THP2_2_ADDRESS_SLICE_CONTRACT.as_bytes()).to_string(),
        THP2_2_ADDRESS_SLICE_SHA256
    );

    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(THP2_2_ADDRESSES_SQLITE_DDL)
        .unwrap();
    verify_projection_table_ddl(&connection, "addresses", THP2_2_ADDRESSES_SQLITE_DDL)
        .expect("canonical address DDL must verify");

    let unchecked = THP2_2_ADDRESSES_SQLITE_DDL
        .replace(" CHECK(id BETWEEN -2147483648 AND 2147483647)", "")
        .replace(" CHECK(latitude_e6_is_nan IN (0, 1))", "");
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch(&unchecked).unwrap();
    assert!(
        verify_projection_table_ddl(&connection, "addresses", THP2_2_ADDRESSES_SQLITE_DDL).is_err(),
        "the verifier must reject an addresses table recreated without physical checks"
    );
}

#[test]
fn schema_2_2_geofence_contract_hash_and_ddl_are_pinned() {
    assert_eq!(
        Sha256Digest::of_bytes(THP2_2_GEOFENCE_SLICE_CONTRACT.as_bytes()).to_string(),
        THP2_2_GEOFENCE_SLICE_SHA256
    );

    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(THP2_2_GEOFENCES_SQLITE_DDL)
        .unwrap();
    verify_projection_table_ddl(&connection, "geofences", THP2_2_GEOFENCES_SQLITE_DDL)
        .expect("canonical geofence DDL must verify");

    let unchecked = THP2_2_GEOFENCES_SQLITE_DDL
        .replace(" CHECK(id BETWEEN -2147483648 AND 2147483647)", "")
        .replace(" CHECK(latitude_e6_is_nan IN (0, 1))", "");
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch(&unchecked).unwrap();
    assert!(
        verify_projection_table_ddl(&connection, "geofences", THP2_2_GEOFENCES_SQLITE_DDL).is_err(),
        "the verifier must reject a geofences table recreated without tagged numeric checks"
    );
}

#[test]
fn schema_2_2_global_settings_contract_hash_ddl_and_singleton_are_pinned() {
    assert_eq!(
        Sha256Digest::of_bytes(THP2_2_GLOBAL_SETTINGS_SLICE_CONTRACT.as_bytes()).to_string(),
        THP2_2_GLOBAL_SETTINGS_SLICE_SHA256
    );
    assert_eq!(
        serde_json::to_string(&ProjectionUnitOfLengthV2_2::Kilometers).unwrap(),
        "\"km\""
    );
    assert_eq!(
        serde_json::to_string(&ProjectionUnitOfLengthV2_2::Miles).unwrap(),
        "\"mi\""
    );
    let length_round_trip = serde_json::to_string(&ProjectionUnitOfLengthV2_2::Miles).unwrap();
    assert_eq!(
        serde_json::from_str::<ProjectionUnitOfLengthV2_2>(&length_round_trip).unwrap(),
        ProjectionUnitOfLengthV2_2::Miles
    );
    assert_eq!(
        serde_json::from_str::<ProjectionUnitOfLengthV2_2>("\"mi\"").unwrap(),
        ProjectionUnitOfLengthV2_2::Miles
    );
    assert!(serde_json::from_str::<ProjectionUnitOfLengthV2_2>("\"Miles\"").is_err());
    assert_eq!(
        serde_json::to_string(&ProjectionUnitOfTemperatureV2_2::Celsius).unwrap(),
        "\"C\""
    );
    assert_eq!(
        serde_json::to_string(&ProjectionUnitOfTemperatureV2_2::Fahrenheit).unwrap(),
        "\"F\""
    );
    let temperature_round_trip =
        serde_json::to_string(&ProjectionUnitOfTemperatureV2_2::Fahrenheit).unwrap();
    assert_eq!(
        serde_json::from_str::<ProjectionUnitOfTemperatureV2_2>(&temperature_round_trip).unwrap(),
        ProjectionUnitOfTemperatureV2_2::Fahrenheit
    );
    assert_eq!(
        serde_json::from_str::<ProjectionUnitOfTemperatureV2_2>("\"F\"").unwrap(),
        ProjectionUnitOfTemperatureV2_2::Fahrenheit
    );
    assert!(serde_json::from_str::<ProjectionUnitOfTemperatureV2_2>("\"Kelvin\"").is_err());
    assert_eq!(
        serde_json::to_string(&ProjectionUnitOfPressureV2_2::Bar).unwrap(),
        "\"bar\""
    );
    assert_eq!(
        serde_json::to_string(&ProjectionUnitOfPressureV2_2::Psi).unwrap(),
        "\"psi\""
    );
    let pressure_round_trip = serde_json::to_string(&ProjectionUnitOfPressureV2_2::Psi).unwrap();
    assert_eq!(
        serde_json::from_str::<ProjectionUnitOfPressureV2_2>(&pressure_round_trip).unwrap(),
        ProjectionUnitOfPressureV2_2::Psi
    );
    assert_eq!(
        serde_json::from_str::<ProjectionUnitOfPressureV2_2>("\"psi\"").unwrap(),
        ProjectionUnitOfPressureV2_2::Psi
    );
    assert!(serde_json::from_str::<ProjectionUnitOfPressureV2_2>("\"kpa\"").is_err());
    assert_eq!(
        serde_json::to_string(&ProjectionPreferredRangeV2_2::Ideal).unwrap(),
        "\"ideal\""
    );
    assert_eq!(
        serde_json::to_string(&ProjectionPreferredRangeV2_2::Rated).unwrap(),
        "\"rated\""
    );
    let range_round_trip = serde_json::to_string(&ProjectionPreferredRangeV2_2::Rated).unwrap();
    assert_eq!(
        serde_json::from_str::<ProjectionPreferredRangeV2_2>(&range_round_trip).unwrap(),
        ProjectionPreferredRangeV2_2::Rated
    );
    assert_eq!(
        serde_json::from_str::<ProjectionPreferredRangeV2_2>("\"rated\"").unwrap(),
        ProjectionPreferredRangeV2_2::Rated
    );
    assert!(serde_json::from_str::<ProjectionPreferredRangeV2_2>("\"preferred\"").is_err());

    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(THP2_2_GLOBAL_SETTINGS_SQLITE_DDL)
        .unwrap();
    verify_projection_table_ddl(
        &connection,
        "global_settings",
        THP2_2_GLOBAL_SETTINGS_SQLITE_DDL,
    )
    .expect("canonical global settings DDL must verify");
    verify_projection_foreign_keys(&connection, "global_settings", &[])
        .expect("global source settings have no local SQLite foreign keys");

    let unchecked = THP2_2_GLOBAL_SETTINGS_SQLITE_DDL
        .replace(" CHECK(unit_of_length IN ('km', 'mi'))", "")
        .replace(" CHECK(base_url IS NULL OR length(base_url) <= 255)", "")
        .replace(
            " CHECK(inserted_at_pg_us = (-9223372036854775807 - 1) OR inserted_at_pg_us = 9223372036854775807 OR (inserted_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND inserted_at_pg_us % 1000000 = 0))",
            "",
        );
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch(&unchecked).unwrap();
    assert!(
        verify_projection_table_ddl(
            &connection,
            "global_settings",
            THP2_2_GLOBAL_SETTINGS_SQLITE_DDL,
        )
        .is_err(),
        "the verifier must reject global settings recreated without physical checks"
    );

    let source = snapshot_v2_2();
    assert!(validate_request_v2_2(&request_v2_2(&source), ProtocolLimits::default()).is_ok());
    let mut missing = source.clone();
    missing.global_settings.clear();
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&missing), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message))
            if message == "schema 2.2 physical snapshot must contain exactly one global_settings row"
    ));
    let mut duplicate = source.clone();
    duplicate
        .global_settings
        .push(source.global_settings[0].clone());
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&duplicate), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message))
            if message == "schema 2.2 physical snapshot must contain exactly one global_settings row"
    ));
    let mut url_boundary = source.clone();
    url_boundary.global_settings[0].base_url = Some("é".repeat(255));
    assert!(validate_request_v2_2(&request_v2_2(&url_boundary), ProtocolLimits::default()).is_ok());
    url_boundary.global_settings[0].grafana_url = Some("é".repeat(256));
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&url_boundary), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message))
            if message == "global_settings.grafana_url exceeds its pinned source width"
    ));
    let mut unsafe_text = source;
    unsafe_text.global_settings[0].language = "x".repeat(MAX_TEXT_BYTES + 1);
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&unsafe_text), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message))
            if message == "global_settings.language is unsafe or too large"
    ));
}

#[test]
fn schema_2_2_cars_and_car_settings_contract_hashes_and_ddl_are_pinned() {
    assert_eq!(
        Sha256Digest::of_bytes(THP2_2_CAR_SETTINGS_SLICE_CONTRACT.as_bytes()).to_string(),
        THP2_2_CAR_SETTINGS_SLICE_SHA256
    );
    assert_eq!(
        Sha256Digest::of_bytes(THP2_2_CARS_SLICE_CONTRACT.as_bytes()).to_string(),
        THP2_2_CARS_SLICE_SHA256
    );

    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(THP2_2_CAR_SETTINGS_SQLITE_DDL)
        .unwrap();
    connection.execute_batch(THP2_2_CARS_SQLITE_DDL).unwrap();
    verify_projection_table_ddl(&connection, "car_settings", THP2_2_CAR_SETTINGS_SQLITE_DDL)
        .expect("canonical car_settings DDL must verify");
    verify_projection_table_ddl(&connection, "cars", THP2_2_CARS_SQLITE_DDL)
        .expect("canonical cars DDL must verify");

    let unchecked_settings = THP2_2_CAR_SETTINGS_SQLITE_DDL
        .replace(" CHECK(suspend_min BETWEEN -2147483648 AND 2147483647)", "")
        .replace(" CHECK(enabled IN (0, 1))", "");
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch(&unchecked_settings).unwrap();
    assert!(
        verify_projection_table_ddl(&connection, "car_settings", THP2_2_CAR_SETTINGS_SQLITE_DDL)
            .is_err(),
        "the verifier must reject a car_settings table recreated without physical checks"
    );

    let unchecked_cars = THP2_2_CARS_SQLITE_DDL
        .replace(" CHECK(id BETWEEN -32768 AND 32767)", "")
        .replace(" CHECK(model IS NULL OR length(model) <= 255)", "")
        .replace(" CHECK(efficiency IS NULL OR length(efficiency) = 8)", "")
        .replace(
            " CHECK(inserted_at_pg_us = (-9223372036854775807 - 1) OR inserted_at_pg_us = 9223372036854775807 OR (inserted_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND inserted_at_pg_us % 1000000 = 0))",
            "",
        )
        .replace(
            " UNIQUE REFERENCES car_settings(id)",
            " REFERENCES car_settings(id)",
        );
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(THP2_2_CAR_SETTINGS_SQLITE_DDL)
        .unwrap();
    connection.execute_batch(&unchecked_cars).unwrap();
    assert!(
        verify_projection_table_ddl(&connection, "cars", THP2_2_CARS_SQLITE_DDL).is_err(),
        "the verifier must reject a cars table recreated without physical checks or unique FK"
    );
}

#[test]
fn schema_2_2_drives_contract_hash_ddl_and_no_outgoing_foreign_keys_are_pinned() {
    assert_eq!(
        Sha256Digest::of_bytes(THP2_2_DRIVES_SLICE_CONTRACT.as_bytes()).to_string(),
        THP2_2_DRIVES_SLICE_SHA256
    );
    assert_eq!(THP2_2_DRIVES_FLOAT_ENCODING, "ieee754_bits_be_blob");

    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch(THP2_2_DRIVES_SQLITE_DDL).unwrap();
    verify_projection_table_ddl(&connection, "drives", THP2_2_DRIVES_SQLITE_DDL)
        .expect("canonical drives DDL must verify");
    verify_projection_foreign_keys(&connection, "drives", &[])
        .expect("raw physical drive IDs must not invent outgoing relations");

    let unchecked = THP2_2_DRIVES_SQLITE_DDL
        .replace(" CHECK(id BETWEEN -2147483648 AND 2147483647)", "")
        .replace(
            " CHECK(start_km_f64_be IS NULL OR length(start_km_f64_be) = 8)",
            "",
        )
        .replace(" CHECK(outside_temp_avg_e1_is_nan IN (0, 1))", "");
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch(&unchecked).unwrap();
    assert!(
        verify_projection_table_ddl(&connection, "drives", THP2_2_DRIVES_SQLITE_DDL).is_err(),
        "the verifier must reject a drives table recreated without exact physical checks"
    );

    let with_fk = THP2_2_DRIVES_SQLITE_DDL.replace(
        "car_id INTEGER NOT NULL CHECK(car_id BETWEEN -32768 AND 32767)",
        "car_id INTEGER NOT NULL REFERENCES cars(id) CHECK(car_id BETWEEN -32768 AND 32767)",
    );
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("CREATE TABLE cars (id INTEGER PRIMARY KEY) STRICT, WITHOUT ROWID;")
        .unwrap();
    connection.execute_batch(&with_fk).unwrap();
    assert!(
        verify_projection_foreign_keys(&connection, "drives", &[]).is_err(),
        "the verifier must reject invented outgoing drive relations"
    );
}

#[test]
fn schema_2_2_drives_preserve_signed_open_soft_refs_and_bit_exact_values() {
    let source = snapshot_v2_2();
    assert!(validate_request_v2_2(&request_v2_2(&source), ProtocolLimits::default()).is_ok());

    let mut signed_open = snapshot_v2_2();
    signed_open.positions.clear();
    signed_open.charges.clear();
    signed_open.addresses[0].id = i32::MIN;
    signed_open.addresses[1].id = 0;
    signed_open.geofences[0].id = -1;
    signed_open.geofences[1].id = 0;
    signed_open.drives[0].id = i32::MIN;
    signed_open.drives[0].end_date_pg_us = None;
    signed_open.drives[0].start_position_id = Some(i32::MIN);
    signed_open.drives[0].end_position_id = Some(i32::MAX);
    signed_open.drives[0].start_address_id = Some(i32::MIN);
    signed_open.drives[0].end_address_id = Some(0);
    signed_open.drives[0].start_geofence_id = Some(-1);
    signed_open.drives[0].end_geofence_id = Some(0);
    signed_open.drives[0].outside_temp_avg_e1 = Some(ProjectionFixedNumericV2_2::NaN);
    signed_open.drives[0].inside_temp_avg_e1 = None;
    signed_open.drives[0].start_km = Some(ProjectionFloat64BitsV2_2((-0.0_f64).to_bits()));
    signed_open.drives[0].end_km = Some(ProjectionFloat64BitsV2_2(f64::NEG_INFINITY.to_bits()));
    signed_open.drives[0].distance = Some(ProjectionFloat64BitsV2_2(0x7ff8_0000_0000_0042));
    assert!(
        validate_request_v2_2(&request_v2_2(&signed_open), ProtocolLimits::default()).is_ok(),
        "raw signed IDs, soft selected-subset refs, open rows, NaN, NULL, and FLOAT8 bits are physical source values"
    );
    let temporary = crate::private_tempdir().unwrap();
    ProjectionPackWriter::new(temporary.path().join("signed-physical"))
        .write_full_snapshot_2_2(&request_v2_2(&signed_open))
        .expect("signed extant address/geofence IDs must survive local physical writing");

    let mut end_before_start = snapshot_v2_2();
    end_before_start.drives[0].start_date_pg_us = i64::MAX;
    end_before_start.drives[0].end_date_pg_us = Some(i64::MIN);
    assert!(
        validate_request_v2_2(&request_v2_2(&end_before_start), ProtocolLimits::default()).is_ok()
    );

    let mut wrong_car = snapshot_v2_2();
    wrong_car.drives[0].car_id = 11;
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&wrong_car), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message))
            if message == "drive.car_id does not match selected_car_id"
    ));

    let mut duplicate = snapshot_v2_2();
    duplicate.drives.push(duplicate.drives[0].clone());
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&duplicate), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message)) if message == "duplicate drive.id"
    ));

    let mut invalid_numeric = snapshot_v2_2();
    invalid_numeric.drives[0].outside_temp_avg_e1 =
        Some(ProjectionFixedNumericV2_2::Finite(10_000));
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&invalid_numeric), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message))
            if message == "drive.outside_temp_avg_e1 is outside its pinned source range"
    ));
}

#[test]
fn schema_2_2_positions_contract_hash_ddl_and_zero_local_foreign_keys_are_pinned() {
    assert_eq!(
        Sha256Digest::of_bytes(THP2_2_POSITIONS_SLICE_CONTRACT.as_bytes()).to_string(),
        THP2_2_POSITIONS_SLICE_SHA256
    );
    assert_eq!(THP2_2_POSITIONS_ODOMETER_ENCODING, "ieee754_bits_be_blob");
    assert_eq!(
        THP2_2_POSITIONS_RELATION_SCOPE,
        "source_car_fk_rust_admission;source_drive_fk_omitted_cross_car_target"
    );

    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(THP2_2_POSITIONS_SQLITE_DDL)
        .unwrap();
    verify_projection_table_ddl(&connection, "positions", THP2_2_POSITIONS_SQLITE_DDL)
        .expect("canonical positions DDL must verify");
    verify_projection_foreign_keys(&connection, "positions", &[])
        .expect("the V3 local positions schema intentionally has no SQLite foreign keys");

    let unchecked = THP2_2_POSITIONS_SQLITE_DDL
        .replace(" CHECK(id BETWEEN -2147483648 AND 2147483647)", "")
        .replace(
            " CHECK(odometer_f64_be IS NULL OR length(odometer_f64_be) = 8)",
            "",
        )
        .replace(" CHECK(latitude_e6_is_nan IN (0, 1))", "");
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch(&unchecked).unwrap();
    assert!(
        verify_projection_table_ddl(&connection, "positions", THP2_2_POSITIONS_SQLITE_DDL).is_err(),
        "the verifier must reject a positions table recreated without physical checks"
    );

    let with_fk = THP2_2_POSITIONS_SQLITE_DDL.replace(
        "car_id INTEGER NOT NULL CHECK(car_id BETWEEN -32768 AND 32767)",
        "car_id INTEGER NOT NULL REFERENCES cars(id) CHECK(car_id BETWEEN -32768 AND 32767)",
    );
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("CREATE TABLE cars (id INTEGER PRIMARY KEY) STRICT, WITHOUT ROWID;")
        .unwrap();
    connection.execute_batch(&with_fk).unwrap();
    assert!(
        verify_projection_foreign_keys(&connection, "positions", &[]).is_err(),
        "the verifier must reject every foreign key in the V3 local positions schema"
    );
}

#[test]
fn schema_2_2_positions_preserve_all_physical_values_without_relation_closure() {
    let source = snapshot_v2_2();
    assert!(validate_request_v2_2(&request_v2_2(&source), ProtocolLimits::default()).is_ok());

    let temporary = crate::private_tempdir().unwrap();
    let built = ProjectionPackWriter::new(temporary.path().join("positions"))
        .write_full_snapshot_2_2(&request_v2_2(&source))
        .expect("exact physical positions must write locally");
    let inspect = temporary.path().join("positions.sqlite");
    fs::write(
        &inspect,
        zstd::stream::decode_all(File::open(&built.path).unwrap()).unwrap(),
    )
    .unwrap();
    let connection = Connection::open(&inspect).unwrap();
    #[derive(Debug, PartialEq)]
    struct PositionRow {
        id: i64,
        car_id: i64,
        drive_id: Option<i64>,
        date_pg_us: i64,
        latitude_e6: Option<i64>,
        latitude_e6_is_nan: i64,
        longitude_e6: Option<i64>,
        longitude_e6_is_nan: i64,
        elevation: Option<i64>,
        speed: Option<i64>,
        power: Option<i64>,
        odometer_f64_be: Option<Vec<u8>>,
        ideal_battery_range_km_e2: Option<i64>,
        ideal_battery_range_km_e2_is_nan: i64,
        est_battery_range_km_e2: Option<i64>,
        est_battery_range_km_e2_is_nan: i64,
        rated_battery_range_km_e2: Option<i64>,
        rated_battery_range_km_e2_is_nan: i64,
        battery_level: Option<i64>,
        usable_battery_level: Option<i64>,
        battery_heater: Option<i64>,
        battery_heater_on: Option<i64>,
        battery_heater_no_power: Option<i64>,
        outside_temp_e1: Option<i64>,
        outside_temp_e1_is_nan: i64,
        inside_temp_e1: Option<i64>,
        inside_temp_e1_is_nan: i64,
        fan_status: Option<i64>,
        driver_temp_setting_e1: Option<i64>,
        driver_temp_setting_e1_is_nan: i64,
        passenger_temp_setting_e1: Option<i64>,
        passenger_temp_setting_e1_is_nan: i64,
        is_climate_on: Option<i64>,
        is_rear_defroster_on: Option<i64>,
        is_front_defroster_on: Option<i64>,
        tpms_pressure_fl_e1: Option<i64>,
        tpms_pressure_fl_e1_is_nan: i64,
        tpms_pressure_fr_e1: Option<i64>,
        tpms_pressure_fr_e1_is_nan: i64,
        tpms_pressure_rl_e1: Option<i64>,
        tpms_pressure_rl_e1_is_nan: i64,
        tpms_pressure_rr_e1: Option<i64>,
        tpms_pressure_rr_e1_is_nan: i64,
    }
    let position = connection
        .query_row(
            "SELECT id, car_id, drive_id, date_pg_us, latitude_e6, latitude_e6_is_nan,
                    longitude_e6, longitude_e6_is_nan, elevation, speed, power,
                    odometer_f64_be, ideal_battery_range_km_e2,
                    ideal_battery_range_km_e2_is_nan, est_battery_range_km_e2,
                    est_battery_range_km_e2_is_nan, rated_battery_range_km_e2,
                    rated_battery_range_km_e2_is_nan, battery_level,
                    usable_battery_level, battery_heater, battery_heater_on,
                    battery_heater_no_power, outside_temp_e1, outside_temp_e1_is_nan,
                    inside_temp_e1, inside_temp_e1_is_nan, fan_status,
                    driver_temp_setting_e1, driver_temp_setting_e1_is_nan,
                    passenger_temp_setting_e1, passenger_temp_setting_e1_is_nan,
                    is_climate_on, is_rear_defroster_on, is_front_defroster_on,
                    tpms_pressure_fl_e1, tpms_pressure_fl_e1_is_nan,
                    tpms_pressure_fr_e1, tpms_pressure_fr_e1_is_nan,
                    tpms_pressure_rl_e1, tpms_pressure_rl_e1_is_nan,
                    tpms_pressure_rr_e1, tpms_pressure_rr_e1_is_nan
             FROM positions WHERE id = 30",
            [],
            |row| {
                Ok(PositionRow {
                    id: row.get(0)?,
                    car_id: row.get(1)?,
                    drive_id: row.get(2)?,
                    date_pg_us: row.get(3)?,
                    latitude_e6: row.get(4)?,
                    latitude_e6_is_nan: row.get(5)?,
                    longitude_e6: row.get(6)?,
                    longitude_e6_is_nan: row.get(7)?,
                    elevation: row.get(8)?,
                    speed: row.get(9)?,
                    power: row.get(10)?,
                    odometer_f64_be: row.get(11)?,
                    ideal_battery_range_km_e2: row.get(12)?,
                    ideal_battery_range_km_e2_is_nan: row.get(13)?,
                    est_battery_range_km_e2: row.get(14)?,
                    est_battery_range_km_e2_is_nan: row.get(15)?,
                    rated_battery_range_km_e2: row.get(16)?,
                    rated_battery_range_km_e2_is_nan: row.get(17)?,
                    battery_level: row.get(18)?,
                    usable_battery_level: row.get(19)?,
                    battery_heater: row.get(20)?,
                    battery_heater_on: row.get(21)?,
                    battery_heater_no_power: row.get(22)?,
                    outside_temp_e1: row.get(23)?,
                    outside_temp_e1_is_nan: row.get(24)?,
                    inside_temp_e1: row.get(25)?,
                    inside_temp_e1_is_nan: row.get(26)?,
                    fan_status: row.get(27)?,
                    driver_temp_setting_e1: row.get(28)?,
                    driver_temp_setting_e1_is_nan: row.get(29)?,
                    passenger_temp_setting_e1: row.get(30)?,
                    passenger_temp_setting_e1_is_nan: row.get(31)?,
                    is_climate_on: row.get(32)?,
                    is_rear_defroster_on: row.get(33)?,
                    is_front_defroster_on: row.get(34)?,
                    tpms_pressure_fl_e1: row.get(35)?,
                    tpms_pressure_fl_e1_is_nan: row.get(36)?,
                    tpms_pressure_fr_e1: row.get(37)?,
                    tpms_pressure_fr_e1_is_nan: row.get(38)?,
                    tpms_pressure_rl_e1: row.get(39)?,
                    tpms_pressure_rl_e1_is_nan: row.get(40)?,
                    tpms_pressure_rr_e1: row.get(41)?,
                    tpms_pressure_rr_e1_is_nan: row.get(42)?,
                })
            },
        )
        .unwrap();
    assert_eq!(
        position,
        PositionRow {
            id: 30,
            car_id: 10,
            drive_id: Some(20),
            date_pg_us: 1_700_000_030_123_456,
            latitude_e6: Some(51_505_000),
            latitude_e6_is_nan: 0,
            longitude_e6: Some(-105_000),
            longitude_e6_is_nan: 0,
            elevation: Some(i64::from(i16::MIN)),
            speed: Some(i64::from(i16::MAX)),
            power: Some(i64::from(i16::MIN)),
            odometer_f64_be: Some((-0.0_f64).to_bits().to_be_bytes().to_vec()),
            ideal_battery_range_km_e2: Some(999_999),
            ideal_battery_range_km_e2_is_nan: 0,
            est_battery_range_km_e2: Some(-999_999),
            est_battery_range_km_e2_is_nan: 0,
            rated_battery_range_km_e2: None,
            rated_battery_range_km_e2_is_nan: 1,
            battery_level: Some(i64::from(i16::MIN)),
            usable_battery_level: Some(i64::from(i16::MAX)),
            battery_heater: Some(0),
            battery_heater_on: Some(1),
            battery_heater_no_power: None,
            outside_temp_e1: None,
            outside_temp_e1_is_nan: 1,
            inside_temp_e1: Some(-9_999),
            inside_temp_e1_is_nan: 0,
            fan_status: Some(i64::from(i32::MIN)),
            driver_temp_setting_e1: None,
            driver_temp_setting_e1_is_nan: 0,
            passenger_temp_setting_e1: Some(9_999),
            passenger_temp_setting_e1_is_nan: 0,
            is_climate_on: Some(1),
            is_rear_defroster_on: Some(0),
            is_front_defroster_on: None,
            tpms_pressure_fl_e1: Some(-9_999),
            tpms_pressure_fl_e1_is_nan: 0,
            tpms_pressure_fr_e1: None,
            tpms_pressure_fr_e1_is_nan: 1,
            tpms_pressure_rl_e1: None,
            tpms_pressure_rl_e1_is_nan: 0,
            tpms_pressure_rr_e1: Some(9_999),
            tpms_pressure_rr_e1_is_nan: 0,
        }
    );

    for statement in [
        "UPDATE positions SET id = 2147483648 WHERE id = 30",
        "UPDATE positions SET car_id = 32768 WHERE id = 30",
        "UPDATE positions SET date_pg_us = -9223372036854775807 WHERE id = 30",
        "UPDATE positions SET latitude_e6 = 100000000 WHERE id = 30",
        "UPDATE positions SET longitude_e6 = -1000000000 WHERE id = 30",
        "UPDATE positions SET latitude_e6 = 1, latitude_e6_is_nan = 1 WHERE id = 30",
        "UPDATE positions SET odometer_f64_be = x'00000000000000' WHERE id = 30",
        "UPDATE positions SET battery_level = 32768 WHERE id = 30",
        "UPDATE positions SET fan_status = 2147483648 WHERE id = 30",
        "UPDATE positions SET battery_heater = 2 WHERE id = 30",
        "UPDATE positions SET tpms_pressure_fl_e1 = 10000, tpms_pressure_fl_e1_is_nan = 0 WHERE id = 30",
    ] {
        assert!(
            connection.execute(statement, []).is_err(),
            "{statement} must violate exact physical positions DDL"
        );
    }

    connection
        .execute(
            "UPDATE positions SET drive_id = -2147483648, date_pg_us = 9223372036854775807,
                    latitude_e6 = 0, latitude_e6_is_nan = 0, longitude_e6 = 0,
                    longitude_e6_is_nan = 0, battery_level = -32768,
                    usable_battery_level = 32767 WHERE id = 30",
            [],
        )
        .expect("an omitted cross-car drive reference and non-policy physical values are valid");

    // The raw source FK can point at an extant drive of another car. That
    // target is deliberately omitted from this selected-car pack, so its
    // signed identity must remain a soft local value rather than a pack FK.
    let mut omitted_cross_car = snapshot_v2_2();
    omitted_cross_car.positions[0].id = i32::MIN;
    omitted_cross_car.positions[0].drive_id = Some(i32::MAX);
    omitted_cross_car.positions[0].date_pg_us = i64::MIN;
    omitted_cross_car.positions[0].latitude_e6 = ProjectionFixedNumericV2_2::NaN;
    omitted_cross_car.positions[0].longitude_e6 = ProjectionFixedNumericV2_2::NaN;
    omitted_cross_car.positions[0].odometer =
        Some(ProjectionFloat64BitsV2_2(0x7ff8_0000_0000_0042));
    assert!(
        validate_request_v2_2(&request_v2_2(&omitted_cross_car), ProtocolLimits::default()).is_ok(),
        "raw signed IDs, source timestamp infinity, NaN, and omitted cross-car drive values stay physical"
    );

    let mut wrong_car = snapshot_v2_2();
    wrong_car.positions[0].car_id = 11;
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&wrong_car), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message))
            if message == "position.car_id does not match selected_car_id"
    ));
    let mut duplicate = snapshot_v2_2();
    duplicate.positions.push(duplicate.positions[0].clone());
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&duplicate), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message)) if message == "duplicate position.id"
    ));
    let mut invalid_numeric = snapshot_v2_2();
    invalid_numeric.positions[0].outside_temp_e1 = Some(ProjectionFixedNumericV2_2::Finite(10_000));
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&invalid_numeric), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message))
            if message == "position.outside_temp_e1 is outside its pinned source range"
    ));
}

#[test]
fn schema_2_2_preserves_signed_and_zero_selected_car_ids() {
    for selected_car_id in [i16::MIN, 0] {
        let mut source = snapshot_v2_2();
        source.cars[0].id = selected_car_id;
        source.drives[0].car_id = selected_car_id;
        source.positions[0].car_id = selected_car_id;
        source.charging_processes[0].car_id = selected_car_id;
        source.states[0].car_id = selected_car_id;
        source.updates[0].car_id = selected_car_id;
        let mut request = request_v2_2(&source);
        request.binding.selected_car_id = i64::from(selected_car_id);
        assert!(
            validate_request_v2_2(&request, ProtocolLimits::default()).is_ok(),
            "source smallint selected_car_id {selected_car_id} must remain physical"
        );

        let temporary = crate::private_tempdir().unwrap();
        let built = ProjectionPackWriter::new(
            temporary
                .path()
                .join(format!("selected-car-{selected_car_id}")),
        )
        .write_full_snapshot_2_2(&request)
        .expect("signed or zero selected car must write schema 2.2 locally");
        let inspect = temporary.path().join("selected-car.sqlite");
        fs::write(
            &inspect,
            zstd::stream::decode_all(File::open(&built.path).unwrap()).unwrap(),
        )
        .unwrap();
        let connection = Connection::open(inspect).unwrap();
        let written_car_id: i64 = connection
            .query_row("SELECT id FROM cars", [], |row| row.get(0))
            .unwrap();
        let metadata_selected_car_id: String = connection
            .query_row(
                "SELECT value FROM hub_pack_metadata WHERE key = 'selected_car_id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(written_car_id, i64::from(selected_car_id));
        assert_eq!(metadata_selected_car_id, selected_car_id.to_string());
    }

    let out_of_range = snapshot_v2_2();
    let mut request = request_v2_2(&out_of_range);
    request.binding.selected_car_id = i64::from(i16::MAX) + 1;
    assert!(matches!(
        validate_request_v2_2(&request, ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message))
            if message == "schema 2.2 selected_car_id is outside the TeslaMate smallint source domain"
    ));
    assert!(matches!(
        validate_binding(&ProjectionBinding {
            selected_car_id: 0,
            ..binding()
        }),
        Err(ProjectionPackError::Invalid(message)) if message == "selected_car_id must be positive"
    ));
}

#[test]
fn schema_2_2_charging_contract_hashes_ddl_and_physical_bounds_are_pinned() {
    assert_eq!(
        Sha256Digest::of_bytes(THP2_2_CHARGING_PROCESSES_SLICE_CONTRACT.as_bytes()).to_string(),
        THP2_2_CHARGING_PROCESSES_SLICE_SHA256
    );
    assert_eq!(
        Sha256Digest::of_bytes(THP2_2_CHARGES_SLICE_CONTRACT.as_bytes()).to_string(),
        THP2_2_CHARGES_SLICE_SHA256
    );
    assert_eq!(
        THP2_2_CHARGING_TRI_STATE_BOOL_ENCODING,
        "sqlite_null_or_0_or_1"
    );
    assert_eq!(
        THP2_2_CHARGES_RELATION_SCOPE,
        "charges_with_extant_selected_car_process;constraint_not_re_attested"
    );

    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(THP2_2_CHARGING_PROCESSES_SQLITE_DDL)
        .unwrap();
    connection.execute_batch(THP2_2_CHARGES_SQLITE_DDL).unwrap();
    verify_projection_table_ddl(
        &connection,
        "charging_processes",
        THP2_2_CHARGING_PROCESSES_SQLITE_DDL,
    )
    .expect("canonical charging-processes DDL must verify");
    verify_projection_table_ddl(&connection, "charges", THP2_2_CHARGES_SQLITE_DDL)
        .expect("canonical charges DDL must verify");
    verify_projection_foreign_keys(&connection, "charging_processes", &[])
        .expect("the local physical charging-process table has no outgoing FKs");
    verify_projection_foreign_keys(&connection, "charges", &[])
        .expect("the local physical charges table has no outgoing FKs");

    let unchecked_processes = THP2_2_CHARGING_PROCESSES_SQLITE_DDL
        .replace(" CHECK(position_id BETWEEN -2147483648 AND 2147483647)", "")
        .replace(" CHECK(cost_e2_is_nan IN (0, 1))", "");
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch(&unchecked_processes).unwrap();
    assert!(
        verify_projection_table_ddl(
            &connection,
            "charging_processes",
            THP2_2_CHARGING_PROCESSES_SQLITE_DDL,
        )
        .is_err(),
        "the verifier must reject a charging-processes table recreated without physical checks"
    );

    let unchecked_charges = THP2_2_CHARGES_SQLITE_DDL
        .replace(" CHECK(charger_power BETWEEN -32768 AND 32767)", "")
        .replace(
            " CHECK(fast_charger_present IS NULL OR fast_charger_present IN (0, 1))",
            "",
        );
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch(&unchecked_charges).unwrap();
    assert!(
        verify_projection_table_ddl(&connection, "charges", THP2_2_CHARGES_SQLITE_DDL).is_err(),
        "the verifier must reject a charges table recreated without physical checks"
    );

    let source = snapshot_v2_2();
    assert!(validate_request_v2_2(&request_v2_2(&source), ProtocolLimits::default()).is_ok());
    let mut bad_timestamp = snapshot_v2_2();
    bad_timestamp.charging_processes[0].start_date_pg_us = i64::MIN + 1;
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&bad_timestamp), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message))
            if message == "charging_process.start_date_pg_us is outside the PostgreSQL timestamp source domain"
    ));
    let mut bad_numeric = snapshot_v2_2();
    bad_numeric.charges[0].charge_energy_added_e2 = ProjectionFixedNumericV2_2::Finite(100_000_000);
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&bad_numeric), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message))
            if message == "charge.charge_energy_added_e2 is outside its pinned source range"
    ));
    let mut bad_width = snapshot_v2_2();
    bad_width.charges[0].fast_charger_type = Some("x".repeat(256));
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&bad_width), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message))
            if message == "charge.fast_charger_type exceeds its pinned source width"
    ));
}

#[test]
fn schema_2_2_states_and_updates_contract_hashes_and_ddl_are_pinned() {
    assert_eq!(
        Sha256Digest::of_bytes(THP2_2_STATES_SLICE_CONTRACT.as_bytes()).to_string(),
        THP2_2_STATES_SLICE_SHA256
    );
    assert_eq!(
        Sha256Digest::of_bytes(THP2_2_UPDATES_SLICE_CONTRACT.as_bytes()).to_string(),
        THP2_2_UPDATES_SLICE_SHA256
    );
    assert_eq!(
        THP2_2_POSTGRES_TIMESTAMP_ENCODING,
        "postgres_timestamp_binary_i64_us_since_2000"
    );

    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(THP2_2_CAR_SETTINGS_SQLITE_DDL)
        .unwrap();
    connection.execute_batch(THP2_2_CARS_SQLITE_DDL).unwrap();
    connection.execute_batch(THP2_2_STATES_SQLITE_DDL).unwrap();
    connection.execute_batch(THP2_2_UPDATES_SQLITE_DDL).unwrap();
    verify_projection_table_ddl(&connection, "states", THP2_2_STATES_SQLITE_DDL)
        .expect("canonical states DDL must verify");
    verify_projection_table_ddl(&connection, "updates", THP2_2_UPDATES_SQLITE_DDL)
        .expect("canonical updates DDL must verify");

    let unchecked_states = THP2_2_STATES_SQLITE_DDL
        .replace(" CHECK(id BETWEEN -2147483648 AND 2147483647)", "")
        .replace(" CHECK(car_id BETWEEN -32768 AND 32767)", "")
        .replace(" CHECK(state IN ('online', 'offline', 'asleep'))", "")
        .replace(
            " CHECK(start_date_pg_us = (-9223372036854775807 - 1) OR start_date_pg_us = 9223372036854775807 OR start_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999)",
            "",
        )
        .replace(
            " CHECK(end_date_pg_us IS NULL OR end_date_pg_us = (-9223372036854775807 - 1) OR end_date_pg_us = 9223372036854775807 OR end_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999)",
            "",
        );
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(THP2_2_CAR_SETTINGS_SQLITE_DDL)
        .unwrap();
    connection.execute_batch(THP2_2_CARS_SQLITE_DDL).unwrap();
    connection.execute_batch(&unchecked_states).unwrap();
    assert!(
        verify_projection_table_ddl(&connection, "states", THP2_2_STATES_SQLITE_DDL).is_err(),
        "the verifier must reject a states table recreated without physical checks"
    );

    let unchecked_updates = THP2_2_UPDATES_SQLITE_DDL
        .replace(" CHECK(id BETWEEN -2147483648 AND 2147483647)", "")
        .replace(" CHECK(car_id BETWEEN -32768 AND 32767)", "")
        .replace(
            " CHECK(start_date_pg_us = (-9223372036854775807 - 1) OR start_date_pg_us = 9223372036854775807 OR start_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999)",
            "",
        )
        .replace(
            " CHECK(end_date_pg_us IS NULL OR end_date_pg_us = (-9223372036854775807 - 1) OR end_date_pg_us = 9223372036854775807 OR end_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999)",
            "",
        )
        .replace(" CHECK(version IS NULL OR length(version) <= 255)", "");
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(THP2_2_CAR_SETTINGS_SQLITE_DDL)
        .unwrap();
    connection.execute_batch(THP2_2_CARS_SQLITE_DDL).unwrap();
    connection.execute_batch(&unchecked_updates).unwrap();
    assert!(
        verify_projection_table_ddl(&connection, "updates", THP2_2_UPDATES_SQLITE_DDL).is_err(),
        "the verifier must reject an updates table recreated without physical checks"
    );
}

#[test]
fn schema_2_2_verifier_rejects_table_metadata_and_foreign_key_tampering() {
    let table_names = [
        "addresses",
        "car_settings",
        "cars",
        "charges",
        "charging_processes",
        "drives",
        "geofences",
        "global_settings",
        "hub_pack_metadata",
        "positions",
        "states",
        "updates",
    ];
    for table in table_names {
        let temporary = crate::private_tempdir().unwrap();
        let source = snapshot_v2_2();
        let request = request_v2_2(&source);
        let built = ProjectionPackWriter::new(temporary.path().join("packs"))
            .write_full_snapshot_2_2(&request)
            .unwrap();
        let inspect = temporary.path().join("tampered.sqlite");
        fs::write(
            &inspect,
            zstd::stream::decode_all(File::open(&built.path).unwrap()).unwrap(),
        )
        .unwrap();
        let connection = Connection::open(&inspect).unwrap();
        connection
            .execute_batch(&format!("ALTER TABLE {table} ADD COLUMN unexpected TEXT"))
            .unwrap();
        drop(connection);
        assert!(
            verify_projection_sqlite_2_2(&inspect, &request, built.metadata.row_count).is_err(),
            "verifier accepted a changed {table} table"
        );
    }

    let temporary = crate::private_tempdir().unwrap();
    let source = snapshot_v2_2();
    let request = request_v2_2(&source);
    let built = ProjectionPackWriter::new(temporary.path().join("packs"))
        .write_full_snapshot_2_2(&request)
        .unwrap();
    let inspect = temporary.path().join("metadata.sqlite");
    fs::write(
        &inspect,
        zstd::stream::decode_all(File::open(&built.path).unwrap()).unwrap(),
    )
    .unwrap();
    let connection = Connection::open(&inspect).unwrap();
    connection
        .execute(
            "INSERT INTO hub_pack_metadata (key, value) VALUES ('unexpected', 'value')",
            [],
        )
        .unwrap();
    drop(connection);
    assert!(
        verify_projection_sqlite_2_2(&inspect, &request, built.metadata.row_count).is_err(),
        "verifier accepted an extra metadata key"
    );

    let connection = Connection::open(&inspect).unwrap();
    verify_projection_foreign_keys(&connection, "drives", &[])
        .expect("exact physical drives have no outgoing SQLite foreign keys");
    verify_projection_foreign_keys(&connection, "positions", &[])
        .expect("the V3 local positions schema intentionally has no SQLite foreign keys");
    verify_projection_foreign_keys(&connection, "charging_processes", &[])
        .expect("the V3 local charging-process schema intentionally has no SQLite foreign keys");
    verify_projection_foreign_keys(&connection, "charges", &[])
        .expect("the V3 local charges schema intentionally has no SQLite foreign keys");
    for (table, expected) in [
        ("cars", vec![("car_settings", "settings_id", "id")]),
        ("states", vec![("cars", "car_id", "id")]),
        ("updates", vec![("cars", "car_id", "id")]),
    ] {
        for missing_index in 0..expected.len() {
            let mut tampered = expected.clone();
            tampered.remove(missing_index);
            assert!(
                verify_projection_foreign_keys(&connection, table, &tampered).is_err(),
                "verifier accepted {table} with a missing foreign key"
            );
        }
    }
}

#[test]
fn schema_2_2_full_snapshot_is_deterministic_across_input_order() {
    let first_dir = crate::private_tempdir().unwrap();
    let second_dir = crate::private_tempdir().unwrap();
    let mut first_snapshot = snapshot_v2_2();
    let mut signed_extra_drive = first_snapshot.drives[0].clone();
    signed_extra_drive.id = i32::MIN;
    signed_extra_drive.start_address_id = None;
    signed_extra_drive.end_address_id = None;
    signed_extra_drive.start_geofence_id = None;
    signed_extra_drive.end_geofence_id = None;
    first_snapshot.drives.push(signed_extra_drive);
    let mut second_snapshot = first_snapshot.clone();
    second_snapshot.cars.reverse();
    second_snapshot.car_settings.reverse();
    second_snapshot.addresses.reverse();
    second_snapshot.geofences.reverse();
    second_snapshot.drives.reverse();
    second_snapshot.positions.reverse();
    second_snapshot.charging_processes.reverse();
    second_snapshot.charges.reverse();
    second_snapshot.states.reverse();
    second_snapshot.updates.reverse();

    let first = ProjectionPackWriter::new(first_dir.path().join("packs"))
        .write_full_snapshot_2_2(&request_v2_2(&first_snapshot))
        .unwrap();
    let second = ProjectionPackWriter::new(second_dir.path().join("packs"))
        .write_full_snapshot_2_2(&request_v2_2(&second_snapshot))
        .unwrap();
    assert_eq!(first.metadata.sha256, second.metadata.sha256);
    assert_eq!(
        fs::read(first.path).unwrap(),
        fs::read(second.path).unwrap()
    );
}

#[test]
fn schema_2_2_address_physical_bounds_and_source_widths_are_exact() {
    let source = snapshot_v2_2();
    assert!(validate_request_v2_2(&request_v2_2(&source), ProtocolLimits::default()).is_ok());

    let mut independent_coordinates = snapshot_v2_2();
    independent_coordinates.addresses[0].latitude_e6 = None;
    independent_coordinates.addresses[0].longitude_e6 =
        Some(ProjectionFixedNumericV2_2::Finite(999_999_999));
    independent_coordinates.addresses[0].osm_id = Some(i64::MIN);
    assert!(
        validate_request_v2_2(
            &request_v2_2(&independent_coordinates),
            ProtocolLimits::default()
        )
        .is_ok(),
        "address physical coordinates have no geography/pair policy and osm_id has no positivity rule"
    );

    let mut display_name_at_unicode_boundary = snapshot_v2_2();
    display_name_at_unicode_boundary.addresses[0].display_name = Some("é".repeat(512));
    assert!(
        validate_request_v2_2(
            &request_v2_2(&display_name_at_unicode_boundary),
            ProtocolLimits::default()
        )
        .is_ok(),
        "PostgreSQL varchar source widths count characters, not UTF-8 bytes"
    );

    let mut overlong_display_name = snapshot_v2_2();
    overlong_display_name.addresses[0].display_name = Some("x".repeat(513));
    assert!(matches!(
        validate_request_v2_2(
            &request_v2_2(&overlong_display_name),
            ProtocolLimits::default()
        ),
        Err(ProjectionPackError::Invalid(message))
            if message == "address.display_name exceeds its pinned source width"
    ));

    let mut overlong_component = snapshot_v2_2();
    overlong_component.addresses[0].country = Some("x".repeat(256));
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&overlong_component), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message))
            if message == "address.country exceeds its pinned source width"
    ));

    let mut invalid_inserted_at = snapshot_v2_2();
    invalid_inserted_at.addresses[0].inserted_at_pg_us = 1;
    assert!(matches!(
        validate_request_v2_2(
            &request_v2_2(&invalid_inserted_at),
            ProtocolLimits::default()
        ),
        Err(ProjectionPackError::Invalid(message))
            if message
                == "address.inserted_at_pg_us is outside the PostgreSQL timestamp(0) source domain"
    ));

    let mut invalid_updated_at = snapshot_v2_2();
    invalid_updated_at.addresses[0].updated_at_pg_us = -1;
    assert!(matches!(
        validate_request_v2_2(
            &request_v2_2(&invalid_updated_at),
            ProtocolLimits::default()
        ),
        Err(ProjectionPackError::Invalid(message))
            if message
                == "address.updated_at_pg_us is outside the PostgreSQL timestamp(0) source domain"
    ));

    let mut latitude_outside_source_range = snapshot_v2_2();
    latitude_outside_source_range.addresses[0].latitude_e6 =
        Some(ProjectionFixedNumericV2_2::Finite(100_000_000));
    assert!(matches!(
        validate_request_v2_2(
            &request_v2_2(&latitude_outside_source_range),
            ProtocolLimits::default()
        ),
        Err(ProjectionPackError::Invalid(message))
            if message == "address.latitude_e6 is outside its pinned source range"
    ));

    let mut longitude_outside_source_range = snapshot_v2_2();
    longitude_outside_source_range.addresses[0].longitude_e6 =
        Some(ProjectionFixedNumericV2_2::Finite(-1_000_000_000));
    assert!(matches!(
        validate_request_v2_2(
            &request_v2_2(&longitude_outside_source_range),
            ProtocolLimits::default()
        ),
        Err(ProjectionPackError::Invalid(message))
            if message == "address.longitude_e6 is outside its pinned source range"
    ));
}

#[test]
fn schema_2_2_geofence_physical_bounds_and_source_widths_are_exact() {
    let source = snapshot_v2_2();
    assert!(validate_request_v2_2(&request_v2_2(&source), ProtocolLimits::default()).is_ok());

    let mut nullable_not_applicable_nan = snapshot_v2_2();
    nullable_not_applicable_nan.geofences[0].name = String::new();
    nullable_not_applicable_nan.geofences[0].latitude_e6 = ProjectionFixedNumericV2_2::NaN;
    nullable_not_applicable_nan.geofences[0].cost_per_unit_e4 = None;
    assert!(
        validate_request_v2_2(
            &request_v2_2(&nullable_not_applicable_nan),
            ProtocolLimits::default()
        )
        .is_ok(),
        "empty varchar, numeric NaN, and nullable source numeric remain distinct physical values"
    );

    let mut unicode_boundary = snapshot_v2_2();
    unicode_boundary.geofences[0].name = "é".repeat(255);
    assert!(
        validate_request_v2_2(&request_v2_2(&unicode_boundary), ProtocolLimits::default()).is_ok(),
        "PostgreSQL varchar source widths count characters, not UTF-8 bytes"
    );

    let mut overlong_name = snapshot_v2_2();
    overlong_name.geofences[0].name = "x".repeat(256);
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&overlong_name), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message))
            if message == "geofence.name exceeds its pinned source width"
    ));

    let mut invalid_inserted_at = snapshot_v2_2();
    invalid_inserted_at.geofences[0].inserted_at_pg_us = 1;
    assert!(matches!(
        validate_request_v2_2(
            &request_v2_2(&invalid_inserted_at),
            ProtocolLimits::default()
        ),
        Err(ProjectionPackError::Invalid(message))
            if message
                == "geofence.inserted_at_pg_us is outside the PostgreSQL timestamp(0) source domain"
    ));

    let mut invalid_updated_at = snapshot_v2_2();
    invalid_updated_at.geofences[0].updated_at_pg_us = i64::MAX - 1;
    assert!(matches!(
        validate_request_v2_2(
            &request_v2_2(&invalid_updated_at),
            ProtocolLimits::default()
        ),
        Err(ProjectionPackError::Invalid(message))
            if message
                == "geofence.updated_at_pg_us is outside the PostgreSQL timestamp(0) source domain"
    ));

    let mut out_of_range_finite = snapshot_v2_2();
    out_of_range_finite.geofences[0].longitude_e6 =
        ProjectionFixedNumericV2_2::Finite(1_000_000_000);
    assert!(matches!(
        validate_request_v2_2(
            &request_v2_2(&out_of_range_finite),
            ProtocolLimits::default()
        ),
        Err(ProjectionPackError::Invalid(message))
            if message == "geofence.longitude_e6 is outside its pinned source range"
    ));
}

#[test]
fn schema_2_2_cars_and_car_settings_are_exact_selected_physical_rows() {
    let source = snapshot_v2_2();
    assert!(validate_request_v2_2(&request_v2_2(&source), ProtocolLimits::default()).is_ok());

    let mut nullable_source_text = snapshot_v2_2();
    nullable_source_text.cars[0].vin = None;
    nullable_source_text.cars[0].name = None;
    nullable_source_text.cars[0].model = None;
    assert!(
        validate_request_v2_2(
            &request_v2_2(&nullable_source_text),
            ProtocolLimits::default()
        )
        .is_ok(),
        "physical nullable cars text must not inherit legacy required names/models"
    );

    let mut source_int8_extremes = snapshot_v2_2();
    source_int8_extremes.cars[0].eid = i64::MIN;
    source_int8_extremes.cars[0].vid = i64::MAX;
    source_int8_extremes.cars[0].settings_id = i64::MIN;
    source_int8_extremes.car_settings[0].id = i64::MIN;
    assert!(
        validate_request_v2_2(
            &request_v2_2(&source_int8_extremes),
            ProtocolLimits::default()
        )
        .is_ok(),
        "physical source bigint values have no inferred positivity policy"
    );

    let mut unicode_varchars = snapshot_v2_2();
    unicode_varchars.cars[0].model = Some("é".repeat(255));
    unicode_varchars.cars[0].marketing_name = Some("é".repeat(255));
    assert!(
        validate_request_v2_2(&request_v2_2(&unicode_varchars), ProtocolLimits::default()).is_ok(),
        "source varchar widths count characters rather than UTF-8 bytes"
    );

    let mut overlong_model = snapshot_v2_2();
    overlong_model.cars[0].model = Some("x".repeat(256));
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&overlong_model), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message))
            if message == "car.model exceeds its pinned source width"
    ));

    let mut overlong_marketing_name = snapshot_v2_2();
    overlong_marketing_name.cars[0].marketing_name = Some("x".repeat(256));
    assert!(matches!(
        validate_request_v2_2(
            &request_v2_2(&overlong_marketing_name),
            ProtocolLimits::default()
        ),
        Err(ProjectionPackError::Invalid(message))
            if message == "car.marketing_name exceeds its pinned source width"
    ));

    let mut generic_text_cap = snapshot_v2_2();
    generic_text_cap.cars[0].vin = Some("x".repeat(MAX_TEXT_BYTES + 1));
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&generic_text_cap), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message)) if message == "car.vin is unsafe or too large"
    ));

    for (label, efficiency) in [
        ("negative-zero", -0.0_f64),
        ("positive-infinity", f64::INFINITY),
        ("negative-infinity", f64::NEG_INFINITY),
        ("nan-payload", f64::from_bits(0x7ff8_0000_0000_00a5)),
    ] {
        let temporary = crate::private_tempdir().unwrap();
        let mut bit_exact = snapshot_v2_2();
        bit_exact.cars[0].efficiency = Some(efficiency);
        let built = ProjectionPackWriter::new(temporary.path().join(label))
            .write_full_snapshot_2_2(&request_v2_2(&bit_exact))
            .expect("FLOAT8 bit pattern is an exact physical value");
        let inspect = temporary.path().join("efficiency.sqlite");
        fs::write(
            &inspect,
            zstd::stream::decode_all(File::open(&built.path).unwrap()).unwrap(),
        )
        .unwrap();
        let connection = Connection::open(inspect).unwrap();
        let bits: Vec<u8> = connection
            .query_row("SELECT efficiency FROM cars WHERE id = 10", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            u64::from_be_bytes(bits.try_into().expect("eight-byte FLOAT8 payload")),
            efficiency.to_bits(),
            "{label} must remain bit-exact"
        );
    }

    let mut invalid_inserted_at = snapshot_v2_2();
    invalid_inserted_at.cars[0].inserted_at_pg_us = 1;
    assert!(matches!(
        validate_request_v2_2(
            &request_v2_2(&invalid_inserted_at),
            ProtocolLimits::default()
        ),
        Err(ProjectionPackError::Invalid(message))
            if message
                == "car.inserted_at_pg_us is outside the PostgreSQL timestamp(0) source domain"
    ));

    let mut invalid_updated_at = snapshot_v2_2();
    invalid_updated_at.cars[0].updated_at_pg_us = -1;
    assert!(matches!(
        validate_request_v2_2(
            &request_v2_2(&invalid_updated_at),
            ProtocolLimits::default()
        ),
        Err(ProjectionPackError::Invalid(message))
            if message
                == "car.updated_at_pg_us is outside the PostgreSQL timestamp(0) source domain"
    ));

    let mut wrong_selected_car = snapshot_v2_2();
    wrong_selected_car.cars[0].id = 11;
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&wrong_selected_car), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message)) if message == "selected_car_id does not match car.id"
    ));

    let mut missing_settings = snapshot_v2_2();
    missing_settings.car_settings.clear();
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&missing_settings), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message)) if message == "one vehicle projection must contain exactly one car_settings row"
    ));

    let mut extra_settings = snapshot_v2_2();
    extra_settings
        .car_settings
        .push(extra_settings.car_settings[0].clone());
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&extra_settings), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message)) if message == "one vehicle projection must contain exactly one car_settings row"
    ));

    let mut mismatched_settings = snapshot_v2_2();
    mismatched_settings.car_settings[0].id = 501;
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&mismatched_settings), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message))
            if message == "car.settings_id does not match the selected car_settings.id"
    ));
}

#[test]
fn schema_2_2_states_and_updates_preserve_raw_physical_values() {
    let source = snapshot_v2_2();
    assert!(validate_request_v2_2(&request_v2_2(&source), ProtocolLimits::default()).is_ok());
    assert_eq!(source.states[0].id, i32::MIN);
    assert_eq!(source.states[0].start_date_pg_us, i64::MIN);
    assert_eq!(source.states[0].end_date_pg_us, None);
    assert_eq!(source.updates[0].id, i32::MAX);
    assert_eq!(source.updates[0].start_date_pg_us, i64::MAX);
    assert_eq!(source.updates[0].end_date_pg_us, Some(i64::MIN));

    for value in [
        i64::MIN,
        POSTGRES_TIMESTAMP_FINITE_MIN_US,
        POSTGRES_TIMESTAMP_FINITE_END_EXCLUSIVE_US - 1,
        i64::MAX,
    ] {
        let mut boundary = snapshot_v2_2();
        boundary.states[0].start_date_pg_us = value;
        boundary.states[0].end_date_pg_us = Some(value);
        boundary.updates[0].start_date_pg_us = value;
        boundary.updates[0].end_date_pg_us = Some(value);
        assert!(
            validate_request_v2_2(&request_v2_2(&boundary), ProtocolLimits::default()).is_ok(),
            "valid PostgreSQL timestamp boundary {value} must be retained"
        );
    }
    for value in [
        i64::MIN + 1,
        POSTGRES_TIMESTAMP_FINITE_MIN_US - 1,
        POSTGRES_TIMESTAMP_FINITE_END_EXCLUSIVE_US,
        i64::MAX - 1,
    ] {
        let mut invalid_timestamp = snapshot_v2_2();
        invalid_timestamp.states[0].start_date_pg_us = value;
        assert!(matches!(
            validate_request_v2_2(
                &request_v2_2(&invalid_timestamp),
                ProtocolLimits::default()
            ),
            Err(ProjectionPackError::Invalid(message))
                if message == "state.start_date_pg_us is outside the PostgreSQL timestamp source domain"
        ));
    }

    let mut nullable = snapshot_v2_2();
    nullable.updates[0].end_date_pg_us = None;
    nullable.updates[0].version = None;
    assert!(
        validate_request_v2_2(&request_v2_2(&nullable), ProtocolLimits::default()).is_ok(),
        "nullable source end/version values must not inherit legacy completion policy"
    );

    let mut empty_version = snapshot_v2_2();
    empty_version.updates[0].version = Some(String::new());
    assert!(
        validate_request_v2_2(&request_v2_2(&empty_version), ProtocolLimits::default()).is_ok(),
        "empty source varchar is distinct from a trimmed/defaulted value"
    );

    let mut unicode_boundary = snapshot_v2_2();
    unicode_boundary.updates[0].version = Some("é".repeat(255));
    assert!(
        validate_request_v2_2(&request_v2_2(&unicode_boundary), ProtocolLimits::default()).is_ok(),
        "source varchar widths count characters rather than UTF-8 bytes"
    );

    let mut overlong = snapshot_v2_2();
    overlong.updates[0].version = Some("x".repeat(256));
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&overlong), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message))
            if message == "update.version exceeds its pinned source width"
    ));

    let mut unsafe_text = snapshot_v2_2();
    unsafe_text.updates[0].version = Some("x".repeat(MAX_TEXT_BYTES + 1));
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&unsafe_text), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message))
            if message == "update.version is unsafe or too large"
    ));

    let mut wrong_car = snapshot_v2_2();
    wrong_car.states[0].car_id = 11;
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&wrong_car), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message))
            if message == "state.car_id does not match selected car"
    ));

    let mut duplicate = snapshot_v2_2();
    duplicate.states.push(duplicate.states[0].clone());
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&duplicate), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message)) if message == "state.id is duplicated"
    ));

    let mut duplicate_update = snapshot_v2_2();
    duplicate_update
        .updates
        .push(duplicate_update.updates[0].clone());
    assert!(matches!(
        validate_request_v2_2(&request_v2_2(&duplicate_update), ProtocolLimits::default()),
        Err(ProjectionPackError::Invalid(message)) if message == "update.id is duplicated"
    ));
}

#[test]
fn schema_2_2_keeps_source_refs_soft_and_enforces_charge_process_scope() {
    let temporary = crate::private_tempdir().unwrap();

    // Exact source reference values stay soft locally: source targets can
    // be extant but omitted from a selected-car subset, so V3 does not
    // invent SQLite closure relations.
    let packs = temporary.path().join("soft-source-refs");
    let mut soft_refs = snapshot_v2_2();
    soft_refs.positions.clear();
    soft_refs.addresses.clear();
    soft_refs.geofences.clear();
    soft_refs.charging_processes[0].position_id = i32::MIN;
    soft_refs.charging_processes[0].address_id = Some(i32::MIN);
    soft_refs.charging_processes[0].geofence_id = Some(i32::MAX);
    soft_refs.drives[0].start_position_id = Some(i32::MIN);
    soft_refs.drives[0].end_position_id = Some(i32::MAX);
    soft_refs.drives[0].start_address_id = Some(i32::MIN);
    soft_refs.drives[0].end_address_id = Some(i32::MAX);
    soft_refs.drives[0].start_geofence_id = Some(i32::MIN);
    soft_refs.drives[0].end_geofence_id = Some(i32::MAX);
    ProjectionPackWriter::new(&packs)
        .write_full_snapshot_2_2(&request_v2_2(&soft_refs))
        .expect("raw signed source references remain physical selected-subset values");

    let packs = temporary.path().join("charge-process-closure");
    let mut missing_process = snapshot_v2_2();
    missing_process.charges[0].charging_process_id = i32::MIN;
    let error = ProjectionPackWriter::new(&packs)
        .write_full_snapshot_2_2(&request_v2_2(&missing_process))
        .expect_err("selected-car charges require their loaded source process");
    assert!(matches!(
        error,
        ProjectionPackError::Invalid(message)
            if message == "charge.charging_process_id is not present in this local physical slice"
    ));
    assert!(!packs.exists());

    let packs = temporary.path().join("unreferenced-relation");
    let mut unreferenced = snapshot_v2_2();
    unreferenced.addresses.push(ProjectionAddressV2_2 {
        id: 102,
        display_name: Some("not selected-car-referenced".into()),
        latitude_e6: None,
        longitude_e6: None,
        name: None,
        house_number: None,
        road: None,
        neighbourhood: None,
        city: None,
        county: None,
        postcode: None,
        state: None,
        state_district: None,
        country: None,
        inserted_at_pg_us: 1_700_000_400_000_000,
        updated_at_pg_us: 1_700_000_500_000_000,
        osm_id: None,
        osm_type: None,
    });
    let error = ProjectionPackWriter::new(&packs)
        .write_full_snapshot_2_2(&request_v2_2(&unreferenced))
        .expect_err("account-wide relation rows are out of scope");
    assert!(matches!(
        error,
        ProjectionPackError::Invalid(message)
            if message.contains("address 102 is not referenced by the selected car")
    ));
    assert!(!packs.exists());
}
