// SPDX-License-Identifier: AGPL-3.0-only

fn validate_request(
    request: &ProjectionPackRequest<'_>,
    limits: ProtocolLimits,
) -> Result<(), ProjectionPackError> {
    if request.pack_id.is_nil() || request.snapshot_id.is_nil() {
        return Err(invalid("pack and snapshot IDs must not be nil"));
    }
    validate_binding(&request.binding)?;
    if !request.sequence.is_ordered() {
        return Err(invalid("full snapshot sequence is unordered"));
    }
    if request.snapshot.row_count()? > limits.max_rows_per_pack {
        return Err(ProjectionPackError::TooManyRows);
    }
    if request.snapshot.cars.len() != 1 {
        return Err(invalid(
            "one vehicle projection must contain exactly one car",
        ));
    }

    let car = &request.snapshot.cars[0];
    require_positive(car.id, "car.id")?;
    if car.id != request.binding.selected_car_id {
        return Err(invalid("selected_car_id does not match car.id"));
    }
    validate_required_text(&car.name, "car.name")?;
    validate_required_text(&car.model, "car.model")?;
    validate_optional_text(car.vin.as_deref(), "car.vin")?;
    validate_optional_text(car.firmware_version.as_deref(), "car.firmware_version")?;
    validate_optional_nonnegative(car.efficiency_wh_per_km, "car.efficiency_wh_per_km")?;
    validate_car_settings(&car.settings)?;

    let mut drive_ids = HashSet::with_capacity(request.snapshot.drives.len());
    for drive in &request.snapshot.drives {
        require_unique_positive(&mut drive_ids, drive.id, "drive.id")?;
        require_same_car(
            drive.car_id,
            request.binding.selected_car_id,
            "drive.car_id",
        )?;
        validate_interval(drive.start_date_ms, drive.end_date_ms, "drive")?;
        validate_optional_positive(drive.optimized_at_ms, "drive.optimized_at_ms")?;
        validate_optional_nonnegative(drive.distance_km, "drive.distance_km")?;
        validate_optional_nonnegative(drive.efficiency, "drive.efficiency")?;
        validate_optional_nonnegative(drive.start_rated_range_km, "drive.start_rated_range_km")?;
        validate_optional_nonnegative(drive.end_rated_range_km, "drive.end_rated_range_km")?;
        validate_optional_finite(drive.outside_temp_avg, "drive.outside_temp_avg")?;
        validate_coordinate_pair(drive.start_latitude, drive.start_longitude, "drive.start")?;
        validate_coordinate_pair(drive.end_latitude, drive.end_longitude, "drive.end")?;
        validate_optional_soc(drive.start_soc, "drive.start_soc")?;
        validate_optional_soc(drive.end_soc, "drive.end_soc")?;
        for (value, name) in [
            (drive.start_address.as_deref(), "drive.start_address"),
            (drive.end_address.as_deref(), "drive.end_address"),
            (drive.start_geofence.as_deref(), "drive.start_geofence"),
            (drive.end_geofence.as_deref(), "drive.end_geofence"),
        ] {
            validate_optional_text(value, name)?;
        }
    }

    let mut charge_ids = HashSet::with_capacity(request.snapshot.charges.len());
    for charge in &request.snapshot.charges {
        require_unique_positive(&mut charge_ids, charge.id, "charge.id")?;
        require_same_car(
            charge.car_id,
            request.binding.selected_car_id,
            "charge.car_id",
        )?;
        require_positive(charge.start_date_ms, "charge.start_date_ms")?;
        if charge
            .end_date_ms
            .is_some_and(|end| end < charge.start_date_ms)
        {
            return Err(invalid("charge.end_date_ms precedes charge.start_date_ms"));
        }
        validate_optional_nonnegative(charge.charge_energy_added, "charge.charge_energy_added")?;
        validate_optional_finite(charge.cost, "charge.cost")?;
        validate_optional_finite(charge.cost_per_unit, "charge.cost_per_unit")?;
        validate_optional_finite(charge.session_fee, "charge.session_fee")?;
        validate_optional_nonnegative(
            charge.charge_energy_used_kwh,
            "charge.charge_energy_used_kwh",
        )?;
        validate_optional_nonnegative(
            charge.charge_rate_km_per_hour,
            "charge.charge_rate_km_per_hour",
        )?;
        validate_optional_nonnegative(charge.max_charger_power_kw, "charge.max_charger_power_kw")?;
        validate_optional_nonnegative(charge.start_rated_range_km, "charge.start_rated_range_km")?;
        validate_optional_nonnegative(charge.end_rated_range_km, "charge.end_rated_range_km")?;
        validate_optional_finite(charge.outside_temp_avg, "charge.outside_temp_avg")?;
        validate_optional_soc(charge.start_battery_level, "charge.start_battery_level")?;
        validate_optional_soc(charge.end_battery_level, "charge.end_battery_level")?;
        for (value, name) in [
            (charge.address.as_deref(), "charge.address"),
            (charge.location_name.as_deref(), "charge.location_name"),
            (charge.geofence.as_deref(), "charge.geofence"),
            (
                charge.fast_charger_type.as_deref(),
                "charge.fast_charger_type",
            ),
        ] {
            validate_optional_text(value, name)?;
        }
    }

    let mut position_ids = HashSet::with_capacity(request.snapshot.positions.len());
    for position in &request.snapshot.positions {
        require_unique_positive(&mut position_ids, position.id, "position.id")?;
        require_same_car(
            position.car_id,
            request.binding.selected_car_id,
            "position.car_id",
        )?;
        require_positive(position.date_ms, "position.date_ms")?;
        if let Some(drive_id) = position.drive_id
            && !drive_ids.contains(&drive_id)
        {
            return Err(invalid("position.drive_id is not present in this pack"));
        }
        validate_coordinate(position.latitude, position.longitude, "position")?;
        validate_optional_soc(position.battery_level, "position.battery_level")?;
        validate_optional_soc(
            position.usable_battery_level,
            "position.usable_battery_level",
        )?;
        validate_optional_nonnegative(position.odometer, "position.odometer")?;
        validate_optional_nonnegative(
            position.ideal_battery_range_km,
            "position.ideal_battery_range_km",
        )?;
        validate_optional_nonnegative(
            position.rated_battery_range_km,
            "position.rated_battery_range_km",
        )?;
        validate_optional_finite(position.inside_temp, "position.inside_temp")?;
        validate_optional_finite(position.outside_temp, "position.outside_temp")?;
    }

    let mut sample_ids = HashSet::with_capacity(request.snapshot.charge_samples.len());
    for sample in &request.snapshot.charge_samples {
        require_unique_positive(&mut sample_ids, sample.id, "charge_sample.id")?;
        require_positive(sample.timestamp_ms, "charge_sample.timestamp_ms")?;
        if !charge_ids.contains(&sample.charge_process_id) {
            return Err(invalid(
                "charge_sample.charge_process_id is not present in this pack",
            ));
        }
        validate_optional_soc(sample.battery_level, "charge_sample.battery_level")?;
        validate_optional_soc(
            sample.usable_battery_level,
            "charge_sample.usable_battery_level",
        )?;
        for (value, name) in [
            (
                sample.charge_energy_added_kwh,
                "charge_sample.charge_energy_added_kwh",
            ),
            (sample.charger_power_kw, "charge_sample.charger_power_kw"),
            (sample.charger_voltage, "charge_sample.charger_voltage"),
            (
                sample.charger_actual_current,
                "charge_sample.charger_actual_current",
            ),
            (
                sample.charger_pilot_current,
                "charge_sample.charger_pilot_current",
            ),
            (sample.ideal_range_km, "charge_sample.ideal_range_km"),
            (sample.rated_range_km, "charge_sample.rated_range_km"),
        ] {
            validate_optional_nonnegative(value, name)?;
        }
        validate_optional_finite(sample.outside_temp_c, "charge_sample.outside_temp_c")?;
        for (value, name) in [
            (
                sample.fast_charger_brand.as_deref(),
                "charge_sample.fast_charger_brand",
            ),
            (
                sample.fast_charger_type.as_deref(),
                "charge_sample.fast_charger_type",
            ),
            (sample.charge_cable.as_deref(), "charge_sample.charge_cable"),
        ] {
            validate_optional_text(value, name)?;
        }
    }
    Ok(())
}

