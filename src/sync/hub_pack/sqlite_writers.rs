// SPDX-License-Identifier: AGPL-3.0-only

fn write_projection_sqlite(
    path: &Path,
    request: &ProjectionPackRequest<'_>,
    limits: ProtocolLimits,
    schema: SchemaVersion,
    states: &[ProjectionState],
    updates: &[ProjectionUpdate],
    row_count: u64,
) -> Result<(), ProjectionPackError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(ProjectionPackError::OpenSqlite)?;
    let max_pages = limits.max_uncompressed_pack_bytes / 4_096;
    if max_pages == 0 {
        return Err(invalid("pack limit is smaller than one SQLite page"));
    }
    connection
        .pragma_update(None, "page_size", 4_096_i64)
        .map_err(ProjectionPackError::ConfigureSqlite)?;
    connection
        .pragma_update(
            None,
            "max_page_count",
            i64::try_from(max_pages).unwrap_or(i64::MAX),
        )
        .map_err(ProjectionPackError::ConfigureSqlite)?;
    if schema == HUB_PROJECTION_SCHEMA_V1 {
        connection
            .execute_batch(HUB_PROJECTION_SCHEMA_V1_SQL)
            .map_err(ProjectionPackError::CreateSchema)?;
    } else if schema != HUB_PROJECTION_SCHEMA_V2 {
        return Err(invalid("unsupported projection schema"));
    } else {
        // `car_settings` is part of the additive 2.1 layout. Keep its DDL
        // byte-for-byte stable for 2.1 because the typed-delta verifier pins
        // that physical contract separately.
        let car_settings_schema = r#"
            CREATE TABLE car_settings (
                car_id INTEGER PRIMARY KEY REFERENCES cars(id),
                enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
                use_streaming_api INTEGER NOT NULL CHECK(use_streaming_api IN (0, 1)),
                suspend_after_idle_min INTEGER NOT NULL CHECK(suspend_after_idle_min > 0),
                suspend_min INTEGER NOT NULL CHECK(suspend_min > 0),
                suspend_min_resolved INTEGER NOT NULL CHECK(suspend_min_resolved IN (0, 1)),
                req_not_unlocked INTEGER NOT NULL CHECK(req_not_unlocked IN (0, 1)),
                free_supercharging INTEGER NOT NULL CHECK(free_supercharging IN (0, 1)),
                lfp_battery INTEGER NOT NULL CHECK(lfp_battery IN (0, 1))
            ) STRICT, WITHOUT ROWID;"#;
        let schema_sql = format!(
            r#"
            PRAGMA journal_mode = OFF;
            PRAGMA synchronous = OFF;
            PRAGMA foreign_keys = ON;
            PRAGMA trusted_schema = OFF;
            PRAGMA temp_store = FILE;
            BEGIN IMMEDIATE;
            CREATE TABLE hub_pack_metadata (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            ) STRICT;
            CREATE TABLE cars (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                model TEXT NOT NULL,
                vin TEXT,
                source_eid INTEGER,
                source_vid INTEGER,
                trim_badging TEXT,
                marketing_name TEXT,
                exterior_color TEXT,
                wheel_type TEXT,
                spoiler_type TEXT,
                firmware_version TEXT,
                efficiency_wh_per_km REAL
            ) STRICT, WITHOUT ROWID;{car_settings_schema}
            CREATE TABLE drives (
                id INTEGER PRIMARY KEY,
                car_id INTEGER NOT NULL REFERENCES cars(id),
                optimized_at_ms INTEGER,
                start_date_ms INTEGER NOT NULL,
                end_date_ms INTEGER NOT NULL,
                distance_km REAL,
                duration_min INTEGER,
                efficiency REAL,
                outside_temp_avg REAL,
                inside_temp_avg REAL,
                speed_max INTEGER,
                power_max REAL,
                power_min REAL,
                start_ideal_range_km REAL,
                end_ideal_range_km REAL,
                start_address TEXT,
                end_address TEXT,
                start_geofence TEXT,
                end_geofence TEXT,
                start_latitude REAL,
                start_longitude REAL,
                end_latitude REAL,
                end_longitude REAL,
                start_soc INTEGER,
                end_soc INTEGER,
                start_rated_range_km REAL,
                end_rated_range_km REAL,
                ascent INTEGER,
                descent INTEGER
            ) STRICT, WITHOUT ROWID;
            CREATE TABLE charges (
                id INTEGER PRIMARY KEY,
                car_id INTEGER NOT NULL REFERENCES cars(id),
                start_date_ms INTEGER NOT NULL,
                end_date_ms INTEGER,
                charge_energy_added REAL,
                charge_energy_used_kwh REAL,
                start_ideal_range_km REAL,
                end_ideal_range_km REAL,
                cost REAL,
                fast_charger_type TEXT,
                billing_type TEXT CHECK (billing_type IS NULL OR billing_type IN ('per_kwh', 'per_minute')),
                cost_per_unit REAL,
                session_fee REAL,
                start_latitude REAL,
                start_longitude REAL,
                start_battery_level INTEGER,
                end_battery_level INTEGER,
                duration_min INTEGER,
                address TEXT,
                location_name TEXT,
                geofence TEXT,
                is_dc INTEGER CHECK (is_dc IN (0, 1)),
                charge_rate_km_per_hour REAL,
                max_charger_power_kw REAL,
                outside_temp_avg REAL,
                start_rated_range_km REAL,
                end_rated_range_km REAL
            ) STRICT, WITHOUT ROWID;
            CREATE TABLE positions (
                id INTEGER PRIMARY KEY,
                drive_id INTEGER REFERENCES drives(id)
                    CHECK(drive_id IS NULL OR drive_id > 0),
                car_id INTEGER NOT NULL REFERENCES cars(id),
                date_ms INTEGER NOT NULL,
                latitude REAL NOT NULL,
                longitude REAL NOT NULL,
                speed INTEGER,
                power REAL,
                battery_level INTEGER,
                usable_battery_level INTEGER,
                elevation INTEGER,
                odometer REAL,
                ideal_battery_range_km REAL,
                est_battery_range_km REAL,
                rated_battery_range_km REAL,
                fan_status INTEGER,
                driver_temp_setting REAL,
                passenger_temp_setting REAL,
                is_climate_on INTEGER CHECK (is_climate_on IN (0, 1)),
                is_rear_defroster_on INTEGER CHECK (is_rear_defroster_on IN (0, 1)),
                is_front_defroster_on INTEGER CHECK (is_front_defroster_on IN (0, 1)),
                inside_temp REAL,
                outside_temp REAL,
                battery_heater INTEGER CHECK (battery_heater IN (0, 1)),
                battery_heater_on INTEGER CHECK (battery_heater_on IN (0, 1)),
                battery_heater_no_power INTEGER CHECK (battery_heater_no_power IN (0, 1)),
                tpms_pressure_fl REAL,
                tpms_pressure_fr REAL,
                tpms_pressure_rl REAL,
                tpms_pressure_rr REAL
            ) STRICT, WITHOUT ROWID;
            CREATE TABLE charge_samples (
                id INTEGER PRIMARY KEY,
                charge_process_id INTEGER NOT NULL REFERENCES charges(id),
                timestamp_ms INTEGER NOT NULL,
                battery_level INTEGER,
                usable_battery_level INTEGER,
                charge_energy_added_kwh REAL,
                charger_power_kw REAL,
                charger_voltage REAL,
                charger_actual_current REAL,
                charger_pilot_current REAL,
                charger_phases INTEGER,
                ideal_range_km REAL,
                rated_range_km REAL,
                outside_temp_c REAL,
                battery_heater_on INTEGER CHECK (battery_heater_on IN (0, 1)),
                battery_heater INTEGER CHECK (battery_heater IN (0, 1)),
                battery_heater_no_power INTEGER CHECK (battery_heater_no_power IN (0, 1)),
                not_enough_power_to_heat INTEGER CHECK (not_enough_power_to_heat IN (0, 1)),
                fast_charger_present INTEGER CHECK (fast_charger_present IN (0, 1)),
                fast_charger_brand TEXT,
                fast_charger_type TEXT,
                charge_cable TEXT
            ) STRICT, WITHOUT ROWID;
            COMMIT;
            "#
        );
        connection
            .execute_batch(&schema_sql)
            .map_err(ProjectionPackError::CreateSchema)?;
    }
    if schema == HUB_PROJECTION_SCHEMA_V2 {
        connection
            .execute_batch(
                "CREATE TABLE states (
                    id INTEGER PRIMARY KEY,
                    car_id INTEGER NOT NULL REFERENCES cars(id),
                    state TEXT NOT NULL CHECK (state IN ('online', 'offline', 'asleep')),
                    start_date_ms INTEGER NOT NULL,
                    end_date_ms INTEGER
                ) STRICT, WITHOUT ROWID;",
            )
            .map_err(ProjectionPackError::CreateSchema)?;
        connection
            .execute_batch(
                "CREATE TABLE updates (
                    id INTEGER PRIMARY KEY,
                    car_id INTEGER NOT NULL REFERENCES cars(id),
                    start_date_ms INTEGER NOT NULL,
                    end_date_ms INTEGER NOT NULL,
                    version TEXT NOT NULL
                ) STRICT, WITHOUT ROWID;",
            )
            .map_err(ProjectionPackError::CreateSchema)?;
    }

    let transaction = connection
        .unchecked_transaction()
        .map_err(ProjectionPackError::BeginTransaction)?;
    insert_metadata(&transaction, request, schema, row_count)?;
    if schema == HUB_PROJECTION_SCHEMA_V1 {
        insert_legacy_cars(&transaction, &request.snapshot.cars)?;
        insert_legacy_drives(&transaction, &request.snapshot.drives)?;
        insert_legacy_charges(&transaction, &request.snapshot.charges)?;
        insert_legacy_positions(&transaction, &request.snapshot.positions)?;
    } else {
        insert_cars(&transaction, &request.snapshot.cars, true)?;
        insert_drives(&transaction, &request.snapshot.drives)?;
        insert_charges(&transaction, &request.snapshot.charges)?;
        insert_positions(&transaction, &request.snapshot.positions)?;
    }
    insert_charge_samples(&transaction, &request.snapshot.charge_samples)?;
    if schema == HUB_PROJECTION_SCHEMA_V2 {
        insert_states(&transaction, states)?;
        insert_updates(&transaction, updates)?;
    }
    crate::durability_fault::check(crate::durability_fault::DurabilityFaultPoint::PackSqliteCommit)
        .map_err(ProjectionPackError::Durability)?;
    transaction.commit().map_err(ProjectionPackError::Commit)?;
    connection
        .execute_batch("PRAGMA optimize; VACUUM;")
        .map_err(ProjectionPackError::FinalizeSqlite)?;
    connection
        .pragma_update(None, "application_id", SQLITE_HUB_PROJECTION_APPLICATION_ID)
        .map_err(ProjectionPackError::ConfigureSqlite)?;
    connection
        .pragma_update(None, "user_version", schema.sqlite_user_version())
        .map_err(ProjectionPackError::ConfigureSqlite)?;
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if integrity != "ok" {
        return Err(ProjectionPackError::IntegrityFailure);
    }
    Ok(())
}

