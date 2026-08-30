// SPDX-License-Identifier: AGPL-3.0-only

struct ClosedDrive {
    drive: ProjectionDrive,
    positions: Vec<ProjectionPosition>,
}

struct DriveClose {
    drive_id: i64,
    completed: Option<ClosedDrive>,
}

struct ClosedCharge {
    charge: ProjectionCharge,
    samples: Vec<ProjectionChargeSample>,
}

fn maybe_close_drive(
    state: &mut OpenSessionState,
    car_id: i64,
    sample: &ParsedSample,
    offline_drive_timeout: Duration,
    force: bool,
) -> Result<Option<DriveClose>, LifecycleError> {
    let Some(open) = state.open_drive.as_ref() else {
        return Ok(None);
    };
    let offline_drive_timeout_ms =
        i64::try_from(offline_drive_timeout.as_millis()).unwrap_or(i64::MAX);
    let last_position_at = open
        .last_position_date_ms
        .or_else(|| open.positions.last().map(|position| position.date_ms))
        .unwrap_or(open.start_date_ms);
    let offline_timed_out = sample.phase == VehiclePhase::Offline
        && sample.drive_timestamp_ms.saturating_sub(last_position_at) >= offline_drive_timeout_ms;
    let should_close = force
        || matches!(sample.phase, VehiclePhase::Asleep | VehiclePhase::Updating)
        || offline_timed_out
        || (matches!(sample.phase, VehiclePhase::Online | VehiclePhase::Charging)
            && sample.drive_data_present
            && !sample.stream_frame
            && !is_drive_shift(sample.shift_state.as_deref()));
    if !should_close {
        return Ok(None);
    }
    let pending_gained_range_charge =
        (offline_timed_out && open.saw_offline).then(|| gained_range_charge_seed_from_drive(open));
    let mut open = state
        .open_drive
        .take()
        .expect("open drive was checked before close");
    let drive_id = open.id;
    let append_endpoint = sample.drive_data_present
        && matches!(sample.phase, VehiclePhase::Online | VehiclePhase::Charging)
        && !is_drive_shift(sample.shift_state.as_deref())
        && sample.odometer.is_some();
    if append_endpoint {
        let position_id = state.next_position_id;
        if let Some(position) = position_from_sample(position_id, Some(open.id), car_id, sample)? {
            state.next_position_id = state
                .next_position_id
                .checked_add(1)
                .ok_or(LifecycleError::IdentifierExhausted)?;
            observe_drive_position(&mut open, &position);
            open.positions.push(position);
        }
    }
    if let Some(seed) = pending_gained_range_charge {
        state.pending_gained_range_charge.get_or_insert(seed);
    }
    Ok(Some(DriveClose {
        drive_id,
        completed: finalize_drive(open)?,
    }))
}

fn position_from_sample(
    position_id: i64,
    drive_id: Option<i64>,
    car_id: i64,
    sample: &ParsedSample,
) -> Result<Option<ProjectionPosition>, LifecycleError> {
    let Some((latitude, longitude)) = valid_coordinates(sample.latitude, sample.longitude) else {
        if sample.latitude.is_some() || sample.longitude.is_some() {
            return Err(LifecycleError::InvalidCoordinates);
        }
        return Ok(None);
    };
    Ok(Some(ProjectionPosition {
        id: position_id,
        drive_id,
        car_id,
        date_ms: sample.drive_timestamp_ms,
        latitude,
        longitude,
        speed: sample.speed,
        power: sample.power,
        battery_level: sample.battery_level,
        usable_battery_level: sample.usable_battery_level,
        elevation: sample.elevation,
        odometer: sample.odometer,
        ideal_battery_range_km: sample.ideal_range_km,
        est_battery_range_km: sample.est_range_km,
        rated_battery_range_km: sample.rated_range_km,
        fan_status: sample.fan_status,
        driver_temp_setting: sample.driver_temp_setting,
        passenger_temp_setting: sample.passenger_temp_setting,
        is_climate_on: sample.is_climate_on,
        is_rear_defroster_on: sample.is_rear_defroster_on,
        is_front_defroster_on: sample.is_front_defroster_on,
        inside_temp: sample.inside_temp,
        outside_temp: sample.outside_temp,
        battery_heater: sample.battery_heater,
        battery_heater_on: sample.battery_heater_on,
        battery_heater_no_power: sample.battery_heater_no_power,
        tpms_pressure_fl: sample.tpms_pressure_fl,
        tpms_pressure_fr: sample.tpms_pressure_fr,
        tpms_pressure_rl: sample.tpms_pressure_rl,
        tpms_pressure_rr: sample.tpms_pressure_rr,
    }))
}