/// Validate the separate schema-2.2 local physical snapshot. Selected-car
/// scope and the charge-to-extant-process source query boundary are checked in
/// Rust; V3 SQLite deliberately carries no local source FKs or normalized
/// compatibility rows.
fn validate_request_v2_2(
    request: &ProjectionPackRequestV2_2<'_>,
    limits: ProtocolLimits,
) -> Result<u64, ProjectionPackError> {
    if request.pack_id.is_nil() || request.snapshot_id.is_nil() {
        return Err(invalid("pack and snapshot IDs must not be nil"));
    }
    if request.ordinal != 0 {
        return Err(invalid("schema 2.2 full snapshot must use ordinal 0"));
    }
    validate_binding_v2_2(&request.binding)?;
    if !request.sequence.is_ordered() {
        return Err(invalid("full snapshot sequence is unordered"));
    }
    let snapshot = request.snapshot;
    let row_count = snapshot.row_count()?;
    if row_count > limits.max_rows_per_pack {
        return Err(ProjectionPackError::TooManyRows);
    }
    if snapshot.global_settings.len() != 1 {
        return Err(invalid(
            "schema 2.2 physical snapshot must contain exactly one global_settings row",
        ));
    }
    let global_settings = &snapshot.global_settings[0];
    validate_optional_text_with_source_width(
        global_settings.base_url.as_deref(),
        255,
        "global_settings.base_url",
    )?;
    validate_optional_text_with_source_width(
        global_settings.grafana_url.as_deref(),
        255,
        "global_settings.grafana_url",
    )?;
    // Required source TEXT permits the empty string. Keep only the generic
    // safety bound; neither field has a reviewed vocabulary restriction.
    validate_optional_text(Some(&global_settings.language), "global_settings.language")?;
    validate_optional_text(
        Some(&global_settings.theme_mode),
        "global_settings.theme_mode",
    )?;
    validate_timestamp_0_pg_us(
        global_settings.inserted_at_pg_us,
        "global_settings.inserted_at_pg_us",
    )?;
    validate_timestamp_0_pg_us(
        global_settings.updated_at_pg_us,
        "global_settings.updated_at_pg_us",
    )?;
    if snapshot.cars.len() != 1 {
        return Err(invalid(
            "one vehicle projection must contain exactly one car",
        ));
    }
    if snapshot.car_settings.len() != 1 {
        return Err(invalid(
            "one vehicle projection must contain exactly one car_settings row",
        ));
    }

    let car = &snapshot.cars[0];
    if i64::from(car.id) != request.binding.selected_car_id {
        return Err(invalid("selected_car_id does not match car.id"));
    }
    // These are physical source values, not the normalized legacy car
    // projection. `efficiency` is encoded as its exact IEEE-754 bit pattern;
    // do not normalize, reject, or convert its FLOAT8 representation.
    validate_optional_text_with_source_width(car.model.as_deref(), 255, "car.model")?;
    validate_optional_text_with_source_width(
        car.marketing_name.as_deref(),
        255,
        "car.marketing_name",
    )?;
    validate_timestamp_0_pg_us(car.inserted_at_pg_us, "car.inserted_at_pg_us")?;
    validate_timestamp_0_pg_us(car.updated_at_pg_us, "car.updated_at_pg_us")?;
    for (value, field) in [
        (car.vin.as_deref(), "car.vin"),
        (car.name.as_deref(), "car.name"),
        (car.trim_badging.as_deref(), "car.trim_badging"),
        (car.exterior_color.as_deref(), "car.exterior_color"),
        (car.wheel_type.as_deref(), "car.wheel_type"),
        (car.spoiler_type.as_deref(), "car.spoiler_type"),
    ] {
        validate_optional_text(value, field)?;
    }
    let car_settings = &snapshot.car_settings[0];
    if car.settings_id != car_settings.id {
        return Err(invalid(
            "car.settings_id does not match the selected car_settings.id",
        ));
    }

    let mut drive_ids = HashSet::with_capacity(snapshot.drives.len());
    let mut referenced_address_ids = HashSet::new();
    let mut referenced_geofence_ids = HashSet::new();
    for drive in &snapshot.drives {
        require_unique_signed_i32(&mut drive_ids, drive.id, "drive.id")?;
        if i64::from(drive.car_id) != request.binding.selected_car_id {
            return Err(invalid("drive.car_id does not match selected_car_id"));
        }
        validate_postgres_timestamp_us(drive.start_date_pg_us, "drive.start_date_pg_us")?;
        if let Some(end_date_pg_us) = drive.end_date_pg_us {
            validate_postgres_timestamp_us(end_date_pg_us, "drive.end_date_pg_us")?;
        }
        for (value, minimum, maximum, field) in [
            (
                drive.outside_temp_avg_e1,
                -9_999,
                9_999,
                "drive.outside_temp_avg_e1",
            ),
            (
                drive.inside_temp_avg_e1,
                -9_999,
                9_999,
                "drive.inside_temp_avg_e1",
            ),
            (
                drive.start_ideal_range_km_e2,
                -999_999,
                999_999,
                "drive.start_ideal_range_km_e2",
            ),
            (
                drive.end_ideal_range_km_e2,
                -999_999,
                999_999,
                "drive.end_ideal_range_km_e2",
            ),
            (
                drive.start_rated_range_km_e2,
                -999_999,
                999_999,
                "drive.start_rated_range_km_e2",
            ),
            (
                drive.end_rated_range_km_e2,
                -999_999,
                999_999,
                "drive.end_rated_range_km_e2",
            ),
        ] {
            validate_optional_fixed_numeric_v2_2(value, minimum, maximum, field)?;
        }
        for id in [drive.start_address_id, drive.end_address_id]
            .into_iter()
            .flatten()
        {
            referenced_address_ids.insert(i64::from(id));
        }
        for id in [drive.start_geofence_id, drive.end_geofence_id]
            .into_iter()
            .flatten()
        {
            referenced_geofence_ids.insert(i64::from(id));
        }
    }

    let mut address_ids = HashSet::with_capacity(snapshot.addresses.len());
    for address in &snapshot.addresses {
        require_unique_signed_i32(&mut address_ids, address.id, "address.id")?;
        validate_optional_text_with_source_width(
            address.display_name.as_deref(),
            512,
            "address.display_name",
        )?;
        validate_optional_fixed_numeric_v2_2(
            address.latitude_e6,
            -99_999_999,
            99_999_999,
            "address.latitude_e6",
        )?;
        validate_optional_fixed_numeric_v2_2(
            address.longitude_e6,
            -999_999_999,
            999_999_999,
            "address.longitude_e6",
        )?;
        for (value, field) in [
            (address.name.as_deref(), "address.name"),
            (address.house_number.as_deref(), "address.house_number"),
            (address.road.as_deref(), "address.road"),
            (address.neighbourhood.as_deref(), "address.neighbourhood"),
            (address.city.as_deref(), "address.city"),
            (address.county.as_deref(), "address.county"),
            (address.postcode.as_deref(), "address.postcode"),
            (address.state.as_deref(), "address.state"),
            (address.state_district.as_deref(), "address.state_district"),
            (address.country.as_deref(), "address.country"),
        ] {
            validate_optional_text_with_source_width(value, 255, field)?;
        }
        // `osm_id` is a nullable source bigint with no source positivity
        // constraint. `osm_type` is source TEXT, so it uses only the generic
        // bounded-string admission.
        validate_optional_text(address.osm_type.as_deref(), "address.osm_type")?;
        validate_timestamp_0_pg_us(address.inserted_at_pg_us, "address.inserted_at_pg_us")?;
        validate_timestamp_0_pg_us(address.updated_at_pg_us, "address.updated_at_pg_us")?;
    }

    let mut geofence_ids = HashSet::with_capacity(snapshot.geofences.len());
    for geofence in &snapshot.geofences {
        require_unique_signed_i32(&mut geofence_ids, geofence.id, "geofence.id")?;
        validate_required_text_with_source_width(&geofence.name, 255, "geofence.name")?;
        // These bounds are the pinned physical `numeric(p,s)` domains, not
        // geography policy. In particular, `(0, 0)` is a valid source value.
        validate_fixed_numeric_v2_2(
            geofence.latitude_e6,
            -99_999_999,
            99_999_999,
            "geofence.latitude_e6",
        )?;
        validate_fixed_numeric_v2_2(
            geofence.longitude_e6,
            -999_999_999,
            999_999_999,
            "geofence.longitude_e6",
        )?;
        // `radius` is already i16, so every physical smallint value—including
        // zero and signed extremes—is deliberately admissible.
        validate_optional_fixed_numeric_v2_2(
            geofence.cost_per_unit_e4,
            -999_999,
            999_999,
            "geofence.cost_per_unit_e4",
        )?;
        validate_optional_fixed_numeric_v2_2(
            geofence.session_fee_e2,
            -999_999,
            999_999,
            "geofence.session_fee_e2",
        )?;
        validate_timestamp_0_pg_us(geofence.inserted_at_pg_us, "geofence.inserted_at_pg_us")?;
        validate_timestamp_0_pg_us(geofence.updated_at_pg_us, "geofence.updated_at_pg_us")?;
    }

    let mut charging_process_ids = HashSet::with_capacity(snapshot.charging_processes.len());
    for process in &snapshot.charging_processes {
        require_unique_signed_i32(&mut charging_process_ids, process.id, "charging_process.id")?;
        if i64::from(process.car_id) != request.binding.selected_car_id {
            return Err(invalid(
                "charging_process.car_id does not match selected_car_id",
            ));
        }
        validate_postgres_timestamp_us(
            process.start_date_pg_us,
            "charging_process.start_date_pg_us",
        )?;
        if let Some(end_date_pg_us) = process.end_date_pg_us {
            validate_postgres_timestamp_us(end_date_pg_us, "charging_process.end_date_pg_us")?;
        }
        for (value, minimum, maximum, field) in [
            (
                process.charge_energy_added_e2,
                -99_999_999,
                99_999_999,
                "charging_process.charge_energy_added_e2",
            ),
            (
                process.charge_energy_used_e2,
                -99_999_999,
                99_999_999,
                "charging_process.charge_energy_used_e2",
            ),
            (
                process.start_ideal_range_km_e2,
                -999_999,
                999_999,
                "charging_process.start_ideal_range_km_e2",
            ),
            (
                process.end_ideal_range_km_e2,
                -999_999,
                999_999,
                "charging_process.end_ideal_range_km_e2",
            ),
            (
                process.start_rated_range_km_e2,
                -999_999,
                999_999,
                "charging_process.start_rated_range_km_e2",
            ),
            (
                process.end_rated_range_km_e2,
                -999_999,
                999_999,
                "charging_process.end_rated_range_km_e2",
            ),
            (
                process.outside_temp_avg_e1,
                -9_999,
                9_999,
                "charging_process.outside_temp_avg_e1",
            ),
            (
                process.cost_e2,
                -999_999,
                999_999,
                "charging_process.cost_e2",
            ),
        ] {
            validate_optional_fixed_numeric_v2_2(value, minimum, maximum, field)?;
        }
        // Source FKs remain physical values in this V3 local snapshot. In
        // particular, `position_id` can name a valid cross-car target omitted
        // by the selected-car subset, so no SQLite closure is invented.
        if let Some(address_id) = process.address_id {
            referenced_address_ids.insert(i64::from(address_id));
        }
        if let Some(geofence_id) = process.geofence_id {
            referenced_geofence_ids.insert(i64::from(geofence_id));
        }
    }

    // Source optional address/geofence references stay soft in this local
    // subset. Their source targets can be extant but omitted here, and source
    // constraint state is not re-attested by V3 SQLite. Any loaded physical
    // address/geofence row must still be selected-car referenced.
    if let Some(unreferenced) = address_ids
        .iter()
        .find(|id| !referenced_address_ids.contains(id))
    {
        return Err(invalid(format!(
            "address {unreferenced} is not referenced by the selected car"
        )));
    }
    if let Some(unreferenced) = geofence_ids
        .iter()
        .find(|id| !referenced_geofence_ids.contains(id))
    {
        return Err(invalid(format!(
            "geofence {unreferenced} is not referenced by the selected car"
        )));
    }

    let mut position_ids = HashSet::with_capacity(snapshot.positions.len());
    for position in &snapshot.positions {
        require_unique_signed_i32(&mut position_ids, position.id, "position.id")?;
        if i64::from(position.car_id) != request.binding.selected_car_id {
            return Err(invalid("position.car_id does not match selected_car_id"));
        }
        validate_postgres_timestamp_us(position.date_pg_us, "position.date_pg_us")?;
        // The source `drive_id` FK is intentionally not reproduced as a pack
        // FK: a selected-car physical slice can retain an extant cross-car
        // drive ID while omitting that target. Car scope remains a Rust
        // admission boundary.
        validate_fixed_numeric_v2_2(
            position.latitude_e6,
            -99_999_999,
            99_999_999,
            "position.latitude_e6",
        )?;
        validate_fixed_numeric_v2_2(
            position.longitude_e6,
            -999_999_999,
            999_999_999,
            "position.longitude_e6",
        )?;
        for (value, minimum, maximum, field) in [
            (
                position.ideal_battery_range_km_e2,
                -999_999,
                999_999,
                "position.ideal_battery_range_km_e2",
            ),
            (
                position.est_battery_range_km_e2,
                -999_999,
                999_999,
                "position.est_battery_range_km_e2",
            ),
            (
                position.rated_battery_range_km_e2,
                -999_999,
                999_999,
                "position.rated_battery_range_km_e2",
            ),
            (
                position.outside_temp_e1,
                -9_999,
                9_999,
                "position.outside_temp_e1",
            ),
            (
                position.inside_temp_e1,
                -9_999,
                9_999,
                "position.inside_temp_e1",
            ),
            (
                position.driver_temp_setting_e1,
                -9_999,
                9_999,
                "position.driver_temp_setting_e1",
            ),
            (
                position.passenger_temp_setting_e1,
                -9_999,
                9_999,
                "position.passenger_temp_setting_e1",
            ),
            (
                position.tpms_pressure_fl_e1,
                -9_999,
                9_999,
                "position.tpms_pressure_fl_e1",
            ),
            (
                position.tpms_pressure_fr_e1,
                -9_999,
                9_999,
                "position.tpms_pressure_fr_e1",
            ),
            (
                position.tpms_pressure_rl_e1,
                -9_999,
                9_999,
                "position.tpms_pressure_rl_e1",
            ),
            (
                position.tpms_pressure_rr_e1,
                -9_999,
                9_999,
                "position.tpms_pressure_rr_e1",
            ),
        ] {
            validate_optional_fixed_numeric_v2_2(value, minimum, maximum, field)?;
        }
    }

    let mut charge_ids = HashSet::with_capacity(snapshot.charges.len());
    for charge in &snapshot.charges {
        require_unique_signed_i32(&mut charge_ids, charge.id, "charge.id")?;
        if !charging_process_ids.contains(&i64::from(charge.charging_process_id)) {
            return Err(invalid(
                "charge.charging_process_id is not present in this local physical slice",
            ));
        }
        validate_postgres_timestamp_us(charge.date_pg_us, "charge.date_pg_us")?;
        validate_fixed_numeric_v2_2(
            charge.charge_energy_added_e2,
            -99_999_999,
            99_999_999,
            "charge.charge_energy_added_e2",
        )?;
        validate_fixed_numeric_v2_2(
            charge.ideal_battery_range_km_e2,
            -999_999,
            999_999,
            "charge.ideal_battery_range_km_e2",
        )?;
        validate_optional_fixed_numeric_v2_2(
            charge.rated_battery_range_km_e2,
            -999_999,
            999_999,
            "charge.rated_battery_range_km_e2",
        )?;
        validate_optional_fixed_numeric_v2_2(
            charge.outside_temp_e1,
            -9_999,
            9_999,
            "charge.outside_temp_e1",
        )?;
        for (value, field) in [
            (
                charge.conn_charge_cable.as_deref(),
                "charge.conn_charge_cable",
            ),
            (
                charge.fast_charger_brand.as_deref(),
                "charge.fast_charger_brand",
            ),
            (
                charge.fast_charger_type.as_deref(),
                "charge.fast_charger_type",
            ),
        ] {
            validate_optional_text_with_source_width(value, 255, field)?;
        }
    }
    validate_states_v2_2(&snapshot.states, request.binding.selected_car_id)?;
    validate_updates_v2_2(&snapshot.updates, request.binding.selected_car_id)?;
    Ok(row_count)
}

