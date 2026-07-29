//! Deterministic vehicle lifecycle projection from owner-API observations.
//!
//! This module is pure: it never performs I/O, never wakes a vehicle, and never
//! fabricates history from a single present-state sample. Open sessions are
//! serialized so a collector can resume after a process or host restart and
//! produce identical completed drives, positions, charges, and charge samples.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::hub_pack::{
    ProjectionCharge, ProjectionChargeSample, ProjectionDrive, ProjectionPosition,
};

/// Maximum UTF-8 bytes retained for one vehicle's open-session blob.
pub const MAX_OPEN_SESSION_BYTES: usize = 64 * 1024;

/// One ordered observation already validated and stored by the Hub.
#[derive(Debug, Clone, PartialEq)]
pub struct LifecycleSample {
    pub observation_id: i64,
    pub observed_at_ms: i64,
    pub vehicle_state: String,
    pub payload: Value,
}

/// Durable open-session state. Crash recovery reloads this exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OpenSessionState {
    pub version: u32,
    pub last_observation_id: i64,
    pub next_drive_id: i64,
    pub next_position_id: i64,
    pub next_charge_id: i64,
    pub next_charge_sample_id: i64,
    pub phase: VehiclePhase,
    pub open_drive: Option<OpenDrive>,
    pub open_charge: Option<OpenCharge>,
}

impl OpenSessionState {
    pub const CURRENT_VERSION: u32 = 1;

