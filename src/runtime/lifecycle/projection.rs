// SPDX-License-Identifier: AGPL-3.0-only

/// Convert sparse Tesla streaming telemetry into the same nested shape used
/// by owner observations. Missing fields remain null and therefore cannot
/// erase metadata supplied by a fuller owner response.
pub fn stream_observation_payload(update: &crate::tesla_stream::StreamUpdate) -> Value {
    json!({
        "record_type": "tesla_stream_update_v1",
        "source_vehicle_state": "online",
        "fields": {
            "drive_state": {
                "timestamp": update.timestamp_ms,
                "speed": update.speed,
                "odometer": update.odometer,
                "elevation": update.elevation,
                "heading": update.heading.or(update.est_heading),
                "est_heading": update.est_heading,
                "latitude": update.est_lat,
                "longitude": update.est_lng,
                "est_lat": update.est_lat,
                "est_lng": update.est_lng,
                "power": update.power,
                "shift_state": update.shift_state,
            },
            "charge_state": {
                "timestamp": update.timestamp_ms,
                "battery_level": update.soc,
                "ideal_battery_range": update.range,
                "est_battery_range": update.est_range,
            },
            "vehicle_state": {
                "timestamp": update.timestamp_ms,
                "odometer": update.odometer,
            },
        },
    })
}

#[cfg(test)]
mod stream_fixture_tests {
    use super::*;
    use crate::tesla_stream::parse_data_update;

    fn sample(id: i64, timestamp_ms: i64, odometer: f64, shift_state: &str) -> LifecycleSample {
        let frame = format!(
            r#"{{"msg_type":"data:update","tag":"9","timestamp":{timestamp_ms},"value":"{speed},{odometer},80,25,180,51.5,-0.1,120,{shift_state},200,210,180"}}"#,
            speed = if shift_state == "P" { 0 } else { 10 },
        );
        let update = parse_data_update(&frame).unwrap();
        LifecycleSample {
            observation_id: id,
            observed_at_ms: timestamp_ms,
            vehicle_state: "online".to_owned(),
            payload: stream_observation_payload(&update),
        }
    }

    fn owner_park(id: i64, timestamp_ms: i64, odometer_miles: f64) -> LifecycleSample {
        LifecycleSample {
            observation_id: id,
            observed_at_ms: timestamp_ms,
            vehicle_state: "online".to_owned(),
            payload: json!({
                "record_type": "owner_api_vehicle_data_v1",
                "vehicle_data": {
                    "drive_state": {
                        "shift_state": "P",
                        "speed": 0,
                        "latitude": 51.5,
                        "longitude": -0.1,
                        "timestamp": timestamp_ms
                    },
                    "vehicle_state": {
                        "odometer": odometer_miles,
                        "timestamp": timestamp_ms
                    }
                }
            }),
        }
    }

    #[test]
    fn stream_drive_closes_before_owner_poll_and_duplicate_is_idempotent() {
        let state = OpenSessionState::new();
        let parked = sample(1, 1_700_000_000_000, 100.0, "P");
        let driving_one = sample(2, 1_700_000_001_000, 100.0, "D");
        let driving_two = sample(3, 1_700_000_002_000, 100.2, "D");
        let parked_again = owner_park(4, 1_700_000_003_000, 100.3);

        let first = apply_sample(state, 9, &parked).unwrap();
        let second = apply_sample(first.state, 9, &driving_one).unwrap();
        let third = apply_sample(second.state, 9, &driving_two).unwrap();
        let closed = apply_sample(third.state, 9, &parked_again).unwrap();
        assert_eq!(closed.delta.drives.len(), 1);
        assert_eq!(
            closed
                .delta
                .positions
                .iter()
                .filter(|position| position.drive_id == Some(1))
                .count(),
            3
        );
        assert_eq!(
            closed.delta.drives[0].start_date_ms,
            driving_one.observed_at_ms
        );
        assert_eq!(
            closed.delta.drives[0].end_date_ms,
            parked_again.observed_at_ms
        );

        let duplicate = apply_sample(closed.state, 9, &parked_again).unwrap();
        assert!(duplicate.delta.drives.is_empty());
        assert!(duplicate.delta.positions.is_empty());
    }