/// Schema 2.0 predates standalone position history and stores `power` as an
/// INTEGER. Keep this narrowing explicit so a pack is never labelled 2.0 and
/// then rejected by the released 2.0 client after transport succeeds.
fn validate_v1_snapshot(request: &ProjectionPackRequest<'_>) -> Result<(), ProjectionPackError> {
    for position in &request.snapshot.positions {
        if position.drive_id.is_none() {
            return Err(invalid("schema 2.0 position.drive_id must be present"));
        }
        let _ = v1_position_power(position.power)?;
    }
    Ok(())
}

fn validate_delta(
    request: &ProjectionDeltaPackRequest<'_>,
    limits: ProtocolLimits,
) -> Result<u64, ProjectionPackError> {
    if request.pack_id.is_nil() || request.snapshot_id.is_nil() {
        return Err(invalid("delta pack and snapshot IDs must not be nil"));
    }
    let delta = request.delta;
    validate_binding(&delta.binding)?;
    if delta.sequence.to_inclusive <= delta.sequence.from_exclusive {
        return Err(invalid("delta sequence must make forward progress"));
    }
    if delta.parent_digest.is_zero() {
        return Err(invalid("delta parent digest must not be zero"));
    }
    let selected_car_id = delta.binding.selected_car_id;
    let mut car_ids = HashSet::with_capacity(delta.cars.len());
    for car in &delta.cars {
        require_unique_positive(&mut car_ids, car.id, "car.id")?;
        require_same_car(car.id, selected_car_id, "car.id")?;
        validate_required_text(&car.name, "car.name")?;
        validate_required_text(&car.model, "car.model")?;
        validate_optional_text(car.vin.as_deref(), "car.vin")?;
        validate_optional_text(car.firmware_version.as_deref(), "car.firmware_version")?;
        validate_optional_nonnegative(car.efficiency_wh_per_km, "car.efficiency_wh_per_km")?;
        validate_car_settings(&car.settings)?;
    }
    let mut setting_ids = HashSet::with_capacity(delta.car_settings.len());
    for patch in &delta.car_settings {
        require_unique_positive(&mut setting_ids, patch.car_id, "car_settings.car_id")?;
        require_same_car(patch.car_id, selected_car_id, "car_settings.car_id")?;
        if car_ids.contains(&patch.car_id) {
            return Err(invalid("car upsert and car settings patch overlap"));
        }
        validate_car_settings(&patch.settings)?;
    }

    let mut drive_ids = HashSet::with_capacity(delta.drives.len());
    for drive in &delta.drives {
        require_unique_positive(&mut drive_ids, drive.id, "drive.id")?;
        require_same_car(drive.car_id, selected_car_id, "drive.car_id")?;
        validate_interval(drive.start_date_ms, drive.end_date_ms, "drive")?;
        validate_optional_nonnegative(drive.distance_km, "drive.distance_km")?;
        validate_optional_finite(drive.efficiency, "drive.efficiency")?;
        validate_optional_finite(drive.power_max, "drive.power_max")?;
        validate_optional_finite(drive.power_min, "drive.power_min")?;
        validate_coordinate_pair(drive.start_latitude, drive.start_longitude, "drive.start")?;
        validate_coordinate_pair(drive.end_latitude, drive.end_longitude, "drive.end")?;
        validate_optional_soc(drive.start_soc, "drive.start_soc")?;
        validate_optional_soc(drive.end_soc, "drive.end_soc")?;
        for (value, name) in [
            (drive.start_address.as_deref(), "drive.start_address"),
            (drive.end_address.as_deref(), "drive.end_address"),
            (drive.start_geofence.as_deref(), "drive.start_geofence"),
            (drive.end_geofence.as_deref(), "drive.end_geofence"),
        ] {
            validate_optional_text(value, name)?;
        }
    }

    let mut charge_ids = HashSet::with_capacity(delta.charges.len());
    for charge in &delta.charges {
        require_unique_positive(&mut charge_ids, charge.id, "charge.id")?;
        require_same_car(charge.car_id, selected_car_id, "charge.car_id")?;
        require_positive(charge.start_date_ms, "charge.start_date_ms")?;
        if charge
            .end_date_ms
            .is_some_and(|end| end < charge.start_date_ms)
        {
            return Err(invalid("charge.end_date_ms precedes start_date_ms"));
        }
        validate_optional_nonnegative(charge.charge_energy_added, "charge.charge_energy_added")?;
        validate_optional_finite(charge.cost, "charge.cost")?;
        validate_coordinate_pair(
            charge.start_latitude,
            charge.start_longitude,
            "charge.start",
        )?;
        validate_optional_soc(charge.start_battery_level, "charge.start_battery_level")?;
        validate_optional_soc(charge.end_battery_level, "charge.end_battery_level")?;
        for (value, name) in [
            (charge.address.as_deref(), "charge.address"),
            (charge.location_name.as_deref(), "charge.location_name"),
            (charge.geofence.as_deref(), "charge.geofence"),
        ] {
            validate_optional_text(value, name)?;
        }
    }

    let mut position_ids = HashSet::with_capacity(delta.positions.len());
    for position in &delta.positions {
        require_unique_positive(&mut position_ids, position.id, "position.id")?;
        require_same_car(position.car_id, selected_car_id, "position.car_id")?;
        require_positive(position.date_ms, "position.date_ms")?;
        if let Some(drive_id) = position.drive_id {
            require_positive(drive_id, "position.drive_id")?;
            // A missing parent is valid: it belongs to the declared external base.
            let _ = drive_ids.contains(&drive_id);
        }
        validate_coordinate(position.latitude, position.longitude, "position")?;
        validate_optional_soc(position.battery_level, "position.battery_level")?;
        validate_optional_soc(
            position.usable_battery_level,
            "position.usable_battery_level",
        )?;
        validate_optional_finite(position.power, "position.power")?;
        validate_optional_nonnegative(position.odometer, "position.odometer")?;
        validate_optional_nonnegative(
            position.ideal_battery_range_km,
            "position.ideal_battery_range_km",
        )?;
    }

    let mut sample_ids = HashSet::with_capacity(delta.charge_samples.len());
    for sample in &delta.charge_samples {
        require_unique_positive(&mut sample_ids, sample.id, "charge_sample.id")?;
        require_positive(sample.timestamp_ms, "charge_sample.timestamp_ms")?;
        require_positive(sample.charge_process_id, "charge_sample.charge_process_id")?;
        let _ = charge_ids.contains(&sample.charge_process_id);
        validate_optional_soc(sample.battery_level, "charge_sample.battery_level")?;
        validate_optional_nonnegative(
            sample.charge_energy_added_kwh,
            "charge_sample.charge_energy_added_kwh",
        )?;
    }
    validate_states(&delta.states, selected_car_id)?;
    validate_updates(&delta.updates, selected_car_id)?;

    let upsert_ids = delta_upsert_ids(delta);
    let mut tombstone_ids = HashSet::with_capacity(delta.tombstones.len());
    for tombstone in &delta.tombstones {
        require_positive(tombstone.id, "tombstone.id")?;
        require_same_car(tombstone.car_id, selected_car_id, "tombstone.car_id")?;
        if tombstone.entity.source_owned_tombstone_order().is_none() {
            return Err(invalid(format!(
                "unsupported source-owned delta tombstone entity {}",
                tombstone.entity.as_str()
            )));
        }
        if !tombstone_ids.insert((tombstone.entity, tombstone.id)) {
            return Err(invalid("duplicate typed tombstone"));
        }
        if upsert_ids.contains(&(tombstone.entity, tombstone.id)) {
            return Err(invalid("typed delta upsert and tombstone overlap"));
        }
    }
    let row_count = delta.row_count()?;
    if row_count == 0 || row_count > limits.max_rows_per_pack {
        return Err(ProjectionPackError::TooManyRows);
    }
    Ok(row_count)
}

