// SPDX-License-Identifier: AGPL-3.0-only

pub(crate) async fn read_cars(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateCar>, TeslaMateReaderError> {
    let stream = client
        .copy_out(&binary_copy_sql(SourceTable::Cars, selected_car_id))
        .await?;
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        car_copy_types()
    ));
    let mut cars = Vec::new();
    while let Some(row) = rows.as_mut().try_next().await? {
        retain_row(retained_rows, limits.maximum_rows)?;
        cars.push(decode_binary_car(&row)?);
    }
    Ok(cars)
}

fn car_copy_types() -> &'static [Type] {
    const TYPES: [Type; 22] = [
        Type::INT2,
        Type::INT8,
        Type::INT8,
        Type::TEXT,
        Type::TEXT,
        Type::TEXT,
        Type::FLOAT8,
        Type::INT4,
        Type::INT4,
        Type::BOOL,
        Type::BOOL,
        Type::BOOL,
        Type::BOOL,
        Type::BOOL,
        Type::TEXT,
        Type::TEXT,
        Type::TEXT,
        Type::TEXT,
        Type::TEXT,
        Type::INT2,
        Type::TIMESTAMP,
        Type::TIMESTAMP,
    ];
    &TYPES
}

fn decode_binary_car(row: &BinaryCopyOutRow) -> Result<TeslaMateCar, TeslaMateReaderError> {
    let id: i16 = binary_cell(row, 0, "cars", "id")?;
    Ok(TeslaMateCar {
        id: i64::from(id),
        eid: binary_cell(row, 1, "cars", "eid")?,
        vid: binary_cell(row, 2, "cars", "vid")?,
        vin: binary_cell::<Option<&str>>(row, 3, "cars", "vin")?.map(ToOwned::to_owned),
        name: binary_cell::<Option<&str>>(row, 4, "cars", "name")?.map(ToOwned::to_owned),
        model: binary_cell::<Option<&str>>(row, 5, "cars", "model")?.map(ToOwned::to_owned),
        trim_badging: binary_cell::<Option<&str>>(row, 14, "cars", "trim_badging")?
            .map(ToOwned::to_owned),
        marketing_name: binary_cell::<Option<&str>>(row, 15, "cars", "marketing_name")?
            .map(ToOwned::to_owned),
        exterior_color: binary_cell::<Option<&str>>(row, 16, "cars", "exterior_color")?
            .map(ToOwned::to_owned),
        wheel_type: binary_cell::<Option<&str>>(row, 17, "cars", "wheel_type")?
            .map(ToOwned::to_owned),
        spoiler_type: binary_cell::<Option<&str>>(row, 18, "cars", "spoiler_type")?
            .map(ToOwned::to_owned),
        efficiency_wh_per_km: binary_cell(row, 6, "cars", "efficiency")?,
        settings: decode_car_settings_binary(row)?,
    })
}

fn decode_car_settings_binary(
    row: &BinaryCopyOutRow,
) -> Result<ProjectionCarSettings, TeslaMateReaderError> {
    let defaults = ProjectionCarSettings::default();
    Ok(ProjectionCarSettings {
        suspend_min_resolved: true,
        suspend_min: binary_optional_smallint(row, 7, "car_settings", "suspend_min")?
            .map(i64::from)
            .unwrap_or(defaults.suspend_min),
        suspend_after_idle_min: binary_optional_smallint(
            row,
            8,
            "car_settings",
            "suspend_after_idle_min",
        )?
        .map(i64::from)
        .unwrap_or(defaults.suspend_after_idle_min),
        req_not_unlocked: binary_cell::<Option<bool>>(row, 9, "cars", "req_not_unlocked")?
            .unwrap_or(defaults.req_not_unlocked),
        free_supercharging: binary_cell::<Option<bool>>(row, 10, "cars", "free_supercharging")?
            .unwrap_or(defaults.free_supercharging),
        use_streaming_api: binary_cell::<Option<bool>>(row, 11, "cars", "use_streaming_api")?
            .unwrap_or(defaults.use_streaming_api),
        enabled: binary_cell::<Option<bool>>(row, 12, "cars", "enabled")?
            .unwrap_or(defaults.enabled),
        lfp_battery: binary_cell::<Option<bool>>(row, 13, "cars", "lfp_battery")?
            .unwrap_or(defaults.lfp_battery),
    })
}

