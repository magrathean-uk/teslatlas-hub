// SPDX-License-Identifier: AGPL-3.0-only

fn verify_projection_sqlite_2_2(
    path: &Path,
    request: &ProjectionPackRequestV2_2<'_>,
    row_count: u64,
) -> Result<(), ProjectionPackError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(ProjectionPackError::OpenSqlite)?;
    let application_id: u32 = connection
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if application_id != SQLITE_HUB_PROJECTION_APPLICATION_ID {
        return Err(invalid("schema 2.2 SQLite application_id is invalid"));
    }
    let user_version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if user_version != HUB_PROJECTION_SCHEMA_V3.sqlite_user_version() {
        return Err(invalid("schema 2.2 SQLite user_version is invalid"));
    }
    let mut tables = connection
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .map_err(ProjectionPackError::IntegrityCheck)?;
    let table_names = tables
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(ProjectionPackError::IntegrityCheck)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(ProjectionPackError::IntegrityCheck)?;
    let expected_tables = vec![
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
    if table_names != expected_tables {
        return Err(invalid("schema 2.2 SQLite table layout is invalid"));
    }
    for (table, without_rowid, expected_columns) in [
        (
            "hub_pack_metadata",
            false,
            &[("key", "TEXT", true, true), ("value", "TEXT", true, false)][..],
        ),
        (
            "cars",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("eid", "INTEGER", true, false),
                ("vid", "INTEGER", true, false),
                ("vin", "TEXT", false, false),
                ("name", "TEXT", false, false),
                ("model", "TEXT", false, false),
                ("efficiency", "BLOB", false, false),
                ("trim_badging", "TEXT", false, false),
                ("marketing_name", "TEXT", false, false),
                ("exterior_color", "TEXT", false, false),
                ("wheel_type", "TEXT", false, false),
                ("spoiler_type", "TEXT", false, false),
                ("display_priority", "INTEGER", true, false),
                ("inserted_at_pg_us", "INTEGER", true, false),
                ("updated_at_pg_us", "INTEGER", true, false),
                ("settings_id", "INTEGER", true, false),
            ][..],
        ),
        (
            "car_settings",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("suspend_min", "INTEGER", true, false),
                ("suspend_after_idle_min", "INTEGER", true, false),
                ("req_not_unlocked", "INTEGER", true, false),
                ("free_supercharging", "INTEGER", true, false),
                ("use_streaming_api", "INTEGER", true, false),
                ("enabled", "INTEGER", true, false),
                ("lfp_battery", "INTEGER", true, false),
            ][..],
        ),
        (
            "addresses",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("display_name", "TEXT", false, false),
                ("latitude_e6", "INTEGER", false, false),
                ("latitude_e6_is_nan", "INTEGER", true, false),
                ("longitude_e6", "INTEGER", false, false),
                ("longitude_e6_is_nan", "INTEGER", true, false),
                ("name", "TEXT", false, false),
                ("house_number", "TEXT", false, false),
                ("road", "TEXT", false, false),
                ("neighbourhood", "TEXT", false, false),
                ("city", "TEXT", false, false),
                ("county", "TEXT", false, false),
                ("postcode", "TEXT", false, false),
                ("state", "TEXT", false, false),
                ("state_district", "TEXT", false, false),
                ("country", "TEXT", false, false),
                ("inserted_at_pg_us", "INTEGER", true, false),
                ("updated_at_pg_us", "INTEGER", true, false),
                ("osm_id", "INTEGER", false, false),
                ("osm_type", "TEXT", false, false),
            ][..],
        ),
        (
            "geofences",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("name", "TEXT", true, false),
                ("latitude_e6", "INTEGER", false, false),
                ("latitude_e6_is_nan", "INTEGER", true, false),
                ("longitude_e6", "INTEGER", false, false),
                ("longitude_e6_is_nan", "INTEGER", true, false),
                ("radius", "INTEGER", true, false),
                ("billing_type", "TEXT", true, false),
                ("cost_per_unit_e4", "INTEGER", false, false),
                ("cost_per_unit_e4_is_nan", "INTEGER", true, false),
                ("session_fee_e2", "INTEGER", false, false),
                ("session_fee_e2_is_nan", "INTEGER", true, false),
                ("inserted_at_pg_us", "INTEGER", true, false),
                ("updated_at_pg_us", "INTEGER", true, false),
            ][..],
        ),
        (
            "global_settings",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("unit_of_length", "TEXT", true, false),
                ("unit_of_temperature", "TEXT", true, false),
                ("unit_of_pressure", "TEXT", true, false),
                ("preferred_range", "TEXT", true, false),
                ("base_url", "TEXT", false, false),
                ("grafana_url", "TEXT", false, false),
                ("language", "TEXT", true, false),
                ("theme_mode", "TEXT", true, false),
                ("inserted_at_pg_us", "INTEGER", true, false),
                ("updated_at_pg_us", "INTEGER", true, false),
            ][..],
        ),
        (
            "drives",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("car_id", "INTEGER", true, false),
                ("start_date_pg_us", "INTEGER", true, false),
                ("end_date_pg_us", "INTEGER", false, false),
                ("start_position_id", "INTEGER", false, false),
                ("end_position_id", "INTEGER", false, false),
                ("start_address_id", "INTEGER", false, false),
                ("end_address_id", "INTEGER", false, false),
                ("start_geofence_id", "INTEGER", false, false),
                ("end_geofence_id", "INTEGER", false, false),
                ("outside_temp_avg_e1", "INTEGER", false, false),
                ("outside_temp_avg_e1_is_nan", "INTEGER", true, false),
                ("inside_temp_avg_e1", "INTEGER", false, false),
                ("inside_temp_avg_e1_is_nan", "INTEGER", true, false),
                ("speed_max", "INTEGER", false, false),
                ("power_max", "INTEGER", false, false),
                ("power_min", "INTEGER", false, false),
                ("start_ideal_range_km_e2", "INTEGER", false, false),
                ("start_ideal_range_km_e2_is_nan", "INTEGER", true, false),
                ("end_ideal_range_km_e2", "INTEGER", false, false),
                ("end_ideal_range_km_e2_is_nan", "INTEGER", true, false),
                ("start_rated_range_km_e2", "INTEGER", false, false),
                ("start_rated_range_km_e2_is_nan", "INTEGER", true, false),
                ("end_rated_range_km_e2", "INTEGER", false, false),
                ("end_rated_range_km_e2_is_nan", "INTEGER", true, false),
                ("start_km_f64_be", "BLOB", false, false),
                ("end_km_f64_be", "BLOB", false, false),
                ("distance_f64_be", "BLOB", false, false),
                ("duration_min", "INTEGER", false, false),
                ("ascent", "INTEGER", false, false),
                ("descent", "INTEGER", false, false),
            ][..],
        ),
        (
            "charging_processes",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("car_id", "INTEGER", true, false),
                ("position_id", "INTEGER", true, false),
                ("address_id", "INTEGER", false, false),
                ("geofence_id", "INTEGER", false, false),
                ("start_date_pg_us", "INTEGER", true, false),
                ("end_date_pg_us", "INTEGER", false, false),
                ("charge_energy_added_e2", "INTEGER", false, false),
                ("charge_energy_added_e2_is_nan", "INTEGER", true, false),
                ("charge_energy_used_e2", "INTEGER", false, false),
                ("charge_energy_used_e2_is_nan", "INTEGER", true, false),
                ("start_ideal_range_km_e2", "INTEGER", false, false),
                ("start_ideal_range_km_e2_is_nan", "INTEGER", true, false),
                ("end_ideal_range_km_e2", "INTEGER", false, false),
                ("end_ideal_range_km_e2_is_nan", "INTEGER", true, false),
                ("start_rated_range_km_e2", "INTEGER", false, false),
                ("start_rated_range_km_e2_is_nan", "INTEGER", true, false),
                ("end_rated_range_km_e2", "INTEGER", false, false),
                ("end_rated_range_km_e2_is_nan", "INTEGER", true, false),
                ("start_battery_level", "INTEGER", false, false),
                ("end_battery_level", "INTEGER", false, false),
                ("duration_min", "INTEGER", false, false),
                ("outside_temp_avg_e1", "INTEGER", false, false),
                ("outside_temp_avg_e1_is_nan", "INTEGER", true, false),
                ("cost_e2", "INTEGER", false, false),
                ("cost_e2_is_nan", "INTEGER", true, false),
            ][..],
        ),
        (
            "charges",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("charging_process_id", "INTEGER", true, false),
                ("date_pg_us", "INTEGER", true, false),
                ("battery_heater", "INTEGER", false, false),
                ("battery_heater_on", "INTEGER", false, false),
                ("battery_heater_no_power", "INTEGER", false, false),
                ("battery_level", "INTEGER", false, false),
                ("usable_battery_level", "INTEGER", false, false),
                ("charge_energy_added_e2", "INTEGER", false, false),
                ("charge_energy_added_e2_is_nan", "INTEGER", true, false),
                ("charger_actual_current", "INTEGER", false, false),
                ("charger_phases", "INTEGER", false, false),
                ("charger_pilot_current", "INTEGER", false, false),
                ("charger_power", "INTEGER", true, false),
                ("charger_voltage", "INTEGER", false, false),
                ("conn_charge_cable", "TEXT", false, false),
                ("fast_charger_present", "INTEGER", false, false),
                ("fast_charger_brand", "TEXT", false, false),
                ("fast_charger_type", "TEXT", false, false),
                ("ideal_battery_range_km_e2", "INTEGER", false, false),
                ("ideal_battery_range_km_e2_is_nan", "INTEGER", true, false),
                ("rated_battery_range_km_e2", "INTEGER", false, false),
                ("rated_battery_range_km_e2_is_nan", "INTEGER", true, false),
                ("not_enough_power_to_heat", "INTEGER", false, false),
                ("outside_temp_e1", "INTEGER", false, false),
                ("outside_temp_e1_is_nan", "INTEGER", true, false),
            ][..],
        ),
        (
            "positions",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("car_id", "INTEGER", true, false),
                ("drive_id", "INTEGER", false, false),
                ("date_pg_us", "INTEGER", true, false),
                ("latitude_e6", "INTEGER", false, false),
                ("latitude_e6_is_nan", "INTEGER", true, false),
                ("longitude_e6", "INTEGER", false, false),
                ("longitude_e6_is_nan", "INTEGER", true, false),
                ("elevation", "INTEGER", false, false),
                ("speed", "INTEGER", false, false),
                ("power", "INTEGER", false, false),
                ("odometer_f64_be", "BLOB", false, false),
                ("ideal_battery_range_km_e2", "INTEGER", false, false),
                ("ideal_battery_range_km_e2_is_nan", "INTEGER", true, false),
                ("est_battery_range_km_e2", "INTEGER", false, false),
                ("est_battery_range_km_e2_is_nan", "INTEGER", true, false),
                ("rated_battery_range_km_e2", "INTEGER", false, false),
                ("rated_battery_range_km_e2_is_nan", "INTEGER", true, false),
                ("battery_level", "INTEGER", false, false),
                ("usable_battery_level", "INTEGER", false, false),
                ("battery_heater", "INTEGER", false, false),
                ("battery_heater_on", "INTEGER", false, false),
                ("battery_heater_no_power", "INTEGER", false, false),
                ("outside_temp_e1", "INTEGER", false, false),
                ("outside_temp_e1_is_nan", "INTEGER", true, false),
                ("inside_temp_e1", "INTEGER", false, false),
                ("inside_temp_e1_is_nan", "INTEGER", true, false),
                ("fan_status", "INTEGER", false, false),
                ("driver_temp_setting_e1", "INTEGER", false, false),
                ("driver_temp_setting_e1_is_nan", "INTEGER", true, false),
                ("passenger_temp_setting_e1", "INTEGER", false, false),
                ("passenger_temp_setting_e1_is_nan", "INTEGER", true, false),
                ("is_climate_on", "INTEGER", false, false),
                ("is_rear_defroster_on", "INTEGER", false, false),
                ("is_front_defroster_on", "INTEGER", false, false),
                ("tpms_pressure_fl_e1", "INTEGER", false, false),
                ("tpms_pressure_fl_e1_is_nan", "INTEGER", true, false),
                ("tpms_pressure_fr_e1", "INTEGER", false, false),
                ("tpms_pressure_fr_e1_is_nan", "INTEGER", true, false),
                ("tpms_pressure_rl_e1", "INTEGER", false, false),
                ("tpms_pressure_rl_e1_is_nan", "INTEGER", true, false),
                ("tpms_pressure_rr_e1", "INTEGER", false, false),
                ("tpms_pressure_rr_e1_is_nan", "INTEGER", true, false),
            ][..],
        ),
        (
            "states",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("car_id", "INTEGER", true, false),
                ("state", "TEXT", true, false),
                ("start_date_pg_us", "INTEGER", true, false),
                ("end_date_pg_us", "INTEGER", false, false),
            ][..],
        ),
        (
            "updates",
            true,
            &[
                ("id", "INTEGER", true, true),
                ("car_id", "INTEGER", true, false),
                ("start_date_pg_us", "INTEGER", true, false),
                ("end_date_pg_us", "INTEGER", false, false),
                ("version", "TEXT", false, false),
            ][..],
        ),
    ] {
        verify_projection_table_layout(&connection, table, without_rowid, expected_columns)?;
    }
    verify_projection_table_ddl(&connection, "car_settings", THP2_2_CAR_SETTINGS_SQLITE_DDL)?;
    verify_projection_table_ddl(&connection, "cars", THP2_2_CARS_SQLITE_DDL)?;
    verify_projection_table_ddl(&connection, "addresses", THP2_2_ADDRESSES_SQLITE_DDL)?;
    verify_projection_table_ddl(&connection, "geofences", THP2_2_GEOFENCES_SQLITE_DDL)?;
    verify_projection_table_ddl(
        &connection,
        "global_settings",
        THP2_2_GLOBAL_SETTINGS_SQLITE_DDL,
    )?;
    verify_projection_table_ddl(&connection, "drives", THP2_2_DRIVES_SQLITE_DDL)?;
    verify_projection_table_ddl(&connection, "positions", THP2_2_POSITIONS_SQLITE_DDL)?;
    verify_projection_table_ddl(
        &connection,
        "charging_processes",
        THP2_2_CHARGING_PROCESSES_SQLITE_DDL,
    )?;
    verify_projection_table_ddl(&connection, "charges", THP2_2_CHARGES_SQLITE_DDL)?;
    verify_projection_table_ddl(&connection, "states", THP2_2_STATES_SQLITE_DDL)?;
    verify_projection_table_ddl(&connection, "updates", THP2_2_UPDATES_SQLITE_DDL)?;
    for (table, expected_foreign_keys) in [
        ("hub_pack_metadata", &[][..]),
        ("cars", &[("car_settings", "settings_id", "id")][..]),
        ("car_settings", &[][..]),
        ("addresses", &[][..]),
        ("geofences", &[][..]),
        ("global_settings", &[][..]),
        ("drives", &[][..]),
        ("positions", &[][..]),
        ("charging_processes", &[][..]),
        ("charges", &[][..]),
        ("states", &[("cars", "car_id", "id")][..]),
        ("updates", &[("cars", "car_id", "id")][..]),
    ] {
        verify_projection_foreign_keys(&connection, table, expected_foreign_keys)?;
    }
    let expected_metadata = [
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
    let mut expected_metadata = expected_metadata
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect::<Vec<_>>();
    expected_metadata.sort_unstable();
    let mut metadata_statement = connection
        .prepare("SELECT key, value FROM hub_pack_metadata ORDER BY key")
        .map_err(ProjectionPackError::IntegrityCheck)?;
    let actual_metadata = metadata_statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(ProjectionPackError::IntegrityCheck)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if actual_metadata != expected_metadata {
        return Err(invalid("schema 2.2 metadata key/value set is invalid"));
    }
    let encoded_row_count: i64 = connection
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM cars)
                + (SELECT COUNT(*) FROM car_settings)
                + (SELECT COUNT(*) FROM addresses)
                + (SELECT COUNT(*) FROM geofences)
                + (SELECT COUNT(*) FROM global_settings)
                + (SELECT COUNT(*) FROM drives)
                + (SELECT COUNT(*) FROM positions)
                + (SELECT COUNT(*) FROM charging_processes)
                + (SELECT COUNT(*) FROM charges)
                + (SELECT COUNT(*) FROM states)
                + (SELECT COUNT(*) FROM updates)",
            [],
            |row| row.get(0),
        )
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if u64::try_from(encoded_row_count).ok() != Some(row_count) {
        return Err(invalid("schema 2.2 row_count does not match SQLite rows"));
    }
    ensure_foreign_keys_clean(&connection)?;
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if integrity != "ok" {
        return Err(ProjectionPackError::IntegrityFailure);
    }
    Ok(())
}