    #[test]
    fn stream_sample_materializes_all_teslamate_position_fields() {
        let stream = sample(1, 1_700_000_000_000, 100.25, "D");
        let parsed = parse_sample(&stream).unwrap();
        let position = position_from_sample(1, Some(1), 9, &parsed)
            .unwrap()
            .unwrap();

        assert_eq!(position.date_ms, stream.observed_at_ms);
        assert_eq!(position.latitude, 51.5);
        assert_eq!(position.longitude, -0.1);
        assert_eq!(position.speed, Some(16));
        assert_eq!(position.power, Some(120.0));
        assert_eq!(position.battery_level, Some(80));
        assert_eq!(position.usable_battery_level, None);
        assert_eq!(position.elevation, Some(25));
        assert_eq!(position.odometer, Some(161.336736));
        assert_eq!(position.ideal_battery_range_km, Some(321.87));
    }

    fn stream_sample(id: i64, timestamp_ms: i64, speed: i64, shift_state: &str) -> LifecycleSample {
        let frame = format!(
            r#"{{"msg_type":"data:update","tag":"9","timestamp":{timestamp_ms},"value":"{speed},100.0,80,25,180,51.5,-0.1,120,{shift_state},200,210,180"}}"#,
        );
        let update = parse_data_update(&frame).unwrap();
        LifecycleSample {
            observation_id: id,
            observed_at_ms: timestamp_ms,
            vehicle_state: "online".to_owned(),
            payload: stream_observation_payload(&update),
        }
    }

    #[test]
    fn numeric_speed_without_drive_shift_does_not_open_a_drive() {
        let parked_with_speed = stream_sample(1, 1_700_000_000_000, 25, "P");
        let empty_shift_with_speed = stream_sample(2, 1_700_000_001_000, 25, "");
        let driving = stream_sample(3, 1_700_000_002_000, 0, "D");
        let reverse = stream_sample(4, 1_700_000_003_000, 0, "R");
        let neutral = stream_sample(5, 1_700_000_004_000, 0, "N");

        let parked = apply_sample(OpenSessionState::new(), 9, &parked_with_speed).unwrap();
        assert!(parked.state.open_drive.is_none());
        assert_eq!(parked.state.phase, VehiclePhase::Online);

        let empty = apply_sample(parked.state, 9, &empty_shift_with_speed).unwrap();
        assert!(empty.state.open_drive.is_none());
        assert_eq!(empty.state.phase, VehiclePhase::Online);

        let in_drive = apply_sample(empty.state, 9, &driving).unwrap();
        assert!(in_drive.state.open_drive.is_some());
        assert_eq!(in_drive.state.phase, VehiclePhase::Driving);

        let in_reverse = apply_sample(OpenSessionState::new(), 9, &reverse).unwrap();
        assert!(in_reverse.state.open_drive.is_some());
        let in_neutral = apply_sample(OpenSessionState::new(), 9, &neutral).unwrap();
        assert!(in_neutral.state.open_drive.is_some());
    }

    #[test]
    fn empty_stream_shift_does_not_close_an_open_drive() {
        let driving = stream_sample(1, 1_700_000_010_000, 20, "D");
        let sparse = stream_sample(2, 1_700_000_011_000, 21, "");
        let parked = owner_park(3, 1_700_000_012_000, 100.4);

        let opened = apply_sample(OpenSessionState::new(), 9, &driving).unwrap();
        assert!(opened.state.open_drive.is_some());
        let continued = apply_sample(opened.state, 9, &sparse).unwrap();
        assert!(
            continued.state.open_drive.is_some(),
            "TeslaMate keeps a drive open on a blank stream shift and fetches Owner/Fleet to confirm park"
        );
        assert!(continued.delta.drives.is_empty());
        let closed = apply_sample(continued.state, 9, &parked).unwrap();
        assert!(closed.state.open_drive.is_none());
        assert_eq!(closed.delta.drives.len(), 1);
    }

    #[test]
    fn newer_local_id_with_older_provider_time_only_advances_cursor() {
        let current = sample(1, 1_700_000_002_000, 100.2, "P");
        let projected = apply_sample(OpenSessionState::new(), 9, &current).unwrap();
        let expected_phase = projected.state.phase;
        let expected_timestamp = projected.state.last_observed_at_ms;

        let stale = sample(2, 1_700_000_001_000, 99.9, "D");
        let ignored = apply_sample(projected.state, 9, &stale).unwrap();

        assert_eq!(ignored.state.last_observation_id, 2);
        assert_eq!(ignored.state.last_observed_at_ms, expected_timestamp);
        assert_eq!(ignored.state.phase, expected_phase);
        assert_eq!(
            ignored.state.last_drive_timestamp_ms,
            Some(current.observed_at_ms)
        );
        assert_eq!(ignored.delta, LifecycleDelta::default());
    }
}