fn binary_cell<'a, T: FromSql<'a>>(
    row: &'a BinaryCopyOutRow,
    index: usize,
    table: &'static str,
    column: &'static str,
) -> Result<T, TeslaMateReaderError> {
    row.try_get(index)
        .map_err(|source| cell(table, column, source))
}

fn binary_optional_smallint(
    row: &BinaryCopyOutRow,
    index: usize,
    table: &'static str,
    column: &'static str,
) -> Result<Option<i16>, TeslaMateReaderError> {
    binary_cell::<Option<i32>>(row, index, table, column)?
        .map(|value| narrow_smallint(value, table, column))
        .transpose()
}

fn binary_optional_decimal(
    row: &BinaryCopyOutRow,
    index: usize,
    table: &'static str,
    column: &'static str,
) -> Result<Option<f64>, TeslaMateReaderError> {
    binary_cell::<Option<Decimal>>(row, index, table, column)?
        .map(|value| {
            value
                .to_f64()
                .ok_or(TeslaMateReaderError::DecimalOutOfRange { table, column })
        })
        .transpose()
}

fn binary_required_decimal(
    row: &BinaryCopyOutRow,
    index: usize,
    table: &'static str,
    column: &'static str,
) -> Result<f64, TeslaMateReaderError> {
    binary_cell::<Decimal>(row, index, table, column)?
        .to_f64()
        .ok_or(TeslaMateReaderError::DecimalOutOfRange { table, column })
}

fn binary_required_timestamp_ms(
    row: &BinaryCopyOutRow,
    index: usize,
    table: &'static str,
    column: &'static str,
) -> Result<i64, TeslaMateReaderError> {
    timestamp_ms(binary_cell(row, index, table, column)?, table, column)
}

fn binary_optional_timestamp_ms(
    row: &BinaryCopyOutRow,
    index: usize,
    table: &'static str,
    column: &'static str,
) -> Result<Option<i64>, TeslaMateReaderError> {
    binary_cell::<Option<PrimitiveDateTime>>(row, index, table, column)?
        .map(|value| timestamp_ms(value, table, column))
        .transpose()
}

pub(crate) async fn read_drives(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateDrive>, TeslaMateReaderError> {
    let stream = client
        .copy_out(&binary_copy_sql(SourceTable::Drives, selected_car_id))
        .await?;
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        drive_copy_types()
    ));
    let mut drives = Vec::new();
    while let Some(row) = rows.as_mut().try_next().await? {
        retain_row(retained_rows, limits.maximum_rows)?;
        drives.push(decode_binary_drive(&row)?);
    }
    Ok(drives)
}

fn drive_copy_types() -> &'static [Type] {
    const TYPES: [Type; 25] = [
        Type::INT4,
        Type::INT2,
        Type::TIMESTAMP,
        Type::TIMESTAMP,
        Type::INT4,
        Type::INT4,
        Type::INT4,
        Type::INT4,
        Type::INT4,
        Type::INT4,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::INT2,
        Type::INT2,
        Type::INT2,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::FLOAT8,
        Type::FLOAT8,
        Type::FLOAT8,
        Type::INT2,
        Type::INT2,
        Type::INT2,
    ];
    &TYPES
}