fn verify_projection_table_layout(
    connection: &Connection,
    table: &str,
    expected_without_rowid: bool,
    expected_columns: &[(&str, &str, bool, bool)],
) -> Result<(), ProjectionPackError> {
    let mut table_statement = connection
        .prepare("PRAGMA table_list")
        .map_err(ProjectionPackError::IntegrityCheck)?;
    let table_flags = table_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(ProjectionPackError::IntegrityCheck)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(ProjectionPackError::IntegrityCheck)?;
    let Some((_, actual_without_rowid, actual_strict)) =
        table_flags.into_iter().find(|(name, _, _)| name == table)
    else {
        return Err(invalid(format!("schema 2.2 {table} table is missing")));
    };
    if actual_without_rowid != i64::from(expected_without_rowid) || actual_strict != 1 {
        return Err(invalid(format!(
            "schema 2.2 {table} table flags are invalid"
        )));
    }

    let mut statement = connection
        .prepare(&format!("PRAGMA table_xinfo('{table}')"))
        .map_err(ProjectionPackError::IntegrityCheck)?;
    let actual_columns = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .map_err(ProjectionPackError::IntegrityCheck)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(ProjectionPackError::IntegrityCheck)?;
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
    if actual_columns != expected_columns {
        return Err(invalid(format!(
            "schema 2.2 {table} column layout is invalid"
        )));
    }
    Ok(())
}

