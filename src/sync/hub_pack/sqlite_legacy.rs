// SPDX-License-Identifier: AGPL-3.0-only

fn insert_metadata(
    transaction: &rusqlite::Transaction<'_>,
    request: &ProjectionPackRequest<'_>,
    schema: SchemaVersion,
    row_count: u64,
) -> Result<(), ProjectionPackError> {
    let values = [
        ("protocol", "teslatlas-sync".to_owned()),
        ("pack_format", "hub_projection_sqlite".to_owned()),
        ("schema_major", schema.major.to_string()),
        ("schema_minor", schema.minor.to_string()),
        ("pack_id", request.pack_id.to_string()),
        ("snapshot_id", request.snapshot_id.to_string()),
        ("ordinal", request.ordinal.to_string()),
        ("mode", "full_snapshot".to_owned()),
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

fn write_delta_rows(
    path: &Path,
    request: &ProjectionDeltaPackRequest<'_>,
    limits: ProtocolLimits,
    row_count: u64,
) -> Result<(), ProjectionPackError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(ProjectionPackError::OpenSqlite)?;
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             PRAGMA synchronous = FULL;
             CREATE TABLE tombstones (
                 entity TEXT NOT NULL,
                 entity_id INTEGER NOT NULL CHECK(entity_id > 0),
                 car_id INTEGER NOT NULL CHECK(car_id > 0),
                 PRIMARY KEY(entity, entity_id)
             ) STRICT, WITHOUT ROWID;",
        )
        .map_err(ProjectionPackError::CreateSchema)?;
    let transaction = connection
        .unchecked_transaction()
        .map_err(ProjectionPackError::BeginTransaction)?;
    transaction
        .execute("DELETE FROM hub_pack_metadata", [])
        .map_err(ProjectionPackError::Insert)?;
    insert_delta_metadata(&transaction, request, row_count)?;
    insert_cars(&transaction, &request.delta.cars, true)?;
    insert_car_settings(&transaction, &request.delta.car_settings)?;
    insert_drives(&transaction, &request.delta.drives)?;
    insert_charges(&transaction, &request.delta.charges)?;
    insert_positions(&transaction, &request.delta.positions)?;
    insert_charge_samples(&transaction, &request.delta.charge_samples)?;
    insert_states(&transaction, &request.delta.states)?;
    insert_updates(&transaction, &request.delta.updates)?;
    insert_tombstones(&transaction, &request.delta.tombstones)?;
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
        .pragma_update(
            None,
            "user_version",
            HUB_PROJECTION_SCHEMA_V2.sqlite_user_version(),
        )
        .map_err(ProjectionPackError::ConfigureSqlite)?;
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if integrity != "ok" {
        return Err(ProjectionPackError::IntegrityFailure);
    }
    let _ = limits;
    Ok(())
}