/// Project one already-stored observation onto the open session.
///
/// Samples with `observation_id <= state.last_observation_id` are ignored so
/// restarts and retries stay idempotent. Out-of-order IDs after the cursor are
/// also ignored rather than rewriting history.
pub fn apply_sample(
    state: OpenSessionState,
    car_id: i64,
    sample: &LifecycleSample,
) -> Result<LifecycleStep, LifecycleError> {
    apply_sample_with_offline_drive_timeout(state, car_id, sample, DEFAULT_OFFLINE_DRIVE_TIMEOUT)
}

pub(crate) fn apply_sample_with_offline_drive_timeout(
    mut state: OpenSessionState,
    car_id: i64,
    sample: &LifecycleSample,
    offline_drive_timeout: Duration,
) -> Result<LifecycleStep, LifecycleError> {
    if car_id <= 0 {
        return Err(LifecycleError::InvalidCarId);
    }
    if offline_drive_timeout.is_zero() {
        return Err(LifecycleError::InvalidOfflineDriveTimeout);
    }
    if sample.observation_id <= state.last_observation_id {
        return Ok(LifecycleStep {
            state,
            delta: LifecycleDelta::default(),
            quarantined: false,
        });
    }
    if state
        .last_observed_at_ms
        .is_some_and(|watermark| sample.observed_at_ms < watermark)
    {
        // Provider responses can arrive out of order even though their local
        // observation IDs are increasing. Consume the durable cursor without
        // letting an older snapshot rewrite lifecycle state or transitions.
        state.last_observation_id = sample.observation_id;
        return Ok(LifecycleStep {
            state,
            delta: LifecycleDelta::default(),
            quarantined: false,
        });
    }

    // A malformed provider sample must not advance the durable cursor or
    // discard an open drive/charge prefix. The enclosing database transaction
    // rolls back and a later valid observation can continue the same session.
    let mut parsed = parse_sample(sample)?;

    let drive_fresh = state
        .imported_drive_watermark_ms
        .is_none_or(|watermark| parsed.drive_timestamp_ms > watermark);
    let charge_fresh = state
        .imported_charge_watermark_ms
        .is_none_or(|watermark| parsed.charge_timestamp_ms > watermark);
    let vehicle_fresh = state
        .imported_state_watermark_ms
        .is_none_or(|watermark| parsed.vehicle_timestamp_ms > watermark);
    if !drive_fresh {
        parsed.drive_data_present = false;
    }
    if !charge_fresh {
        parsed.charge_data_present = false;
    }
    if !vehicle_fresh {
        parsed.vehicle_state_present = false;
        if !parsed.drive_data_present && !parsed.charge_data_present {
            parsed.phase = state.phase;
        }
    }

    let mut delta = LifecycleDelta::default();
    parsed.battery_heater = parsed.battery_heater.or(state.last_position_battery_heater);
    parsed.battery_heater_on = parsed
        .battery_heater_on
        .or(state.last_position_battery_heater_on);
    parsed.battery_heater_no_power = parsed
        .battery_heater_no_power
        .or(state.last_position_battery_heater_no_power);
    state.last_position_battery_heater = parsed.battery_heater;
    state.last_position_battery_heater_on = parsed.battery_heater_on;
    state.last_position_battery_heater_no_power = parsed.battery_heater_no_power;
    parsed.est_range_km = parsed
        .est_range_km
        .or(state.last_position_est_battery_range_km);
    parsed.fan_status = parsed.fan_status.or(state.last_position_fan_status);
    parsed.driver_temp_setting = parsed
        .driver_temp_setting
        .or(state.last_position_driver_temp_setting);
    parsed.passenger_temp_setting = parsed
        .passenger_temp_setting
        .or(state.last_position_passenger_temp_setting);
    parsed.is_rear_defroster_on = parsed
        .is_rear_defroster_on
        .or(state.last_position_is_rear_defroster_on);
    parsed.is_front_defroster_on = parsed
        .is_front_defroster_on
        .or(state.last_position_is_front_defroster_on);
    parsed.tpms_pressure_fl = parsed
        .tpms_pressure_fl
        .or(state.last_position_tpms_pressure_fl);
    parsed.tpms_pressure_fr = parsed
        .tpms_pressure_fr
        .or(state.last_position_tpms_pressure_fr);
    parsed.tpms_pressure_rl = parsed
        .tpms_pressure_rl
        .or(state.last_position_tpms_pressure_rl);
    parsed.tpms_pressure_rr = parsed
        .tpms_pressure_rr
        .or(state.last_position_tpms_pressure_rr);
    state.last_position_est_battery_range_km = parsed.est_range_km;
    state.last_position_fan_status = parsed.fan_status;
    state.last_position_driver_temp_setting = parsed.driver_temp_setting;
    state.last_position_passenger_temp_setting = parsed.passenger_temp_setting;
    state.last_position_is_rear_defroster_on = parsed.is_rear_defroster_on;
    state.last_position_is_front_defroster_on = parsed.is_front_defroster_on;
    state.last_position_tpms_pressure_fl = parsed.tpms_pressure_fl;
    state.last_position_tpms_pressure_fr = parsed.tpms_pressure_fr;
    state.last_position_tpms_pressure_rl = parsed.tpms_pressure_rl;
    state.last_position_tpms_pressure_rr = parsed.tpms_pressure_rr;
    let prior_phase = state.phase;
    let prior_drive_timestamp_ms = state.last_drive_timestamp_ms;
    let prior_charge_timestamp_ms = state.last_charge_timestamp_ms;
    let prior_vehicle_timestamp_ms = state.last_vehicle_timestamp_ms;
    let previous_firmware_version = state
        .car_metadata
        .as_ref()
        .and_then(|metadata| metadata.firmware_version.as_deref())
        .map(str::to_owned);
    parsed.drive_timestamp_ms = monotonic_timestamp(
        parsed.drive_timestamp_ms,
        sample.observed_at_ms,
        prior_drive_timestamp_ms,
    );
    parsed.charge_timestamp_ms = monotonic_timestamp(
        parsed.charge_timestamp_ms,
        sample.observed_at_ms,
        prior_charge_timestamp_ms,
    );
    parsed.vehicle_timestamp_ms = monotonic_timestamp(
        parsed.vehicle_timestamp_ms,
        sample.observed_at_ms,
        prior_vehicle_timestamp_ms,
    );
    if let Some(newer) = parsed.car_metadata.as_ref() {
        state
            .car_metadata
            .get_or_insert_with(ProjectionCarPatch::default)
            .merge_newer(newer);
    }
    if parsed.service_mode.is_some() {
        state.service_mode = parsed.service_mode;
    }
    if parsed.phase == VehiclePhase::Offline
        && let Some(open) = state.open_drive.as_mut()
    {
        open.saw_offline = true;
    }

    // Charge and drive transitions are independent enough that either can
    // close while the other opens on successive samples, but one sample never
    // starts both simultaneously in a coherent Tesla response.
    let pending_gained_range_evaluated = state.pending_gained_range_charge.is_some()
        && !parsed.stream_frame
        && parsed.charge_data_present
        && parsed.ideal_range_km.is_some()
        && !matches!(
            parsed.phase,
            VehiclePhase::Offline
                | VehiclePhase::Asleep
                | VehiclePhase::Updating
                | VehiclePhase::Unknown
        );
    let gained_range_charge = teslamate_gained_range_charge_seed(&state, &parsed);
    if pending_gained_range_evaluated {
        state.pending_gained_range_charge = None;
    }
    if let Some(closed) = maybe_close_drive(
        &mut state,
        car_id,
        &parsed,
        offline_drive_timeout,
        gained_range_charge
            .as_ref()
            .is_some_and(|candidate| candidate.close_open_drive),
    )? {
        if let Some(completed) = closed.completed {
            delta.positions.extend(completed.positions);
            delta.drives.push(completed.drive);
        } else {
            delta.discarded_drive_ids.push(closed.drive_id);
        }
    }
    if let Some(candidate) = gained_range_charge
        && let Some(closed) =
            materialize_gained_range_charge(&mut state, car_id, &parsed, candidate.seed)?
    {
        if let (Some(latitude), Some(longitude)) =
            (closed.charge.start_latitude, closed.charge.start_longitude)
        {
            delta
                .charge_start_coordinates
                .push((closed.charge.id, latitude, longitude));
        }
        delta.charge_samples.extend(closed.samples);
        delta.charges.push(closed.charge);
    }
    if let Some(closed) = maybe_close_charge(&mut state, car_id, &parsed)? {
        if let (Some(latitude), Some(longitude)) =
            (closed.charge.start_latitude, closed.charge.start_longitude)
        {
            delta
                .charge_start_coordinates
                .push((closed.charge.id, latitude, longitude));
        }
        delta.charge_samples.extend(closed.samples);
        delta.charges.push(closed.charge);
    }
    maybe_open_or_extend_drive(&mut state, car_id, &parsed, &mut delta)?;
    if let Some(open) = state.open_drive.as_mut() {
        if let Some(energy) = parsed.charge_energy_added {
            open.last_charge_energy_added = Some(energy);
        }
        if let Some(ideal) = parsed.ideal_range_km {
            open.last_ideal_range_km = Some(ideal);
        }
    }
    maybe_open_or_extend_charge(&mut state, car_id, &parsed, &mut delta)?;
    maybe_emit_stationary_position(
        &mut state,
        car_id,
        prior_phase,
        prior_drive_timestamp_ms,
        &parsed,
        &mut delta,
    )?;
    state.phase = match (prior_phase, parsed.phase) {
        (VehiclePhase::Charging, VehiclePhase::Offline) if state.open_charge.is_some() => {
            VehiclePhase::Charging
        }
        (VehiclePhase::Charging, VehiclePhase::Online)
            if !parsed.charge_data_present && state.open_charge.is_some() =>
        {
            VehiclePhase::Charging
        }
        (VehiclePhase::Updating, VehiclePhase::Offline) => VehiclePhase::Updating,
        (VehiclePhase::Driving, VehiclePhase::Offline) if state.open_drive.is_some() => {
            VehiclePhase::Driving
        }
        (VehiclePhase::Driving, VehiclePhase::Online)
            if !parsed.drive_data_present && state.open_drive.is_some() =>
        {
            VehiclePhase::Driving
        }
        (_, phase) => phase,
    };

    let effective_phase = state.phase;
    update_state_interval(
        &mut state,
        car_id,
        effective_phase,
        parsed.observed_at_ms,
        &mut delta,
    )?;
    update_software_update(
        &mut state,
        car_id,
        &parsed,
        previous_firmware_version.as_deref(),
        &mut delta,
    )?;

    state.last_observation_id = sample.observation_id;
    state.last_observed_at_ms = Some(sample.observed_at_ms);
    if parsed.drive_data_present {
        state.last_drive_timestamp_ms = Some(parsed.drive_timestamp_ms);
    }
    if parsed.charge_data_present {
        state.last_charge_timestamp_ms = Some(parsed.charge_timestamp_ms);
    }
    if parsed.vehicle_state_present {
        state.last_vehicle_timestamp_ms = Some(parsed.vehicle_timestamp_ms);
    }
    Ok(LifecycleStep {
        state,
        delta,
        quarantined: false,
    })
}

