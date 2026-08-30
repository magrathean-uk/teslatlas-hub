// SPDX-License-Identifier: AGPL-3.0-only

fn insert_metadata_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    request: &ProjectionPackRequestV2_2<'_>,
    row_count: u64,
) -> Result<(), ProjectionPackError> {
    let values = [
        ("protocol", "teslatlas-sync".to_owned()),
        ("pack_format", "hub_projection_sqlite".to_owned()),
        ("schema_major", HUB_PROJECTION_SCHEMA_V3.major.to_string()),
        ("schema_minor", HUB_PROJECTION_SCHEMA_V3.minor.to_string()),
        ("pack_id", request.pack_id.to_string()),
        ("snapshot_id", request.snapshot_id.to_string()),
        ("ordinal", request.ordinal.to_string()),
        ("mode", "full_snapshot".to_owned()),
        ("schema_support", "full_snapshot_only".to_owned()),
        ("publication_scope", "local_validation_only".to_owned()),
        ("ledger_state", "draft_blocked".to_owned()),
        (
            "ledger_slice",
            "settings+car_settings+cars+drives+positions+charging_processes+charges+addresses+geofences+states+updates".to_owned(),
        ),
        ("mapped_fields", THP2_2_MAPPED_FIELD_COUNT.to_string()),
        (
            "unreconciled_fields",
            THP2_2_UNRECONCILED_FIELD_COUNT.to_string(),
        ),
        ("source_revision", TESLAMATE_V4_SOURCE_REVISION.to_owned()),
        (
            "migration_set_sha256",
            TESLAMATE_V4_MIGRATION_SET_SHA256.to_owned(),
        ),
        (
            "car_settings_slice_sha256",
            thp2_2_car_settings_slice_sha256(),
        ),
        (
            "settings_slice_sha256",
            thp2_2_global_settings_slice_sha256(),
        ),
        (
            "cars_efficiency_encoding",
            THP2_2_CARS_EFFICIENCY_ENCODING.to_owned(),
        ),
        (
            "fixed_numeric_encoding",
            THP2_2_FIXED_NUMERIC_ENCODING.to_owned(),
        ),
        (
            "drives_float_encoding",
            THP2_2_DRIVES_FLOAT_ENCODING.to_owned(),
        ),
        (
            "positions_odometer_encoding",
            THP2_2_POSITIONS_ODOMETER_ENCODING.to_owned(),
        ),
        (
            "positions_relation_scope",
            THP2_2_POSITIONS_RELATION_SCOPE.to_owned(),
        ),
        (
            "charging_boolean_encoding",
            THP2_2_CHARGING_TRI_STATE_BOOL_ENCODING.to_owned(),
        ),
        (
            "charges_relation_scope",
            THP2_2_CHARGES_RELATION_SCOPE.to_owned(),
        ),
        ("cars_slice_sha256", thp2_2_cars_slice_sha256()),
        ("drives_slice_sha256", thp2_2_drives_slice_sha256()),
        ("positions_slice_sha256", thp2_2_positions_slice_sha256()),
        (
            "charging_processes_slice_sha256",
            thp2_2_charging_processes_slice_sha256(),
        ),
        ("charges_slice_sha256", thp2_2_charges_slice_sha256()),
        ("address_slice_sha256", thp2_2_address_slice_sha256()),
        ("geofence_slice_sha256", thp2_2_geofence_slice_sha256()),
        (
            "postgres_timestamp_encoding",
            THP2_2_POSTGRES_TIMESTAMP_ENCODING.to_owned(),
        ),
        (
            "postgres_timestamp_0_encoding",
            THP2_2_POSTGRES_TIMESTAMP_0_ENCODING.to_owned(),
        ),
        ("states_slice_sha256", thp2_2_states_slice_sha256()),
        ("updates_slice_sha256", thp2_2_updates_slice_sha256()),
        ("reconciliation", "not_run".to_owned()),
        (
            "installation_id",
            request.binding.installation_id.to_string(),
        ),
        ("account_id", request.binding.account_id.to_string()),
        ("vehicle_id", request.binding.vehicle_id.to_string()),
        ("generation", request.binding.generation.to_string()),
        (
            "selected_car_id",
            request.binding.selected_car_id.to_string(),
        ),
        ("base_sequence", request.sequence.from_exclusive.to_string()),
        ("head_sequence", request.sequence.to_inclusive.to_string()),
        ("row_count", row_count.to_string()),
    ];
    let mut statement = transaction
        .prepare_cached("INSERT INTO hub_pack_metadata (key, value) VALUES (?1, ?2)")
        .map_err(ProjectionPackError::Prepare)?;
    for (key, value) in values {
        statement
            .execute(params![key, value])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_states_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionStateV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO states (id, car_id, state, start_date_pg_us, end_date_pg_us)\n             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.car_id,
                row.state.as_str(),
                row.start_date_pg_us,
                row.end_date_pg_us,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_updates_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionUpdateV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO updates (id, car_id, start_date_pg_us, end_date_pg_us, version)\n             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.car_id,
                row.start_date_pg_us,
                row.end_date_pg_us,
                row.version,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_addresses_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionAddressV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO addresses (
                id, display_name, latitude_e6, latitude_e6_is_nan, longitude_e6,
                longitude_e6_is_nan, name, house_number, road,
                neighbourhood, city, county, postcode, state, state_district, country,
                inserted_at_pg_us, updated_at_pg_us, osm_id, osm_type
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                ?17, ?18, ?19, ?20
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        let (latitude_e6, latitude_e6_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.latitude_e6);
        let (longitude_e6, longitude_e6_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.longitude_e6);
        statement
            .execute(params![
                row.id,
                row.display_name,
                latitude_e6,
                latitude_e6_is_nan,
                longitude_e6,
                longitude_e6_is_nan,
                row.name,
                row.house_number,
                row.road,
                row.neighbourhood,
                row.city,
                row.county,
                row.postcode,
                row.state,
                row.state_district,
                row.country,
                row.inserted_at_pg_us,
                row.updated_at_pg_us,
                row.osm_id,
                row.osm_type,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_geofences_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionGeofenceV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO geofences (
                id, name, latitude_e6, latitude_e6_is_nan, longitude_e6, longitude_e6_is_nan,
                radius, billing_type, cost_per_unit_e4, cost_per_unit_e4_is_nan, session_fee_e2,
                session_fee_e2_is_nan, inserted_at_pg_us, updated_at_pg_us
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        let (latitude_e6, latitude_e6_is_nan) = row.latitude_e6.sqlite_parts();
        let (longitude_e6, longitude_e6_is_nan) = row.longitude_e6.sqlite_parts();
        let (cost_per_unit_e4, cost_per_unit_e4_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.cost_per_unit_e4);
        let (session_fee_e2, session_fee_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.session_fee_e2);
        statement
            .execute(params![
                row.id,
                row.name,
                latitude_e6,
                latitude_e6_is_nan,
                longitude_e6,
                longitude_e6_is_nan,
                row.radius,
                row.billing_type.as_str(),
                cost_per_unit_e4,
                cost_per_unit_e4_is_nan,
                session_fee_e2,
                session_fee_e2_is_nan,
                row.inserted_at_pg_us,
                row.updated_at_pg_us,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_drives_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionDriveV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|value| value.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO drives (
                id, car_id, start_date_pg_us, end_date_pg_us, start_position_id,
                end_position_id, start_address_id, end_address_id, start_geofence_id,
                end_geofence_id, outside_temp_avg_e1, outside_temp_avg_e1_is_nan,
                inside_temp_avg_e1, inside_temp_avg_e1_is_nan, speed_max, power_max,
                power_min, start_ideal_range_km_e2, start_ideal_range_km_e2_is_nan,
                end_ideal_range_km_e2, end_ideal_range_km_e2_is_nan,
                start_rated_range_km_e2, start_rated_range_km_e2_is_nan,
                end_rated_range_km_e2, end_rated_range_km_e2_is_nan, start_km_f64_be,
                end_km_f64_be, distance_f64_be, duration_min, ascent, descent
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26,
                ?27, ?28, ?29, ?30, ?31
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        let (outside_temp_avg_e1, outside_temp_avg_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.outside_temp_avg_e1);
        let (inside_temp_avg_e1, inside_temp_avg_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.inside_temp_avg_e1);
        let (start_ideal_range_km_e2, start_ideal_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.start_ideal_range_km_e2);
        let (end_ideal_range_km_e2, end_ideal_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.end_ideal_range_km_e2);
        let (start_rated_range_km_e2, start_rated_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.start_rated_range_km_e2);
        let (end_rated_range_km_e2, end_rated_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.end_rated_range_km_e2);
        statement
            .execute(params![
                row.id,
                row.car_id,
                row.start_date_pg_us,
                row.end_date_pg_us,
                row.start_position_id,
                row.end_position_id,
                row.start_address_id,
                row.end_address_id,
                row.start_geofence_id,
                row.end_geofence_id,
                outside_temp_avg_e1,
                outside_temp_avg_e1_is_nan,
                inside_temp_avg_e1,
                inside_temp_avg_e1_is_nan,
                row.speed_max,
                row.power_max,
                row.power_min,
                start_ideal_range_km_e2,
                start_ideal_range_km_e2_is_nan,
                end_ideal_range_km_e2,
                end_ideal_range_km_e2_is_nan,
                start_rated_range_km_e2,
                start_rated_range_km_e2_is_nan,
                end_rated_range_km_e2,
                end_rated_range_km_e2_is_nan,
                row.start_km.map(|value| value.to_be_bytes().to_vec()),
                row.end_km.map(|value| value.to_be_bytes().to_vec()),
                row.distance.map(|value| value.to_be_bytes().to_vec()),
                row.duration_min,
                row.ascent,
                row.descent,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_positions_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionPositionV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO positions (
                id, car_id, drive_id, date_pg_us, latitude_e6, latitude_e6_is_nan,
                longitude_e6, longitude_e6_is_nan, elevation, speed, power, odometer_f64_be,
                ideal_battery_range_km_e2, ideal_battery_range_km_e2_is_nan,
                est_battery_range_km_e2, est_battery_range_km_e2_is_nan,
                rated_battery_range_km_e2, rated_battery_range_km_e2_is_nan,
                battery_level, usable_battery_level, battery_heater, battery_heater_on,
                battery_heater_no_power, outside_temp_e1, outside_temp_e1_is_nan,
                inside_temp_e1, inside_temp_e1_is_nan, fan_status, driver_temp_setting_e1,
                driver_temp_setting_e1_is_nan, passenger_temp_setting_e1,
                passenger_temp_setting_e1_is_nan, is_climate_on, is_rear_defroster_on,
                is_front_defroster_on, tpms_pressure_fl_e1, tpms_pressure_fl_e1_is_nan,
                tpms_pressure_fr_e1, tpms_pressure_fr_e1_is_nan, tpms_pressure_rl_e1,
                tpms_pressure_rl_e1_is_nan, tpms_pressure_rr_e1, tpms_pressure_rr_e1_is_nan
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30,
                ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40, ?41, ?42, ?43
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        let (latitude_e6, latitude_e6_is_nan) = row.latitude_e6.sqlite_parts();
        let (longitude_e6, longitude_e6_is_nan) = row.longitude_e6.sqlite_parts();
        let (ideal_battery_range_km_e2, ideal_battery_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.ideal_battery_range_km_e2);
        let (est_battery_range_km_e2, est_battery_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.est_battery_range_km_e2);
        let (rated_battery_range_km_e2, rated_battery_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.rated_battery_range_km_e2);
        let (outside_temp_e1, outside_temp_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.outside_temp_e1);
        let (inside_temp_e1, inside_temp_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.inside_temp_e1);
        let (driver_temp_setting_e1, driver_temp_setting_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.driver_temp_setting_e1);
        let (passenger_temp_setting_e1, passenger_temp_setting_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.passenger_temp_setting_e1);
        let (tpms_pressure_fl_e1, tpms_pressure_fl_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.tpms_pressure_fl_e1);
        let (tpms_pressure_fr_e1, tpms_pressure_fr_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.tpms_pressure_fr_e1);
        let (tpms_pressure_rl_e1, tpms_pressure_rl_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.tpms_pressure_rl_e1);
        let (tpms_pressure_rr_e1, tpms_pressure_rr_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.tpms_pressure_rr_e1);
        statement
            .execute(params![
                row.id,
                row.car_id,
                row.drive_id,
                row.date_pg_us,
                latitude_e6,
                latitude_e6_is_nan,
                longitude_e6,
                longitude_e6_is_nan,
                row.elevation,
                row.speed,
                row.power,
                row.odometer.map(|value| value.to_be_bytes().to_vec()),
                ideal_battery_range_km_e2,
                ideal_battery_range_km_e2_is_nan,
                est_battery_range_km_e2,
                est_battery_range_km_e2_is_nan,
                rated_battery_range_km_e2,
                rated_battery_range_km_e2_is_nan,
                row.battery_level,
                row.usable_battery_level,
                bool_as_sql(row.battery_heater),
                bool_as_sql(row.battery_heater_on),
                bool_as_sql(row.battery_heater_no_power),
                outside_temp_e1,
                outside_temp_e1_is_nan,
                inside_temp_e1,
                inside_temp_e1_is_nan,
                row.fan_status,
                driver_temp_setting_e1,
                driver_temp_setting_e1_is_nan,
                passenger_temp_setting_e1,
                passenger_temp_setting_e1_is_nan,
                bool_as_sql(row.is_climate_on),
                bool_as_sql(row.is_rear_defroster_on),
                bool_as_sql(row.is_front_defroster_on),
                tpms_pressure_fl_e1,
                tpms_pressure_fl_e1_is_nan,
                tpms_pressure_fr_e1,
                tpms_pressure_fr_e1_is_nan,
                tpms_pressure_rl_e1,
                tpms_pressure_rl_e1_is_nan,
                tpms_pressure_rr_e1,
                tpms_pressure_rr_e1_is_nan,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_charging_processes_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionChargingProcessV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO charging_processes (
                id, car_id, position_id, address_id, geofence_id, start_date_pg_us,
                end_date_pg_us, charge_energy_added_e2, charge_energy_added_e2_is_nan,
                charge_energy_used_e2, charge_energy_used_e2_is_nan,
                start_ideal_range_km_e2, start_ideal_range_km_e2_is_nan,
                end_ideal_range_km_e2, end_ideal_range_km_e2_is_nan,
                start_rated_range_km_e2, start_rated_range_km_e2_is_nan,
                end_rated_range_km_e2, end_rated_range_km_e2_is_nan,
                start_battery_level, end_battery_level, duration_min,
                outside_temp_avg_e1, outside_temp_avg_e1_is_nan, cost_e2, cost_e2_is_nan
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        let (charge_energy_added_e2, charge_energy_added_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.charge_energy_added_e2);
        let (charge_energy_used_e2, charge_energy_used_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.charge_energy_used_e2);
        let (start_ideal_range_km_e2, start_ideal_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.start_ideal_range_km_e2);
        let (end_ideal_range_km_e2, end_ideal_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.end_ideal_range_km_e2);
        let (start_rated_range_km_e2, start_rated_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.start_rated_range_km_e2);
        let (end_rated_range_km_e2, end_rated_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.end_rated_range_km_e2);
        let (outside_temp_avg_e1, outside_temp_avg_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.outside_temp_avg_e1);
        let (cost_e2, cost_e2_is_nan) = optional_fixed_numeric_sqlite_parts(row.cost_e2);
        statement
            .execute(params![
                row.id,
                row.car_id,
                row.position_id,
                row.address_id,
                row.geofence_id,
                row.start_date_pg_us,
                row.end_date_pg_us,
                charge_energy_added_e2,
                charge_energy_added_e2_is_nan,
                charge_energy_used_e2,
                charge_energy_used_e2_is_nan,
                start_ideal_range_km_e2,
                start_ideal_range_km_e2_is_nan,
                end_ideal_range_km_e2,
                end_ideal_range_km_e2_is_nan,
                start_rated_range_km_e2,
                start_rated_range_km_e2_is_nan,
                end_rated_range_km_e2,
                end_rated_range_km_e2_is_nan,
                row.start_battery_level,
                row.end_battery_level,
                row.duration_min,
                outside_temp_avg_e1,
                outside_temp_avg_e1_is_nan,
                cost_e2,
                cost_e2_is_nan,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_charges_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionChargeV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO charges (
                id, charging_process_id, date_pg_us, battery_heater, battery_heater_on,
                battery_heater_no_power, battery_level, usable_battery_level,
                charge_energy_added_e2, charge_energy_added_e2_is_nan,
                charger_actual_current, charger_phases, charger_pilot_current, charger_power,
                charger_voltage, conn_charge_cable, fast_charger_present, fast_charger_brand,
                fast_charger_type, ideal_battery_range_km_e2,
                ideal_battery_range_km_e2_is_nan, rated_battery_range_km_e2,
                rated_battery_range_km_e2_is_nan, not_enough_power_to_heat, outside_temp_e1,
                outside_temp_e1_is_nan
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        let (charge_energy_added_e2, charge_energy_added_e2_is_nan) =
            row.charge_energy_added_e2.sqlite_parts();
        let (ideal_battery_range_km_e2, ideal_battery_range_km_e2_is_nan) =
            row.ideal_battery_range_km_e2.sqlite_parts();
        let (rated_battery_range_km_e2, rated_battery_range_km_e2_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.rated_battery_range_km_e2);
        let (outside_temp_e1, outside_temp_e1_is_nan) =
            optional_fixed_numeric_sqlite_parts(row.outside_temp_e1);
        statement
            .execute(params![
                row.id,
                row.charging_process_id,
                row.date_pg_us,
                bool_as_sql(row.battery_heater),
                bool_as_sql(row.battery_heater_on),
                bool_as_sql(row.battery_heater_no_power),
                row.battery_level,
                row.usable_battery_level,
                charge_energy_added_e2,
                charge_energy_added_e2_is_nan,
                row.charger_actual_current,
                row.charger_phases,
                row.charger_pilot_current,
                row.charger_power,
                row.charger_voltage,
                row.conn_charge_cable,
                bool_as_sql(row.fast_charger_present),
                row.fast_charger_brand,
                row.fast_charger_type,
                ideal_battery_range_km_e2,
                ideal_battery_range_km_e2_is_nan,
                rated_battery_range_km_e2,
                rated_battery_range_km_e2_is_nan,
                bool_as_sql(row.not_enough_power_to_heat),
                outside_temp_e1,
                outside_temp_e1_is_nan,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}