fn insert_delta_metadata(
    transaction: &rusqlite::Transaction<'_>,
    request: &ProjectionDeltaPackRequest<'_>,
    row_count: u64,
) -> Result<(), ProjectionPackError> {
    let delta = request.delta;
    let values = [
        ("protocol", "teslatlas-sync".to_owned()),
        ("pack_format", "hub_projection_sqlite".to_owned()),
        ("schema_major", HUB_PROJECTION_SCHEMA_V2.major.to_string()),
        ("schema_minor", HUB_PROJECTION_SCHEMA_V2.minor.to_string()),
        ("delta_schema_version", "1".to_owned()),
        ("pack_id", request.pack_id.to_string()),
        ("snapshot_id", request.snapshot_id.to_string()),
        ("ordinal", request.ordinal.to_string()),
        ("mode", "typed_delta".to_owned()),
        ("installation_id", delta.binding.installation_id.to_string()),
        ("account_id", delta.binding.account_id.to_string()),
        ("vehicle_id", delta.binding.vehicle_id.to_string()),
        ("generation", delta.binding.generation.to_string()),
        ("selected_car_id", delta.binding.selected_car_id.to_string()),
        ("from_sequence", delta.sequence.from_exclusive.to_string()),
        ("to_sequence", delta.sequence.to_inclusive.to_string()),
        ("parent_digest", delta.parent_digest.to_string()),
        ("external_base", "true".to_owned()),
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

fn insert_car_settings(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionCarSettingsPatch],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.car_id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO car_settings(
                car_id, enabled, use_streaming_api, suspend_after_idle_min, suspend_min,
                suspend_min_resolved, req_not_unlocked, free_supercharging, lfp_battery
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.car_id,
                row.settings.enabled,
                row.settings.use_streaming_api,
                row.settings.suspend_after_idle_min,
                row.settings.suspend_min,
                row.settings.suspend_min_resolved,
                row.settings.req_not_unlocked,
                row.settings.free_supercharging,
                row.settings.lfp_battery,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_tombstones(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionTombstone],
) -> Result<(), ProjectionPackError> {
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO tombstones(entity, entity_id, car_id)
             VALUES (?1, ?2, ?3)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in source_owned_tombstones_in_canonical_order(values) {
        statement
            .execute(params![row.entity.as_str(), row.id, row.car_id])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_states(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionState],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO states (id, car_id, state, start_date_ms, end_date_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.car_id,
                row.state,
                row.start_date_ms,
                row.end_date_ms,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_updates(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionUpdate],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO updates (id, car_id, start_date_ms, end_date_ms, version)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.car_id,
                row.start_date_ms,
                row.end_date_ms,
                row.version,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_legacy_cars(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionCar],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO cars (
                id, name, model, vin, firmware_version, efficiency_wh_per_km
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.name,
                normalize_tesla_model_code(&row.model),
                row.vin,
                row.firmware_version,
                row.efficiency_wh_per_km,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_legacy_drives(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionDrive],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO drives (
                id, car_id, optimized_at_ms, start_date_ms, end_date_ms, distance_km,
                duration_min, efficiency, outside_temp_avg, speed_max, start_address,
                end_address, start_geofence, end_geofence, start_latitude, start_longitude,
                end_latitude, end_longitude, start_soc, end_soc, start_rated_range_km,
                end_rated_range_km
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.car_id,
                // The released 2.0 client accepts this legacy column only
                // when it is NULL, so it is deliberately not projected.
                Option::<i64>::None,
                row.start_date_ms,
                row.end_date_ms,
                row.distance_km,
                row.duration_min,
                row.efficiency,
                row.outside_temp_avg,
                row.speed_max,
                row.start_address,
                row.end_address,
                row.start_geofence,
                row.end_geofence,
                row.start_latitude,
                row.start_longitude,
                row.end_latitude,
                row.end_longitude,
                row.start_soc,
                row.end_soc,
                row.start_rated_range_km,
                row.end_rated_range_km,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_legacy_charges(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionCharge],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO charges (
                id, car_id, start_date_ms, end_date_ms, charge_energy_added,
                start_battery_level, end_battery_level, duration_min, address, location_name,
                geofence, is_dc, charge_rate_km_per_hour, max_charger_power_kw,
                outside_temp_avg, start_rated_range_km, end_rated_range_km
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.car_id,
                row.start_date_ms,
                row.end_date_ms,
                row.charge_energy_added,
                row.start_battery_level,
                row.end_battery_level,
                row.duration_min,
                row.address,
                row.location_name,
                row.geofence,
                bool_as_sql(row.is_dc),
                row.charge_rate_km_per_hour,
                row.max_charger_power_kw,
                row.outside_temp_avg,
                row.start_rated_range_km,
                row.end_rated_range_km,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_legacy_positions(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionPosition],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO positions (
                id, drive_id, car_id, date_ms, latitude, longitude, speed, power,
                battery_level, usable_battery_level, elevation, odometer,
                ideal_battery_range_km, rated_battery_range_km, is_climate_on,
                inside_temp, outside_temp
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        let drive_id = row
            .drive_id
            .ok_or_else(|| invalid("schema 2.0 position.drive_id must be present"))?;
        statement
            .execute(params![
                row.id,
                drive_id,
                row.car_id,
                row.date_ms,
                row.latitude,
                row.longitude,
                row.speed,
                v1_position_power(row.power)?,
                row.battery_level,
                row.usable_battery_level,
                row.elevation,
                row.odometer,
                row.ideal_battery_range_km,
                row.rated_battery_range_km,
                bool_as_sql(row.is_climate_on),
                row.inside_temp,
                row.outside_temp,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn v1_position_power(value: Option<f64>) -> Result<Option<i64>, ProjectionPackError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if !value.is_finite()
        || value.fract() != 0.0
        || value < i64::MIN as f64
        || value >= -(i64::MIN as f64)
    {
        return Err(invalid("schema 2.0 position.power must be an integer"));
    }
    Ok(Some(value as i64))
}

fn insert_global_settings_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionGlobalSettingsV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO global_settings (
                id, unit_of_length, unit_of_temperature, unit_of_pressure, preferred_range,
                base_url, grafana_url, language, theme_mode, inserted_at_pg_us, updated_at_pg_us
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.unit_of_length.as_str(),
                row.unit_of_temperature.as_str(),
                row.unit_of_pressure.as_str(),
                row.preferred_range.as_str(),
                row.base_url,
                row.grafana_url,
                row.language,
                row.theme_mode,
                row.inserted_at_pg_us,
                row.updated_at_pg_us,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_car_settings_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionCarSettingsV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO car_settings (
                id, suspend_min, suspend_after_idle_min, req_not_unlocked,
                free_supercharging, use_streaming_api, enabled, lfp_battery
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.suspend_min,
                row.suspend_after_idle_min,
                row.req_not_unlocked,
                row.free_supercharging,
                row.use_streaming_api,
                row.enabled,
                row.lfp_battery,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_cars_v2_2(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionCarV2_2],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO cars (
                id, eid, vid, vin, name, model, efficiency, trim_badging,
                marketing_name, exterior_color, wheel_type, spoiler_type,
                display_priority, inserted_at_pg_us, updated_at_pg_us, settings_id
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        let efficiency_bits = row
            .efficiency
            .map(|value| value.to_bits().to_be_bytes().to_vec());
        statement
            .execute(params![
                row.id,
                row.eid,
                row.vid,
                row.vin,
                row.name,
                row.model,
                efficiency_bits,
                row.trim_badging,
                row.marketing_name,
                row.exterior_color,
                row.wheel_type,
                row.spoiler_type,
                row.display_priority,
                row.inserted_at_pg_us,
                row.updated_at_pg_us,
                row.settings_id,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_cars(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionCar],
    include_settings: bool,
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO cars (
                id, name, model, vin, source_eid, source_vid, trim_badging,
                marketing_name, exterior_color, wheel_type, spoiler_type,
                firmware_version, efficiency_wh_per_km
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in &rows {
        let model = normalize_tesla_model_code(&row.model);
        statement
            .execute(params![
                row.id,
                row.name,
                model,
                row.vin,
                row.source_eid,
                row.source_vid,
                row.trim_badging,
                row.marketing_name,
                row.exterior_color,
                row.wheel_type,
                row.spoiler_type,
                row.firmware_version,
                row.efficiency_wh_per_km,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    if !include_settings {
        return Ok(());
    }
    let mut settings = transaction
        .prepare_cached(
            "INSERT INTO car_settings(
                car_id, enabled, use_streaming_api, suspend_after_idle_min, suspend_min,
                suspend_min_resolved,
                req_not_unlocked, free_supercharging, lfp_battery
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        settings
            .execute(params![
                row.id,
                row.settings.enabled,
                row.settings.use_streaming_api,
                row.settings.suspend_after_idle_min,
                row.settings.suspend_min,
                row.settings.suspend_min_resolved,
                row.settings.req_not_unlocked,
                row.settings.free_supercharging,
                row.settings.lfp_battery,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_drives(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionDrive],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO drives (
                id, car_id, optimized_at_ms, start_date_ms, end_date_ms, distance_km,
                duration_min, efficiency, outside_temp_avg, inside_temp_avg, speed_max,
                power_max, power_min, start_ideal_range_km, end_ideal_range_km, start_address,
                end_address, start_geofence, end_geofence, start_latitude, start_longitude,
                end_latitude, end_longitude, start_soc, end_soc, start_rated_range_km,
                end_rated_range_km, ascent, descent
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26,
                ?27, ?28, ?29
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.car_id,
                row.optimized_at_ms,
                row.start_date_ms,
                row.end_date_ms,
                row.distance_km,
                row.duration_min,
                row.efficiency,
                row.outside_temp_avg,
                row.inside_temp_avg,
                row.speed_max,
                row.power_max,
                row.power_min,
                row.start_ideal_range_km,
                row.end_ideal_range_km,
                row.start_address,
                row.end_address,
                row.start_geofence,
                row.end_geofence,
                row.start_latitude,
                row.start_longitude,
                row.end_latitude,
                row.end_longitude,
                row.start_soc,
                row.end_soc,
                row.start_rated_range_km,
                row.end_rated_range_km,
                row.ascent,
                row.descent,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_charges(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionCharge],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO charges (
                id, car_id, start_date_ms, end_date_ms, charge_energy_added,
                charge_energy_used_kwh, start_ideal_range_km, end_ideal_range_km,
                cost, fast_charger_type, billing_type, cost_per_unit, session_fee,
                start_latitude, start_longitude, start_battery_level,
                end_battery_level, duration_min, address, location_name, geofence,
                is_dc, charge_rate_km_per_hour, max_charger_power_kw,
                outside_temp_avg, start_rated_range_km, end_rated_range_km
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.car_id,
                row.start_date_ms,
                row.end_date_ms,
                row.charge_energy_added,
                row.charge_energy_used_kwh,
                row.start_ideal_range_km,
                row.end_ideal_range_km,
                row.cost,
                row.fast_charger_type,
                row.billing_type.map(GeofenceBillingType::as_str),
                row.cost_per_unit,
                row.session_fee,
                row.start_latitude,
                row.start_longitude,
                row.start_battery_level,
                row.end_battery_level,
                row.duration_min,
                row.address,
                row.location_name,
                row.geofence,
                bool_as_sql(row.is_dc),
                row.charge_rate_km_per_hour,
                row.max_charger_power_kw,
                row.outside_temp_avg,
                row.start_rated_range_km,
                row.end_rated_range_km,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_positions(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionPosition],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO positions (
                id, drive_id, car_id, date_ms, latitude, longitude, speed, power,
                battery_level, usable_battery_level, elevation, odometer,
                ideal_battery_range_km, est_battery_range_km, rated_battery_range_km,
                fan_status, driver_temp_setting, passenger_temp_setting, is_climate_on,
                is_rear_defroster_on, is_front_defroster_on, inside_temp, outside_temp,
                battery_heater, battery_heater_on, battery_heater_no_power,
                tpms_pressure_fl, tpms_pressure_fr, tpms_pressure_rl, tpms_pressure_rr
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26,
                ?27, ?28, ?29, ?30
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.drive_id,
                row.car_id,
                row.date_ms,
                row.latitude,
                row.longitude,
                row.speed,
                row.power,
                row.battery_level,
                row.usable_battery_level,
                row.elevation,
                row.odometer,
                row.ideal_battery_range_km,
                row.est_battery_range_km,
                row.rated_battery_range_km,
                row.fan_status,
                row.driver_temp_setting,
                row.passenger_temp_setting,
                bool_as_sql(row.is_climate_on),
                bool_as_sql(row.is_rear_defroster_on),
                bool_as_sql(row.is_front_defroster_on),
                row.inside_temp,
                row.outside_temp,
                bool_as_sql(row.battery_heater),
                bool_as_sql(row.battery_heater_on),
                bool_as_sql(row.battery_heater_no_power),
                row.tpms_pressure_fl,
                row.tpms_pressure_fr,
                row.tpms_pressure_rl,
                row.tpms_pressure_rr,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn insert_charge_samples(
    transaction: &rusqlite::Transaction<'_>,
    values: &[ProjectionChargeSample],
) -> Result<(), ProjectionPackError> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| row.id);
    let mut statement = transaction
        .prepare_cached(
            "INSERT INTO charge_samples (
                id, charge_process_id, timestamp_ms, battery_level, usable_battery_level,
                charge_energy_added_kwh, charger_power_kw, charger_voltage,
                charger_actual_current, charger_pilot_current, charger_phases, ideal_range_km,
                rated_range_km, outside_temp_c, battery_heater_on, battery_heater,
                battery_heater_no_power, not_enough_power_to_heat, fast_charger_present,
                fast_charger_brand, fast_charger_type, charge_cable
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22
             )",
        )
        .map_err(ProjectionPackError::Prepare)?;
    for row in rows {
        statement
            .execute(params![
                row.id,
                row.charge_process_id,
                row.timestamp_ms,
                row.battery_level,
                row.usable_battery_level,
                row.charge_energy_added_kwh,
                row.charger_power_kw,
                row.charger_voltage,
                row.charger_actual_current,
                row.charger_pilot_current,
                row.charger_phases,
                row.ideal_range_km,
                row.rated_range_km,
                row.outside_temp_c,
                bool_as_sql(row.battery_heater_on),
                bool_as_sql(row.battery_heater),
                bool_as_sql(row.battery_heater_no_power),
                bool_as_sql(row.not_enough_power_to_heat),
                bool_as_sql(row.fast_charger_present),
                row.fast_charger_brand,
                row.fast_charger_type,
                row.charge_cable,
            ])
            .map_err(ProjectionPackError::Insert)?;
    }
    Ok(())
}

fn bool_as_sql(value: Option<bool>) -> Option<i64> {
    value.map(i64::from)
}