fn delta_upsert_ids(delta: &ProjectionDelta) -> HashSet<(ProjectionDeltaEntity, i64)> {
    let mut ids = HashSet::new();
    ids.extend(
        delta
            .cars
            .iter()
            .map(|row| (ProjectionDeltaEntity::Car, row.id)),
    );
    ids.extend(
        delta
            .car_settings
            .iter()
            .map(|row| (ProjectionDeltaEntity::CarSetting, row.car_id)),
    );
    ids.extend(
        delta
            .drives
            .iter()
            .map(|row| (ProjectionDeltaEntity::Drive, row.id)),
    );
    ids.extend(
        delta
            .positions
            .iter()
            .map(|row| (ProjectionDeltaEntity::Position, row.id)),
    );
    ids.extend(
        delta
            .charges
            .iter()
            .map(|row| (ProjectionDeltaEntity::Charge, row.id)),
    );
    ids.extend(
        delta
            .charge_samples
            .iter()
            .map(|row| (ProjectionDeltaEntity::ChargeSample, row.id)),
    );
    ids.extend(
        delta
            .states
            .iter()
            .map(|row| (ProjectionDeltaEntity::State, row.id)),
    );
    ids.extend(
        delta
            .updates
            .iter()
            .map(|row| (ProjectionDeltaEntity::Update, row.id)),
    );
    ids
}