fn maybe_close_charge(
    state: &mut OpenSessionState,
    _car_id: i64,
    sample: &ParsedSample,
) -> Result<Option<ClosedCharge>, LifecycleError> {
    let Some(_open) = state.open_charge.as_ref() else {
        return Ok(None);
    };
    let terminal = sample.charging_state.as_deref().is_some_and(|state| {
        matches!(
            state.to_ascii_lowercase().as_str(),
            "complete" | "disconnected" | "stopped" | "nopower" | "unplugged"
        )
    });
    let charging = is_charging_state(sample.charging_state.as_deref());
    // Sparse stream frames carry charge values but omit `charging_state`.
    // Only an explicit non-charging state may close an online session.
    let should_close = terminal
        || matches!(sample.phase, VehiclePhase::Asleep | VehiclePhase::Updating)
        || (sample.phase == VehiclePhase::Online
            && sample.charging_state.is_some()
            && sample.charge_data_present
            && !charging
            && state
                .open_charge
                .as_ref()
                .is_some_and(|open| !open.samples.is_empty()));
    if !should_close {
        return Ok(None);
    }
    let mut open = state
        .open_charge
        .take()
        .expect("open charge was checked before close");
    // Terminal sample still carries the final energy/SoC even though it is no
    // longer "Charging"; fold those fields in before sealing the session.
    observe_charge_aggregate(&mut open, sample);
    if terminal && sample.charge_data_present {
        let sample_id = state.next_charge_sample_id;
        state.next_charge_sample_id = state
            .next_charge_sample_id
            .checked_add(1)
            .ok_or(LifecycleError::IdentifierExhausted)?;
        open.samples.push(ProjectionChargeSample {
            id: sample_id,
            charge_process_id: open.id,
            timestamp_ms: sample.charge_timestamp_ms,
            battery_level: sample.battery_level,
            usable_battery_level: sample.usable_battery_level,
            charge_energy_added_kwh: sample.charge_energy_added,
            charger_power_kw: sample.charger_power_kw,
            charger_voltage: sample.charger_voltage,
            charger_actual_current: sample.charger_actual_current,
            charger_pilot_current: sample.charger_pilot_current,
            charger_phases: sample.charger_phases,
            ideal_range_km: sample.ideal_range_km,
            rated_range_km: sample.rated_range_km,
            outside_temp_c: sample.outside_temp,
            battery_heater_on: sample.battery_heater_on,
            battery_heater: sample.battery_heater,
            battery_heater_no_power: sample.battery_heater_no_power,
            not_enough_power_to_heat: sample.not_enough_power_to_heat,
            fast_charger_present: sample.fast_charger_present,
            fast_charger_brand: sample.fast_charger_brand.clone(),
            fast_charger_type: sample.fast_charger_type.clone(),
            charge_cable: sample.charge_cable.clone(),
        });
    }
    finalize_charge(open, Some(sample.charge_timestamp_ms)).map(Some)
}

