// SPDX-License-Identifier: AGPL-3.0-only

/// Read the exact selected-car physical `cars` plus `car_settings` slice for
/// the production schema-2.2 capture. It remains separate from the legacy car
/// projection so publication cannot inherit compatibility defaults.
pub(crate) async fn read_car_and_car_settings_v2_2(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<(TeslaMateCarPhysicalV2_2, TeslaMateCarSettingsPhysicalV2_2), TeslaMateReaderError> {
    let rows = client
        .query(CARS_AND_CAR_SETTINGS_V2_2_SQL, &[&selected_car_id])
        .await?;
    let row = rows
        .first()
        .ok_or(TeslaMateReaderError::SelectedCarMissing {
            selected_car_id: i64::from(selected_car_id),
        })?;
    retain_row(retained_rows, limits.maximum_rows)?;
    let (car, car_settings) = decode_car_and_car_settings_v2_2(row)?;
    if car.id != selected_car_id {
        return Err(TeslaMateReaderError::NonProgressingPage { table: "cars" });
    }
    Ok((car, car_settings))
}

/// Read the source-wide TeslaMate `settings` singleton for schema-2.2
/// production capture. This has no selected-car argument: zero rows and two-or-more
/// rows are both rejected rather than silently defaulted or truncated.
pub(crate) async fn read_settings_v2_2(
    client: &Client,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<TeslaMateSettingsPhysicalV2_2, TeslaMateReaderError> {
    let rows = client.query(SETTINGS_V2_2_SQL, &[]).await?;
    let row = match rows.as_slice() {
        [] => return Err(TeslaMateReaderError::SettingsSingletonMissing),
        [row] => row,
        _ => return Err(TeslaMateReaderError::SettingsSingletonAmbiguous),
    };
    retain_row(retained_rows, limits.maximum_rows)?;
    decode_settings_v2_2(row)
}

/// Read the exact selected-car physical `updates` slice for schema-2.2
/// production publication. Nullable source end/version values are retained verbatim.
pub(crate) async fn read_updates_v2_2(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateUpdatePhysicalV2_2>, TeslaMateReaderError> {
    let mut updates = Vec::new();
    let mut last_id = None;
    let page_size = i64::from(limits.page_size);
    loop {
        let rows = client
            .query(UPDATES_V2_2_SQL, &[&last_id, &page_size, &selected_car_id])
            .await?;
        let page_len = rows.len();
        for row in rows {
            let id = required_i32(&row, "updates", "id")?;
            last_id = advance_signed_v2_2_cursor(last_id, id, "updates")?;
            retain_row(retained_rows, limits.maximum_rows)?;
            let update = decode_update_v2_2(&row)?;
            if update.car_id != selected_car_id {
                return Err(TeslaMateReaderError::NonProgressingPage { table: "updates" });
            }
            updates.push(update);
        }
        if page_len < limits.page_size as usize {
            break;
        }
    }
    Ok(updates)
}

fn advance_signed_v2_2_cursor(
    previous_id: Option<i32>,
    id: i32,
    table: &'static str,
) -> Result<Option<i32>, TeslaMateReaderError> {
    if previous_id.is_some_and(|previous_id| id <= previous_id) {
        return Err(TeslaMateReaderError::NonProgressingPage { table });
    }
    Ok(Some(id))
}

pub(crate) async fn read_updates(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateUpdate>, TeslaMateReaderError> {
    let stream = client
        .copy_out(&binary_copy_sql(SourceTable::Updates, selected_car_id))
        .await?;
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        update_copy_types()
    ));
    let mut updates = Vec::new();
    while let Some(row) = rows.as_mut().try_next().await? {
        retain_row(retained_rows, limits.maximum_rows)?;
        let id: i32 = binary_cell(&row, 0, "updates", "id")?;
        let car_id: i16 = binary_cell(&row, 1, "updates", "car_id")?;
        updates.push(TeslaMateUpdate {
            id: i64::from(id),
            car_id: i64::from(car_id),
            start_date_ms: binary_required_timestamp_ms(&row, 2, "updates", "start_date")?,
            end_date_ms: binary_optional_timestamp_ms(&row, 3, "updates", "end_date")?,
            version: binary_cell::<Option<&str>>(&row, 4, "updates", "version")?
                .map(ToOwned::to_owned),
        });
    }
    Ok(updates)
}

async fn read_states(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateState>, TeslaMateReaderError> {
    let mut last_id = 0_i32;
    let page_size = i64::from(limits.page_size);
    let mut states = Vec::new();
    loop {
        let page = client
            .query(
                projection(SourceTable::States).sql,
                &[&last_id, &page_size, &selected_car_id],
            )
            .await?;
        let page_len = page.len();
        for row in page {
            let id = required_i32(&row, "states", "id")?;
            if id <= last_id {
                return Err(TeslaMateReaderError::NonProgressingPage { table: "states" });
            }
            last_id = id;
            retain_row(retained_rows, limits.maximum_rows)?;
            states.push(decode_state(&row)?);
        }
        if page_len < limits.page_size as usize {
            return Ok(states);
        }
    }
}

fn update_copy_types() -> &'static [Type] {
    const TYPES: [Type; 5] = [
        Type::INT4,
        Type::INT2,
        Type::TIMESTAMP,
        Type::TIMESTAMP,
        Type::TEXT,
    ];
    &TYPES
}

fn retain_row(total: &mut usize, maximum: usize) -> Result<(), TeslaMateReaderError> {
    *total = total
        .checked_add(1)
        .ok_or(TeslaMateReaderError::MaximumRowsExceeded { maximum })?;
    if *total > maximum {
        return Err(TeslaMateReaderError::MaximumRowsExceeded { maximum });
    }
    Ok(())
}

fn decode_car(row: &Row) -> Result<TeslaMateCar, TeslaMateReaderError> {
    Ok(TeslaMateCar {
        id: i64::from(required_i16(row, "cars", "id")?),
        eid: required_i64(row, "cars", "eid")?,
        vid: optional_i64(row, "cars", "vid")?,
        vin: optional_text(row, "cars", "vin")?,
        name: optional_text(row, "cars", "name")?,
        model: optional_text(row, "cars", "model")?,
        trim_badging: optional_text(row, "cars", "trim_badging")?,
        marketing_name: optional_text(row, "cars", "marketing_name")?,
        exterior_color: optional_text(row, "cars", "exterior_color")?,
        wheel_type: optional_text(row, "cars", "wheel_type")?,
        spoiler_type: optional_text(row, "cars", "spoiler_type")?,
        efficiency_wh_per_km: optional_float(row, "cars", "efficiency")?,
        settings: decode_car_settings_row(row)?,
    })
}

#[allow(dead_code)] // reached only from the intentionally unlinked candidate reader.
fn decode_settings_v2_2(row: &Row) -> Result<TeslaMateSettingsPhysicalV2_2, TeslaMateReaderError> {
    Ok(TeslaMateSettingsPhysicalV2_2 {
        id: required_i64(row, "settings", "id")?,
        unit_of_length: required_text(row, "settings", "unit_of_length")?
            .parse::<ProjectionUnitOfLengthV2_2>()
            .map_err(|_| TeslaMateReaderError::InvalidSettingsEnum {
                column: "unit_of_length",
            })?,
        unit_of_temperature: required_text(row, "settings", "unit_of_temperature")?
            .parse::<ProjectionUnitOfTemperatureV2_2>()
            .map_err(|_| TeslaMateReaderError::InvalidSettingsEnum {
                column: "unit_of_temperature",
            })?,
        unit_of_pressure: required_text(row, "settings", "unit_of_pressure")?
            .parse::<ProjectionUnitOfPressureV2_2>()
            .map_err(|_| TeslaMateReaderError::InvalidSettingsEnum {
                column: "unit_of_pressure",
            })?,
        preferred_range: required_text(row, "settings", "preferred_range")?
            .parse::<ProjectionPreferredRangeV2_2>()
            .map_err(|_| TeslaMateReaderError::InvalidSettingsEnum {
                column: "preferred_range",
            })?,
        base_url: optional_text(row, "settings", "base_url")?,
        grafana_url: optional_text(row, "settings", "grafana_url")?,
        language: required_text(row, "settings", "language")?,
        theme_mode: required_text(row, "settings", "theme_mode")?,
        inserted_at_pg_us: required_timestamp_0_pg_us(row, "settings", "inserted_at")?,
        updated_at_pg_us: required_timestamp_0_pg_us(row, "settings", "updated_at")?,
    })
}

#[allow(dead_code)] // reached only from the intentionally unlinked candidate reader.
fn decode_car_and_car_settings_v2_2(
    row: &Row,
) -> Result<(TeslaMateCarPhysicalV2_2, TeslaMateCarSettingsPhysicalV2_2), TeslaMateReaderError> {
    let car = TeslaMateCarPhysicalV2_2 {
        id: required_i16(row, "cars", "id")?,
        eid: required_i64(row, "cars", "eid")?,
        vid: required_i64(row, "cars", "vid")?,
        vin: optional_text(row, "cars", "vin")?,
        name: optional_text(row, "cars", "name")?,
        model: optional_text(row, "cars", "model")?,
        // Preserve the source FLOAT8 exactly. The schema-2.2 pack boundary
        // later stores its raw IEEE-754 bit pattern without Wh conversion.
        efficiency: optional_float(row, "cars", "efficiency")?,
        trim_badging: optional_text(row, "cars", "trim_badging")?,
        marketing_name: optional_text(row, "cars", "marketing_name")?,
        exterior_color: optional_text(row, "cars", "exterior_color")?,
        wheel_type: optional_text(row, "cars", "wheel_type")?,
        spoiler_type: optional_text(row, "cars", "spoiler_type")?,
        display_priority: required_i16(row, "cars", "display_priority")?,
        inserted_at_pg_us: required_timestamp_0_pg_us(row, "cars", "inserted_at")?,
        updated_at_pg_us: required_timestamp_0_pg_us(row, "cars", "updated_at")?,
        settings_id: required_i64(row, "cars", "settings_id")?,
    };
    let car_settings = TeslaMateCarSettingsPhysicalV2_2 {
        id: required_i64(row, "car_settings", "car_settings_row_id")?,
        suspend_min: required_i32(row, "car_settings", "suspend_min")?,
        suspend_after_idle_min: required_i32(row, "car_settings", "suspend_after_idle_min")?,
        req_not_unlocked: required_bool(row, "car_settings", "req_not_unlocked")?,
        free_supercharging: required_bool(row, "car_settings", "free_supercharging")?,
        use_streaming_api: required_bool(row, "car_settings", "use_streaming_api")?,
        enabled: required_bool(row, "car_settings", "enabled")?,
        lfp_battery: required_bool(row, "car_settings", "lfp_battery")?,
    };
    Ok((car, car_settings))
}

fn decode_update_v2_2(row: &Row) -> Result<TeslaMateUpdatePhysicalV2_2, TeslaMateReaderError> {
    Ok(TeslaMateUpdatePhysicalV2_2 {
        id: required_i32(row, "updates", "id")?,
        car_id: required_i16(row, "updates", "car_id")?,
        start_date_pg_us: required_timestamp_pg_us(row, "updates", "start_date")?,
        end_date_pg_us: optional_timestamp_pg_us(row, "updates", "end_date")?,
        version: optional_text(row, "updates", "version")?,
    })
}

fn decode_car_settings_row(row: &Row) -> Result<ProjectionCarSettings, TeslaMateReaderError> {
    let defaults = ProjectionCarSettings::default();
    Ok(ProjectionCarSettings {
        suspend_min_resolved: true,
        suspend_min: optional_smallint(row, "car_settings", "suspend_min")?
            .map(i64::from)
            .unwrap_or(defaults.suspend_min),
        suspend_after_idle_min: optional_smallint(row, "car_settings", "suspend_after_idle_min")?
            .map(i64::from)
            .unwrap_or(defaults.suspend_after_idle_min),
        req_not_unlocked: optional_bool(row, "cars", "req_not_unlocked")?
            .unwrap_or(defaults.req_not_unlocked),
        free_supercharging: optional_bool(row, "cars", "free_supercharging")?
            .unwrap_or(defaults.free_supercharging),
        use_streaming_api: optional_bool(row, "cars", "use_streaming_api")?
            .unwrap_or(defaults.use_streaming_api),
        enabled: optional_bool(row, "cars", "enabled")?.unwrap_or(defaults.enabled),
        lfp_battery: optional_bool(row, "cars", "lfp_battery")?.unwrap_or(defaults.lfp_battery),
    })
}

fn decode_drive(row: &Row) -> Result<TeslaMateDrive, TeslaMateReaderError> {
    Ok(TeslaMateDrive {
        id: i64::from(required_i32(row, "drives", "id")?),
        car_id: i64::from(required_i16(row, "drives", "car_id")?),
        start_date_ms: required_timestamp_ms(row, "drives", "start_date")?,
        end_date_ms: optional_timestamp_ms(row, "drives", "end_date")?,
        start_position_id: optional_i32(row, "drives", "start_position_id")?.map(i64::from),
        end_position_id: optional_i32(row, "drives", "end_position_id")?.map(i64::from),
        start_address_id: optional_i32(row, "drives", "start_address_id")?.map(i64::from),
        end_address_id: optional_i32(row, "drives", "end_address_id")?.map(i64::from),
        start_geofence_id: optional_i32(row, "drives", "start_geofence_id")?.map(i64::from),
        end_geofence_id: optional_i32(row, "drives", "end_geofence_id")?.map(i64::from),
        outside_temp_avg: optional_decimal(row, "drives", "outside_temp_avg")?,
        inside_temp_avg: optional_decimal(row, "drives", "inside_temp_avg")?,
        speed_max: optional_i16(row, "drives", "speed_max")?.map(i64::from),
        power_max: optional_i16(row, "drives", "power_max")?.map(f64::from),
        power_min: optional_i16(row, "drives", "power_min")?.map(f64::from),
        start_ideal_range_km: optional_decimal(row, "drives", "start_ideal_range_km")?,
        end_ideal_range_km: optional_decimal(row, "drives", "end_ideal_range_km")?,
        start_rated_range_km: optional_decimal(row, "drives", "start_rated_range_km")?,
        end_rated_range_km: optional_decimal(row, "drives", "end_rated_range_km")?,
        start_km: optional_float(row, "drives", "start_km")?,
        end_km: optional_float(row, "drives", "end_km")?,
        distance_km: optional_float(row, "drives", "distance")?,
        duration_min: optional_i16(row, "drives", "duration_min")?.map(i64::from),
        ascent: optional_i16(row, "drives", "ascent")?.map(i64::from),
        descent: optional_i16(row, "drives", "descent")?.map(i64::from),
    })
}

pub(crate) fn decode_position(row: &Row) -> Result<TeslaMatePosition, TeslaMateReaderError> {
    Ok(TeslaMatePosition {
        id: i64::from(required_i32(row, "positions", "id")?),
        car_id: i64::from(required_i16(row, "positions", "car_id")?),
        drive_id: optional_i64(row, "positions", "drive_id")?,
        date_ms: required_timestamp_ms(row, "positions", "date")?,
        latitude: required_decimal(row, "positions", "latitude")?,
        longitude: required_decimal(row, "positions", "longitude")?,
        elevation: optional_i64(row, "positions", "elevation")?,
        speed: optional_i64(row, "positions", "speed")?,
        power: optional_float(row, "positions", "power")?,
        odometer: optional_float(row, "positions", "odometer")?,
        ideal_battery_range_km: optional_decimal(row, "positions", "ideal_battery_range_km")?,
        est_battery_range_km: optional_decimal(row, "positions", "est_battery_range_km")?,
        rated_battery_range_km: optional_decimal(row, "positions", "rated_battery_range_km")?,
        battery_level: optional_i64(row, "positions", "battery_level")?,
        usable_battery_level: optional_i64(row, "positions", "usable_battery_level")?,
        fan_status: optional_i64(row, "positions", "fan_status")?,
        driver_temp_setting: optional_decimal(row, "positions", "driver_temp_setting")?,
        passenger_temp_setting: optional_decimal(row, "positions", "passenger_temp_setting")?,
        is_climate_on: optional_bool(row, "positions", "is_climate_on")?,
        is_rear_defroster_on: optional_bool(row, "positions", "is_rear_defroster_on")?,
        is_front_defroster_on: optional_bool(row, "positions", "is_front_defroster_on")?,
        outside_temp: optional_decimal(row, "positions", "outside_temp")?,
        inside_temp: optional_decimal(row, "positions", "inside_temp")?,
        battery_heater: optional_bool(row, "positions", "battery_heater")?,
        battery_heater_on: optional_bool(row, "positions", "battery_heater_on")?,
        battery_heater_no_power: optional_bool(row, "positions", "battery_heater_no_power")?,
        tpms_pressure_fl: optional_decimal(row, "positions", "tpms_pressure_fl")?,
        tpms_pressure_fr: optional_decimal(row, "positions", "tpms_pressure_fr")?,
        tpms_pressure_rl: optional_decimal(row, "positions", "tpms_pressure_rl")?,
        tpms_pressure_rr: optional_decimal(row, "positions", "tpms_pressure_rr")?,
    })
}

fn decode_charging_process(row: &Row) -> Result<TeslaMateChargingProcess, TeslaMateReaderError> {
    Ok(TeslaMateChargingProcess {
        id: i64::from(required_i32(row, "charging_processes", "id")?),
        car_id: i64::from(required_i16(row, "charging_processes", "car_id")?),
        position_id: optional_i32(row, "charging_processes", "position_id")?.map(i64::from),
        address_id: optional_i32(row, "charging_processes", "address_id")?.map(i64::from),
        geofence_id: optional_i32(row, "charging_processes", "geofence_id")?.map(i64::from),
        start_date_ms: required_timestamp_ms(row, "charging_processes", "start_date")?,
        end_date_ms: optional_timestamp_ms(row, "charging_processes", "end_date")?,
        charge_energy_added: optional_decimal(row, "charging_processes", "charge_energy_added")?,
        charge_energy_used_kwh: optional_decimal(row, "charging_processes", "charge_energy_used")?,
        start_ideal_range_km: optional_decimal(row, "charging_processes", "start_ideal_range_km")?,
        end_ideal_range_km: optional_decimal(row, "charging_processes", "end_ideal_range_km")?,
        start_battery_level: optional_i16(row, "charging_processes", "start_battery_level")?
            .map(i64::from),
        end_battery_level: optional_i16(row, "charging_processes", "end_battery_level")?
            .map(i64::from),
        duration_min: optional_i16(row, "charging_processes", "duration_min")?.map(i64::from),
        outside_temp_avg: optional_decimal(row, "charging_processes", "outside_temp_avg")?,
        cost: optional_decimal(row, "charging_processes", "cost")?,
        start_rated_range_km: optional_decimal(row, "charging_processes", "start_rated_range_km")?,
        end_rated_range_km: optional_decimal(row, "charging_processes", "end_rated_range_km")?,
    })
}

pub(crate) fn decode_charge(row: &Row) -> Result<TeslaMateCharge, TeslaMateReaderError> {
    Ok(TeslaMateCharge {
        id: i64::from(required_i32(row, "charges", "id")?),
        charging_process_id: i64::from(required_i32(row, "charges", "charging_process_id")?),
        date_ms: required_timestamp_ms(row, "charges", "date")?,
        battery_heater: optional_bool(row, "charges", "battery_heater")?,
        battery_heater_on: optional_bool(row, "charges", "battery_heater_on")?,
        battery_heater_no_power: optional_bool(row, "charges", "battery_heater_no_power")?,
        battery_level: optional_i16(row, "charges", "battery_level")?.map(i64::from),
        usable_battery_level: optional_i16(row, "charges", "usable_battery_level")?.map(i64::from),
        charge_energy_added_kwh: optional_decimal(row, "charges", "charge_energy_added")?,
        charger_actual_current: optional_i16(row, "charges", "charger_actual_current")?
            .map(f64::from),
        charger_phases: optional_i16(row, "charges", "charger_phases")?.map(i64::from),
        charger_pilot_current: optional_i16(row, "charges", "charger_pilot_current")?
            .map(f64::from),
        charger_power_kw: optional_i16(row, "charges", "charger_power")?.map(f64::from),
        charger_voltage: optional_i16(row, "charges", "charger_voltage")?.map(f64::from),
        charge_cable: optional_text(row, "charges", "conn_charge_cable")?,
        fast_charger_present: optional_bool(row, "charges", "fast_charger_present")?,
        fast_charger_brand: optional_text(row, "charges", "fast_charger_brand")?,
        fast_charger_type: optional_text(row, "charges", "fast_charger_type")?,
        ideal_range_km: optional_decimal(row, "charges", "ideal_battery_range_km")?,
        rated_range_km: optional_decimal(row, "charges", "rated_battery_range_km")?,
        not_enough_power_to_heat: optional_bool(row, "charges", "not_enough_power_to_heat")?,
        outside_temp_c: optional_decimal(row, "charges", "outside_temp")?,
    })
}

fn decode_address(row: &Row) -> Result<TeslaMateAddress, TeslaMateReaderError> {
    Ok(TeslaMateAddress {
        id: i64::from(required_i32(row, "addresses", "id")?),
        display_name: optional_text(row, "addresses", "display_name")?,
        name: optional_text(row, "addresses", "name")?,
    })
}

fn decode_geofence(row: &Row) -> Result<TeslaMateGeofence, TeslaMateReaderError> {
    Ok(TeslaMateGeofence {
        id: i64::from(required_i32(row, "geofences", "id")?),
        name: required_text(row, "geofences", "name")?,
        latitude: row
            .try_get("latitude")
            .map_err(|source| cell("geofences", "latitude", source))?,
        longitude: row
            .try_get("longitude")
            .map_err(|source| cell("geofences", "longitude", source))?,
        radius_m: row
            .try_get("radius_m")
            .map_err(|source| cell("geofences", "radius", source))?,
        billing_type: optional_text(row, "geofences", "billing_type")?
            .map(|value| value.parse::<GeofenceBillingType>())
            .transpose()
            .map_err(|_| TeslaMateReaderError::InvalidGeofenceBillingType)?,
        cost_per_unit: optional_float(row, "geofences", "cost_per_unit")?,
        session_fee: optional_float(row, "geofences", "session_fee")?,
    })
}

fn decode_state(row: &Row) -> Result<TeslaMateState, TeslaMateReaderError> {
    let state: TeslaMateStateStatus = row
        .try_get("state")
        .map_err(|source| cell("states", "state", source))?;
    Ok(TeslaMateState {
        id: i64::from(required_i32(row, "states", "id")?),
        car_id: i64::from(required_i16(row, "states", "car_id")?),
        state: state.0,
        start_date_ms: required_timestamp_ms(row, "states", "start_date")?,
        end_date_ms: optional_timestamp_ms(row, "states", "end_date")?,
    })
}

fn decode_update(row: &Row) -> Result<TeslaMateUpdate, TeslaMateReaderError> {
    Ok(TeslaMateUpdate {
        id: i64::from(required_i32(row, "updates", "id")?),
        car_id: i64::from(required_i16(row, "updates", "car_id")?),
        start_date_ms: required_timestamp_ms(row, "updates", "start_date")?,
        end_date_ms: optional_timestamp_ms(row, "updates", "end_date")?,
        version: optional_text(row, "updates", "version")?,
    })
}

struct TeslaMateStateStatus(String);

impl<'a> FromSql<'a> for TeslaMateStateStatus {
    fn from_sql(
        _ty: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self(std::str::from_utf8(raw)?.to_owned()))
    }

    fn accepts(ty: &Type) -> bool {
        ty.name() == "states_status"
    }
}