/// Apply a contiguous observation page, preserving intermediate deltas.
pub fn apply_samples(
    mut state: OpenSessionState,
    car_id: i64,
    samples: &[LifecycleSample],
) -> Result<LifecycleStep, LifecycleError> {
    let mut total = LifecycleDelta::default();
    let mut quarantined = false;
    for sample in samples {
        let step = apply_sample(state, car_id, sample)?;
        state = step.state;
        quarantined |= step.quarantined;
        total.drives.extend(step.delta.drives);
        for discarded_drive_id in &step.delta.discarded_drive_ids {
            total
                .open_drive_positions
                .retain(|position| position.drive_id != Some(*discarded_drive_id));
        }
        total
            .discarded_drive_ids
            .extend(step.delta.discarded_drive_ids);
        total.positions.extend(step.delta.positions);
        total.charges.extend(step.delta.charges);
        total.charge_samples.extend(step.delta.charge_samples);
        total.states.extend(step.delta.states);
        total.updates.extend(step.delta.updates);
        total
            .open_drive_positions
            .extend(step.delta.open_drive_positions);
        total
            .open_charge_samples
            .extend(step.delta.open_charge_samples);
    }
    Ok(LifecycleStep {
        state,
        delta: total,
        quarantined,
    })
}

/// Force-close every open session. Used when a vehicle becomes unavailable for
/// long enough that continuing the open segment would invent duration.
pub fn force_close_open_sessions(
    mut state: OpenSessionState,
    car_id: i64,
    closed_at_ms: i64,
) -> Result<LifecycleStep, LifecycleError> {
    if car_id <= 0 {
        return Err(LifecycleError::InvalidCarId);
    }
    let mut delta = LifecycleDelta::default();
    if let Some(open) = state.open_drive.take() {
        let drive_id = open.id;
        if let Some(closed) = finalize_drive(open)? {
            delta.positions.extend(closed.positions);
            delta.drives.push(closed.drive);
        } else {
            delta.discarded_drive_ids.push(drive_id);
        }
    }
    if let Some(open) = state.open_charge.take() {
        let closed = finalize_charge(open, Some(closed_at_ms))?;
        delta.charge_samples.extend(closed.samples);
        delta.charges.push(closed.charge);
    }
    if matches!(state.phase, VehiclePhase::Driving | VehiclePhase::Charging) {
        state.phase = VehiclePhase::Online;
    }
    Ok(LifecycleStep {
        state,
        delta,
        quarantined: false,
    })
}