fn maybe_open_or_extend_drive(
    state: &mut OpenSessionState,
    car_id: i64,
    sample: &ParsedSample,
    delta: &mut LifecycleDelta,
) -> Result<(), LifecycleError> {
    let driving = is_drive_shift(sample.shift_state.as_deref());
    if !driving {
        return Ok(());
    }
    // Charging takes precedence when Tesla reports both inconsistently.
    if is_charging_state(sample.charging_state.as_deref()) {
        return Ok(());
    }

    if state.open_drive.is_none() {
        let id = state.next_drive_id;
        state.next_drive_id = state
            .next_drive_id
            .checked_add(1)
            .ok_or(LifecycleError::IdentifierExhausted)?;
        state.open_drive = Some(OpenDrive {
            id,
            car_id,
            start_date_ms: sample.drive_timestamp_ms,
            start_latitude: sample.latitude,
            start_longitude: sample.longitude,
            start_soc: sample.battery_level,
            start_rated_range_km: sample.rated_range_km,
            speed_max: sample.speed,
            outside_temp_sum: sample.outside_temp.unwrap_or(0.0),
            outside_temp_count: u32::from(sample.outside_temp.is_some()),
            position_count: 0,
            last_position_date_ms: None,
            last_latitude: None,
            last_longitude: None,
            last_soc: None,
            last_rated_range_km: None,
            last_odometer: None,
            first_odometer: None,
            power_max: None,
            power_min: None,
            inside_temp_sum: 0.0,
            inside_temp_count: 0,
            start_ideal_range_km: None,
            end_ideal_range_km: None,
            elevation_ascent: 0,
            elevation_descent: 0,
            last_elevation: None,
            saw_offline: false,
            last_charge_energy_added: sample.charge_energy_added,
            last_ideal_range_km: sample.ideal_range_km,
            positions: Vec::new(),
        });
    }

    if let Some(open) = state.open_drive.as_mut() {
        open.speed_max = match (open.speed_max, sample.speed) {
            (Some(current), Some(next)) => Some(current.max(next)),
            (None, Some(next)) => Some(next),
            (current, None) => current,
        };
        if let Some(temp) = sample.outside_temp {
            open.outside_temp_sum += temp;
            open.outside_temp_count = open.outside_temp_count.saturating_add(1);
        }
        if sample.latitude.is_some() || sample.longitude.is_some() {
            let position_id = state.next_position_id;
            let position = position_from_sample(position_id, Some(open.id), car_id, sample)?
                .expect("coordinates were present");
            state.next_position_id = state
                .next_position_id
                .checked_add(1)
                .ok_or(LifecycleError::IdentifierExhausted)?;
            observe_drive_position(open, &position);
            // Pure in-process callers may accumulate children for the session.
            // The durable db path clears vectors before encode and does not
            // rehydrate the full history on every observation.
            open.positions.push(position.clone());
            delta.open_drive_positions.push(position);
        }
    }
    Ok(())
}

fn observe_drive_position(open: &mut OpenDrive, position: &ProjectionPosition) {
    open.position_count = open.position_count.saturating_add(1);
    open.last_position_date_ms = Some(position.date_ms);
    open.last_latitude = Some(position.latitude);
    open.last_longitude = Some(position.longitude);
    open.last_soc = position.battery_level.or(open.last_soc);
    open.last_rated_range_km = position.rated_battery_range_km.or(open.last_rated_range_km);
    if open.first_odometer.is_none() {
        open.first_odometer = position.odometer;
    }
    open.last_odometer = position.odometer.or(open.last_odometer);
    open.power_max = match (open.power_max, position.power) {
        (Some(current), Some(next)) => Some(current.max(next)),
        (None, Some(next)) => Some(next),
        (current, None) => current,
    };
    open.power_min = match (open.power_min, position.power) {
        (Some(current), Some(next)) => Some(current.min(next)),
        (None, Some(next)) => Some(next),
        (current, None) => current,
    };
    if let Some(temp) = position.inside_temp {
        open.inside_temp_sum += temp;
        open.inside_temp_count = open.inside_temp_count.saturating_add(1);
    }
    if position.ideal_battery_range_km.is_some() && position.odometer.is_some() {
        if open.start_ideal_range_km.is_none() {
            open.start_ideal_range_km = position.ideal_battery_range_km;
        }
        open.end_ideal_range_km = position.ideal_battery_range_km;
    }
    if let Some(elevation) = position.elevation {
        if let Some(previous) = open.last_elevation {
            let delta = elevation - previous;
            if delta > 0 {
                open.elevation_ascent = open.elevation_ascent.saturating_add(delta);
            } else if delta < 0 {
                open.elevation_descent = open
                    .elevation_descent
                    .saturating_add(delta.unsigned_abs() as i64);
            }
        }
        open.last_elevation.replace(elevation);
    }
}