fn decode_binary_drive(row: &BinaryCopyOutRow) -> Result<TeslaMateDrive, TeslaMateReaderError> {
    let id: i32 = binary_cell(row, 0, "drives", "id")?;
    let car_id: i16 = binary_cell(row, 1, "drives", "car_id")?;
    Ok(TeslaMateDrive {
        id: i64::from(id),
        car_id: i64::from(car_id),
        start_date_ms: binary_required_timestamp_ms(row, 2, "drives", "start_date")?,
        end_date_ms: binary_optional_timestamp_ms(row, 3, "drives", "end_date")?,
        start_position_id: binary_cell::<Option<i32>>(row, 4, "drives", "start_position_id")?
            .map(i64::from),
        end_position_id: binary_cell::<Option<i32>>(row, 5, "drives", "end_position_id")?
            .map(i64::from),
        start_address_id: binary_cell::<Option<i32>>(row, 6, "drives", "start_address_id")?
            .map(i64::from),
        end_address_id: binary_cell::<Option<i32>>(row, 7, "drives", "end_address_id")?
            .map(i64::from),
        start_geofence_id: binary_cell::<Option<i32>>(row, 8, "drives", "start_geofence_id")?
            .map(i64::from),
        end_geofence_id: binary_cell::<Option<i32>>(row, 9, "drives", "end_geofence_id")?
            .map(i64::from),
        outside_temp_avg: binary_optional_decimal(row, 10, "drives", "outside_temp_avg")?,
        inside_temp_avg: binary_optional_decimal(row, 11, "drives", "inside_temp_avg")?,
        speed_max: binary_cell::<Option<i16>>(row, 12, "drives", "speed_max")?.map(i64::from),
        power_max: binary_cell::<Option<i16>>(row, 13, "drives", "power_max")?.map(f64::from),
        power_min: binary_cell::<Option<i16>>(row, 14, "drives", "power_min")?.map(f64::from),
        start_ideal_range_km: binary_optional_decimal(row, 15, "drives", "start_ideal_range_km")?,
        end_ideal_range_km: binary_optional_decimal(row, 16, "drives", "end_ideal_range_km")?,
        start_rated_range_km: binary_optional_decimal(row, 17, "drives", "start_rated_range_km")?,
        end_rated_range_km: binary_optional_decimal(row, 18, "drives", "end_rated_range_km")?,
        start_km: binary_cell(row, 19, "drives", "start_km")?,
        end_km: binary_cell(row, 20, "drives", "end_km")?,
        distance_km: binary_cell(row, 21, "drives", "distance")?,
        duration_min: binary_cell::<Option<i16>>(row, 22, "drives", "duration_min")?.map(i64::from),
        ascent: binary_cell::<Option<i16>>(row, 23, "drives", "ascent")?.map(i64::from),
        descent: binary_cell::<Option<i16>>(row, 24, "drives", "descent")?.map(i64::from),
    })
}

pub(crate) fn position_copy_types() -> &'static [Type] {
    const TYPES: [Type; 30] = [
        Type::INT4,
        Type::INT2,
        Type::INT8,
        Type::TIMESTAMP,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::INT8,
        Type::INT8,
        Type::FLOAT8,
        Type::FLOAT8,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::INT8,
        Type::INT8,
        Type::BOOL,
        Type::BOOL,
        Type::BOOL,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::INT8,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::BOOL,
        Type::BOOL,
        Type::BOOL,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::NUMERIC,
    ];
    &TYPES
}

pub(crate) fn decode_binary_position(
    row: &BinaryCopyOutRow,
) -> Result<TeslaMatePosition, TeslaMateReaderError> {
    let id: i32 = binary_cell(row, 0, "positions", "id")?;
    let car_id: i16 = binary_cell(row, 1, "positions", "car_id")?;
    Ok(TeslaMatePosition {
        id: i64::from(id),
        car_id: i64::from(car_id),
        drive_id: binary_cell::<Option<i64>>(row, 2, "positions", "drive_id")?,
        date_ms: binary_required_timestamp_ms(row, 3, "positions", "date")?,
        latitude: binary_required_decimal(row, 4, "positions", "latitude")?,
        longitude: binary_required_decimal(row, 5, "positions", "longitude")?,
        elevation: binary_cell::<Option<i64>>(row, 6, "positions", "elevation")?,
        speed: binary_cell::<Option<i64>>(row, 7, "positions", "speed")?,
        power: binary_cell(row, 8, "positions", "power")?,
        odometer: binary_cell(row, 9, "positions", "odometer")?,
        ideal_battery_range_km: binary_optional_decimal(
            row,
            10,
            "positions",
            "ideal_battery_range_km",
        )?,
        est_battery_range_km: binary_optional_decimal(
            row,
            11,
            "positions",
            "est_battery_range_km",
        )?,
        rated_battery_range_km: binary_optional_decimal(
            row,
            12,
            "positions",
            "rated_battery_range_km",
        )?,
        battery_level: binary_cell::<Option<i64>>(row, 13, "positions", "battery_level")?,
        usable_battery_level: binary_cell::<Option<i64>>(
            row,
            14,
            "positions",
            "usable_battery_level",
        )?,
        battery_heater: binary_cell(row, 15, "positions", "battery_heater")?,
        battery_heater_on: binary_cell(row, 16, "positions", "battery_heater_on")?,
        battery_heater_no_power: binary_cell(row, 17, "positions", "battery_heater_no_power")?,
        is_climate_on: binary_cell(row, 23, "positions", "is_climate_on")?,
        outside_temp: binary_optional_decimal(row, 18, "positions", "outside_temp")?,
        inside_temp: binary_optional_decimal(row, 19, "positions", "inside_temp")?,
        fan_status: binary_cell(row, 20, "positions", "fan_status")?,
        driver_temp_setting: binary_optional_decimal(row, 21, "positions", "driver_temp_setting")?,
        passenger_temp_setting: binary_optional_decimal(
            row,
            22,
            "positions",
            "passenger_temp_setting",
        )?,
        is_rear_defroster_on: binary_cell(row, 24, "positions", "is_rear_defroster_on")?,
        is_front_defroster_on: binary_cell(row, 25, "positions", "is_front_defroster_on")?,
        tpms_pressure_fl: binary_optional_decimal(row, 26, "positions", "tpms_pressure_fl")?,
        tpms_pressure_fr: binary_optional_decimal(row, 27, "positions", "tpms_pressure_fr")?,
        tpms_pressure_rl: binary_optional_decimal(row, 28, "positions", "tpms_pressure_rl")?,
        tpms_pressure_rr: binary_optional_decimal(row, 29, "positions", "tpms_pressure_rr")?,
    })
}