fn verify_projection_foreign_keys(
    connection: &Connection,
    table: &str,
    expected_foreign_keys: &[(&str, &str, &str)],
) -> Result<(), ProjectionPackError> {
    let mut statement = connection
        .prepare(&format!("PRAGMA foreign_key_list('{table}')"))
        .map_err(ProjectionPackError::IntegrityCheck)?;
    let mut values = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(ProjectionPackError::IntegrityCheck)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(ProjectionPackError::IntegrityCheck)?;
    values.sort_unstable();
    let mut expected = expected_foreign_keys
        .iter()
        .map(|(target_table, from_column, to_column)| {
            (
                (*target_table).to_owned(),
                (*from_column).to_owned(),
                (*to_column).to_owned(),
            )
        })
        .collect::<Vec<_>>();
    expected.sort_unstable();
    if values != expected {
        return Err(invalid(format!(
            "schema 2.2 {table} foreign-key layout is invalid"
        )));
    }
    Ok(())
}

fn verify_projection_table_ddl(
    connection: &Connection,
    table: &str,
    expected_sql: &str,
) -> Result<(), ProjectionPackError> {
    let actual_sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if normalize_sqlite_ddl(&actual_sql) != normalize_sqlite_ddl(expected_sql) {
        return Err(invalid(format!("schema 2.2 {table} DDL is invalid")));
    }
    Ok(())
}