fn source_owned_tombstones_in_canonical_order(
    values: &[ProjectionTombstone],
) -> Vec<&ProjectionTombstone> {
    let mut rows = values.iter().collect::<Vec<_>>();
    rows.sort_unstable_by_key(|row| {
        (
            row.entity.source_owned_tombstone_order().unwrap_or(u8::MAX),
            row.id,
        )
    });
    rows
}

fn tables_for_delta(delta: &ProjectionDelta) -> Vec<MirrorTable> {
    let mut tables = Vec::new();
    if !delta.cars.is_empty() || !delta.car_settings.is_empty() {
        tables.push(MirrorTable::Car);
    }
    if !delta.drives.is_empty() {
        tables.push(MirrorTable::Drive);
    }
    if !delta.charges.is_empty() {
        tables.push(MirrorTable::Charge);
    }
    if !delta.positions.is_empty() {
        tables.push(MirrorTable::Position);
    }
    if !delta.charge_samples.is_empty() {
        tables.push(MirrorTable::ChargeSample);
    }
    if !delta.states.is_empty() {
        tables.push(MirrorTable::State);
    }
    if !delta.updates.is_empty() {
        tables.push(MirrorTable::Update);
    }
    if !delta.tombstones.is_empty() {
        tables.push(MirrorTable::Tombstone);
    }
    tables
}