pub fn force_close_for_service(
    state: OpenSessionState,
    car_id: i64,
    closed_at_ms: i64,
) -> Result<LifecycleStep, LifecycleError> {
    let mut step = force_close_open_sessions(state, car_id, closed_at_ms)?;
    step.state.service_mode = Some(true);
    Ok(step)
}

#[derive(Debug)]
struct ParsedSample {
    observed_at_ms: i64,
    drive_timestamp_ms: i64,
    charge_timestamp_ms: i64,
    vehicle_timestamp_ms: i64,
    vehicle_state_present: bool,
    vehicle_state_is_online: bool,
    service_mode: Option<bool>,
    installing_update: bool,
    car_version: Option<String>,
    car_metadata: Option<ProjectionCarPatch>,
    phase: VehiclePhase,
    shift_state: Option<String>,
    stream_frame: bool,
    drive_data_present: bool,
    speed: Option<i64>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    power: Option<f64>,
    battery_level: Option<i64>,
    usable_battery_level: Option<i64>,
    rated_range_km: Option<f64>,
    ideal_range_km: Option<f64>,
    est_range_km: Option<f64>,
    odometer: Option<f64>,
    elevation: Option<i64>,
    is_climate_on: Option<bool>,
    inside_temp: Option<f64>,
    outside_temp: Option<f64>,
    fan_status: Option<i64>,
    driver_temp_setting: Option<f64>,
    passenger_temp_setting: Option<f64>,
    is_rear_defroster_on: Option<bool>,
    is_front_defroster_on: Option<bool>,
    tpms_pressure_fl: Option<f64>,
    tpms_pressure_fr: Option<f64>,
    tpms_pressure_rl: Option<f64>,
    tpms_pressure_rr: Option<f64>,
    charging_state: Option<String>,
    software_update_status: Option<String>,
    charge_data_present: bool,
    charge_energy_added: Option<f64>,
    charger_power_kw: Option<f64>,
    charger_voltage: Option<f64>,
    charger_actual_current: Option<f64>,
    charger_pilot_current: Option<f64>,
    charger_phases: Option<i64>,
    fast_charger_present: Option<bool>,
    fast_charger_brand: Option<String>,
    fast_charger_type: Option<String>,
    charge_cable: Option<String>,
    battery_heater_on: Option<bool>,
    battery_heater: Option<bool>,
    battery_heater_no_power: Option<bool>,
    not_enough_power_to_heat: Option<bool>,
}