fn maybe_open_or_extend_charge(
    state: &mut OpenSessionState,
    car_id: i64,
    sample: &ParsedSample,
    delta: &mut LifecycleDelta,
) -> Result<(), LifecycleError> {
    let charging = is_charging_state(sample.charging_state.as_deref());
    if !charging {
        return Ok(());
    }

    if state.open_charge.is_none() {
        let id = state.next_charge_id;
        state.next_charge_id = state
            .next_charge_id
            .checked_add(1)
            .ok_or(LifecycleError::IdentifierExhausted)?;
        state.open_charge = Some(OpenCharge {
            id,
            car_id,
            start_date_ms: sample.charge_timestamp_ms,
            start_battery_level: sample.battery_level,
            start_ideal_range_km: sample.ideal_range_km,
            start_rated_range_km: sample.rated_range_km,
            start_latitude: sample.latitude,
            start_longitude: sample.longitude,
            is_dc: sample.fast_charger_present,
            fast_charger_type: sample.fast_charger_type.clone(),
            max_charger_power_kw: sample.charger_power_kw,
            outside_temp_sum: 0.0,
            outside_temp_count: 0,
            first_energy_added: sample.charge_energy_added,
            max_energy_added: sample.charge_energy_added,
            last_energy_added: sample.charge_energy_added,
            last_battery_level: sample.battery_level,
            last_ideal_range_km: sample.ideal_range_km,
            last_rated_range_km: sample.rated_range_km,
            sample_count: 0,
            energy_used_kwh: None,
            last_sample_timestamp_ms: None,
            last_sample_power_kw: None,
            samples: Vec::new(),
        });
        if let (Some(latitude), Some(longitude)) = (sample.latitude, sample.longitude) {
            delta
                .charge_start_coordinates
                .push((id, latitude, longitude));
        }
    }

    if let Some(open) = state.open_charge.as_mut() {
        if let Some(power) = sample.charger_power_kw {
            open.max_charger_power_kw = Some(open.max_charger_power_kw.unwrap_or(power).max(power));
        }
        if let Some(temp) = sample.outside_temp {
            open.outside_temp_sum += temp;
            open.outside_temp_count = open.outside_temp_count.saturating_add(1);
        }
        observe_charge_aggregate(open, sample);
        if sample.fast_charger_present == Some(true) {
            open.is_dc = Some(true);
        }

        let sample_id = state.next_charge_sample_id;
        state.next_charge_sample_id = state
            .next_charge_sample_id
            .checked_add(1)
            .ok_or(LifecycleError::IdentifierExhausted)?;
        let charge_sample = ProjectionChargeSample {
            id: sample_id,
            charge_process_id: open.id,
            timestamp_ms: sample.charge_timestamp_ms,
            battery_level: sample.battery_level,
            usable_battery_level: sample.usable_battery_level,
            charge_energy_added_kwh: sample.charge_energy_added,
            charger_power_kw: sample.charger_power_kw,
            charger_voltage: sample.charger_voltage,
            charger_actual_current: sample.charger_actual_current,
            charger_pilot_current: sample.charger_pilot_current,
            charger_phases: sample.charger_phases,
            ideal_range_km: sample.ideal_range_km,
            rated_range_km: sample.rated_range_km,
            outside_temp_c: sample.outside_temp,
            battery_heater_on: sample.battery_heater_on,
            battery_heater: sample.battery_heater,
            battery_heater_no_power: sample.battery_heater_no_power,
            not_enough_power_to_heat: sample.not_enough_power_to_heat,
            fast_charger_present: sample.fast_charger_present,
            fast_charger_brand: sample.fast_charger_brand.clone(),
            fast_charger_type: sample.fast_charger_type.clone(),
            charge_cable: sample.charge_cable.clone(),
        };
        observe_charge_sample(open, &charge_sample);
        open.samples.push(charge_sample.clone());
        delta.open_charge_samples.push(charge_sample);
    }
    Ok(())
}

