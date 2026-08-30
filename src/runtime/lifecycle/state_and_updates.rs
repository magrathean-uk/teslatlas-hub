// SPDX-License-Identifier: AGPL-3.0-only

pub(crate) fn teslamate_gained_range_implies_charge(
    last_position_at_ms: i64,
    last_ideal_range_km: Option<f64>,
    sample_at_ms: i64,
    sample_ideal_range_km: Option<f64>,
) -> bool {
    if sample_at_ms.saturating_sub(last_position_at_ms)
        < i64::try_from(TESLAMATE_GAINED_RANGE_MIN_OFFLINE.as_millis()).unwrap_or(i64::MAX)
    {
        return false;
    }
    match (last_ideal_range_km, sample_ideal_range_km) {
        (Some(previous), Some(current)) if previous.is_finite() && current.is_finite() => {
            (current - previous) / KM_PER_MILE > TESLAMATE_GAINED_RANGE_MILES
        }
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GainedRangeChargeSeed {
    pub start_date_ms: i64,
    pub start_battery_level: Option<i64>,
    pub start_ideal_range_km: Option<f64>,
    pub start_rated_range_km: Option<f64>,
    pub start_latitude: Option<f64>,
    pub start_longitude: Option<f64>,
    pub first_energy_added: f64,
}

fn gained_range_charge_seed_from_drive(open: &OpenDrive) -> GainedRangeChargeSeed {
    GainedRangeChargeSeed {
        start_date_ms: open.last_position_date_ms.unwrap_or(open.start_date_ms),
        start_battery_level: open.last_soc.or(open.start_soc),
        start_ideal_range_km: open
            .last_ideal_range_km
            .or(open.end_ideal_range_km)
            .or(open.start_ideal_range_km),
        start_rated_range_km: open.last_rated_range_km.or(open.start_rated_range_km),
        start_latitude: open.last_latitude.or(open.start_latitude),
        start_longitude: open.last_longitude.or(open.start_longitude),
        first_energy_added: open.last_charge_energy_added.unwrap_or(0.0),
    }
}

struct GainedRangeChargeCandidate {
    seed: GainedRangeChargeSeed,
    close_open_drive: bool,
}

fn teslamate_gained_range_charge_seed(
    state: &OpenSessionState,
    sample: &ParsedSample,
) -> Option<GainedRangeChargeCandidate> {
    if sample.stream_frame
        || state.open_charge.is_some()
        || is_charging_state(sample.charging_state.as_deref())
    {
        return None;
    }
    if matches!(
        sample.phase,
        VehiclePhase::Offline
            | VehiclePhase::Asleep
            | VehiclePhase::Updating
            | VehiclePhase::Unknown
    ) {
        return None;
    }
    let (seed, close_open_drive) = match state.pending_gained_range_charge.clone() {
        Some(seed) => (seed, false),
        None => (
            state
                .open_drive
                .as_ref()
                .filter(|open| open.saw_offline)
                .map(gained_range_charge_seed_from_drive)?,
            true,
        ),
    };
    teslamate_gained_range_implies_charge(
        seed.start_date_ms,
        seed.start_ideal_range_km,
        sample.drive_timestamp_ms.max(sample.charge_timestamp_ms),
        sample.ideal_range_km,
    )
    .then_some(GainedRangeChargeCandidate {
        seed,
        close_open_drive,
    })
}

fn materialize_gained_range_charge(
    state: &mut OpenSessionState,
    car_id: i64,
    sample: &ParsedSample,
    seed: GainedRangeChargeSeed,
) -> Result<Option<ClosedCharge>, LifecycleError> {
    if state.open_charge.is_some() {
        return Ok(None);
    }
    let id = state.next_charge_id;
    state.next_charge_id = state
        .next_charge_id
        .checked_add(1)
        .ok_or(LifecycleError::IdentifierExhausted)?;
    let last_energy_added = sample.charge_energy_added.unwrap_or(0.0);
    let open = OpenCharge {
        id,
        car_id,
        start_date_ms: seed.start_date_ms,
        start_battery_level: seed.start_battery_level,
        start_ideal_range_km: seed.start_ideal_range_km,
        start_rated_range_km: seed.start_rated_range_km,
        start_latitude: seed.start_latitude,
        start_longitude: seed.start_longitude,
        is_dc: sample.fast_charger_present,
        fast_charger_type: sample.fast_charger_type.clone(),
        max_charger_power_kw: sample.charger_power_kw,
        outside_temp_sum: 0.0,
        outside_temp_count: 0,
        first_energy_added: Some(seed.first_energy_added),
        max_energy_added: Some(seed.first_energy_added.max(last_energy_added)),
        last_energy_added: Some(last_energy_added),
        last_battery_level: sample.battery_level,
        last_ideal_range_km: sample.ideal_range_km,
        last_rated_range_km: sample.rated_range_km,
        sample_count: 0,
        energy_used_kwh: None,
        last_sample_timestamp_ms: None,
        last_sample_power_kw: None,
        samples: Vec::new(),
    };
    finalize_charge(
        open,
        Some(sample.charge_timestamp_ms.max(sample.drive_timestamp_ms)),
    )
    .map(Some)
}

fn is_charging_state(state: Option<&str>) -> bool {
    state
        .is_some_and(|state| matches!(state.to_ascii_lowercase().as_str(), "starting" | "charging"))
}

fn phase_from_vehicle_state(state: &str) -> VehiclePhase {
    match state.to_ascii_lowercase().as_str() {
        "online" => VehiclePhase::Online,
        "asleep" => VehiclePhase::Asleep,
        "offline" => VehiclePhase::Offline,
        "suspended" => VehiclePhase::Suspended,
        "updating" => VehiclePhase::Updating,
        _ => VehiclePhase::Unknown,
    }
}

fn logical_state(phase: VehiclePhase) -> Option<&'static str> {
    match phase {
        VehiclePhase::Online
        | VehiclePhase::Driving
        | VehiclePhase::Charging
        | VehiclePhase::Suspended
        | VehiclePhase::Updating => Some("online"),
        VehiclePhase::Asleep => Some("asleep"),
        VehiclePhase::Offline => Some("offline"),
        VehiclePhase::Unknown => None,
    }
}

fn update_state_interval(
    state: &mut OpenSessionState,
    car_id: i64,
    phase: VehiclePhase,
    at_ms: i64,
    delta: &mut LifecycleDelta,
) -> Result<(), LifecycleError> {
    let Some(target) = logical_state(phase) else {
        return Ok(());
    };
    if let Some(open) = state.open_state.as_ref()
        && open.state == target
    {
        delta.states.push(ProjectionState {
            id: open.id,
            car_id,
            state: open.state.clone(),
            start_date_ms: open.start_date_ms,
            end_date_ms: None,
        });
        return Ok(());
    }

    if let Some(open) = state.open_state.take() {
        if at_ms < open.start_date_ms {
            return Err(LifecycleError::InvalidTimeline);
        }
        delta.states.push(ProjectionState {
            id: open.id,
            car_id,
            state: open.state,
            start_date_ms: open.start_date_ms,
            end_date_ms: Some(at_ms),
        });
    }

    let id = state.next_state_id;
    state.next_state_id = state
        .next_state_id
        .checked_add(1)
        .ok_or(LifecycleError::IdentifierExhausted)?;
    state.open_state = Some(OpenState {
        id,
        car_id,
        state: target.to_owned(),
        start_date_ms: at_ms,
    });
    delta.states.push(ProjectionState {
        id,
        car_id,
        state: target.to_owned(),
        start_date_ms: at_ms,
        end_date_ms: None,
    });
    Ok(())
}

fn update_software_update(
    state: &mut OpenSessionState,
    car_id: i64,
    sample: &ParsedSample,
    previous_firmware_version: Option<&str>,
    delta: &mut LifecycleDelta,
) -> Result<(), LifecycleError> {
    // TeslaMate removes an update when the vehicle reports that it is
    // available again: installation was cancelled, not completed.
    if sample
        .software_update_status
        .as_deref()
        .is_some_and(|status| status.eq_ignore_ascii_case("available"))
    {
        state.open_update = None;
        return Ok(());
    }

    if sample.installing_update {
        if state.open_update.is_none() {
            let id = state.next_update_id;
            state.next_update_id = state
                .next_update_id
                .checked_add(1)
                .ok_or(LifecycleError::IdentifierExhausted)?;
            state.open_update = Some(OpenUpdate {
                id,
                car_id,
                start_date_ms: sample.vehicle_timestamp_ms,
            });
        }
        return Ok(());
    }

    if state.open_update.is_none()
        && sample.vehicle_state_present
        && sample.vehicle_state_is_online
        && let Some(version) = sample.car_version.as_deref()
        && previous_firmware_version
            .is_some_and(|previous| firmware_version_is_newer(version, previous))
    {
        let id = state.next_update_id;
        state.next_update_id = state
            .next_update_id
            .checked_add(1)
            .ok_or(LifecycleError::IdentifierExhausted)?;
        delta.updates.push(ProjectionUpdate {
            id,
            car_id,
            start_date_ms: sample.vehicle_timestamp_ms,
            end_date_ms: sample.vehicle_timestamp_ms,
            version: version.to_owned(),
        });
        return Ok(());
    }

    let Some(_) = state.open_update.as_ref() else {
        return Ok(());
    };
    if sample.vehicle_state_present
        && sample.phase != VehiclePhase::Offline
        && sample.vehicle_state_is_online
        && sample.car_version.is_some()
    {
        let open = state.open_update.take().expect("open update was checked");
        if sample.vehicle_timestamp_ms < open.start_date_ms {
            return Err(LifecycleError::InvalidTimeline);
        }
        delta.updates.push(ProjectionUpdate {
            id: open.id,
            car_id: open.car_id,
            start_date_ms: open.start_date_ms,
            end_date_ms: sample.vehicle_timestamp_ms,
            version: sample.car_version.clone().expect("checked above"),
        });
    }
    Ok(())
}

fn firmware_version_is_newer(candidate: &str, previous: &str) -> bool {
    let candidate = normalized_firmware_version(candidate);
    let previous = normalized_firmware_version(previous);
    candidate > previous
}

fn normalized_firmware_version(version: &str) -> Vec<u64> {
    version
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

fn object_field<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a Map<String, Value>> {
    map.get(key).and_then(Value::as_object)
}

fn int_field(map: &Map<String, Value>, key: &str) -> Option<i64> {
    map.get(key).and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_f64().map(|number| number.round() as i64))
    })
}