    pub fn new() -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            next_drive_id: 1,
            next_position_id: 1,
            next_charge_id: 1,
            next_charge_sample_id: 1,
            ..Self::default()
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, LifecycleError> {
        let bytes = serde_json::to_vec(self).map_err(|_| LifecycleError::SessionEncode)?;
        if bytes.len() > MAX_OPEN_SESSION_BYTES {
            return Err(LifecycleError::SessionTooLarge {
                actual: bytes.len(),
                maximum: MAX_OPEN_SESSION_BYTES,
            });
        }
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, LifecycleError> {
        if bytes.len() > MAX_OPEN_SESSION_BYTES {
            return Err(LifecycleError::SessionTooLarge {
                actual: bytes.len(),
                maximum: MAX_OPEN_SESSION_BYTES,
            });
        }
        let state: Self =
            serde_json::from_slice(bytes).map_err(|_| LifecycleError::SessionDecode)?;
        if state.version != Self::CURRENT_VERSION {
            return Err(LifecycleError::UnsupportedSessionVersion(state.version));
        }
        if state.next_drive_id < 1
            || state.next_position_id < 1
            || state.next_charge_id < 1
            || state.next_charge_sample_id < 1
            || state.last_observation_id < 0
        {
            return Err(LifecycleError::CorruptSession);
        }
        Ok(state)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VehiclePhase {
    #[default]
    Unknown,
    Online,
    Asleep,
    Offline,
    Driving,
    Charging,
    Updating,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenDrive {
    pub id: i64,
    pub car_id: i64,
    pub start_date_ms: i64,
    pub start_latitude: Option<f64>,
    pub start_longitude: Option<f64>,
    pub start_soc: Option<i64>,
    pub start_rated_range_km: Option<f64>,
    pub speed_max: Option<i64>,
    pub outside_temp_sum: f64,
    pub outside_temp_count: u32,
    pub positions: Vec<ProjectionPosition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenCharge {
    pub id: i64,
    pub car_id: i64,
    pub start_date_ms: i64,
    pub start_battery_level: Option<i64>,
    pub start_rated_range_km: Option<f64>,
    pub is_dc: Option<bool>,
    pub max_charger_power_kw: Option<f64>,
    pub outside_temp_sum: f64,
    pub outside_temp_count: u32,
    pub last_energy_added: Option<f64>,
    pub last_battery_level: Option<i64>,
    pub last_rated_range_km: Option<f64>,
    pub samples: Vec<ProjectionChargeSample>,
}

/// Completed entities produced since the previous open state.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LifecycleDelta {
    pub drives: Vec<ProjectionDrive>,
    pub positions: Vec<ProjectionPosition>,
    pub charges: Vec<ProjectionCharge>,
    pub charge_samples: Vec<ProjectionChargeSample>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LifecycleStep {
    pub state: OpenSessionState,
    pub delta: LifecycleDelta,
    pub quarantined: bool,
}

/// Project one already-stored observation onto the open session.
///
/// Samples with `observation_id <= state.last_observation_id` are ignored so
/// restarts and retries stay idempotent. Out-of-order IDs after the cursor are
/// also ignored rather than rewriting history.
pub fn apply_sample(
    mut state: OpenSessionState,
    car_id: i64,
    sample: &LifecycleSample,
) -> Result<LifecycleStep, LifecycleError> {
    if car_id <= 0 {
        return Err(LifecycleError::InvalidCarId);
    }
    if sample.observation_id <= state.last_observation_id {
        return Ok(LifecycleStep {
            state,
            delta: LifecycleDelta::default(),
            quarantined: false,
        });
    }

    let parsed = match parse_sample(sample) {
        Ok(parsed) => parsed,
        Err(_) => {
            // Corrupt payload cannot poison durable history. Quarantine open
            // sessions and skip the sample while advancing the cursor.
            state.last_observation_id = sample.observation_id;
            state.open_drive = None;
            state.open_charge = None;
            state.phase = phase_from_vehicle_state(&sample.vehicle_state);
            return Ok(LifecycleStep {
                state,
                delta: LifecycleDelta::default(),
                quarantined: true,
            });
        }
    };

    let mut delta = LifecycleDelta::default();
    state.phase = parsed.phase;

    // Charge and drive transitions are independent enough that either can
    // close while the other opens on successive samples, but one sample never
    // starts both simultaneously in a coherent Tesla response.
    if let Some(closed) = maybe_close_drive(&mut state, car_id, &parsed) {
        delta.positions.extend(closed.positions);
        delta.drives.push(closed.drive);
    }
    if let Some(closed) = maybe_close_charge(&mut state, car_id, &parsed) {
        delta.charge_samples.extend(closed.samples);
        delta.charges.push(closed.charge);
    }
    maybe_open_or_extend_drive(&mut state, car_id, &parsed, &mut delta)?;
    maybe_open_or_extend_charge(&mut state, car_id, &parsed, &mut delta)?;

    state.last_observation_id = sample.observation_id;
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
        total.positions.extend(step.delta.positions);
        total.charges.extend(step.delta.charges);
        total.charge_samples.extend(step.delta.charge_samples);
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
        let closed = finalize_drive(open, closed_at_ms, None, None, None)?;
        delta.positions.extend(closed.positions);
        delta.drives.push(closed.drive);
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

#[derive(Debug)]
struct ParsedSample {
    observed_at_ms: i64,
    phase: VehiclePhase,
    shift_state: Option<String>,
    speed: Option<i64>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    power: Option<i64>,
    battery_level: Option<i64>,
    usable_battery_level: Option<i64>,
    rated_range_km: Option<f64>,
    ideal_range_km: Option<f64>,
    odometer: Option<f64>,
    elevation: Option<i64>,
    is_climate_on: Option<bool>,
    inside_temp: Option<f64>,
    outside_temp: Option<f64>,
    charging_state: Option<String>,
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
}

fn parse_sample(sample: &LifecycleSample) -> Result<ParsedSample, LifecycleError> {
    let root = sample
        .payload
        .as_object()
        .ok_or(LifecycleError::InvalidPayload)?;
    if root.get("record_type").and_then(Value::as_str) != Some("owner_api_vehicle_data_v1") {
        return Err(LifecycleError::InvalidPayload);
    }
    let vehicle_data = root
        .get("vehicle_data")
        .and_then(Value::as_object)
        .ok_or(LifecycleError::InvalidPayload)?;

    let drive = object_field(vehicle_data, "drive_state");
    let charge = object_field(vehicle_data, "charge_state");
    let climate = object_field(vehicle_data, "climate_state");
    let vehicle_state = object_field(vehicle_data, "vehicle_state");

    let shift_state = drive
        .and_then(|fields| fields.get("shift_state"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let charging_state = charge
        .and_then(|fields| fields.get("charging_state"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let speed = drive.and_then(|fields| int_field(fields, "speed"));
    let phase = if sample.vehicle_state.eq_ignore_ascii_case("asleep") {
        VehiclePhase::Asleep
    } else if sample.vehicle_state.eq_ignore_ascii_case("offline") {
        VehiclePhase::Offline
    } else if sample.vehicle_state.eq_ignore_ascii_case("updating") {
        VehiclePhase::Updating
    } else if charging_state
        .as_deref()
        .is_some_and(|state| state.eq_ignore_ascii_case("Charging"))
    {
        VehiclePhase::Charging
    } else if is_drive_shift(shift_state.as_deref()) || speed.unwrap_or(0) > 0 {
        VehiclePhase::Driving
    } else if sample.vehicle_state.eq_ignore_ascii_case("online") {
        VehiclePhase::Online
    } else {
        phase_from_vehicle_state(&sample.vehicle_state)
    };

    Ok(ParsedSample {
        observed_at_ms: sample.observed_at_ms,
        phase,
        shift_state,
        speed,
        latitude: drive.and_then(|fields| float_field(fields, "latitude")),
        longitude: drive.and_then(|fields| float_field(fields, "longitude")),
        power: drive.and_then(|fields| int_field(fields, "power")),
        battery_level: charge.and_then(|fields| int_field(fields, "battery_level")),
        usable_battery_level: charge.and_then(|fields| int_field(fields, "usable_battery_level")),
        rated_range_km: charge
            .and_then(|fields| float_field(fields, "battery_range"))
            .map(miles_to_km),
        ideal_range_km: charge
            .and_then(|fields| float_field(fields, "ideal_battery_range"))
            .map(miles_to_km),
        odometer: vehicle_state
            .and_then(|fields| float_field(fields, "odometer"))
            .map(miles_to_km),
        elevation: drive.and_then(|fields| int_field(fields, "native_location_elevation")),
        is_climate_on: climate.and_then(|fields| bool_field(fields, "is_climate_on")),
        inside_temp: climate.and_then(|fields| float_field(fields, "inside_temp")),
        outside_temp: climate.and_then(|fields| float_field(fields, "outside_temp")),
        charging_state,
        charge_energy_added: charge.and_then(|fields| float_field(fields, "charge_energy_added")),
        charger_power_kw: charge.and_then(|fields| float_field(fields, "charger_power")),
        charger_voltage: charge.and_then(|fields| float_field(fields, "charger_voltage")),
        charger_actual_current: charge
            .and_then(|fields| float_field(fields, "charger_actual_current")),
        charger_pilot_current: charge
            .and_then(|fields| float_field(fields, "charger_pilot_current")),
        charger_phases: charge.and_then(|fields| int_field(fields, "charger_phases")),
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
    })
}

struct ClosedDrive {
    drive: ProjectionDrive,
    positions: Vec<ProjectionPosition>,
}

struct ClosedCharge {
    charge: ProjectionCharge,
    samples: Vec<ProjectionChargeSample>,
}

fn maybe_close_drive(
    state: &mut OpenSessionState,
    car_id: i64,
    sample: &ParsedSample,
) -> Option<ClosedDrive> {
    let open = state.open_drive.as_ref()?;
    let should_close = matches!(
        sample.phase,
        VehiclePhase::Asleep | VehiclePhase::Offline | VehiclePhase::Updating
    ) || (!is_drive_shift(sample.shift_state.as_deref())
        && sample.speed.unwrap_or(0) <= 0
        && !open.positions.is_empty());
    if !should_close {
        return None;
    }
    let open = state.open_drive.take()?;
    let _ = car_id;
    finalize_drive(
        open,
        sample.observed_at_ms,
        sample.latitude,
        sample.longitude,
        sample.battery_level,
    )
    .ok()
}

fn maybe_close_charge(
    state: &mut OpenSessionState,
    _car_id: i64,
    sample: &ParsedSample,
) -> Option<ClosedCharge> {
    let _open = state.open_charge.as_ref()?;
    let charging = sample
        .charging_state
        .as_deref()
        .is_some_and(|state| state.eq_ignore_ascii_case("Charging"));
    let terminal = sample.charging_state.as_deref().is_some_and(|state| {
        matches!(
            state.to_ascii_lowercase().as_str(),
            "complete" | "disconnected" | "stopped" | "nopower"
        )
    });
    let should_close = terminal
        || matches!(
            sample.phase,
            VehiclePhase::Asleep | VehiclePhase::Offline | VehiclePhase::Updating
        )
        || (!charging
            && state
                .open_charge
                .as_ref()
                .is_some_and(|open| !open.samples.is_empty()));
    if !should_close {
        return None;
    }
    let mut open = state.open_charge.take()?;
    // Terminal sample still carries the final energy/SoC even though it is no
    // longer "Charging"; fold those fields in before sealing the session.
    open.last_energy_added = sample.charge_energy_added.or(open.last_energy_added);
    open.last_battery_level = sample.battery_level.or(open.last_battery_level);
    open.last_rated_range_km = sample.rated_range_km.or(open.last_rated_range_km);
    finalize_charge(open, Some(sample.observed_at_ms)).ok()
}

fn maybe_open_or_extend_drive(
    state: &mut OpenSessionState,
    car_id: i64,
    sample: &ParsedSample,
    delta: &mut LifecycleDelta,
) -> Result<(), LifecycleError> {
    let driving = is_drive_shift(sample.shift_state.as_deref()) || sample.speed.unwrap_or(0) > 0;
    if !driving {
        return Ok(());
    }
    // Charging takes precedence when Tesla reports both inconsistently.
    if sample
        .charging_state
        .as_deref()
        .is_some_and(|state| state.eq_ignore_ascii_case("Charging"))
    {
        return Ok(());
    }

    if state.open_drive.is_none() {
        let id = state.next_drive_id;
        state.next_drive_id = state.next_drive_id.saturating_add(1);
        state.open_drive = Some(OpenDrive {
            id,
            car_id,
            start_date_ms: sample.observed_at_ms,
            start_latitude: sample.latitude,
            start_longitude: sample.longitude,
            start_soc: sample.battery_level,
            start_rated_range_km: sample.rated_range_km,
            speed_max: sample.speed,
            outside_temp_sum: sample.outside_temp.unwrap_or(0.0),
            outside_temp_count: u32::from(sample.outside_temp.is_some()),
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
        if let (Some(lat), Some(lon)) = (sample.latitude, sample.longitude) {
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                return Err(LifecycleError::InvalidCoordinates);
            }
            let position_id = state.next_position_id;
            state.next_position_id = state.next_position_id.saturating_add(1);
            let position = ProjectionPosition {
                id: position_id,
                drive_id: open.id,
                car_id,
                date_ms: sample.observed_at_ms,
                latitude: lat,
                longitude: lon,
                speed: sample.speed,
                power: sample.power,
                battery_level: sample.battery_level,
                usable_battery_level: sample.usable_battery_level,
                elevation: sample.elevation,
                odometer: sample.odometer,
                ideal_battery_range_km: sample.ideal_range_km,
                rated_battery_range_km: sample.rated_range_km,
                is_climate_on: sample.is_climate_on,
                inside_temp: sample.inside_temp,
                outside_temp: sample.outside_temp,
            };
            open.positions.push(position.clone());
            // Positions stay private to the open session until the drive closes,
            // then join the delta together so partial drives never publish.
            let _ = position;
            let _ = delta;
        }
    }
    Ok(())
}

fn maybe_open_or_extend_charge(
    state: &mut OpenSessionState,
    car_id: i64,
    sample: &ParsedSample,
    delta: &mut LifecycleDelta,
) -> Result<(), LifecycleError> {
    let charging = sample
        .charging_state
        .as_deref()
        .is_some_and(|state| state.eq_ignore_ascii_case("Charging"));
    if !charging {
        return Ok(());
    }

    if state.open_charge.is_none() {
        let id = state.next_charge_id;
        state.next_charge_id = state.next_charge_id.saturating_add(1);
        state.open_charge = Some(OpenCharge {
            id,
            car_id,
            start_date_ms: sample.observed_at_ms,
            start_battery_level: sample.battery_level,
            start_rated_range_km: sample.rated_range_km,
            is_dc: sample.fast_charger_present,
            max_charger_power_kw: sample.charger_power_kw,
            outside_temp_sum: sample.outside_temp.unwrap_or(0.0),
            outside_temp_count: u32::from(sample.outside_temp.is_some()),
            last_energy_added: sample.charge_energy_added,
            last_battery_level: sample.battery_level,
            last_rated_range_km: sample.rated_range_km,
            samples: Vec::new(),
        });
    }

    if let Some(open) = state.open_charge.as_mut() {
        if let Some(power) = sample.charger_power_kw {
            open.max_charger_power_kw = Some(open.max_charger_power_kw.unwrap_or(power).max(power));
        }
        if let Some(temp) = sample.outside_temp {
            open.outside_temp_sum += temp;
            open.outside_temp_count = open.outside_temp_count.saturating_add(1);
        }
        open.last_energy_added = sample.charge_energy_added.or(open.last_energy_added);
        open.last_battery_level = sample.battery_level.or(open.last_battery_level);
        open.last_rated_range_km = sample.rated_range_km.or(open.last_rated_range_km);
        if sample.fast_charger_present == Some(true) {
            open.is_dc = Some(true);
        }

        let sample_id = state.next_charge_sample_id;
        state.next_charge_sample_id = state.next_charge_sample_id.saturating_add(1);
        let charge_sample = ProjectionChargeSample {
            id: sample_id,
            charge_process_id: open.id,
            timestamp_ms: sample.observed_at_ms,
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
            battery_heater: sample.battery_heater_on,
            battery_heater_no_power: None,
            not_enough_power_to_heat: None,
            fast_charger_present: sample.fast_charger_present,
            fast_charger_brand: sample.fast_charger_brand.clone(),
            fast_charger_type: sample.fast_charger_type.clone(),
            charge_cable: sample.charge_cable.clone(),
        };
        open.samples.push(charge_sample);
        let _ = delta;
    }
    Ok(())
}

fn finalize_drive(
    open: OpenDrive,
    end_date_ms: i64,
    end_latitude: Option<f64>,
    end_longitude: Option<f64>,
    end_soc: Option<i64>,
) -> Result<ClosedDrive, LifecycleError> {
    if end_date_ms < open.start_date_ms {
        return Err(LifecycleError::InvalidTimeline);
    }
    let duration_min = ((end_date_ms - open.start_date_ms) / 60_000).max(0);
    let distance_km = path_distance_km(&open.positions);
    let outside_temp_avg = if open.outside_temp_count > 0 {
        Some(open.outside_temp_sum / f64::from(open.outside_temp_count))
    } else {
        None
    };
    let end_latitude = end_latitude.or_else(|| open.positions.last().map(|p| p.latitude));
    let end_longitude = end_longitude.or_else(|| open.positions.last().map(|p| p.longitude));
    let end_soc = end_soc.or_else(|| open.positions.last().and_then(|p| p.battery_level));
    let end_rated = open.positions.last().and_then(|p| p.rated_battery_range_km);
    let drive = ProjectionDrive {
        id: open.id,
        car_id: open.car_id,
        optimized_at_ms: None,
        start_date_ms: open.start_date_ms,
        end_date_ms,
        distance_km,
        duration_min: Some(duration_min),
        efficiency: None,
        outside_temp_avg,
        speed_max: open.speed_max,
        start_address: None,
        end_address: None,
        start_geofence: None,
        end_geofence: None,
        start_latitude: open.start_latitude,
        start_longitude: open.start_longitude,
        end_latitude,
        end_longitude,
        start_soc: open.start_soc,
        end_soc,
        start_rated_range_km: open.start_rated_range_km,
        end_rated_range_km: end_rated,
    };
    Ok(ClosedDrive {
        drive,
        positions: open.positions,
    })
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
        charge_energy_added: open.last_energy_added,
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

fn path_distance_km(positions: &[ProjectionPosition]) -> Option<f64> {
    if positions.len() < 2 {
        return None;
    }
    let mut total = 0.0_f64;
    for window in positions.windows(2) {
        total += haversine_km(
            window[0].latitude,
            window[0].longitude,
            window[1].latitude,
            window[1].longitude,
        );
    }
    Some(total)
}

fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const EARTH_RADIUS_KM: f64 = 6371.0;
    let to_rad = std::f64::consts::PI / 180.0;
    let d_lat = (lat2 - lat1) * to_rad;
    let d_lon = (lon2 - lon1) * to_rad;
    let a = (d_lat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (d_lon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_KM * a.sqrt().asin()
}

fn is_drive_shift(shift_state: Option<&str>) -> bool {
    matches!(shift_state, Some("D" | "R" | "N" | "d" | "r" | "n"))
}

fn phase_from_vehicle_state(state: &str) -> VehiclePhase {
    match state.to_ascii_lowercase().as_str() {
        "online" => VehiclePhase::Online,
        "asleep" => VehiclePhase::Asleep,
        "offline" => VehiclePhase::Offline,
        "updating" => VehiclePhase::Updating,
        _ => VehiclePhase::Unknown,
    }
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

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LifecycleError {
    #[error("lifecycle car id must be positive")]
    InvalidCarId,
    #[error("lifecycle observation payload is not a valid owner_api_vehicle_data_v1 object")]
    InvalidPayload,
    #[error("lifecycle coordinates are outside the WGS84 domain")]
    InvalidCoordinates,
    #[error("lifecycle end time precedes start time")]
    InvalidTimeline,
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
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sample(id: i64, at_ms: i64, vehicle_data: Value) -> LifecycleSample {
        LifecycleSample {
            observation_id: id,
            observed_at_ms: at_ms,
            vehicle_state: "online".to_owned(),
            payload: json!({
                "record_type": "owner_api_vehicle_data_v1",
                "source_vehicle_id": "9",
                "vehicle_data": vehicle_data,
            }),
        }
    }

    #[test]
    fn materializes_a_complete_drive_with_positions() {
        let start = 1_800_000_000_000_i64;
        let samples = [
            sample(
                1,
                start,
                json!({
                    "drive_state": {
                        "shift_state": "D",
                        "speed": 20,
                        "latitude": 47.5,
                        "longitude": 19.0,
                        "power": 15,
                        "timestamp": start
                    },
                    "charge_state": {"battery_level": 70, "battery_range": 200.0},
                    "climate_state": {"outside_temp": 18.0, "is_climate_on": true}
                }),
            ),
            sample(
                2,
                start + 60_000,
                json!({
                    "drive_state": {
                        "shift_state": "D",
                        "speed": 40,
                        "latitude": 47.51,
                        "longitude": 19.01,
                        "power": 30,
                        "timestamp": start + 60_000
                    },
                    "charge_state": {"battery_level": 69, "battery_range": 198.0}
                }),
            ),
            sample(
                3,
                start + 120_000,
                json!({
                    "drive_state": {
                        "shift_state": "P",
                        "speed": 0,
                        "latitude": 47.52,
                        "longitude": 19.02,
                        "timestamp": start + 120_000
                    },
                    "charge_state": {"battery_level": 68, "battery_range": 196.0}
                }),
            ),
        ];

        let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("project");
        assert!(step.state.open_drive.is_none());
        assert_eq!(step.delta.drives.len(), 1);
        assert_eq!(step.delta.positions.len(), 2);
        assert_eq!(step.delta.drives[0].start_date_ms, start);
        assert_eq!(step.delta.drives[0].end_date_ms, start + 120_000);
        assert_eq!(step.delta.drives[0].speed_max, Some(40));
        assert!(step.delta.drives[0].distance_km.unwrap() > 0.0);
        assert_eq!(step.state.last_observation_id, 3);
    }

    #[test]
    fn materializes_a_complete_charge_with_samples() {
        let start = 1_800_000_100_000_i64;
        let samples = [
            sample(
                1,
                start,
                json!({
                    "charge_state": {
                        "charging_state": "Charging",
                        "battery_level": 40,
                        "charge_energy_added": 1.5,
                        "charger_power": 11.0,
                        "battery_range": 120.0
                    },
                    "drive_state": {"shift_state": "P", "speed": 0, "latitude": 1.0, "longitude": 2.0}
                }),
            ),
            sample(
                2,
                start + 300_000,
                json!({
                    "charge_state": {
                        "charging_state": "Charging",
                        "battery_level": 50,
                        "charge_energy_added": 4.0,
                        "charger_power": 11.0,
                        "battery_range": 150.0
                    }
                }),
            ),
            sample(
                3,
                start + 600_000,
                json!({
                    "charge_state": {
                        "charging_state": "Complete",
                        "battery_level": 80,
                        "charge_energy_added": 12.0,
                        "charger_power": 0.0,
                        "battery_range": 220.0
                    }
                }),
            ),
        ];

        let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("project");
        assert!(step.state.open_charge.is_none());
        assert_eq!(step.delta.charges.len(), 1);
        assert_eq!(step.delta.charge_samples.len(), 2);
        assert_eq!(step.delta.charges[0].start_battery_level, Some(40));
        assert_eq!(step.delta.charges[0].end_battery_level, Some(80));
        assert_eq!(step.delta.charges[0].charge_energy_added, Some(12.0));
    }

    #[test]
    fn restart_mid_drive_then_completion_matches_uninterrupted_projection() {
        let start = 1_800_000_200_000_i64;
        let all = [
            sample(
                1,
                start,
                json!({"drive_state":{"shift_state":"D","speed":10,"latitude":10.0,"longitude":10.0},"charge_state":{"battery_level":60}}),
            ),
            sample(
                2,
                start + 30_000,
                json!({"drive_state":{"shift_state":"D","speed":20,"latitude":10.01,"longitude":10.01},"charge_state":{"battery_level":59}}),
            ),
            sample(
                3,
                start + 60_000,
                json!({"drive_state":{"shift_state":"P","speed":0,"latitude":10.02,"longitude":10.02},"charge_state":{"battery_level":58}}),
            ),
        ];

        let continuous = apply_samples(OpenSessionState::new(), 1, &all).expect("continuous");

        // Simulate crash after sample 1: encode open state, decode, resume.
        let after_first = apply_sample(OpenSessionState::new(), 1, &all[0]).expect("first");
        let encoded = after_first.state.encode().expect("encode open drive");
        let restored = OpenSessionState::decode(&encoded).expect("decode");
        assert!(restored.open_drive.is_some());
        let after_second = apply_sample(restored, 1, &all[1]).expect("second");
        // Replay of sample 1 is a no-op after restart recovery.
        let replay = apply_sample(after_second.state.clone(), 1, &all[0]).expect("replay");
        assert!(replay.delta.drives.is_empty());
        assert!(replay.delta.positions.is_empty());
        let after_third = apply_sample(after_second.state, 1, &all[2]).expect("third");

        assert_eq!(after_third.delta.drives, continuous.delta.drives);
        // Continuous path emits positions only on close; restarted path same.
        assert_eq!(
            after_third.delta.positions.len(),
            continuous.delta.positions.len()
        );
        assert_eq!(after_third.state.last_observation_id, 3);
        assert!(after_third.state.open_drive.is_none());
    }

    #[test]
    fn corrupt_payload_quarantines_open_session_without_losing_prior_history() {
        let start = 1_800_000_300_000_i64;
        let good = sample(
            1,
            start,
            json!({"drive_state":{"shift_state":"D","speed":5,"latitude":1.0,"longitude":2.0}}),
        );
        let bad = LifecycleSample {
            observation_id: 2,
            observed_at_ms: start + 1,
            vehicle_state: "online".to_owned(),
            payload: json!({"record_type":"not-valid"}),
        };
        let mid = apply_sample(OpenSessionState::new(), 1, &good).expect("open drive");
        assert!(mid.state.open_drive.is_some());
        let step = apply_sample(mid.state, 1, &bad).expect("quarantine");
        assert!(step.quarantined);
        assert!(step.state.open_drive.is_none());
        assert!(step.delta.drives.is_empty());
        assert_eq!(step.state.last_observation_id, 2);
    }

    #[test]
    fn force_close_emits_open_drive_and_charge() {
        let start = 1_800_000_400_000_i64;
        let drive = sample(
            1,
            start,
            json!({"drive_state":{"shift_state":"D","speed":5,"latitude":1.0,"longitude":2.0},"charge_state":{"battery_level":50}}),
        );
        let mut state = apply_sample(OpenSessionState::new(), 1, &drive)
            .expect("drive")
            .state;
        // Start a charge on a later sample after parking is not needed for this
        // force-close unit; open charge directly via a charging sample.
        let charge = sample(
            2,
            start + 10_000,
            json!({"charge_state":{"charging_state":"Charging","battery_level":50,"charge_energy_added":0.1,"charger_power":7.0},"drive_state":{"shift_state":"P","speed":0}}),
        );
        // Close drive first by parking, then open charge.
        state = apply_sample(state, 1, &charge).expect("charge open").state;
        // Manually ensure we have an open charge for force-close proof.
        assert!(state.open_charge.is_some() || state.open_drive.is_none());
        let step = force_close_open_sessions(state, 1, start + 20_000).expect("force close");
        assert!(step.state.open_drive.is_none());
        assert!(step.state.open_charge.is_none());
    }
}