fn row_count_with_states_and_updates(
    snapshot: &ProjectionSnapshot,
    states: &[ProjectionState],
    updates: &[ProjectionUpdate],
) -> Result<u64, ProjectionPackError> {
    let with_states = snapshot
        .row_count()?
        .checked_add(u64::try_from(states.len()).map_err(|_| ProjectionPackError::TooManyRows)?)
        .ok_or(ProjectionPackError::TooManyRows)?;
    with_states
        .checked_add(u64::try_from(updates.len()).map_err(|_| ProjectionPackError::TooManyRows)?)
        .ok_or(ProjectionPackError::TooManyRows)
}

fn validate_states(
    states: &[ProjectionState],
    selected_car_id: i64,
) -> Result<(), ProjectionPackError> {
    let mut ids = HashSet::with_capacity(states.len());
    let mut open_cars = HashSet::new();
    for state in states {
        require_unique_positive(&mut ids, state.id, "state.id")?;
        require_same_car(state.car_id, selected_car_id, "state.car_id")?;
        if !matches!(state.state.as_str(), "online" | "offline" | "asleep") {
            return Err(invalid("state.state is not a TeslaMate state"));
        }
        require_positive(state.start_date_ms, "state.start_date_ms")?;
        if let Some(end) = state.end_date_ms {
            if end < state.start_date_ms {
                return Err(invalid("state.end_date_ms precedes state.start_date_ms"));
            }
        } else if !open_cars.insert(state.car_id) {
            return Err(invalid("more than one open state exists for a car"));
        }
    }
    Ok(())
}