fn parse_sample(sample: &LifecycleSample) -> Result<ParsedSample, LifecycleError> {
    let root = sample
        .payload
        .as_object()
        .ok_or(LifecycleError::InvalidPayload)?;
    let stream_frame = matches!(
        root.get("record_type").and_then(Value::as_str),
        Some("tesla_stream_update_v1")
    );
    let vehicle_data = match root.get("record_type").and_then(Value::as_str) {
        Some("owner_api_vehicle_data_v1" | "fleet_api_vehicle_data_v1") => Some(
            root.get("vehicle_data")
                .or_else(|| {
                    root.get("provider_raw_json")
                        .and_then(|raw| raw.get("response"))
                })
                .and_then(Value::as_object)
                .ok_or(LifecycleError::InvalidPayload)?,
        ),
        Some("owner_api_discovery_v1" | "fleet_api_discovery_v1") => None,
        Some("tesla_stream_update_v1") => Some(
            root.get("fields")
                .and_then(Value::as_object)
                .ok_or(LifecycleError::InvalidPayload)?,
        ),
        _ => return Err(LifecycleError::InvalidPayload),
    };

    let drive = vehicle_data.and_then(|fields| object_field(fields, "drive_state"));
    let charge = vehicle_data.and_then(|fields| object_field(fields, "charge_state"));
    let climate = vehicle_data.and_then(|fields| object_field(fields, "climate_state"));
    let vehicle_state = vehicle_data.and_then(|fields| object_field(fields, "vehicle_state"));

    let shift_state = drive
        .and_then(|fields| fields.get("shift_state"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let charging_state = charge
        .and_then(|fields| fields.get("charging_state"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let drive_timestamp_ms = drive
        .and_then(|fields| timestamp_field(fields, "timestamp"))
        .unwrap_or(sample.observed_at_ms);
    let charge_timestamp_ms = charge
        .and_then(|fields| timestamp_field(fields, "timestamp"))
        .unwrap_or(sample.observed_at_ms);
    let vehicle_timestamp_ms = vehicle_state
        .and_then(|fields| timestamp_field(fields, "timestamp"))
        .unwrap_or(sample.observed_at_ms);
    let car_metadata = if sample.vehicle_state.eq_ignore_ascii_case("online")
        && vehicle_data.is_some()
        && vehicle_state.is_some()
    {
        let vehicle_config = vehicle_data.and_then(|fields| object_field(fields, "vehicle_config"));
        let raw_car_type = text_field(vehicle_config, "car_type");
        let model = raw_car_type.as_deref().map(normalize_tesla_model_code);
        let trim_badging = text_field(vehicle_config, "trim_badging")
            .map(|value| crate::hub_pack::normalize_tesla_trim(&value));
        let vin = text_field(Some(root), "vin")
            .or_else(|| vehicle_data.and_then(|fields| text_field(Some(fields), "vin")));
        let marketing_name = raw_car_type.as_deref().and_then(|raw| {
            model.as_deref().and_then(|model| {
                crate::hub_pack::derive_tesla_marketing_name(
                    model,
                    trim_badging.as_deref(),
                    Some(raw),
                    vin.as_deref(),
                )
            })
        });
        let patch = ProjectionCarPatch {
            name: text_field(Some(root), "display_name")
                .or_else(|| {
                    vehicle_data.and_then(|fields| text_field(Some(fields), "display_name"))
                })
                .or_else(|| text_field(vehicle_state, "vehicle_name")),
            model,
            vin,
            trim_badging,
            marketing_name,
            exterior_color: text_field(vehicle_config, "exterior_color"),
            wheel_type: text_field(vehicle_config, "wheel_type"),
            spoiler_type: text_field(vehicle_config, "spoiler_type"),
            firmware_version: text_field(vehicle_state, "car_version"),
        };
        (!patch.is_empty()).then_some(patch)
    } else {
        None
    };
    let speed = drive
        .and_then(|fields| float_field(fields, "speed"))
        .map(mph_to_kmh);
    let installing_update = vehicle_state
        .and_then(|fields| object_field(fields, "software_update"))
        .and_then(|update| update.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|status| status.eq_ignore_ascii_case("installing"));
    let software_update_status = vehicle_state
        .and_then(|fields| object_field(fields, "software_update"))
        .and_then(|update| update.get("status"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let phase = if sample.vehicle_state.eq_ignore_ascii_case("asleep") {
        VehiclePhase::Asleep
    } else if sample.vehicle_state.eq_ignore_ascii_case("offline") {
        VehiclePhase::Offline
    } else if sample.vehicle_state.eq_ignore_ascii_case("updating") || installing_update {
        VehiclePhase::Updating
    } else if is_charging_state(charging_state.as_deref()) {
        VehiclePhase::Charging
    } else if is_drive_shift(shift_state.as_deref()) {
        VehiclePhase::Driving
    } else if sample.vehicle_state.eq_ignore_ascii_case("online") {
        VehiclePhase::Online
    } else {
        phase_from_vehicle_state(&sample.vehicle_state)
    };

    Ok(ParsedSample {
        observed_at_ms: sample.observed_at_ms,
        drive_timestamp_ms,
        charge_timestamp_ms,
        vehicle_timestamp_ms,
        vehicle_state_present: vehicle_state.is_some(),
        vehicle_state_is_online: sample.vehicle_state.eq_ignore_ascii_case("online"),
        service_mode: vehicle_state.and_then(|fields| bool_field(fields, "service_mode")),
        installing_update,
        car_version: vehicle_state
            .and_then(|fields| fields.get("car_version"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        car_metadata,
        phase,
        shift_state,
        stream_frame,
        drive_data_present: drive.is_some(),
        speed,
        latitude: drive.and_then(|fields| float_field(fields, "latitude")),
        longitude: drive.and_then(|fields| float_field(fields, "longitude")),
        power: drive.and_then(|fields| float_field(fields, "power")),
        battery_level: charge.and_then(|fields| int_field(fields, "battery_level")),
        usable_battery_level: charge.and_then(|fields| int_field(fields, "usable_battery_level")),
        rated_range_km: charge
            .and_then(|fields| float_field(fields, "battery_range"))
            .map(|miles| miles_to_km_rounded(miles, 2)),
        ideal_range_km: charge
            .and_then(|fields| float_field(fields, "ideal_battery_range"))
            .map(|miles| miles_to_km_rounded(miles, 2)),
        est_range_km: charge
            .and_then(|fields| float_field(fields, "est_battery_range"))
            .map(|miles| miles_to_km_rounded(miles, 2)),
        odometer: vehicle_state
            .and_then(|fields| float_field(fields, "odometer"))
            .map(|miles| miles_to_km_rounded(miles, 6)),
        elevation: drive.and_then(|fields| {
            int_field(fields, "native_location_elevation")
                .or_else(|| int_field(fields, "elevation"))
        }),
        is_climate_on: climate.and_then(|fields| bool_field(fields, "is_climate_on")),
        inside_temp: climate.and_then(|fields| float_field(fields, "inside_temp")),
        outside_temp: climate.and_then(|fields| float_field(fields, "outside_temp")),
        fan_status: climate.and_then(|fields| int_field(fields, "fan_status")),
        driver_temp_setting: climate.and_then(|fields| float_field(fields, "driver_temp_setting")),
        passenger_temp_setting: climate
            .and_then(|fields| float_field(fields, "passenger_temp_setting")),
        is_rear_defroster_on: climate.and_then(|fields| bool_field(fields, "is_rear_defroster_on")),
        is_front_defroster_on: climate
            .and_then(|fields| bool_field(fields, "is_front_defroster_on")),
        tpms_pressure_fl: vehicle_state.and_then(|fields| float_field(fields, "tpms_pressure_fl")),
        tpms_pressure_fr: vehicle_state.and_then(|fields| float_field(fields, "tpms_pressure_fr")),
        tpms_pressure_rl: vehicle_state.and_then(|fields| float_field(fields, "tpms_pressure_rl")),
        tpms_pressure_rr: vehicle_state.and_then(|fields| float_field(fields, "tpms_pressure_rr")),
        charging_state,
        software_update_status,
        charge_data_present: charge.is_some(),
        charge_energy_added: charge.and_then(|fields| float_field(fields, "charge_energy_added")),
        charger_power_kw: charge.and_then(|fields| float_field(fields, "charger_power")),
        charger_voltage: charge.and_then(|fields| float_field(fields, "charger_voltage")),
        charger_actual_current: charge
            .and_then(|fields| float_field(fields, "charger_actual_current")),
        charger_pilot_current: charge
            .and_then(|fields| float_field(fields, "charger_pilot_current")),
        charger_phases: charge
            .and_then(|fields| int_field(fields, "charger_phases"))
            .filter(|phases| *phases > 0),
        fast_charger_present: charge.and_then(|fields| bool_field(fields, "fast_charger_present")),
        fast_charger_brand: charge
            .and_then(|fields| fields.get("fast_charger_brand"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        fast_charger_type: charge
            .and_then(|fields| fields.get("fast_charger_type"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        charge_cable: charge
            .and_then(|fields| fields.get("conn_charge_cable"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        battery_heater_on: charge.and_then(|fields| bool_field(fields, "battery_heater_on")),
        battery_heater: climate.and_then(|fields| bool_field(fields, "battery_heater")),
        battery_heater_no_power: climate
            .and_then(|fields| bool_field(fields, "battery_heater_no_power")),
        not_enough_power_to_heat: charge
            .and_then(|fields| bool_field(fields, "not_enough_power_to_heat")),
    })
}

fn text_field(fields: Option<&Map<String, Value>>, key: &str) -> Option<String> {
    fields
        .and_then(|fields| fields.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn timestamp_field(fields: &Map<String, Value>, key: &str) -> Option<i64> {
    let value = fields.get(key)?;
    let timestamp = value
        .as_i64()
        .or_else(|| value.as_str().and_then(|text| text.parse::<i64>().ok()))
        .or_else(|| {
            let number = value.as_f64()?;
            (number.is_finite()
                && number >= 1.0
                && number <= i64::MAX as f64
                && number.fract() == 0.0)
                .then_some(number as i64)
        })?;
    (timestamp > 0).then_some(timestamp)
}

fn monotonic_timestamp(candidate: i64, fallback: i64, previous: Option<i64>) -> i64 {
    let candidate = if candidate > 0 { candidate } else { fallback };
    match previous {
        Some(previous) if candidate < previous => {
            if fallback >= previous {
                fallback
            } else {
                previous
            }
        }
        _ => candidate,
    }
}