fn float_field(map: &Map<String, Value>, key: &str) -> Option<f64> {
    map.get(key).and_then(Value::as_f64)
}

fn bool_field(map: &Map<String, Value>, key: &str) -> Option<bool> {
    map.get(key).and_then(Value::as_bool)
}

fn miles_to_km(miles: f64) -> f64 {
    miles * 1.609_344
}

fn miles_to_km_rounded(miles: f64, precision: u32) -> f64 {
    round_to_precision(miles_to_km(miles), precision)
}

fn round_to_precision(value: f64, precision: u32) -> f64 {
    let factor = 10_f64.powi(i32::try_from(precision).expect("small conversion precision"));
    (value * factor).round() / factor
}

fn mph_to_kmh(mph: f64) -> i64 {
    (mph / 0.621_371_192_237_33).round() as i64
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LifecycleError {
    #[error("lifecycle car id must be positive")]
    InvalidCarId,
    #[error("offline drive timeout must be positive")]
    InvalidOfflineDriveTimeout,
    #[error("lifecycle observation payload is not a valid owner_api_vehicle_data_v1 object")]
    InvalidPayload,
    #[error("lifecycle coordinates are outside the WGS84 domain")]
    InvalidCoordinates,
    #[error("lifecycle end time precedes start time")]
    InvalidTimeline,
    #[error("lifecycle local identifier space is exhausted")]
    IdentifierExhausted,
    #[error("open session exceeds {maximum} bytes ({actual})")]
    SessionTooLarge { actual: usize, maximum: usize },
    #[error("cannot encode open session state")]
    SessionEncode,
    #[error("cannot decode open session state")]
    SessionDecode,
    #[error("unsupported open session version {0}")]
    UnsupportedSessionVersion(u32),
    #[error("open session state is corrupt")]
    CorruptSession,
    #[error("imported open session is invalid")]
    InvalidImportedSession,
}

#[cfg(test)]
mod thermal_flag_tests {
    use super::*;
    use serde_json::json;

    fn sample(id: i64, at_ms: i64, vehicle_data: Value) -> LifecycleSample {
        LifecycleSample {
            observation_id: id,
            observed_at_ms: at_ms,
            vehicle_state: "online".to_owned(),
            payload: json!({
                "record_type": "owner_api_vehicle_data_v1",
                "source_vehicle_id": "thermal-test",
                "vehicle_data": vehicle_data,
            }),
        }
    }

    fn charging_data(at_ms: i64, charge_state: Value, climate_state: Value) -> Value {
        let mut charge = json!({
            "charging_state": "Charging",
            "timestamp": at_ms,
            "battery_level": 50,
            "charge_energy_added": 1.0
        });
        if let (Some(base), Some(extra)) = (charge.as_object_mut(), charge_state.as_object()) {
            for (key, value) in extra {
                base.insert(key.clone(), value.clone());
            }
        }
        json!({
            "charge_state": charge,
            "climate_state": climate_state,
            "drive_state": {"shift_state": "P", "speed": 0}
        })
    }

    #[test]
    fn conflicting_charge_thermal_flags_stay_distinct() {
        let first = apply_sample(
            OpenSessionState::new(),
            1,
            &sample(
                1,
                1_900_000_000_000,
                charging_data(1_900_000_000_000, json!({}), json!({})),
            ),
        )
        .expect("open charging session");
        let second = apply_sample(
            first.state,
            1,
            &sample(
                2,
                1_900_000_001_000,
                charging_data(
                    1_900_000_001_000,
                    json!({
                        "battery_heater_on": true,
                        "not_enough_power_to_heat": false
                    }),
                    json!({
                        "battery_heater": false,
                        "battery_heater_no_power": true
                    }),
                ),
            ),
        )
        .expect("append charging sample");
        let third = apply_sample(
            second.state,
            1,
            &sample(
                3,
                1_900_000_002_000,
                charging_data(
                    1_900_000_002_000,
                    json!({"charging_state": "Complete"}),
                    json!({}),
                ),
            ),
        )
        .expect("close charging session");

        let charge = &third.delta.charge_samples[1];
        assert_eq!(charge.battery_heater_on, Some(true));
        assert_eq!(charge.battery_heater, Some(false));
        assert_eq!(charge.battery_heater_no_power, Some(true));
        assert_eq!(charge.not_enough_power_to_heat, Some(false));
    }

    #[test]
    fn missing_charge_thermal_flags_remain_none() {
        let first = apply_sample(
            OpenSessionState::new(),
            1,
            &sample(
                1,
                1_900_000_000_000,
                charging_data(1_900_000_000_000, json!({}), json!({})),
            ),
        )
        .expect("open charging session");
        let second = apply_sample(
            first.state,
            1,
            &sample(
                2,
                1_900_000_001_000,
                charging_data(1_900_000_001_000, json!({}), json!({})),
            ),
        )
        .expect("append charging sample");
        let third = apply_sample(
            second.state,
            1,
            &sample(
                3,
                1_900_000_002_000,
                charging_data(
                    1_900_000_002_000,
                    json!({"charging_state": "Complete"}),
                    json!({}),
                ),
            ),
        )
        .expect("close charging session");

        let charge = &third.delta.charge_samples[1];
        assert_eq!(charge.battery_heater_on, None);
        assert_eq!(charge.battery_heater, None);
        assert_eq!(charge.battery_heater_no_power, None);
        assert_eq!(charge.not_enough_power_to_heat, None);
    }
}