async fn read_positions(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMatePosition>, TeslaMateReaderError> {
    let remaining_rows = limits.maximum_rows.saturating_sub(*retained_rows);
    let maximum = MAX_MATERIALIZED_HISTORY_POSITIONS.min(remaining_rows);
    let count_sql = materialized_position_count_sql(maximum);
    let count: i64 = client
        .query_one(&count_sql, &[&selected_car_id])
        .await?
        .try_get("position_count")?;
    let count = validate_materialized_history_position_count(count, maximum)?;
    let query_limit = i64::try_from(maximum.saturating_add(1)).unwrap_or(i64::MAX);
    let stream = client
        .copy_out(&bounded_position_binary_copy_sql(
            selected_car_id,
            query_limit,
        ))
        .await?;
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        position_copy_types()
    ));
    let mut positions = Vec::with_capacity(count);
    while let Some(row) = rows.as_mut().try_next().await? {
        if positions.len() >= maximum {
            return Err(
                TeslaMateReaderError::MaterializedHistoryPositionLimitExceeded {
                    maximum,
                    count: positions.len().saturating_add(1),
                },
            );
        }
        retain_row(retained_rows, limits.maximum_rows)?;
        positions.push(decode_binary_position(&row)?);
    }
    Ok(positions)
}

fn materialized_position_count_sql(maximum: usize) -> String {
    let limit = maximum.saturating_add(1);
    format!(
        "SELECT COUNT(*)::bigint AS position_count FROM (\
         SELECT 1 FROM \"public\".\"positions\" \
         WHERE \"car_id\" = $1 ORDER BY \"id\" ASC LIMIT {limit}\
         ) AS bounded_positions"
    )
}

fn validate_materialized_history_position_count(
    count: i64,
    maximum: usize,
) -> Result<usize, TeslaMateReaderError> {
    let count = usize::try_from(count).map_err(|_| TeslaMateReaderError::InvalidSourceCount {
        column: "positions",
        count,
    })?;
    if count > maximum {
        return Err(
            TeslaMateReaderError::MaterializedHistoryPositionLimitExceeded { maximum, count },
        );
    }
    Ok(count)
}

pub(crate) async fn read_charging_processes(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateChargingProcess>, TeslaMateReaderError> {
    let stream = client
        .copy_out(&binary_copy_sql(
            SourceTable::ChargingProcesses,
            selected_car_id,
        ))
        .await?;
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        charging_process_copy_types()
    ));
    let mut processes = Vec::new();
    while let Some(row) = rows.as_mut().try_next().await? {
        retain_row(retained_rows, limits.maximum_rows)?;
        processes.push(decode_binary_charging_process(&row)?);
    }
    Ok(processes)
}

fn charging_process_copy_types() -> &'static [Type] {
    const TYPES: [Type; 18] = [
        Type::INT4,
        Type::INT2,
        Type::INT4,
        Type::INT4,
        Type::INT4,
        Type::TIMESTAMP,
        Type::TIMESTAMP,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::INT2,
        Type::INT2,
        Type::INT2,
        Type::NUMERIC,
        Type::NUMERIC,
    ];
    &TYPES
}