fn validate_updates(
    updates: &[ProjectionUpdate],
    selected_car_id: i64,
) -> Result<(), ProjectionPackError> {
    let mut ids = HashSet::with_capacity(updates.len());
    for update in updates {
        require_unique_positive(&mut ids, update.id, "update.id")?;
        require_same_car(update.car_id, selected_car_id, "update.car_id")?;
        require_positive(update.start_date_ms, "update.start_date_ms")?;
        if update.end_date_ms < update.start_date_ms {
            return Err(invalid("update.end_date_ms precedes update.start_date_ms"));
        }
        validate_required_text(&update.version, "update.version")?;
    }
    Ok(())
}

/// Validate the raw physical state slice without importing compatibility
/// policies such as positive identifiers, interval ordering, or a single open
/// state. PostgreSQL's signed int4/timestamp domains are represented exactly.
fn validate_states_v2_2(
    states: &[ProjectionStateV2_2],
    selected_car_id: i64,
) -> Result<(), ProjectionPackError> {
    let mut ids = HashSet::with_capacity(states.len());
    for state in states {
        if !ids.insert(state.id) {
            return Err(invalid("state.id is duplicated"));
        }
        if i64::from(state.car_id) != selected_car_id {
            return Err(invalid("state.car_id does not match selected car"));
        }
        validate_postgres_timestamp_us(state.start_date_pg_us, "state.start_date_pg_us")?;
        if let Some(end_date_pg_us) = state.end_date_pg_us {
            validate_postgres_timestamp_us(end_date_pg_us, "state.end_date_pg_us")?;
        }
    }
    Ok(())
}