fn write_projection_sqlite_2_2(
    path: &Path,
    request: &ProjectionPackRequestV2_2<'_>,
    limits: ProtocolLimits,
    row_count: u64,
) -> Result<(), ProjectionPackError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(ProjectionPackError::OpenSqlite)?;
    let max_pages = limits.max_uncompressed_pack_bytes / 4_096;
    if max_pages == 0 {
        return Err(invalid("pack limit is smaller than one SQLite page"));
    }
    connection
        .pragma_update(None, "page_size", 4_096_i64)
        .map_err(ProjectionPackError::ConfigureSqlite)?;
    connection
        .pragma_update(
            None,
            "max_page_count",
            i64::try_from(max_pages).unwrap_or(i64::MAX),
        )
        .map_err(ProjectionPackError::ConfigureSqlite)?;
    connection
        .execute_batch(
            r#"
            PRAGMA journal_mode = OFF;
            PRAGMA synchronous = OFF;
            PRAGMA foreign_keys = ON;
            PRAGMA trusted_schema = OFF;
            PRAGMA temp_store = FILE;
            BEGIN IMMEDIATE;
            CREATE TABLE hub_pack_metadata (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            ) STRICT;
            CREATE TABLE car_settings (
                id INTEGER PRIMARY KEY,
                suspend_min INTEGER NOT NULL CHECK(suspend_min BETWEEN -2147483648 AND 2147483647),
                suspend_after_idle_min INTEGER NOT NULL CHECK(suspend_after_idle_min BETWEEN -2147483648 AND 2147483647),
                req_not_unlocked INTEGER NOT NULL CHECK(req_not_unlocked IN (0, 1)),
                free_supercharging INTEGER NOT NULL CHECK(free_supercharging IN (0, 1)),
                use_streaming_api INTEGER NOT NULL CHECK(use_streaming_api IN (0, 1)),
                enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
                lfp_battery INTEGER NOT NULL CHECK(lfp_battery IN (0, 1))
            ) STRICT, WITHOUT ROWID;
            CREATE TABLE cars (
                id INTEGER PRIMARY KEY CHECK(id BETWEEN -32768 AND 32767),
                eid INTEGER NOT NULL,
                vid INTEGER NOT NULL,
                vin TEXT,
                name TEXT,
                model TEXT CHECK(model IS NULL OR length(model) <= 255),
                efficiency BLOB CHECK(efficiency IS NULL OR length(efficiency) = 8),
                trim_badging TEXT,
                marketing_name TEXT CHECK(marketing_name IS NULL OR length(marketing_name) <= 255),
                exterior_color TEXT,
                wheel_type TEXT,
                spoiler_type TEXT,
                display_priority INTEGER NOT NULL CHECK(display_priority BETWEEN -32768 AND 32767),
                inserted_at_pg_us INTEGER NOT NULL CHECK(inserted_at_pg_us = (-9223372036854775807 - 1) OR inserted_at_pg_us = 9223372036854775807 OR (inserted_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND inserted_at_pg_us % 1000000 = 0)),
                updated_at_pg_us INTEGER NOT NULL CHECK(updated_at_pg_us = (-9223372036854775807 - 1) OR updated_at_pg_us = 9223372036854775807 OR (updated_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND updated_at_pg_us % 1000000 = 0)),
                settings_id INTEGER NOT NULL UNIQUE REFERENCES car_settings(id)
            ) STRICT, WITHOUT ROWID;
            CREATE TABLE addresses (
                id INTEGER PRIMARY KEY CHECK(id BETWEEN -2147483648 AND 2147483647),
                display_name TEXT,
                latitude_e6 INTEGER,
                latitude_e6_is_nan INTEGER NOT NULL CHECK(latitude_e6_is_nan IN (0, 1)),
                longitude_e6 INTEGER,
                longitude_e6_is_nan INTEGER NOT NULL CHECK(longitude_e6_is_nan IN (0, 1)),
                name TEXT,
                house_number TEXT,
                road TEXT,
                neighbourhood TEXT,
                city TEXT,
                county TEXT,
                postcode TEXT,
                state TEXT,
                state_district TEXT,
                country TEXT,
                inserted_at_pg_us INTEGER NOT NULL CHECK(inserted_at_pg_us = (-9223372036854775807 - 1) OR inserted_at_pg_us = 9223372036854775807 OR (inserted_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND inserted_at_pg_us % 1000000 = 0)),
                updated_at_pg_us INTEGER NOT NULL CHECK(updated_at_pg_us = (-9223372036854775807 - 1) OR updated_at_pg_us = 9223372036854775807 OR (updated_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND updated_at_pg_us % 1000000 = 0)),
                osm_id INTEGER,
                osm_type TEXT,
                CHECK((latitude_e6 IS NULL AND latitude_e6_is_nan IN (0, 1)) OR (latitude_e6 IS NOT NULL AND latitude_e6_is_nan = 0 AND latitude_e6 BETWEEN -99999999 AND 99999999)),
                CHECK((longitude_e6 IS NULL AND longitude_e6_is_nan IN (0, 1)) OR (longitude_e6 IS NOT NULL AND longitude_e6_is_nan = 0 AND longitude_e6 BETWEEN -999999999 AND 999999999))
            ) STRICT, WITHOUT ROWID;
            CREATE TABLE geofences (
                id INTEGER PRIMARY KEY CHECK(id BETWEEN -2147483648 AND 2147483647),
                name TEXT NOT NULL CHECK(length(name) <= 255),
                latitude_e6 INTEGER,
                latitude_e6_is_nan INTEGER NOT NULL CHECK(latitude_e6_is_nan IN (0, 1)),
                longitude_e6 INTEGER,
                longitude_e6_is_nan INTEGER NOT NULL CHECK(longitude_e6_is_nan IN (0, 1)),
                radius INTEGER NOT NULL CHECK(radius BETWEEN -32768 AND 32767),
                billing_type TEXT NOT NULL CHECK(billing_type IN ('per_kwh', 'per_minute')),
                cost_per_unit_e4 INTEGER,
                cost_per_unit_e4_is_nan INTEGER NOT NULL CHECK(cost_per_unit_e4_is_nan IN (0, 1)),
                session_fee_e2 INTEGER,
                session_fee_e2_is_nan INTEGER NOT NULL CHECK(session_fee_e2_is_nan IN (0, 1)),
                inserted_at_pg_us INTEGER NOT NULL CHECK(inserted_at_pg_us = (-9223372036854775807 - 1) OR inserted_at_pg_us = 9223372036854775807 OR (inserted_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND inserted_at_pg_us % 1000000 = 0)),
                updated_at_pg_us INTEGER NOT NULL CHECK(updated_at_pg_us = (-9223372036854775807 - 1) OR updated_at_pg_us = 9223372036854775807 OR (updated_at_pg_us BETWEEN -211813488000000000 AND 9223371331199999999 AND updated_at_pg_us % 1000000 = 0)),
                CHECK((latitude_e6 IS NULL AND latitude_e6_is_nan = 1) OR (latitude_e6 IS NOT NULL AND latitude_e6_is_nan = 0 AND latitude_e6 BETWEEN -99999999 AND 99999999)),
                CHECK((longitude_e6 IS NULL AND longitude_e6_is_nan = 1) OR (longitude_e6 IS NOT NULL AND longitude_e6_is_nan = 0 AND longitude_e6 BETWEEN -999999999 AND 999999999)),
                CHECK((cost_per_unit_e4 IS NULL AND cost_per_unit_e4_is_nan IN (0, 1)) OR (cost_per_unit_e4 IS NOT NULL AND cost_per_unit_e4_is_nan = 0 AND cost_per_unit_e4 BETWEEN -999999 AND 999999)),
                CHECK((session_fee_e2 IS NULL AND session_fee_e2_is_nan IN (0, 1)) OR (session_fee_e2 IS NOT NULL AND session_fee_e2_is_nan = 0 AND session_fee_e2 BETWEEN -999999 AND 999999))
            ) STRICT, WITHOUT ROWID;
            CREATE TABLE states (
                id INTEGER PRIMARY KEY CHECK(id BETWEEN -2147483648 AND 2147483647),
                car_id INTEGER NOT NULL CHECK(car_id BETWEEN -32768 AND 32767) REFERENCES cars(id),
                state TEXT NOT NULL CHECK(state IN ('online', 'offline', 'asleep')),
                start_date_pg_us INTEGER NOT NULL CHECK(start_date_pg_us = (-9223372036854775807 - 1) OR start_date_pg_us = 9223372036854775807 OR start_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999),
                end_date_pg_us INTEGER CHECK(end_date_pg_us IS NULL OR end_date_pg_us = (-9223372036854775807 - 1) OR end_date_pg_us = 9223372036854775807 OR end_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999)
            ) STRICT, WITHOUT ROWID;
            CREATE TABLE updates (
                id INTEGER PRIMARY KEY CHECK(id BETWEEN -2147483648 AND 2147483647),
                car_id INTEGER NOT NULL CHECK(car_id BETWEEN -32768 AND 32767) REFERENCES cars(id),
                start_date_pg_us INTEGER NOT NULL CHECK(start_date_pg_us = (-9223372036854775807 - 1) OR start_date_pg_us = 9223372036854775807 OR start_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999),
                end_date_pg_us INTEGER CHECK(end_date_pg_us IS NULL OR end_date_pg_us = (-9223372036854775807 - 1) OR end_date_pg_us = 9223372036854775807 OR end_date_pg_us BETWEEN -211813488000000000 AND 9223371331199999999),
                version TEXT CHECK(version IS NULL OR length(version) <= 255)
            ) STRICT, WITHOUT ROWID;
            COMMIT;
            "#,
        )
        .map_err(ProjectionPackError::CreateSchema)?;
    connection
        .execute_batch(THP2_2_DRIVES_SQLITE_DDL)
        .map_err(ProjectionPackError::CreateSchema)?;
    connection
        .execute_batch(THP2_2_GLOBAL_SETTINGS_SQLITE_DDL)
        .map_err(ProjectionPackError::CreateSchema)?;
    connection
        .execute_batch(THP2_2_POSITIONS_SQLITE_DDL)
        .map_err(ProjectionPackError::CreateSchema)?;
    connection
        .execute_batch(THP2_2_CHARGING_PROCESSES_SQLITE_DDL)
        .map_err(ProjectionPackError::CreateSchema)?;
    connection
        .execute_batch(THP2_2_CHARGES_SQLITE_DDL)
        .map_err(ProjectionPackError::CreateSchema)?;

    let transaction = connection
        .unchecked_transaction()
        .map_err(ProjectionPackError::BeginTransaction)?;
    insert_metadata_v2_2(&transaction, request, row_count)?;
    insert_global_settings_v2_2(&transaction, &request.snapshot.global_settings)?;
    insert_car_settings_v2_2(&transaction, &request.snapshot.car_settings)?;
    insert_cars_v2_2(&transaction, &request.snapshot.cars)?;
    insert_addresses_v2_2(&transaction, &request.snapshot.addresses)?;
    insert_geofences_v2_2(&transaction, &request.snapshot.geofences)?;
    insert_drives_v2_2(&transaction, &request.snapshot.drives)?;
    insert_positions_v2_2(&transaction, &request.snapshot.positions)?;
    insert_charging_processes_v2_2(&transaction, &request.snapshot.charging_processes)?;
    insert_charges_v2_2(&transaction, &request.snapshot.charges)?;
    insert_states_v2_2(&transaction, &request.snapshot.states)?;
    insert_updates_v2_2(&transaction, &request.snapshot.updates)?;
    crate::durability_fault::check(crate::durability_fault::DurabilityFaultPoint::PackSqliteCommit)
        .map_err(ProjectionPackError::Durability)?;
    transaction.commit().map_err(ProjectionPackError::Commit)?;
    ensure_foreign_keys_clean(&connection)?;
    connection
        .execute_batch("PRAGMA optimize; VACUUM;")
        .map_err(ProjectionPackError::FinalizeSqlite)?;
    connection
        .pragma_update(None, "application_id", SQLITE_HUB_PROJECTION_APPLICATION_ID)
        .map_err(ProjectionPackError::ConfigureSqlite)?;
    connection
        .pragma_update(
            None,
            "user_version",
            HUB_PROJECTION_SCHEMA_V3.sqlite_user_version(),
        )
        .map_err(ProjectionPackError::ConfigureSqlite)?;
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if integrity != "ok" {
        return Err(ProjectionPackError::IntegrityFailure);
    }
    Ok(())
}