fn decode_binary_charging_process(
    row: &BinaryCopyOutRow,
) -> Result<TeslaMateChargingProcess, TeslaMateReaderError> {
    let id: i32 = binary_cell(row, 0, "charging_processes", "id")?;
    let car_id: i16 = binary_cell(row, 1, "charging_processes", "car_id")?;
    Ok(TeslaMateChargingProcess {
        id: i64::from(id),
        car_id: i64::from(car_id),
        position_id: binary_cell::<Option<i32>>(row, 2, "charging_processes", "position_id")?
            .map(i64::from),
        address_id: binary_cell::<Option<i32>>(row, 3, "charging_processes", "address_id")?
            .map(i64::from),
        geofence_id: binary_cell::<Option<i32>>(row, 4, "charging_processes", "geofence_id")?
            .map(i64::from),
        start_date_ms: binary_required_timestamp_ms(row, 5, "charging_processes", "start_date")?,
        end_date_ms: binary_optional_timestamp_ms(row, 6, "charging_processes", "end_date")?,
        charge_energy_added: binary_optional_decimal(
            row,
            7,
            "charging_processes",
            "charge_energy_added",
        )?,
        charge_energy_used_kwh: binary_optional_decimal(
            row,
            8,
            "charging_processes",
            "charge_energy_used",
        )?,
        start_ideal_range_km: binary_optional_decimal(
            row,
            9,
            "charging_processes",
            "start_ideal_range_km",
        )?,
        end_ideal_range_km: binary_optional_decimal(
            row,
            10,
            "charging_processes",
            "end_ideal_range_km",
        )?,
        start_battery_level: binary_cell::<Option<i16>>(
            row,
            13,
            "charging_processes",
            "start_battery_level",
        )?
        .map(i64::from),
        end_battery_level: binary_cell::<Option<i16>>(
            row,
            14,
            "charging_processes",
            "end_battery_level",
        )?
        .map(i64::from),
        duration_min: binary_cell::<Option<i16>>(row, 15, "charging_processes", "duration_min")?
            .map(i64::from),
        outside_temp_avg: binary_optional_decimal(
            row,
            16,
            "charging_processes",
            "outside_temp_avg",
        )?,
        cost: binary_optional_decimal(row, 17, "charging_processes", "cost")?,
        start_rated_range_km: binary_optional_decimal(
            row,
            11,
            "charging_processes",
            "start_rated_range_km",
        )?,
        end_rated_range_km: binary_optional_decimal(
            row,
            12,
            "charging_processes",
            "end_rated_range_km",
        )?,
    })
}

async fn read_charges(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateCharge>, TeslaMateReaderError> {
    let stream = client
        .copy_out(&binary_copy_sql(SourceTable::Charges, selected_car_id))
        .await?;
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        charge_copy_types()
    ));
    let mut charges = Vec::new();
    while let Some(row) = rows.as_mut().try_next().await? {
        retain_row(retained_rows, limits.maximum_rows)?;
        charges.push(decode_binary_charge(&row)?);
    }
    Ok(charges)
}

pub(crate) fn charge_copy_types() -> &'static [Type] {
    const TYPES: [Type; 22] = [
        Type::INT4,
        Type::INT4,
        Type::TIMESTAMP,
        Type::BOOL,
        Type::BOOL,
        Type::BOOL,
        Type::INT2,
        Type::INT2,
        Type::NUMERIC,
        Type::INT2,
        Type::INT2,
        Type::INT2,
        Type::INT2,
        Type::INT2,
        Type::TEXT,
        Type::BOOL,
        Type::TEXT,
        Type::TEXT,
        Type::NUMERIC,
        Type::NUMERIC,
        Type::BOOL,
        Type::NUMERIC,
    ];
    &TYPES
}