fn observe_charge_sample(open: &mut OpenCharge, sample: &ProjectionChargeSample) {
    let power_kw = sample
        .charger_power_kw
        .filter(|power| power.is_finite() && *power >= 0.0);
    if let (Some(previous_ts), Some(current_power)) = (open.last_sample_timestamp_ms, power_kw) {
        let elapsed_ms = sample.timestamp_ms.saturating_sub(previous_ts);
        if elapsed_ms > 0 {
            let increment = current_power * elapsed_ms as f64 / 3_600_000.0;
            open.energy_used_kwh = Some(open.energy_used_kwh.unwrap_or(0.0) + increment);
        }
    }
    open.sample_count = open.sample_count.saturating_add(1);
    open.last_sample_timestamp_ms = Some(sample.timestamp_ms);
    open.last_sample_power_kw = power_kw.or(open.last_sample_power_kw);
}

const STATIONARY_POSITION_INTERVAL_MS: i64 = 5 * 60 * 1_000;

fn maybe_emit_stationary_position(
    state: &mut OpenSessionState,
    car_id: i64,
    prior_phase: VehiclePhase,
    prior_drive_timestamp_ms: Option<i64>,
    sample: &ParsedSample,
    delta: &mut LifecycleDelta,
) -> Result<(), LifecycleError> {
    if !matches!(sample.phase, VehiclePhase::Online | VehiclePhase::Charging) {
        return Ok(());
    }
    if is_drive_shift(sample.shift_state.as_deref()) {
        return Ok(());
    }
    if prior_drive_timestamp_ms.is_some_and(|last| sample.drive_timestamp_ms <= last) {
        return Ok(());
    }

    let Some((latitude, longitude)) = valid_coordinates(sample.latitude, sample.longitude) else {
        return Ok(());
    };

    let phase_entry = prior_phase != sample.phase || prior_drive_timestamp_ms.is_none();
    let interval_elapsed = state.last_stationary_position_at_ms.is_none_or(|last| {
        sample.drive_timestamp_ms.saturating_sub(last) >= STATIONARY_POSITION_INTERVAL_MS
    });
    if !phase_entry && !interval_elapsed {
        return Ok(());
    }

    let id = state
        .next_position_id
        .checked_add(1)
        .ok_or(LifecycleError::IdentifierExhausted)?;
    let position_id = state.next_position_id;
    state.next_position_id = id;
    state.last_stationary_position_at_ms = Some(sample.drive_timestamp_ms);
    delta.positions.push(ProjectionPosition {
        id: position_id,
        drive_id: None,
        car_id,
        date_ms: sample.drive_timestamp_ms,
        latitude,
        longitude,
        speed: sample.speed,
        power: sample.power,
        battery_level: sample.battery_level,
        usable_battery_level: sample.usable_battery_level,
        elevation: sample.elevation,
        odometer: sample.odometer,
        ideal_battery_range_km: sample.ideal_range_km,
        est_battery_range_km: sample.est_range_km,
        rated_battery_range_km: sample.rated_range_km,
        fan_status: sample.fan_status,
        driver_temp_setting: sample.driver_temp_setting,
        passenger_temp_setting: sample.passenger_temp_setting,
        is_climate_on: sample.is_climate_on,
        is_rear_defroster_on: sample.is_rear_defroster_on,
        is_front_defroster_on: sample.is_front_defroster_on,
        inside_temp: sample.inside_temp,
        outside_temp: sample.outside_temp,
        battery_heater: sample.battery_heater,
        battery_heater_on: sample.battery_heater_on,
        battery_heater_no_power: sample.battery_heater_no_power,
        tpms_pressure_fl: sample.tpms_pressure_fl,
        tpms_pressure_fr: sample.tpms_pressure_fr,
        tpms_pressure_rl: sample.tpms_pressure_rl,
        tpms_pressure_rr: sample.tpms_pressure_rr,
    });
    Ok(())
}

fn valid_coordinates(latitude: Option<f64>, longitude: Option<f64>) -> Option<(f64, f64)> {
    match (latitude, longitude) {
        (Some(latitude), Some(longitude))
            if latitude.is_finite()
                && longitude.is_finite()
                && (-90.0..=90.0).contains(&latitude)
                && (-180.0..=180.0).contains(&longitude) =>
        {
            Some((latitude, longitude))
        }
        _ => None,
    }
}