fn normalize_sqlite_ddl(sql: &str) -> String {
    sql.split_whitespace()
        .collect::<String>()
        .to_ascii_lowercase()
}

fn ensure_foreign_keys_clean(connection: &Connection) -> Result<(), ProjectionPackError> {
    let mut statement = connection
        .prepare("PRAGMA foreign_key_check")
        .map_err(ProjectionPackError::IntegrityCheck)?;
    let mut rows = statement
        .query([])
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if rows
        .next()
        .map_err(ProjectionPackError::IntegrityCheck)?
        .is_some()
    {
        return Err(ProjectionPackError::IntegrityFailure);
    }
    Ok(())
}

fn thp2_2_geofence_slice_sha256() -> String {
    THP2_2_GEOFENCE_SLICE_SHA256.to_owned()
}

fn thp2_2_car_settings_slice_sha256() -> String {
    THP2_2_CAR_SETTINGS_SLICE_SHA256.to_owned()
}

fn thp2_2_global_settings_slice_sha256() -> String {
    THP2_2_GLOBAL_SETTINGS_SLICE_SHA256.to_owned()
}

fn thp2_2_cars_slice_sha256() -> String {
    THP2_2_CARS_SLICE_SHA256.to_owned()
}

fn thp2_2_address_slice_sha256() -> String {
    THP2_2_ADDRESS_SLICE_SHA256.to_owned()
}

fn thp2_2_drives_slice_sha256() -> String {
    THP2_2_DRIVES_SLICE_SHA256.to_owned()
}

fn thp2_2_positions_slice_sha256() -> String {
    THP2_2_POSITIONS_SLICE_SHA256.to_owned()
}

fn thp2_2_charging_processes_slice_sha256() -> String {
    THP2_2_CHARGING_PROCESSES_SLICE_SHA256.to_owned()
}

fn thp2_2_charges_slice_sha256() -> String {
    THP2_2_CHARGES_SLICE_SHA256.to_owned()
}

fn thp2_2_states_slice_sha256() -> String {
    THP2_2_STATES_SLICE_SHA256.to_owned()
}

fn thp2_2_updates_slice_sha256() -> String {
    THP2_2_UPDATES_SLICE_SHA256.to_owned()
}