pub(crate) fn decode_binary_charge(
    row: &BinaryCopyOutRow,
) -> Result<TeslaMateCharge, TeslaMateReaderError> {
    let id: i32 = binary_cell(row, 0, "charges", "id")?;
    let process_id: i32 = binary_cell(row, 1, "charges", "charging_process_id")?;
    Ok(TeslaMateCharge {
        id: i64::from(id),
        charging_process_id: i64::from(process_id),
        date_ms: binary_required_timestamp_ms(row, 2, "charges", "date")?,
        battery_heater: binary_cell(row, 3, "charges", "battery_heater")?,
        battery_heater_on: binary_cell(row, 4, "charges", "battery_heater_on")?,
        battery_heater_no_power: binary_cell(row, 5, "charges", "battery_heater_no_power")?,
        battery_level: binary_cell::<Option<i16>>(row, 6, "charges", "battery_level")?
            .map(i64::from),
        usable_battery_level: binary_cell::<Option<i16>>(
            row,
            7,
            "charges",
            "usable_battery_level",
        )?
        .map(i64::from),
        charge_energy_added_kwh: binary_optional_decimal(row, 8, "charges", "charge_energy_added")?,
        charger_actual_current: binary_cell::<Option<i16>>(
            row,
            9,
            "charges",
            "charger_actual_current",
        )?
        .map(f64::from),
        charger_phases: binary_cell::<Option<i16>>(row, 10, "charges", "charger_phases")?
            .map(i64::from),
        charger_pilot_current: binary_cell::<Option<i16>>(
            row,
            11,
            "charges",
            "charger_pilot_current",
        )?
        .map(f64::from),
        charger_power_kw: binary_cell::<Option<i16>>(row, 12, "charges", "charger_power")?
            .map(f64::from),
        charger_voltage: binary_cell::<Option<i16>>(row, 13, "charges", "charger_voltage")?
            .map(f64::from),
        charge_cable: binary_cell::<Option<&str>>(row, 14, "charges", "conn_charge_cable")?
            .map(ToOwned::to_owned),
        fast_charger_present: binary_cell(row, 15, "charges", "fast_charger_present")?,
        fast_charger_brand: binary_cell::<Option<&str>>(row, 16, "charges", "fast_charger_brand")?
            .map(ToOwned::to_owned),
        fast_charger_type: binary_cell::<Option<&str>>(row, 17, "charges", "fast_charger_type")?
            .map(ToOwned::to_owned),
        ideal_range_km: binary_optional_decimal(row, 18, "charges", "ideal_battery_range_km")?,
        rated_range_km: binary_optional_decimal(row, 19, "charges", "rated_battery_range_km")?,
        not_enough_power_to_heat: binary_cell(row, 20, "charges", "not_enough_power_to_heat")?,
        outside_temp_c: binary_optional_decimal(row, 21, "charges", "outside_temp")?,
    })
}

pub(crate) async fn read_addresses(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateAddress>, TeslaMateReaderError> {
    let stream = client
        .copy_out(&binary_copy_sql(SourceTable::Addresses, selected_car_id))
        .await?;
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        address_copy_types()
    ));
    let mut addresses = Vec::new();
    while let Some(row) = rows.as_mut().try_next().await? {
        retain_row(retained_rows, limits.maximum_rows)?;
        let id: i32 = binary_cell(&row, 0, "addresses", "id")?;
        addresses.push(TeslaMateAddress {
            id: i64::from(id),
            display_name: binary_cell::<Option<&str>>(&row, 1, "addresses", "display_name")?
                .map(ToOwned::to_owned),
            name: binary_cell::<Option<&str>>(&row, 2, "addresses", "name")?.map(ToOwned::to_owned),
        });
    }
    Ok(addresses)
}

fn address_copy_types() -> &'static [Type] {
    const TYPES: [Type; 3] = [Type::INT4, Type::TEXT, Type::TEXT];
    &TYPES
}

pub(crate) async fn read_geofences(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateGeofence>, TeslaMateReaderError> {
    let mut geofences = Vec::new();
    let mut last_id = 0_i32;
    let page_size = i64::from(limits.page_size);
    loop {
        let rows = client
            .query(
                GEOFENCE_GEOMETRY_SQL,
                &[&last_id, &page_size, &selected_car_id],
            )
            .await?;
        let page_len = rows.len();
        for row in rows {
            let id = required_i32(&row, "geofences", "id")?;
            if id <= last_id {
                return Err(TeslaMateReaderError::NonProgressingPage { table: "geofences" });
            }
            last_id = id;
            retain_row(retained_rows, limits.maximum_rows)?;
            geofences.push(decode_geofence(&row)?);
        }
        if page_len < limits.page_size as usize {
            break;
        }
    }
    Ok(geofences)
}