fn finalize_drive(open: OpenDrive) -> Result<Option<ClosedDrive>, LifecycleError> {
    // Prefer running aggregates so close remains correct without a full child
    // rehydrate. Fall back to in-memory positions when present (unit tests /
    // single-batch close).
    let position_count = open
        .position_count
        .max(u32::try_from(open.positions.len()).unwrap_or(0));
    if position_count < 2 {
        return Ok(None);
    }
    let first_from_vec = open.positions.first();
    let last_from_vec = open.positions.last();
    // Prefer the open-session start. Incremental materialisation clears the
    // in-memory child buffer between collect-once shots, so `positions.first()`
    // may only hold the terminal park sample — using that as start collapses
    // the drive interval and leaves earlier durable positions outside
    // [start, end] (client V2 integrity rejects those deltas).
    let start_date_ms = open.start_date_ms;
    let end_date_ms = open
        .last_position_date_ms
        .or_else(|| last_from_vec.map(|position| position.date_ms))
        .unwrap_or(open.start_date_ms);
    let first_odometer = open
        .first_odometer
        .or_else(|| first_from_vec.and_then(|position| position.odometer));
    let last_odometer = open
        .last_odometer
        .or_else(|| last_from_vec.and_then(|position| position.odometer));
    let (Some(first_odometer), Some(last_odometer)) = (first_odometer, last_odometer) else {
        return Ok(None);
    };
    let distance_km = round_to_precision(last_odometer - first_odometer, 6);
    if distance_km < 0.01 {
        return Ok(None);
    }
    if end_date_ms < start_date_ms {
        return Err(LifecycleError::InvalidTimeline);
    }
    let duration_min = ((end_date_ms - start_date_ms) as f64 / 60_000.0).round() as i64;
    let outside_temp_avg = if open.outside_temp_count > 0 {
        Some(open.outside_temp_sum / f64::from(open.outside_temp_count))
    } else {
        None
    };
    let inside_temp_avg = if open.inside_temp_count > 0 {
        Some(open.inside_temp_sum / f64::from(open.inside_temp_count))
    } else if !open.positions.is_empty() {
        let inside_values = open
            .positions
            .iter()
            .filter_map(|position| position.inside_temp);
        let (inside_sum, inside_count) = inside_values.fold((0.0, 0_u32), |(sum, count), value| {
            (sum + value, count.saturating_add(1))
        });
        (inside_count > 0).then_some(inside_sum / f64::from(inside_count))
    } else {
        None
    };
    let power_max = open.power_max.or_else(|| {
        open.positions
            .iter()
            .filter_map(|position| position.power)
            .reduce(f64::max)
    });
    let power_min = open.power_min.or_else(|| {
        open.positions
            .iter()
            .filter_map(|position| position.power)
            .reduce(f64::min)
    });
    let start_ideal_range_km = open.start_ideal_range_km.or_else(|| {
        open.positions
            .iter()
            .find(|position| {
                position.ideal_battery_range_km.is_some() && position.odometer.is_some()
            })
            .and_then(|position| position.ideal_battery_range_km)
    });
    let end_ideal_range_km = open.end_ideal_range_km.or_else(|| {
        open.positions
            .iter()
            .rev()
            .find(|position| {
                position.ideal_battery_range_km.is_some() && position.odometer.is_some()
            })
            .and_then(|position| position.ideal_battery_range_km)
    });
    let (ascent, descent) = if open.position_count > 0 {
        (
            cap_elevation_total(open.elevation_ascent),
            cap_elevation_total(open.elevation_descent),
        )
    } else {
        elevation_totals(&open.positions)
    };
    let start_latitude = open
        .start_latitude
        .or_else(|| first_from_vec.map(|position| position.latitude));
    let start_longitude = open
        .start_longitude
        .or_else(|| first_from_vec.map(|position| position.longitude));
    let end_latitude = open
        .last_latitude
        .or_else(|| last_from_vec.map(|position| position.latitude));
    let end_longitude = open
        .last_longitude
        .or_else(|| last_from_vec.map(|position| position.longitude));
    let start_soc = open
        .start_soc
        .or_else(|| first_from_vec.and_then(|position| position.battery_level));
    let end_soc = open
        .last_soc
        .or_else(|| last_from_vec.and_then(|position| position.battery_level));
    let start_rated_range_km = open
        .start_rated_range_km
        .or_else(|| first_from_vec.and_then(|position| position.rated_battery_range_km));
    let end_rated_range_km = open
        .last_rated_range_km
        .or_else(|| last_from_vec.and_then(|position| position.rated_battery_range_km));
    let drive = ProjectionDrive {
        id: open.id,
        car_id: open.car_id,
        optimized_at_ms: None,
        start_date_ms,
        end_date_ms,
        distance_km: Some(distance_km),
        duration_min: Some(duration_min),
        efficiency: None,
        outside_temp_avg,
        inside_temp_avg,
        speed_max: open.speed_max,
        power_max,
        power_min,
        start_ideal_range_km,
        end_ideal_range_km,
        start_address: None,
        end_address: None,
        start_geofence: None,
        end_geofence: None,
        start_latitude,
        start_longitude,
        end_latitude,
        end_longitude,
        start_soc,
        end_soc,
        start_rated_range_km,
        end_rated_range_km,
        ascent: Some(ascent),
        descent: Some(descent),
    };
    Ok(Some(ClosedDrive {
        drive,
        // May be only the tail in the durable path; commit reloads open rows.
        positions: open.positions,
    }))
}