/// Validate the raw physical update slice without applying the legacy
/// completed-update, interval, trimming, or defaulting rules.
fn validate_updates_v2_2(
    updates: &[ProjectionUpdateV2_2],
    selected_car_id: i64,
) -> Result<(), ProjectionPackError> {
    let mut ids = HashSet::with_capacity(updates.len());
    for update in updates {
        if !ids.insert(update.id) {
            return Err(invalid("update.id is duplicated"));
        }
        if i64::from(update.car_id) != selected_car_id {
            return Err(invalid("update.car_id does not match selected car"));
        }
        validate_postgres_timestamp_us(update.start_date_pg_us, "update.start_date_pg_us")?;
        if let Some(end_date_pg_us) = update.end_date_pg_us {
            validate_postgres_timestamp_us(end_date_pg_us, "update.end_date_pg_us")?;
        }
        validate_optional_text_with_source_width(update.version.as_deref(), 255, "update.version")?;
    }
    Ok(())
}

fn validate_postgres_timestamp_us(value: i64, field: &str) -> Result<(), ProjectionPackError> {
    if value == i64::MIN
        || value == i64::MAX
        || (POSTGRES_TIMESTAMP_FINITE_MIN_US..POSTGRES_TIMESTAMP_FINITE_END_EXCLUSIVE_US)
            .contains(&value)
    {
        return Ok(());
    }
    Err(invalid(format!(
        "{field} is outside the PostgreSQL timestamp source domain"
    )))
}

fn validate_binding(binding: &ProjectionBinding) -> Result<(), ProjectionPackError> {
    if binding.installation_id.is_nil()
        || binding.account_id.is_nil()
        || binding.vehicle_id.is_nil()
        || binding.generation == 0
    {
        return Err(invalid("projection binding is incomplete"));
    }
    require_positive(binding.selected_car_id, "selected_car_id")
}

/// Schema 2.2 carries the physical TeslaMate `cars.id` domain: source
/// `smallint` permits signed and zero values even though legacy projection
/// bindings deliberately require a positive normalized mirror identity.
fn validate_binding_v2_2(binding: &ProjectionBinding) -> Result<(), ProjectionPackError> {
    if binding.installation_id.is_nil()
        || binding.account_id.is_nil()
        || binding.vehicle_id.is_nil()
        || binding.generation == 0
    {
        return Err(invalid("projection binding is incomplete"));
    }
    if i16::try_from(binding.selected_car_id).is_err() {
        return Err(invalid(
            "schema 2.2 selected_car_id is outside the TeslaMate smallint source domain",
        ));
    }
    Ok(())
}

fn tables_for_snapshot(snapshot: &ProjectionSnapshot, includes_states: bool) -> Vec<MirrorTable> {
    let mut tables = vec![MirrorTable::Car];
    if !snapshot.drives.is_empty() {
        tables.push(MirrorTable::Drive);
    }
    if !snapshot.charges.is_empty() {
        tables.push(MirrorTable::Charge);
    }
    if !snapshot.positions.is_empty() {
        tables.push(MirrorTable::Position);
    }
    if !snapshot.charge_samples.is_empty() {
        tables.push(MirrorTable::ChargeSample);
    }
    // Schema 2.1 writes the state/update pair together. Advertise both
    // tables whenever that extension is present so consumers can discover
    // every table emitted by the pack without inferring it from SQLite.
    if includes_states {
        tables.push(MirrorTable::State);
        tables.push(MirrorTable::Update);
    }
    tables
}

fn tables_for_snapshot_v2_2(snapshot: &ProjectionSnapshotV2_2) -> Vec<MirrorTable> {
    // The protocol's current table vocabulary intentionally has no
    // address/geofence variants.  Schema 2.2 is locally validated and cannot
    // reach the catalogue, so retain only the established logical streams in
    // `TransportPack` metadata.  The SQLite verifier below checks the full
    // exact local physical layout, including optional address/geofence rows
    // and raw drive references that intentionally remain soft.
    let mut tables = vec![MirrorTable::Car];
    if !snapshot.drives.is_empty() {
        tables.push(MirrorTable::Drive);
    }
    // Protocol `Charge` is the parent/session vocabulary. The source-shaped
    // physical table is `charging_processes`, so advertise it as the parent.
    if !snapshot.charging_processes.is_empty() {
        tables.push(MirrorTable::Charge);
    }
    if !snapshot.positions.is_empty() {
        tables.push(MirrorTable::Position);
    }
    // Protocol `ChargeSample` is the child vocabulary. Exact physical source
    // `charges` rows retain that child stream without compatibility reshape.
    if !snapshot.charges.is_empty() {
        tables.push(MirrorTable::ChargeSample);
    }
    if !snapshot.states.is_empty() {
        tables.push(MirrorTable::State);
    }
    if !snapshot.updates.is_empty() {
        tables.push(MirrorTable::Update);
    }
    tables
}