fn elevation_totals(positions: &[ProjectionPosition]) -> (i64, i64) {
    let mut previous = None;
    let mut ascent = 0_i64;
    let mut descent = 0_i64;
    for elevation in positions.iter().filter_map(|position| position.elevation) {
        if let Some(previous_elevation) = previous {
            let delta = elevation - previous_elevation;
            if delta > 0 {
                ascent = ascent.saturating_add(delta);
            } else if delta < 0 {
                descent = descent.saturating_add(delta.unsigned_abs() as i64);
            }
        }
        previous = Some(elevation);
    }
    (cap_elevation_total(ascent), cap_elevation_total(descent))
}

fn cap_elevation_total(value: i64) -> i64 {
    if value >= 32_768 { 0 } else { value }
}

fn finalize_charge(
    open: OpenCharge,
    end_date_ms: Option<i64>,
) -> Result<ClosedCharge, LifecycleError> {
    let end_date_ms = end_date_ms.unwrap_or(open.start_date_ms);
    if end_date_ms < open.start_date_ms {
        return Err(LifecycleError::InvalidTimeline);
    }
    let duration_min = ((end_date_ms - open.start_date_ms) / 60_000).max(0);
    let outside_temp_avg = if open.outside_temp_count > 0 {
        Some(open.outside_temp_sum / f64::from(open.outside_temp_count))
    } else {
        None
    };
    let charge = ProjectionCharge {
        id: open.id,
        car_id: open.car_id,
        start_date_ms: open.start_date_ms,
        end_date_ms: Some(end_date_ms),
        charge_energy_added: charge_energy_added_delta(
            open.first_energy_added,
            open.last_energy_added,
            open.max_energy_added,
        ),
        charge_energy_used_kwh: open
            .energy_used_kwh
            .or_else(|| calculate_energy_used_kwh(&open.samples)),
        start_ideal_range_km: open.start_ideal_range_km,
        end_ideal_range_km: open.last_ideal_range_km,
        cost: None,
        fast_charger_type: open.fast_charger_type,
        billing_type: None,
        cost_per_unit: None,
        session_fee: None,
        start_latitude: open.start_latitude,
        start_longitude: open.start_longitude,
        start_battery_level: open.start_battery_level,
        end_battery_level: open.last_battery_level,
        duration_min: Some(duration_min),
        address: None,
        location_name: None,
        geofence: None,
        is_dc: open.is_dc,
        charge_rate_km_per_hour: None,
        max_charger_power_kw: open.max_charger_power_kw,
        outside_temp_avg,
        start_rated_range_km: open.start_rated_range_km,
        end_rated_range_km: open.last_rated_range_km,
    };
    Ok(ClosedCharge {
        charge,
        samples: open.samples,
    })
}

fn observe_charge_aggregate(open: &mut OpenCharge, sample: &ParsedSample) {
    if open.first_energy_added.is_none() {
        open.first_energy_added = open.last_energy_added;
    }
    if let Some(energy) = sample.charge_energy_added {
        open.first_energy_added.get_or_insert(energy);
        open.max_energy_added = Some(open.max_energy_added.map_or(energy, |max| max.max(energy)));
        open.last_energy_added = Some(energy);
    }
    open.last_battery_level = sample.battery_level.or(open.last_battery_level);
    open.last_ideal_range_km = sample.ideal_range_km.or(open.last_ideal_range_km);
    open.last_rated_range_km = sample.rated_range_km.or(open.last_rated_range_km);
    if sample.fast_charger_type.is_some() {
        open.fast_charger_type = sample.fast_charger_type.clone();
    }
}

fn charge_energy_added_delta(
    first: Option<f64>,
    last: Option<f64>,
    max: Option<f64>,
) -> Option<f64> {
    let first = first?;
    let end = match last {
        Some(value) if value > 0.0 => value,
        _ => max?,
    };
    let delta = end - first;
    (delta >= 0.0).then_some(delta)
}

pub(crate) fn calculate_energy_used_kwh(samples: &[ProjectionChargeSample]) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let mut ordered = samples.to_vec();
    ordered.sort_by_key(|sample| sample.timestamp_ms);
    let phases = determine_phases(&ordered);
    let mut total = 0.0;
    let mut usable_interval = false;
    for pair in ordered.windows(2) {
        let elapsed_ms = pair[1].timestamp_ms.saturating_sub(pair[0].timestamp_ms);
        if elapsed_ms == 0 {
            continue;
        }
        let sample = &pair[1];
        let power_kw = match sample.charger_phases.filter(|phases| *phases > 0) {
            None => sample.charger_power_kw,
            Some(_) => phases
                .and_then(|phases| {
                    sample
                        .charger_actual_current
                        .zip(sample.charger_voltage)
                        .map(|(current, voltage)| current * voltage * phases / 1_000.0)
                })
                .or(sample.charger_power_kw),
        };
        if let Some(power_kw) = power_kw.filter(|power| power.is_finite() && *power >= 0.0) {
            total += power_kw * elapsed_ms as f64 / 3_600_000.0;
            usable_interval = true;
        }
    }
    usable_interval.then_some(total)
}

fn determine_phases(samples: &[ProjectionChargeSample]) -> Option<f64> {
    if samples.len() <= 15 {
        return None;
    }
    let ratios = samples.iter().filter_map(|sample| {
        let power = sample.charger_power_kw?;
        let current = sample.charger_actual_current?;
        let voltage = sample.charger_voltage?;
        let denominator = current * voltage;
        (denominator != 0.0 && denominator.is_finite())
            .then_some(power * 1_000.0 / denominator)
            .filter(|ratio| ratio.is_finite() && *ratio > 0.0)
    });
    let ratios = ratios.collect::<Vec<_>>();
    if ratios.is_empty() {
        return None;
    }
    let power_phase_ratio = ratios.iter().copied().sum::<f64>() / ratios.len() as f64;
    let raw_phases = samples
        .iter()
        .filter_map(|sample| sample.charger_phases.filter(|phases| *phases > 0));
    let raw_phases = raw_phases.collect::<Vec<_>>();
    let average_phases = (!raw_phases.is_empty())
        .then(|| {
            raw_phases.iter().map(|value| *value as f64).sum::<f64>() / raw_phases.len() as f64
        })
        .map(f64::round);
    if average_phases == Some(power_phase_ratio.round()) {
        return average_phases;
    }
    if average_phases == Some(3.0) && (power_phase_ratio / 3.0_f64.sqrt() - 1.0).abs() <= 0.1 {
        return Some(3.0_f64.sqrt());
    }
    let rounded = power_phase_ratio.round();
    (rounded > 0.0 && (rounded - power_phase_ratio).abs() <= 0.3).then_some(rounded)
}

fn is_drive_shift(shift_state: Option<&str>) -> bool {
    matches!(shift_state, Some("D" | "R" | "N" | "d" | "r" | "n"))
}

pub(crate) const TESLAMATE_GAINED_RANGE_MIN_OFFLINE: Duration = Duration::from_secs(300);
const TESLAMATE_GAINED_RANGE_MILES: f64 = 5.0;
const KM_PER_MILE: f64 = 1.609_344;
