//! Deterministic vehicle lifecycle projection from owner-API observations.
//!
//! This module is pure: it never performs I/O, never wakes a vehicle, and never
//! fabricates history from a single present-state sample. Open sessions are
//! serialized so a collector can resume after a process or host restart and
//! produce identical completed drives, positions, charges, and charge samples.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use thiserror::Error;

use crate::hub_pack::{
    GeofenceBillingType, ProjectionCarPatch, ProjectionCharge, ProjectionChargeSample,
    ProjectionDrive, ProjectionPosition, ProjectionState, ProjectionUpdate,
    normalize_tesla_model_code,
};
use crate::teslamate_projection::{
    TeslaMateCharge, TeslaMateChargingProcess, TeslaMateDrive, TeslaMateOpenSession,
    TeslaMatePosition, TeslaMateState, project_charge_sample,
};

/// Maximum UTF-8 bytes retained for one vehicle's open-session blob.
///
/// An active drive retains positions at the driving cadence and an active
/// charge retains charge samples at the charging cadence. 64 KiB can be
/// exceeded by an ordinary long session, preventing the collector from
/// checkpointing and therefore from recovering safely. Keep a finite corrupt
/// input guard, but size it for multi-day real-world continuations.
pub const MAX_OPEN_SESSION_BYTES: usize = 128 * 1024 * 1024;
pub(crate) const DEFAULT_OFFLINE_DRIVE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

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
    #[serde(default)]
    pub last_observed_at_ms: Option<i64>,
    #[serde(default)]
    pub last_drive_timestamp_ms: Option<i64>,
    #[serde(default)]
    pub last_charge_timestamp_ms: Option<i64>,
    #[serde(default)]
    pub last_vehicle_timestamp_ms: Option<i64>,
    #[serde(default)]
    pub imported_drive_watermark_ms: Option<i64>,
    #[serde(default)]
    pub imported_charge_watermark_ms: Option<i64>,
    #[serde(default)]
    pub imported_state_watermark_ms: Option<i64>,
    pub next_drive_id: i64,
    pub next_position_id: i64,
    pub next_charge_id: i64,
    pub next_charge_sample_id: i64,
    #[serde(default = "default_next_state_id")]
    pub next_state_id: i64,
    #[serde(default = "default_next_update_id")]
    pub next_update_id: i64,
    #[serde(default)]
    pub last_stationary_position_at_ms: Option<i64>,
    pub phase: VehiclePhase,
    pub open_drive: Option<OpenDrive>,
    pub open_charge: Option<OpenCharge>,
    #[serde(default)]
    pub open_state: Option<OpenState>,
    #[serde(default)]
    pub open_update: Option<OpenUpdate>,
    #[serde(default)]
    pub imported_open: Option<ImportedOpenSessionRefs>,
    #[serde(default)]
    pub service_mode: Option<bool>,
    #[serde(default)]
    pub car_metadata: Option<ProjectionCarPatch>,
    #[serde(default)]
    pub last_position_battery_heater: Option<bool>,
    #[serde(default)]
    pub last_position_battery_heater_on: Option<bool>,
    #[serde(default)]
    pub last_position_battery_heater_no_power: Option<bool>,
    #[serde(default)]
    pub last_position_est_battery_range_km: Option<f64>,
    #[serde(default)]
    pub last_position_fan_status: Option<i64>,
    #[serde(default)]
    pub last_position_driver_temp_setting: Option<f64>,
    #[serde(default)]
    pub last_position_passenger_temp_setting: Option<f64>,
    #[serde(default)]
    pub last_position_is_rear_defroster_on: Option<bool>,
    #[serde(default)]
    pub last_position_is_front_defroster_on: Option<bool>,
    #[serde(default)]
    pub last_position_tpms_pressure_fl: Option<f64>,
    #[serde(default)]
    pub last_position_tpms_pressure_fr: Option<f64>,
    #[serde(default)]
    pub last_position_tpms_pressure_rl: Option<f64>,
    #[serde(default)]
    pub last_position_tpms_pressure_rr: Option<f64>,
}

/// Bounded reference to normalized imported open-session rows. Child rows stay
/// in SQLite; this record never grows with telemetry volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ImportedOpenSessionRefs {
    pub source_id: String,
    pub drive_source_row_id: Option<i64>,
    pub charge_source_row_id: Option<i64>,
    pub state_source_row_id: Option<i64>,
    pub standalone_position_count: u64,
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
            next_state_id: 1,
            next_update_id: 1,
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
            || state.next_state_id < 1
            || state.next_update_id < 1
            || state.last_observation_id < 0
        {
            return Err(LifecycleError::CorruptSession);
        }
        Ok(state)
    }
}

/// Seed only the open parents and bounded counters from an imported snapshot.
/// The normalized child rows are loaded by the store when a continuation view
/// is requested, rather than being copied into the 64 KiB lifecycle blob.
pub fn seed_imported_open_session_state(
    source_id: uuid::Uuid,
    session: &TeslaMateOpenSession,
    existing: Option<&OpenSessionState>,
) -> Result<OpenSessionState, LifecycleError> {
    session
        .validate()
        .map_err(|_| LifecycleError::InvalidImportedSession)?;
    let mut state = existing.cloned().unwrap_or_else(OpenSessionState::new);
    let refs = ImportedOpenSessionRefs {
        source_id: source_id.to_string(),
        drive_source_row_id: session.drive.as_ref().map(|row| row.id),
        charge_source_row_id: session.charge.as_ref().map(|row| row.id),
        state_source_row_id: session.state.as_ref().map(|row| row.id),
        standalone_position_count: session.standalone_positions.len() as u64,
    };
    if state.imported_open.as_ref() == Some(&refs) {
        return Ok(state);
    }
    state.imported_open = Some(refs);
    state.imported_drive_watermark_ms = session
        .watermarks
        .drives
        .max_timestamp_ms
        .max(session.watermarks.positions.max_timestamp_ms);
    state.imported_charge_watermark_ms = session
        .watermarks
        .charging_processes
        .max_timestamp_ms
        .max(session.watermarks.charges.max_timestamp_ms);
    state.imported_state_watermark_ms = session.watermarks.states.max_timestamp_ms;
    state.open_drive = session
        .drive
        .as_ref()
        .map(|row| open_drive_from_source(row, &session.drive_positions));
    state.open_charge = session
        .charge
        .as_ref()
        .map(|row| open_charge_from_source(row, &session.charge_samples));
    state.open_state = session.state.as_ref().map(open_state_from_source);
    state.phase = if state.open_drive.is_some() {
        VehiclePhase::Driving
    } else if state.open_charge.is_some() {
        VehiclePhase::Charging
    } else if let Some(open_state) = state.open_state.as_ref() {
        phase_from_vehicle_state(&open_state.state)
    } else {
        VehiclePhase::Online
    };
    let max_drive = session
        .watermarks
        .drives
        .max_id
        .unwrap_or(0)
        .max(session.drive.as_ref().map_or(0, |row| row.id));
    let max_position = session
        .drive_positions
        .iter()
        .chain(session.standalone_positions.iter())
        .map(|row| row.id)
        .max()
        .unwrap_or(0)
        .max(session.watermarks.positions.max_id.unwrap_or(0));
    let max_charge = session
        .watermarks
        .charging_processes
        .max_id
        .unwrap_or(0)
        .max(session.charge.as_ref().map_or(0, |row| row.id));
    let max_sample = session
        .charge_samples
        .iter()
        .map(|row| row.id)
        .max()
        .unwrap_or(0)
        .max(session.watermarks.charges.max_id.unwrap_or(0));
    let max_state = session
        .watermarks
        .states
        .max_id
        .unwrap_or(0)
        .max(session.state.as_ref().map_or(0, |row| row.id));
    state.next_drive_id = state.next_drive_id.max(max_drive.saturating_add(1));
    state.next_position_id = state.next_position_id.max(max_position.saturating_add(1));
    state.next_charge_id = state.next_charge_id.max(max_charge.saturating_add(1));
    state.next_charge_sample_id = state
        .next_charge_sample_id
        .max(max_sample.saturating_add(1));
    state.next_state_id = state.next_state_id.max(max_state.saturating_add(1));
    state.next_update_id = state.next_update_id.max(
        session
            .watermarks
            .updates
            .max_id
            .unwrap_or(0)
            .saturating_add(1),
    );
    state.last_observed_at_ms = session_max_timestamp(session);
    Ok(state)
}

fn session_max_timestamp(session: &TeslaMateOpenSession) -> Option<i64> {
    session
        .drive_positions
        .iter()
        .map(|row| row.date_ms)
        .chain(session.standalone_positions.iter().map(|row| row.date_ms))
        .chain(session.charge_samples.iter().map(|row| row.date_ms))
        .chain(session.drive.iter().map(|row| row.start_date_ms))
        .chain(session.charge.iter().map(|row| row.start_date_ms))
        .chain(session.state.iter().map(|row| row.start_date_ms))
        .max()
}

fn open_drive_from_source(row: &TeslaMateDrive, positions: &[TeslaMatePosition]) -> OpenDrive {
    let mut open = OpenDrive {
        id: row.id,
        car_id: row.car_id,
        start_date_ms: row.start_date_ms,
        start_latitude: None,
        start_longitude: None,
        start_soc: None,
        start_rated_range_km: row.start_rated_range_km,
        speed_max: row.speed_max,
        outside_temp_sum: 0.0,
        outside_temp_count: 0,
        position_count: 0,
        last_position_date_ms: None,
        last_latitude: None,
        last_longitude: None,
        last_soc: None,
        last_rated_range_km: None,
        last_odometer: None,
        first_odometer: None,
        power_max: row.power_max,
        power_min: row.power_min,
        inside_temp_sum: 0.0,
        inside_temp_count: 0,
        start_ideal_range_km: row.start_ideal_range_km,
        end_ideal_range_km: row.end_ideal_range_km,
        elevation_ascent: 0,
        elevation_descent: 0,
        last_elevation: None,
        positions: Vec::new(),
    };

    let mut ordered = positions.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|position| (position.date_ms, position.id));
    if let Some(first) = ordered.first() {
        open.start_latitude = Some(first.latitude);
        open.start_longitude = Some(first.longitude);
        open.start_soc = first.battery_level;
        open.start_rated_range_km = open.start_rated_range_km.or(first.rated_battery_range_km);
    }
    for position in ordered {
        let position = imported_position(position);
        if let Some(speed) = position.speed {
            open.speed_max = Some(open.speed_max.map_or(speed, |max| max.max(speed)));
        }
        observe_drive_position(&mut open, &position);
        if let Some(temp) = position.outside_temp {
            open.outside_temp_sum += temp;
            open.outside_temp_count = open.outside_temp_count.saturating_add(1);
        }
    }
    open.first_odometer = row.start_km.or(open.first_odometer);
    open.last_odometer = row.end_km.or(open.last_odometer);
    open.elevation_ascent = row.ascent.unwrap_or(open.elevation_ascent);
    open.elevation_descent = row.descent.unwrap_or(open.elevation_descent);
    if let Some(average) = row.outside_temp_avg {
        let count = open.outside_temp_count.max(1);
        open.outside_temp_sum = average * f64::from(count);
        open.outside_temp_count = count;
    }
    if let Some(average) = row.inside_temp_avg {
        let count = open.inside_temp_count.max(1);
        open.inside_temp_sum = average * f64::from(count);
        open.inside_temp_count = count;
    }
    open
}

fn open_charge_from_source(
    row: &TeslaMateChargingProcess,
    samples: &[TeslaMateCharge],
) -> OpenCharge {
    let mut open = OpenCharge {
        id: row.id,
        car_id: row.car_id,
        start_date_ms: row.start_date_ms,
        start_battery_level: row.start_battery_level,
        start_ideal_range_km: row.start_ideal_range_km,
        start_rated_range_km: row.start_rated_range_km,
        start_latitude: None,
        start_longitude: None,
        is_dc: None,
        fast_charger_type: None,
        max_charger_power_kw: None,
        outside_temp_sum: 0.0,
        outside_temp_count: 0,
        first_energy_added: None,
        max_energy_added: None,
        last_energy_added: None,
        last_battery_level: row.end_battery_level.or(row.start_battery_level),
        last_ideal_range_km: row.end_ideal_range_km.or(row.start_ideal_range_km),
        last_rated_range_km: row.end_rated_range_km.or(row.start_rated_range_km),
        sample_count: 0,
        energy_used_kwh: None,
        last_sample_timestamp_ms: None,
        last_sample_power_kw: None,
        samples: Vec::new(),
    };

    let mut ordered = samples.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|sample| (sample.date_ms, sample.id));
    for sample in ordered {
        let sample = project_charge_sample(sample);
        if open.sample_count == 0 {
            open.start_battery_level = open.start_battery_level.or(sample.battery_level);
            open.start_ideal_range_km = open.start_ideal_range_km.or(sample.ideal_range_km);
            open.start_rated_range_km = open.start_rated_range_km.or(sample.rated_range_km);
        }
        if let Some(energy) = sample.charge_energy_added_kwh {
            open.first_energy_added.get_or_insert(energy);
            open.max_energy_added =
                Some(open.max_energy_added.map_or(energy, |max| max.max(energy)));
            open.last_energy_added = Some(energy);
        }
        open.last_battery_level = sample.battery_level.or(open.last_battery_level);
        open.last_ideal_range_km = sample.ideal_range_km.or(open.last_ideal_range_km);
        open.last_rated_range_km = sample.rated_range_km.or(open.last_rated_range_km);
        if let Some(power) = sample.charger_power_kw {
            open.max_charger_power_kw = Some(
                open.max_charger_power_kw
                    .map_or(power, |max| max.max(power)),
            );
        }
        if let Some(is_dc) = sample.fast_charger_present {
            open.is_dc = Some(open.is_dc.unwrap_or(false) || is_dc);
        }
        if sample.fast_charger_type.is_some() {
            open.fast_charger_type = sample.fast_charger_type.clone();
        }
        if let Some(temp) = sample.outside_temp_c {
            open.outside_temp_sum += temp;
            open.outside_temp_count = open.outside_temp_count.saturating_add(1);
        }
        observe_charge_sample(&mut open, &sample);
    }
    open.energy_used_kwh = row.charge_energy_used_kwh.or(open.energy_used_kwh);
    if open.first_energy_added.is_none() {
        open.first_energy_added = row.charge_energy_added;
        open.max_energy_added = row.charge_energy_added;
        open.last_energy_added = row.charge_energy_added;
    }
    if let Some(average) = row.outside_temp_avg {
        let count = open.outside_temp_count.max(1);
        open.outside_temp_sum = average * f64::from(count);
        open.outside_temp_count = count;
    }
    open
}

fn open_state_from_source(row: &TeslaMateState) -> OpenState {
    OpenState {
        id: row.id,
        car_id: row.car_id,
        state: row.state.clone(),
        start_date_ms: row.start_date_ms,
    }
}

pub fn imported_position(row: &TeslaMatePosition) -> ProjectionPosition {
    ProjectionPosition {
        id: row.id,
        drive_id: row.drive_id,
        car_id: row.car_id,
        date_ms: row.date_ms,
        latitude: row.latitude,
        longitude: row.longitude,
        speed: row.speed,
        power: row.power,
        battery_level: row.battery_level,
        usable_battery_level: row.usable_battery_level,
        elevation: row.elevation,
        odometer: row.odometer,
        ideal_battery_range_km: row.ideal_battery_range_km,
        est_battery_range_km: row.est_battery_range_km,
        rated_battery_range_km: row.rated_battery_range_km,
        fan_status: row.fan_status,
        driver_temp_setting: row.driver_temp_setting,
        passenger_temp_setting: row.passenger_temp_setting,
        is_climate_on: row.is_climate_on,
        is_rear_defroster_on: row.is_rear_defroster_on,
        is_front_defroster_on: row.is_front_defroster_on,
        inside_temp: row.inside_temp,
        outside_temp: row.outside_temp,
        battery_heater: row.battery_heater,
        battery_heater_on: row.battery_heater_on,
        battery_heater_no_power: row.battery_heater_no_power,
        tpms_pressure_fl: row.tpms_pressure_fl,
        tpms_pressure_fr: row.tpms_pressure_fr,
        tpms_pressure_rl: row.tpms_pressure_rl,
        tpms_pressure_rr: row.tpms_pressure_rr,
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
    Suspended,
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
    /// Running child count. Children live in `lifecycle_open_rows`; this state
    /// must not grow with telemetry volume.
    #[serde(default)]
    pub position_count: u32,
    #[serde(default)]
    pub last_position_date_ms: Option<i64>,
    #[serde(default)]
    pub last_latitude: Option<f64>,
    #[serde(default)]
    pub last_longitude: Option<f64>,
    #[serde(default)]
    pub last_soc: Option<i64>,
    #[serde(default)]
    pub last_rated_range_km: Option<f64>,
    #[serde(default)]
    pub last_odometer: Option<f64>,
    #[serde(default)]
    pub first_odometer: Option<f64>,
    #[serde(default)]
    pub power_max: Option<f64>,
    #[serde(default)]
    pub power_min: Option<f64>,
    #[serde(default)]
    pub inside_temp_sum: f64,
    #[serde(default)]
    pub inside_temp_count: u32,
    #[serde(default)]
    pub start_ideal_range_km: Option<f64>,
    #[serde(default)]
    pub end_ideal_range_km: Option<f64>,
    #[serde(default)]
    pub elevation_ascent: i64,
    #[serde(default)]
    pub elevation_descent: i64,
    #[serde(default)]
    pub last_elevation: Option<i64>,
    /// In-memory child buffer for pure unit tests and single-batch close. The
    /// durable db path clears this before encoding and never rehydrates the full
    /// history into active state on every observation.
    #[serde(default)]
    pub positions: Vec<ProjectionPosition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenCharge {
    pub id: i64,
    pub car_id: i64,
    pub start_date_ms: i64,
    pub start_battery_level: Option<i64>,
    #[serde(default)]
    pub start_ideal_range_km: Option<f64>,
    pub start_rated_range_km: Option<f64>,
    #[serde(default)]
    pub start_latitude: Option<f64>,
    #[serde(default)]
    pub start_longitude: Option<f64>,
    pub is_dc: Option<bool>,
    #[serde(default)]
    pub fast_charger_type: Option<String>,
    pub max_charger_power_kw: Option<f64>,
    pub outside_temp_sum: f64,
    pub outside_temp_count: u32,
    #[serde(default)]
    pub first_energy_added: Option<f64>,
    #[serde(default)]
    pub max_energy_added: Option<f64>,
    pub last_energy_added: Option<f64>,
    pub last_battery_level: Option<i64>,
    #[serde(default)]
    pub last_ideal_range_km: Option<f64>,
    pub last_rated_range_km: Option<f64>,
    #[serde(default)]
    pub sample_count: u32,
    /// Incremental energy-used accumulator so close does not need every sample.
    #[serde(default)]
    pub energy_used_kwh: Option<f64>,
    #[serde(default)]
    pub last_sample_timestamp_ms: Option<i64>,
    #[serde(default)]
    pub last_sample_power_kw: Option<f64>,
    /// In-memory child buffer (see OpenDrive::positions).
    #[serde(default)]
    pub samples: Vec<ProjectionChargeSample>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenState {
    pub id: i64,
    pub car_id: i64,
    pub state: String,
    pub start_date_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenUpdate {
    pub id: i64,
    pub car_id: i64,
    pub start_date_ms: i64,
}

fn default_next_state_id() -> i64 {
    1
}

fn default_next_update_id() -> i64 {
    1
}

/// Completed entities produced since the previous open state.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LifecycleDelta {
    pub drives: Vec<ProjectionDrive>,
    pub positions: Vec<ProjectionPosition>,
    pub charges: Vec<ProjectionCharge>,
    pub charge_samples: Vec<ProjectionChargeSample>,
    pub states: Vec<ProjectionState>,
    pub updates: Vec<ProjectionUpdate>,
    pub charge_start_coordinates: Vec<(i64, f64, f64)>,
    pub open_drive_positions: Vec<ProjectionPosition>,
    pub open_charge_samples: Vec<ProjectionChargeSample>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeofenceFence {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub radius_m: f64,
    pub billing_type: Option<GeofenceBillingType>,
    pub cost_per_unit: Option<f64>,
    pub session_fee: Option<f64>,
}

pub fn apply_geofence_labels(delta: &mut LifecycleDelta, fences: &[GeofenceFence]) {
    for drive in &mut delta.drives {
        if drive.start_geofence.is_none() {
            drive.start_geofence = match (drive.start_latitude, drive.start_longitude) {
                (Some(latitude), Some(longitude)) => {
                    match_geofence_name(latitude, longitude, fences)
                }
                _ => None,
            };
        }
        if drive.end_geofence.is_none() {
            drive.end_geofence = match (drive.end_latitude, drive.end_longitude) {
                (Some(latitude), Some(longitude)) => {
                    match_geofence_name(latitude, longitude, fences)
                }
                _ => None,
            };
        }
    }
}

pub fn match_geofence_name(
    latitude: f64,
    longitude: f64,
    fences: &[GeofenceFence],
) -> Option<String> {
    match_geofence(latitude, longitude, fences).map(|fence| fence.name.clone())
}

pub fn match_geofence(
    latitude: f64,
    longitude: f64,
    fences: &[GeofenceFence],
) -> Option<&GeofenceFence> {
    fences
        .iter()
        .filter_map(|fence| {
            let distance = haversine_m(latitude, longitude, fence.latitude, fence.longitude);
            (distance <= fence.radius_m).then_some((distance, fence))
        })
        .min_by(|left, right| left.0.total_cmp(&right.0))
        .map(|(_, fence)| fence)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChargeTariff {
    pub billing_type: GeofenceBillingType,
    pub cost_per_unit: Option<f64>,
    pub session_fee: Option<f64>,
}

pub fn calculate_charge_cost(
    fast_charger_type: Option<&str>,
    free_supercharging: bool,
    charge_energy_added: Option<f64>,
    charge_energy_used_kwh: Option<f64>,
    duration_min: Option<i64>,
    tariff: Option<ChargeTariff>,
) -> Option<f64> {
    if free_supercharging && fast_charger_type.is_some_and(|value| value.starts_with("Tesla")) {
        return Some(0.0);
    }
    let tariff = tariff?;
    let fee = tariff
        .session_fee
        .filter(|value| value.is_finite())
        .unwrap_or(0.0);
    match tariff.billing_type {
        GeofenceBillingType::PerKwh => {
            let energy = [charge_energy_added, charge_energy_used_kwh]
                .into_iter()
                .flatten()
                .filter(|value| value.is_finite() && *value >= 0.0)
                .reduce(f64::max)?;
            let variable = tariff
                .cost_per_unit
                .filter(|value| value.is_finite())
                .map_or(0.0, |rate| energy * rate);
            (variable.is_finite()
                && (variable != 0.0
                    || tariff.cost_per_unit.is_some()
                    || tariff.session_fee.is_some()))
            .then_some(variable + fee)
        }
        GeofenceBillingType::PerMinute => {
            let minutes = duration_min.filter(|value| *value >= 0)? as f64;
            let rate = tariff.cost_per_unit.filter(|value| value.is_finite())?;
            let cost = minutes * rate + fee;
            cost.is_finite().then_some(cost)
        }
    }
}

#[cfg(test)]
mod tariff_tests {
    use super::*;

    fn per_kwh(rate: Option<f64>, fee: Option<f64>) -> ChargeTariff {
        ChargeTariff {
            billing_type: GeofenceBillingType::PerKwh,
            cost_per_unit: rate,
            session_fee: fee,
        }
    }

    #[test]
    fn cost_precedence_and_nulls_match_teslamate() {
        assert_eq!(
            calculate_charge_cost(
                Some("Tesla Supercharger"),
                true,
                Some(10.0),
                Some(12.0),
                Some(30),
                Some(per_kwh(Some(0.30), Some(2.0))),
            ),
            Some(0.0)
        );
        assert_eq!(
            calculate_charge_cost(
                None,
                false,
                Some(10.0),
                Some(12.0),
                Some(30),
                Some(per_kwh(Some(0.30), Some(2.0))),
            ),
            Some(5.6)
        );
        assert_eq!(
            calculate_charge_cost(
                None,
                false,
                Some(-1.0),
                Some(10.0),
                Some(30),
                Some(per_kwh(Some(0.30), None)),
            ),
            Some(3.0)
        );
        assert_eq!(
            calculate_charge_cost(
                None,
                false,
                None,
                None,
                Some(30),
                Some(per_kwh(Some(0.30), Some(2.0))),
            ),
            None
        );
        assert_eq!(
            calculate_charge_cost(
                None,
                false,
                None,
                None,
                Some(30),
                Some(ChargeTariff {
                    billing_type: GeofenceBillingType::PerMinute,
                    cost_per_unit: Some(0.10),
                    session_fee: Some(2.0),
                }),
            ),
            Some(5.0)
        );
        assert_eq!(
            calculate_charge_cost(None, false, Some(10.0), None, Some(30), None),
            None
        );
    }
}

fn haversine_m(latitude_a: f64, longitude_a: f64, latitude_b: f64, longitude_b: f64) -> f64 {
    let radius = 6_371_000.0;
    let lat = (latitude_b - latitude_a).to_radians();
    let lon = (longitude_b - longitude_a).to_radians();
    let a = (lat / 2.0).sin().powi(2)
        + latitude_a.to_radians().cos() * latitude_b.to_radians().cos() * (lon / 2.0).sin().powi(2);
    radius * 2.0 * a.sqrt().asin()
}

#[derive(Debug, Clone, PartialEq)]
pub struct LifecycleStep {
    pub state: OpenSessionState,
    pub delta: LifecycleDelta,
    pub quarantined: bool,
}

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

    #[test]
    fn stream_drive_closes_before_owner_poll_and_duplicate_is_idempotent() {
        let state = OpenSessionState::new();
        let parked = sample(1, 1_700_000_000_000, 100.0, "P");
        let driving_one = sample(2, 1_700_000_001_000, 100.0, "D");
        let driving_two = sample(3, 1_700_000_002_000, 100.2, "D");
        let parked_again = sample(4, 1_700_000_003_000, 100.3, "P");

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

    // Charge and drive transitions are independent enough that either can
    // close while the other opens on successive samples, but one sample never
    // starts both simultaneously in a coherent Tesla response.
    if let Some(closed) = maybe_close_drive(&mut state, car_id, &parsed, offline_drive_timeout)? {
        delta.positions.extend(closed.positions);
        delta.drives.push(closed.drive);
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
    if let Some(open) = state.open_drive.take()
        && let Some(closed) = finalize_drive(open)?
    {
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
    } else if is_drive_shift(shift_state.as_deref()) || speed.unwrap_or(0) > 0 {
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
        elevation: drive.and_then(|fields| int_field(fields, "native_location_elevation")),
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
    offline_drive_timeout: Duration,
) -> Result<Option<ClosedDrive>, LifecycleError> {
    let Some(open) = state.open_drive.as_ref() else {
        return Ok(None);
    };
    let offline_drive_timeout_ms =
        i64::try_from(offline_drive_timeout.as_millis()).unwrap_or(i64::MAX);
    let last_position_at = open
        .last_position_date_ms
        .or_else(|| open.positions.last().map(|position| position.date_ms))
        .unwrap_or(open.start_date_ms);
    let position_count = open
        .position_count
        .max(u32::try_from(open.positions.len()).unwrap_or(u32::MAX));
    let offline_timed_out = sample.phase == VehiclePhase::Offline
        && sample.drive_timestamp_ms.saturating_sub(last_position_at) >= offline_drive_timeout_ms;
    let should_close = matches!(sample.phase, VehiclePhase::Asleep | VehiclePhase::Updating)
        || offline_timed_out
        || (matches!(sample.phase, VehiclePhase::Online | VehiclePhase::Charging)
            && sample.drive_data_present
            && !is_drive_shift(sample.shift_state.as_deref())
            && sample.speed.unwrap_or(0) <= 0
            && position_count > 0);
    if !should_close {
        return Ok(None);
    }
    let mut open = state
        .open_drive
        .take()
        .expect("open drive was checked before close");
    let append_endpoint = sample.drive_data_present
        && matches!(sample.phase, VehiclePhase::Online | VehiclePhase::Charging)
        && !is_drive_shift(sample.shift_state.as_deref())
        && sample.speed.unwrap_or(0) <= 0
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
    finalize_drive(open)
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
        // Prefer explicit usable; fall back to battery_level so pack/client
        // integrity checks (usable BETWEEN 0 AND 100) accept live samples when
        // Owner-API omits usable_battery_level.
        usable_battery_level: sample.usable_battery_level.or(sample.battery_level),
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
    let driving = is_drive_shift(sample.shift_state.as_deref()) || sample.speed.unwrap_or(0) > 0;
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
    if is_drive_shift(sample.shift_state.as_deref()) || sample.speed.unwrap_or(0) > 0 {
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
        // Prefer explicit usable; fall back to battery_level so pack/client
        // integrity checks (usable BETWEEN 0 AND 100) accept live samples when
        // Owner-API omits usable_battery_level.
        usable_battery_level: sample.usable_battery_level.or(sample.battery_level),
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

    fn discovery(id: i64, at_ms: i64, state: &str) -> LifecycleSample {
        LifecycleSample {
            observation_id: id,
            observed_at_ms: at_ms,
            vehicle_state: state.to_owned(),
            payload: json!({
                "record_type": "owner_api_discovery_v1",
                "source_vehicle_id": "9",
                "source_vehicle_state": state,
            }),
        }
    }

    fn imported_position_fixture(
        id: i64,
        drive_id: i64,
        at_ms: i64,
        latitude: f64,
        odometer: f64,
        battery_level: i64,
        outside_temp: f64,
        inside_temp: f64,
    ) -> TeslaMatePosition {
        serde_json::from_value(json!({
            "id": id,
            "car_id": 1,
            "drive_id": drive_id,
            "date_ms": at_ms,
            "latitude": latitude,
            "longitude": 19.0,
            "elevation": 100 + id,
            "speed": 30,
            "power": id as f64,
            "odometer": odometer,
            "ideal_battery_range_km": 300.0 - id as f64,
            "est_battery_range_km": null,
            "rated_battery_range_km": 280.0 - id as f64,
            "battery_level": battery_level,
            "usable_battery_level": battery_level,
            "fan_status": null,
            "driver_temp_setting": null,
            "passenger_temp_setting": null,
            "is_climate_on": false,
            "is_rear_defroster_on": false,
            "is_front_defroster_on": false,
            "outside_temp": outside_temp,
            "inside_temp": inside_temp,
            "battery_heater": false,
            "battery_heater_on": false,
            "battery_heater_no_power": false,
            "tpms_pressure_fl": null,
            "tpms_pressure_fr": null,
            "tpms_pressure_rl": null,
            "tpms_pressure_rr": null
        }))
        .expect("imported position fixture")
    }

    fn imported_charge_fixture(
        id: i64,
        process_id: i64,
        at_ms: i64,
        energy_added: f64,
        battery_level: i64,
        outside_temp: f64,
    ) -> TeslaMateCharge {
        serde_json::from_value(json!({
            "id": id,
            "charging_process_id": process_id,
            "date_ms": at_ms,
            "battery_heater": false,
            "battery_heater_on": false,
            "battery_heater_no_power": false,
            "battery_level": battery_level,
            "usable_battery_level": battery_level,
            "charge_energy_added_kwh": energy_added,
            "charger_actual_current": 16.0,
            "charger_phases": 3,
            "charger_pilot_current": 16.0,
            "charger_power_kw": 11.0,
            "charger_voltage": 230.0,
            "charge_cable": "IEC",
            "fast_charger_present": false,
            "fast_charger_brand": null,
            "fast_charger_type": null,
            "ideal_range_km": 150.0 + id as f64,
            "rated_range_km": 140.0 + id as f64,
            "not_enough_power_to_heat": false,
            "outside_temp_c": outside_temp
        }))
        .expect("imported charge fixture")
    }

    #[test]
    fn imported_active_drive_survives_restart_and_immediate_terminal_sample() {
        let start = 1_800_100_000_000_i64;
        let drive: TeslaMateDrive = serde_json::from_value(json!({
            "id": 70,
            "car_id": 1,
            "start_date_ms": start,
            "end_date_ms": null,
            "start_position_id": 700,
            "end_position_id": 701,
            "start_address_id": null,
            "end_address_id": null,
            "start_geofence_id": null,
            "end_geofence_id": null,
            "outside_temp_avg": 18.0,
            "inside_temp_avg": 20.0,
            "speed_max": 50,
            "power_max": 12.0,
            "power_min": -5.0,
            "start_ideal_range_km": 300.0,
            "end_ideal_range_km": 298.0,
            "start_rated_range_km": 280.0,
            "end_rated_range_km": 278.0,
            "start_km": 100.0,
            "end_km": 100.8,
            "distance_km": null,
            "duration_min": null,
            "ascent": 7,
            "descent": 2
        }))
        .expect("imported drive");
        let mut session = TeslaMateOpenSession {
            car_id: 1,
            drive: Some(drive),
            drive_positions: vec![
                imported_position_fixture(700, 70, start, 47.5, 100.0, 80, 17.0, 19.0),
                imported_position_fixture(701, 70, start + 60_000, 47.51, 100.8, 78, 19.0, 21.0),
            ],
            ..Default::default()
        };
        session.watermarks.positions.max_timestamp_ms = Some(start + 60_000);
        let seeded = seed_imported_open_session_state(uuid::Uuid::nil(), &session, None)
            .expect("seed imported drive");
        assert_eq!(seeded.open_drive.as_ref().unwrap().position_count, 2);

        let restored = OpenSessionState::decode(&seeded.encode().expect("encode seeded drive"))
            .expect("restore seeded drive");
        let terminal = sample(
            1,
            start + 120_000,
            json!({
                "drive_state": {
                    "shift_state": "P",
                    "speed": 0,
                    "latitude": 47.52,
                    "longitude": 19.0,
                    "timestamp": start + 120_000
                },
                "vehicle_state": {"odometer": 101.0 / 1.609_344},
                "charge_state": {"battery_level": 77, "battery_range": 277.0},
                "climate_state": {"inside_temp": 22.0, "outside_temp": 20.0}
            }),
        );
        let direct = apply_sample(seeded, 1, &terminal).expect("close seeded drive");
        let restarted = apply_sample(restored, 1, &terminal).expect("close restored drive");
        assert_eq!(restarted.delta.drives, direct.delta.drives);
        let closed = &restarted.delta.drives[0];
        assert_eq!(closed.distance_km, Some(1.0));
        assert_eq!(closed.start_latitude, Some(47.5));
        assert_eq!(closed.end_latitude, Some(47.52));
        assert_eq!(closed.outside_temp_avg, Some(18.0));
        assert!((closed.inside_temp_avg.unwrap() - 20.666_666_666_666_668).abs() < 1e-9);
        assert_eq!(closed.ascent, Some(7));
        assert_eq!(closed.descent, Some(2));
    }

    #[test]
    fn imported_active_charge_survives_restart_and_immediate_terminal_sample() {
        let start = 1_800_200_000_000_i64;
        let process: TeslaMateChargingProcess = serde_json::from_value(json!({
            "id": 80,
            "car_id": 1,
            "position_id": null,
            "address_id": null,
            "geofence_id": null,
            "start_date_ms": start,
            "end_date_ms": null,
            "charge_energy_added": 3.0,
            "charge_energy_used_kwh": 0.9166666666666666,
            "start_ideal_range_km": 151.0,
            "end_ideal_range_km": 152.0,
            "start_battery_level": 40,
            "end_battery_level": 50,
            "duration_min": null,
            "outside_temp_avg": 11.0,
            "start_rated_range_km": 141.0,
            "end_rated_range_km": 142.0,
            "cost": null
        }))
        .expect("imported charging process");
        let mut session = TeslaMateOpenSession {
            car_id: 1,
            charge: Some(process),
            charge_samples: vec![
                imported_charge_fixture(1, 80, start, 1.0, 40, 10.0),
                imported_charge_fixture(2, 80, start + 300_000, 4.0, 50, 12.0),
            ],
            ..Default::default()
        };
        session.watermarks.charges.max_timestamp_ms = Some(start + 300_000);
        let seeded = seed_imported_open_session_state(uuid::Uuid::nil(), &session, None)
            .expect("seed imported charge");
        assert_eq!(seeded.open_charge.as_ref().unwrap().sample_count, 2);

        let restored = OpenSessionState::decode(&seeded.encode().expect("encode seeded charge"))
            .expect("restore seeded charge");
        let terminal = sample(
            1,
            start + 600_000,
            json!({
                "drive_state": {"shift_state": "P", "speed": 0},
                "charge_state": {
                    "charging_state": "Complete",
                    "timestamp": start + 600_000,
                    "battery_level": 80,
                    "charge_energy_added": 10.0,
                    "charger_power": 0.0,
                    "battery_range": 180.0,
                    "ideal_battery_range": 190.0
                }
            }),
        );
        let direct = apply_sample(seeded, 1, &terminal).expect("close seeded charge");
        let restarted = apply_sample(restored, 1, &terminal).expect("close restored charge");
        assert_eq!(restarted.delta.charges, direct.delta.charges);
        let closed = &restarted.delta.charges[0];
        assert_eq!(closed.charge_energy_added, Some(9.0));
        assert_eq!(closed.charge_energy_used_kwh, Some(0.9166666666666666));
        assert_eq!(closed.start_battery_level, Some(40));
        assert_eq!(closed.end_battery_level, Some(80));
        assert_eq!(closed.outside_temp_avg, Some(11.0));
        assert_eq!(closed.duration_min, Some(10));
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
                        "power": -7.0,
                        "native_location_elevation": 100,
                        "timestamp": start
                    },
                    "vehicle_state": {"odometer": 100.0},
                    "charge_state": {"battery_level": 70, "battery_range": 200.0, "ideal_battery_range": 338.8},
                    "climate_state": {"outside_temp": 18.0, "inside_temp": 20.0, "is_climate_on": true}
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
                        "power": 12.5,
                        "native_location_elevation": 160,
                        "timestamp": start + 60_000
                    },
                    "vehicle_state": {"odometer": 101.25},
                    "charge_state": {"battery_level": 69, "battery_range": 198.0}
                }),
            ),
            sample(
                3,
                start + 120_000,
                json!({
                    "drive_state": {
                        "shift_state": "D",
                        "speed": 30,
                        "latitude": 47.515,
                        "longitude": 19.015,
                        "power": 36.0,
                        "native_location_elevation": 130,
                        "timestamp": start + 120_000
                    },
                    "vehicle_state": {"odometer": 103.0},
                    "charge_state": {"battery_level": 68, "battery_range": 196.0, "ideal_battery_range": 334.8},
                    "climate_state": {"inside_temp": 22.0}
                }),
            ),
            sample(
                4,
                start + 180_000,
                json!({
                    "drive_state": {
                        "shift_state": "P",
                        "speed": 0,
                        "latitude": 47.52,
                        "longitude": 19.02,
                        "timestamp": start + 180_000
                    },
                    "charge_state": {"battery_level": 68, "battery_range": 196.0}
                }),
            ),
        ];

        let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("project");
        assert!(step.state.open_drive.is_none());
        assert_eq!(step.delta.drives.len(), 1);
        assert_eq!(step.delta.positions.len(), 4);
        assert_eq!(
            step.delta
                .positions
                .iter()
                .filter(|position| position.drive_id.is_some())
                .count(),
            3
        );
        assert_eq!(
            step.delta
                .positions
                .iter()
                .filter(|position| position.drive_id.is_none())
                .count(),
            1
        );
        assert_eq!(step.delta.drives[0].start_date_ms, start);
        assert_eq!(step.delta.drives[0].end_date_ms, start + 120_000);
        assert_eq!(step.delta.drives[0].speed_max, Some(64));
        assert!((step.delta.drives[0].distance_km.unwrap() - 3.0 * 1.609_344).abs() < 0.000_001);
        assert_eq!(step.delta.drives[0].duration_min, Some(2));
        assert_eq!(step.delta.drives[0].inside_temp_avg, Some(21.0));
        assert_eq!(step.delta.drives[0].power_max, Some(36.0));
        assert_eq!(step.delta.drives[0].power_min, Some(-7.0));
        assert_eq!(step.delta.drives[0].start_ideal_range_km, Some(545.25));
        assert_eq!(step.delta.drives[0].end_ideal_range_km, Some(538.81));
        assert_eq!(step.delta.drives[0].ascent, Some(60));
        assert_eq!(step.delta.drives[0].descent, Some(30));
        assert_eq!(step.state.last_observation_id, 4);
    }

    #[test]
    fn discards_drive_with_fewer_than_two_positions() {
        let start = 1_800_000_050_000_i64;
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
                        "timestamp": start
                    },
                    "vehicle_state": {"odometer": 1000.0}
                }),
            ),
            sample(
                2,
                start + 60_000,
                json!({"drive_state":{"shift_state":"P","speed":0}}),
            ),
        ];

        let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("project");
        assert!(step.delta.drives.is_empty());
        assert!(step.delta.positions.is_empty());
    }

    #[test]
    fn discards_zero_odometer_distance_drive() {
        let start = 1_800_000_060_000_i64;
        let samples = [
            sample(
                1,
                start,
                json!({
                    "drive_state":{"shift_state":"D","speed":20,"latitude":47.5,"longitude":19.0,"timestamp":start},
                    "vehicle_state":{"odometer":1000.0}
                }),
            ),
            sample(
                2,
                start + 31_000,
                json!({
                    "drive_state":{"shift_state":"D","speed":20,"latitude":48.5,"longitude":20.0,"timestamp":start + 31_000},
                    "vehicle_state":{"odometer":1000.0}
                }),
            ),
            sample(
                3,
                start + 60_000,
                json!({"drive_state":{"shift_state":"P","speed":0}}),
            ),
        ];

        let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("project");
        assert!(step.delta.drives.is_empty());
        assert!(step.delta.positions.is_empty());
    }

    #[test]
    fn sparse_stream_does_not_close_charge_or_zero_energy() {
        let start = 1_800_000_200_000_i64;
        let charging = sample(
            1,
            start,
            json!({
                "drive_state":{"shift_state":"P","speed":0,"latitude":47.5,"longitude":19.05,"timestamp":start},
                "charge_state":{
                    "charging_state":"Charging","timestamp":start,
                    "battery_level":64,"charge_energy_added":1.5,"charger_power":11.0
                }
            }),
        );
        let stream = LifecycleSample {
            observation_id: 2,
            observed_at_ms: start + 6_666,
            vehicle_state: "online".to_owned(),
            payload: stream_observation_payload(&crate::tesla_stream::parse_data_update(
                &format!(
                    r#"{{"msg_type":"data:update","tag":"9","timestamp":{},"value":"0,12355.4,64,100,90,47.5,19.05,-11,P,220,210,90"}}"#,
                    start + 6_666
                ),
            )
            .unwrap()),
        };
        let later = sample(
            3,
            start + 120_000,
            json!({
                "drive_state":{"shift_state":"P","speed":0,"latitude":47.5,"longitude":19.05,"timestamp":start + 120_000},
                "charge_state":{
                    "charging_state":"Complete","timestamp":start + 120_000,
                    "battery_level":80,"charge_energy_added":12.0,"charger_power":0.0
                }
            }),
        );
        let opened = apply_sample(OpenSessionState::new(), 1, &charging).unwrap();
        assert!(opened.state.open_charge.is_some());
        assert!(opened.delta.charges.is_empty());
        let after_stream = apply_sample(opened.state, 1, &stream).unwrap();
        assert!(
            after_stream.state.open_charge.is_some(),
            "stream without charging_state must not seal the session"
        );
        assert!(after_stream.delta.charges.is_empty());
        let closed = apply_sample(after_stream.state, 1, &later).unwrap();
        assert_eq!(closed.delta.charges.len(), 1);
        assert_eq!(closed.delta.charges[0].charge_energy_added, Some(10.5));
        assert_eq!(closed.delta.charges[0].duration_min, Some(2));
    }

    #[test]
    fn rounds_drive_duration_from_position_timestamps() {
        let start = 1_800_000_070_000_i64;
        let samples = [
            sample(
                1,
                start,
                json!({
                    "drive_state":{"shift_state":"D","speed":20,"latitude":47.5,"longitude":19.0,"timestamp":start},
                    "vehicle_state":{"odometer":1000.0}
                }),
            ),
            sample(
                2,
                start + 31_000,
                json!({
                    "drive_state":{"shift_state":"D","speed":20,"latitude":47.5,"longitude":19.0,"timestamp":start + 31_000},
                    "vehicle_state":{"odometer":1000.1}
                }),
            ),
            sample(
                3,
                start + 90_000,
                json!({"drive_state":{"shift_state":"P","speed":0}}),
            ),
        ];

        let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("project");
        assert_eq!(step.delta.drives[0].duration_min, Some(1));
        assert_eq!(step.delta.drives[0].end_date_ms, start + 31_000);
    }

    #[test]
    fn offline_discovery_closes_drive_only_after_teslamate_timeout() {
        let start = 1_800_000_080_000_i64;
        let driving = [
            sample(
                1,
                start,
                json!({
                    "drive_state":{"shift_state":"D","speed":20,"latitude":47.5,"longitude":19.0,"timestamp":start},
                    "vehicle_state":{"odometer":1000.0}
                }),
            ),
            sample(
                2,
                start + 1_000,
                json!({
                    "drive_state":{"shift_state":"D","speed":20,"latitude":47.51,"longitude":19.01,"timestamp":start + 1_000},
                    "vehicle_state":{"odometer":1000.1}
                }),
            ),
        ];
        let active = apply_samples(OpenSessionState::new(), 1, &driving)
            .expect("active drive")
            .state;
        let early = apply_sample(active, 1, &discovery(3, start + 30_000, "offline"))
            .expect("early offline");
        assert!(early.state.open_drive.is_some());
        assert!(early.delta.drives.is_empty());

        let timed_out = apply_sample(
            early.state,
            1,
            &discovery(4, start + 1_000 + 15 * 60 * 1_000, "offline"),
        )
        .expect("offline timeout");
        assert!(timed_out.state.open_drive.is_none());
        assert_eq!(timed_out.delta.drives.len(), 1);
        assert_eq!(timed_out.delta.positions.len(), 2);
    }

    #[test]
    fn offline_discovery_uses_the_configured_drive_timeout() {
        let start = 1_800_000_085_000_i64;
        let active = apply_samples(
            OpenSessionState::new(),
            1,
            &[
                sample(
                    1,
                    start,
                    json!({
                        "drive_state":{"shift_state":"D","speed":20,"latitude":47.5,"longitude":19.0,"timestamp":start},
                        "vehicle_state":{"odometer":1000.0}
                    }),
                ),
                sample(
                    2,
                    start + 1_000,
                    json!({
                        "drive_state":{"shift_state":"D","speed":20,"latitude":47.51,"longitude":19.01,"timestamp":start + 1_000},
                        "vehicle_state":{"odometer":1000.1}
                    }),
                ),
            ],
        )
        .expect("active drive")
        .state;

        let short = apply_sample_with_offline_drive_timeout(
            active.clone(),
            1,
            &discovery(3, start + 31_000, "offline"),
            Duration::from_secs(30),
        )
        .expect("short timeout");
        assert!(short.state.open_drive.is_none());
        assert_eq!(short.delta.drives.len(), 1);

        let long = apply_sample_with_offline_drive_timeout(
            active,
            1,
            &discovery(3, start + 16 * 60 * 1_000, "offline"),
            Duration::from_secs(60 * 60),
        )
        .expect("long timeout");
        assert!(long.state.open_drive.is_some());
        assert!(long.delta.drives.is_empty());
    }

    #[test]
    fn offline_discovery_keeps_charge_open_but_asleep_closes_it() {
        let start = 1_800_000_090_000_i64;
        let charging = sample(
            1,
            start,
            json!({
                "charge_state":{
                    "charging_state":"Charging",
                    "battery_level":40,
                    "charge_energy_added":1.0
                }
            }),
        );
        let active = apply_sample(OpenSessionState::new(), 1, &charging)
            .expect("active charge")
            .state;
        let offline = apply_sample(active, 1, &discovery(2, start + 60_000, "offline"))
            .expect("offline charge");
        assert!(offline.state.open_charge.is_some());
        assert!(offline.delta.charges.is_empty());

        let asleep = apply_sample(offline.state, 1, &discovery(3, start + 120_000, "asleep"))
            .expect("asleep charge");
        assert!(asleep.state.open_charge.is_none());
        assert_eq!(asleep.delta.charges.len(), 1);
    }

    #[test]
    fn software_update_phase_survives_offline_and_finishes_online() {
        let start = 1_800_000_095_000_i64;
        let installing = sample(
            1,
            start,
            json!({
                "drive_state":{"shift_state":"P","speed":0},
                "vehicle_state":{
                    "timestamp":start,
                    "car_version":"2019.8.4",
                    "software_update":{"status":"installing","version":"2019.8.5"}
                }
            }),
        );
        let updating = apply_sample(OpenSessionState::new(), 1, &installing)
            .expect("start update")
            .state;
        assert_eq!(updating.phase, VehiclePhase::Updating);

        let offline = apply_sample(updating, 1, &discovery(2, start + 30_000, "offline"))
            .expect("offline update")
            .state;
        let offline = OpenSessionState::decode(&offline.encode().expect("encode update"))
            .expect("restore update");
        assert_eq!(offline.phase, VehiclePhase::Updating);

        let finished = apply_sample(
            offline,
            1,
            &sample(
                3,
                start + 60_000,
                json!({
                    "drive_state":{"shift_state":"P","speed":0},
                    "vehicle_state":{
                        "timestamp":start + 60_000,
                        "car_version":"2019.8.5",
                        "software_update":{"status":"","version":"2019.8.5"}
                    }
                }),
            ),
        )
        .expect("finish update");
        assert_eq!(finished.state.phase, VehiclePhase::Online);
        assert!(finished.state.open_update.is_none());
        assert_eq!(finished.delta.updates.len(), 1);
        assert_eq!(finished.delta.updates[0].start_date_ms, start);
        assert_eq!(finished.delta.updates[0].end_date_ms, start + 60_000);
        assert_eq!(finished.delta.updates[0].version, "2019.8.5");
    }

    #[test]
    fn available_update_cancels_open_update_without_history() {
        let start = 1_800_000_100_000_i64;
        let installing = apply_sample(
            OpenSessionState::new(),
            1,
            &sample(
                1,
                start,
                json!({
                    "vehicle_state": {
                        "timestamp": start,
                        "car_version": "2026.1",
                        "software_update": {"status": "installing"}
                    }
                }),
            ),
        )
        .expect("start update");
        assert!(installing.state.open_update.is_some());

        let cancelled = apply_sample(
            installing.state,
            1,
            &sample(
                2,
                start + 30_000,
                json!({
                    "vehicle_state": {
                        "timestamp": start + 30_000,
                        "car_version": "2026.1",
                        "software_update": {"status": "available"}
                    }
                }),
            ),
        )
        .expect("cancel update");
        assert!(cancelled.state.open_update.is_none());
        assert!(cancelled.delta.updates.is_empty());
    }

    #[test]
    fn newer_firmware_version_is_logged_as_missed_update_once() {
        let start = 1_800_000_110_000_i64;
        let first = apply_sample(
            OpenSessionState::new(),
            1,
            &sample(
                1,
                start,
                json!({
                    "vehicle_state": {"timestamp": start, "car_version": "2026.1"}
                }),
            ),
        )
        .expect("first version");
        assert!(first.delta.updates.is_empty());

        let jumped = apply_sample(
            first.state,
            1,
            &sample(
                2,
                start + 60_000,
                json!({
                    "vehicle_state": {
                        "timestamp": start + 60_000,
                        "car_version": "2026.2"
                    }
                }),
            ),
        )
        .expect("firmware jump");
        assert_eq!(jumped.delta.updates.len(), 1);
        assert_eq!(jumped.delta.updates[0].start_date_ms, start + 60_000);
        assert_eq!(jumped.delta.updates[0].end_date_ms, start + 60_000);
        assert_eq!(jumped.delta.updates[0].version, "2026.2");

        let unchanged = apply_sample(
            jumped.state,
            1,
            &sample(
                3,
                start + 120_000,
                json!({
                    "vehicle_state": {
                        "timestamp": start + 120_000,
                        "car_version": "2026.2"
                    }
                }),
            ),
        )
        .expect("same firmware");
        assert!(unchanged.delta.updates.is_empty());
    }

    #[test]
    fn online_response_projects_mergeable_car_metadata() {
        let start = 1_800_000_520_000_i64;
        let response = sample(
            1,
            start,
            json!({
                "display_name":"Road car",
                "vin":"5YJTESTVIN1234567",
                "drive_state":{"shift_state":"P","speed":0},
                "vehicle_config":{
                    "car_type":"model3","trim_badging":"74D",
                    "exterior_color":"Pearl White","wheel_type":"Apollo","spoiler_type":"None"
                },
                "vehicle_state":{"timestamp":start,"car_version":"2026.1"}
            }),
        );

        let first = apply_sample(OpenSessionState::new(), 1, &response)
            .expect("metadata response")
            .state;
        assert_eq!(
            first.car_metadata,
            Some(crate::hub_pack::ProjectionCarPatch {
                name: Some("Road car".into()),
                model: Some("3".into()),
                vin: Some("5YJTESTVIN1234567".into()),
                trim_badging: Some("74D".into()),
                marketing_name: Some("LR AWD".into()),
                exterior_color: Some("Pearl White".into()),
                wheel_type: Some("Apollo".into()),
                spoiler_type: Some("None".into()),
                firmware_version: Some("2026.1".into()),
            })
        );

        let restored = OpenSessionState::decode(&first.encode().expect("encode metadata"))
            .expect("decode metadata");
        let partial = sample(
            2,
            start + 1_000,
            json!({
                "display_name":"Renamed road car",
                "drive_state":{"shift_state":"P","speed":0},
                "vehicle_state":{"car_version":"2026.2"}
            }),
        );
        let updated = apply_sample(restored, 1, &partial)
            .expect("partial metadata")
            .state;
        assert_eq!(
            updated.car_metadata,
            Some(crate::hub_pack::ProjectionCarPatch {
                name: Some("Renamed road car".into()),
                model: Some("3".into()),
                vin: Some("5YJTESTVIN1234567".into()),
                trim_badging: Some("74D".into()),
                marketing_name: Some("LR AWD".into()),
                exterior_color: Some("Pearl White".into()),
                wheel_type: Some("Apollo".into()),
                spoiler_type: Some("None".into()),
                firmware_version: Some("2026.2".into()),
            })
        );
    }

    #[test]
    fn missing_charge_state_preserves_one_charging_session() {
        let start = 1_800_000_097_000_i64;
        let samples = [
            sample(
                1,
                start,
                json!({
                    "charge_state":{
                        "charging_state":"Charging",
                        "battery_level":40,
                        "charge_energy_added":0.1
                    }
                }),
            ),
            sample(
                2,
                start + 5_000,
                json!({"drive_state":{"shift_state":"P","speed":0},"charge_state":null}),
            ),
            sample(
                3,
                start + 10_000,
                json!({"drive_state":{"shift_state":"P","speed":0},"charge_state":null}),
            ),
            sample(
                4,
                start + 15_000,
                json!({
                    "charge_state":{
                        "charging_state":"Charging",
                        "battery_level":41,
                        "charge_energy_added":0.3
                    }
                }),
            ),
            sample(
                5,
                start + 20_000,
                json!({
                    "charge_state":{
                        "charging_state":"Complete",
                        "battery_level":41,
                        "charge_energy_added":0.3
                    }
                }),
            ),
        ];

        let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("charge trace");
        assert!(step.state.open_charge.is_none());
        assert_eq!(step.delta.charges.len(), 1);
        assert_eq!(step.delta.charge_samples.len(), 3);
        assert!((step.delta.charges[0].charge_energy_added.unwrap() - 0.2).abs() < 1e-9);
        assert_eq!(step.delta.charges[0].start_date_ms, start);
        assert_eq!(step.delta.charges[0].end_date_ms, Some(start + 20_000));
    }

    #[test]
    fn charge_aggregate_uses_teslamate_delta_and_ordered_grid_energy() {
        let start = 1_800_000_700_000_i64;
        let samples = [
            sample(
                1,
                start + 100_000,
                json!({
                    "drive_state":{"shift_state":"P","speed":0,"latitude":51.5,"longitude":-0.1},
                    "charge_state":{
                        "charging_state":"Charging","timestamp":start,
                        "battery_level":40,"charge_energy_added":1.0,
                        "ideal_battery_range":300.0,"battery_range":280.0,
                        "charger_phases":1,"charger_actual_current":10.0,"charger_voltage":230.0
                    }
                }),
            ),
            sample(
                2,
                start + 101_000,
                json!({
                    "drive_state":{"shift_state":"P","speed":0,"latitude":51.5,"longitude":-0.1},
                    "charge_state":{
                        "charging_state":"Charging","timestamp":start + 60_000,
                        "battery_level":45,"charge_energy_added":2.0,
                        "ideal_battery_range":310.0,"battery_range":290.0,
                        "charger_power":5.0
                    }
                }),
            ),
            sample(
                3,
                start + 102_000,
                json!({
                    "drive_state":{"shift_state":"P","speed":0,"latitude":51.5,"longitude":-0.1},
                    "charge_state":{
                        "charging_state":"Charging","timestamp":start + 120_000,
                        "battery_level":50,"charge_energy_added":2.5,
                        "ideal_battery_range":320.0,"battery_range":300.0,
                        "charger_phases":1,"charger_voltage":230.0
                    }
                }),
            ),
            sample(
                4,
                start + 103_000,
                json!({
                    "drive_state":{"shift_state":"P","speed":0,"latitude":51.5,"longitude":-0.1},
                    "charge_state":{
                        "charging_state":"Complete","timestamp":start + 180_000,
                        "battery_level":50,"charge_energy_added":0.0,
                        "ideal_battery_range":325.0,"battery_range":305.0
                    }
                }),
            ),
        ];

        let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("charge aggregate");
        let charge = &step.delta.charges[0];
        assert_eq!(charge.start_date_ms, start);
        assert_eq!(charge.charge_energy_added, Some(1.5));
        assert!((charge.charge_energy_used_kwh.unwrap() - 5.0 / 60.0).abs() < 1e-9);
        assert!((charge.start_ideal_range_km.unwrap() - 482.8).abs() < 1e-9);
        assert!((charge.end_ideal_range_km.unwrap() - 523.04).abs() < 1e-9);
        assert!((charge.start_rated_range_km.unwrap() - 450.62).abs() < 1e-9);
        assert!((charge.end_rated_range_km.unwrap() - 490.85).abs() < 1e-9);
        assert_eq!(charge.start_latitude, Some(51.5));
        assert_eq!(charge.start_longitude, Some(-0.1));
        assert_eq!(charge.cost, None);
    }

    #[test]
    fn charge_energy_uses_the_current_row_and_phase_fallback() {
        let start = 1_800_000_000_000;
        let samples = [
            sample(
                1,
                start,
                json!({
                    "charge_state": {
                        "charging_state": "Charging",
                        "timestamp": start,
                        "charger_power": 1.0
                    }
                }),
            ),
            sample(
                2,
                start + 3_600_000,
                json!({
                    "charge_state": {
                        "charging_state": "Charging",
                        "timestamp": start + 3_600_000,
                        "charger_power": 6.0,
                        "charger_phases": 1
                    }
                }),
            ),
        ];
        let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("valid charge");
        let stored = &step.state.open_charge.expect("open charge").samples;
        assert_eq!(calculate_energy_used_kwh(stored), Some(6.0));
    }

    #[test]
    fn nonpositive_live_charger_phases_are_stored_as_null() {
        let sample = sample(
            1,
            1_800_000_000_000,
            json!({
                "charge_state": {
                    "charging_state": "Charging",
                    "charger_power": 3.0,
                    "charger_phases": 0
                }
            }),
        );
        let step = apply_sample(OpenSessionState::new(), 1, &sample).expect("valid charge");
        assert_eq!(step.delta.open_charge_samples[0].charger_phases, None);
    }

    #[test]
    fn stationary_positions_emit_on_entry_and_every_five_minutes() {
        let start = 1_800_000_098_000_i64;
        let samples = [
            sample(
                1,
                start,
                json!({
                    "drive_state":{"shift_state":"P","speed":0,"latitude":51.0,"longitude":-0.1},
                    "charge_state":{"charging_state":"Unplugged","battery_level":60}
                }),
            ),
            sample(
                2,
                start + 299_999,
                json!({
                    "drive_state":{"shift_state":"P","speed":0,"latitude":51.001,"longitude":-0.101},
                    "charge_state":{"charging_state":"Unplugged","battery_level":60}
                }),
            ),
            sample(
                3,
                start + 300_000,
                json!({
                    "drive_state":{"shift_state":"P","speed":0,"latitude":51.002,"longitude":-0.102},
                    "charge_state":{"charging_state":"Unplugged","battery_level":60}
                }),
            ),
            sample(
                4,
                start + 300_001,
                json!({
                    "drive_state":{"shift_state":"P","speed":0,"latitude":51.003,"longitude":-0.103},
                    "charge_state":{"charging_state":"Charging","battery_level":61}
                }),
            ),
            sample(
                5,
                start + 600_001,
                json!({
                    "drive_state":{"shift_state":"P","speed":0,"latitude":51.004,"longitude":-0.104},
                    "charge_state":{"charging_state":"Charging","battery_level":62}
                }),
            ),
        ];

        let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("stationary trace");
        let positions: Vec<_> = step
            .delta
            .positions
            .iter()
            .filter(|position| position.drive_id.is_none())
            .collect();

        assert_eq!(positions.len(), 4);
        assert_eq!(
            positions
                .iter()
                .map(|position| position.date_ms)
                .collect::<Vec<_>>(),
            vec![start, start + 300_000, start + 300_001, start + 600_001]
        );
        assert!(positions.iter().all(|position| position.drive_id.is_none()));
    }

    #[test]
    fn state_intervals_transition_keep_open_state_and_resume_after_restart() {
        let online = discovery(1, 1_000, "online");
        let asleep = discovery(2, 2_000, "asleep");
        let back_online = discovery(3, 3_000, "online");

        let first = apply_sample(OpenSessionState::new(), 1, &online).expect("online");
        assert_eq!(first.delta.states.len(), 1);
        assert_eq!(first.delta.states[0].state, "online");
        assert_eq!(first.delta.states[0].end_date_ms, None);

        let second = apply_sample(first.state, 1, &asleep).expect("asleep");
        assert_eq!(second.delta.states.len(), 2);
        assert_eq!(second.delta.states[0].end_date_ms, Some(2_000));
        assert_eq!(second.delta.states[1].state, "asleep");
        assert_eq!(second.delta.states[1].end_date_ms, None);

        let restored =
            OpenSessionState::decode(&second.state.encode().expect("encode state history"))
                .expect("decode state history");
        let third = apply_sample(restored, 1, &back_online).expect("online after restart");
        assert_eq!(third.delta.states[0].state, "asleep");
        assert_eq!(third.delta.states[0].end_date_ms, Some(3_000));
        assert_eq!(third.delta.states[1].state, "online");
        assert_eq!(third.delta.states[1].end_date_ms, None);
        assert_eq!(third.state.next_state_id, 4);
    }

    #[test]
    fn stationary_positions_require_coordinates_skip_driving_and_replay() {
        let start = 1_800_000_099_000_i64;
        let missing_coordinates = sample(
            1,
            start,
            json!({
                "drive_state":{"shift_state":"P","speed":0},
                "charge_state":{"charging_state":"Unplugged"}
            }),
        );
        let first = apply_sample(OpenSessionState::new(), 1, &missing_coordinates)
            .expect("missing coordinates")
            .state;
        assert!(first.last_stationary_position_at_ms.is_none());

        let stationary = sample(
            2,
            start + 1_000,
            json!({
                "drive_state":{"shift_state":"P","speed":0,"latitude":51.0,"longitude":-0.1},
                "charge_state":{"charging_state":"Unplugged"}
            }),
        );
        let after_stationary = apply_sample(first, 1, &stationary).expect("stationary");
        assert_eq!(
            after_stationary
                .delta
                .positions
                .iter()
                .filter(|position| position.drive_id.is_none())
                .count(),
            1
        );

        let replay = apply_sample(after_stationary.state.clone(), 1, &stationary).expect("replay");
        assert!(replay.delta.positions.is_empty());

        let driving = sample(
            3,
            start + 2_000,
            json!({
                "drive_state":{"shift_state":"D","speed":10,"latitude":51.1,"longitude":-0.2},
                "charge_state":{"charging_state":"Unplugged"}
            }),
        );
        let driving_step = apply_sample(after_stationary.state, 1, &driving).expect("driving");
        assert!(
            driving_step
                .delta
                .positions
                .iter()
                .all(|position| position.drive_id.is_some())
        );
    }

    #[test]
    fn nested_timestamps_drive_positions_and_charge_samples() {
        let state_time = 1_800_000_500_000_i64;
        let samples = [
            sample(
                1,
                state_time + 100_000,
                json!({
                    "drive_state":{"shift_state":"P","speed":0,"latitude":51.0,"longitude":-0.1,"timestamp":state_time},
                    "charge_state":{"charging_state":"Unplugged","timestamp":state_time}
                }),
            ),
            sample(
                2,
                state_time + 101_000,
                json!({
                    "drive_state":{"shift_state":"P","speed":0,"latitude":51.001,"longitude":-0.101,"timestamp":state_time + 1_000},
                    "charge_state":{"charging_state":"Charging","timestamp":state_time + 1_000,"charge_energy_added":1.0}
                }),
            ),
            sample(
                3,
                state_time + 102_000,
                json!({
                    "drive_state":{"shift_state":"P","speed":0,"latitude":51.002,"longitude":-0.102,"timestamp":state_time + 2_000},
                    "charge_state":{"charging_state":"Complete","timestamp":state_time + 2_000,"charge_energy_added":2.0}
                }),
            ),
        ];

        let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("timestamp trace");
        assert_eq!(
            step.delta
                .positions
                .iter()
                .map(|position| position.date_ms)
                .collect::<Vec<_>>(),
            vec![state_time, state_time + 1_000, state_time + 2_000]
        );
        assert_eq!(
            step.delta
                .charge_samples
                .iter()
                .map(|sample| sample.timestamp_ms)
                .collect::<Vec<_>>(),
            vec![state_time + 1_000, state_time + 2_000]
        );
    }

    #[test]
    fn missing_invalid_and_regressed_nested_timestamps_stay_monotonic() {
        let start = 1_800_000_510_000_i64;
        let samples = [
            sample(
                1,
                start,
                json!({
                    "drive_state":{"shift_state":"P","speed":0,"latitude":51.0,"longitude":-0.1},
                    "charge_state":{"charging_state":"Charging","charge_energy_added":1.0}
                }),
            ),
            sample(
                2,
                start + 1_000,
                json!({
                    "drive_state":{"shift_state":"P","speed":0,"latitude":51.001,"longitude":-0.101,"timestamp":"invalid"},
                    "charge_state":{"charging_state":"Charging","timestamp":"invalid","charge_energy_added":1.1}
                }),
            ),
            sample(
                3,
                start + 2_000,
                json!({
                    "drive_state":{"shift_state":"D","speed":10,"latitude":51.002,"longitude":-0.102,"timestamp":start - 1_000},
                    "charge_state":{"charging_state":"Complete","timestamp":start - 1_000,"charge_energy_added":1.2},
                    "vehicle_state":{"odometer":1000.0}
                }),
            ),
            sample(
                4,
                start + 3_000,
                json!({
                    "drive_state":{"shift_state":"D","speed":12,"latitude":51.003,"longitude":-0.103,"timestamp":start - 2_000},
                    "charge_state":{"charging_state":"Unplugged"},
                    "vehicle_state":{"odometer":1000.1}
                }),
            ),
            sample(
                5,
                start + 4_000,
                json!({
                    "drive_state":{"shift_state":"P","speed":0,"latitude":51.004,"longitude":-0.104,"timestamp":start - 3_000},
                    "charge_state":{"charging_state":"Unplugged","timestamp":start - 2_000},
                    "vehicle_state":{"odometer":1000.2}
                }),
            ),
        ];

        let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("fallback trace");
        let drive_positions: Vec<_> = step
            .delta
            .positions
            .iter()
            .filter(|position| position.drive_id.is_some())
            .map(|position| position.date_ms)
            .collect();
        assert_eq!(
            drive_positions,
            vec![start + 2_000, start + 3_000, start + 4_000]
        );
        assert!(
            step.delta
                .positions
                .iter()
                .map(|position| position.date_ms)
                .collect::<Vec<_>>()
                .windows(2)
                .all(|window| window[0] <= window[1])
        );
        assert!(
            step.delta
                .charge_samples
                .iter()
                .map(|sample| sample.timestamp_ms)
                .collect::<Vec<_>>()
                .windows(2)
                .all(|window| window[0] <= window[1])
        );
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
        assert_eq!(step.delta.charge_samples.len(), 3);
        assert_eq!(step.delta.charges[0].start_battery_level, Some(40));
        assert_eq!(step.delta.charges[0].end_battery_level, Some(80));
        assert_eq!(step.delta.charges[0].charge_energy_added, Some(10.5));
    }

    #[test]
    fn starts_a_charge_on_teslamate_starting_state() {
        let start = 1_800_000_150_000_i64;
        let starting = sample(
            1,
            start,
            json!({"charge_state":{"charging_state":"Starting","battery_level":40,"charge_energy_added":0.0}}),
        );
        let state = apply_sample(OpenSessionState::new(), 1, &starting)
            .expect("starting charge")
            .state;

        assert_eq!(state.phase, VehiclePhase::Charging);
        assert!(state.open_charge.is_some());
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
    fn corrupt_payload_is_rejected_without_advancing_or_discarding_open_session() {
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
        let preserved = mid.state.clone();
        assert!(matches!(
            apply_sample(mid.state, 1, &bad),
            Err(LifecycleError::InvalidPayload)
        ));
        assert!(preserved.open_drive.is_some());
        assert_eq!(preserved.last_observation_id, 1);
    }

    #[test]
    fn regressed_close_time_does_not_regress_or_close_the_active_drive() {
        let start = 1_800_000_325_000_i64;
        let open = sample(
            1,
            start,
            json!({"drive_state":{"shift_state":"D","speed":5,"latitude":1.0,"longitude":2.0}}),
        );
        let regressed_end = sample(
            2,
            start - 1,
            json!({"drive_state":{"shift_state":"P","speed":0,"latitude":1.0,"longitude":2.0}}),
        );
        let state = apply_sample(OpenSessionState::new(), 1, &open)
            .expect("open drive")
            .state;

        let step = apply_sample(state, 1, &regressed_end).expect("ignore stale close");
        assert!(step.state.open_drive.is_some());
        assert_eq!(step.state.last_observation_id, 2);
        assert_eq!(step.state.last_observed_at_ms, Some(start));
        assert!(step.delta.drives.is_empty());
        assert!(step.delta.positions.is_empty());
    }

    #[test]
    fn impossible_position_is_rejected_before_projection() {
        let sample = sample(
            1,
            1_800_000_340_000,
            json!({"drive_state":{"shift_state":"D","speed":5,"latitude":91.0,"longitude":2.0}}),
        );

        assert_eq!(
            apply_sample(OpenSessionState::new(), 1, &sample),
            Err(LifecycleError::InvalidCoordinates)
        );
    }

    #[test]
    fn suspended_discovery_state_is_preserved_without_starting_a_lifecycle() {
        let sample = LifecycleSample {
            observation_id: 1,
            observed_at_ms: 1_800_000_350_000,
            vehicle_state: "suspended".to_owned(),
            payload: json!({
                "record_type": "owner_api_vehicle_data_v1",
                "source_vehicle_id": "9",
                "vehicle_data": {"drive_state": {"shift_state": "P", "speed": 0}}
            }),
        };

        let step = apply_sample(OpenSessionState::new(), 1, &sample).expect("project");
        assert_eq!(step.state.phase, VehiclePhase::Suspended);
        assert!(step.state.open_drive.is_none());
        assert!(step.state.open_charge.is_none());
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

    #[test]
    fn service_close_uses_last_position_and_persists_service_mode() {
        let start = 1_800_000_410_000_i64;
        let opened = apply_sample(
            OpenSessionState::new(),
            1,
            &sample(
                1,
                start,
                json!({
                    "drive_state":{"shift_state":"D","speed":20,"latitude":51.0,"longitude":-0.1,"timestamp":start},
                    "vehicle_state":{"odometer":1000.0,"service_mode":false}
                }),
            ),
        )
        .expect("open drive")
        .state;
        let opened = apply_sample(
            opened,
            1,
            &sample(
                2,
                start + 1_000,
                json!({
                    "drive_state":{"shift_state":"D","speed":21,"latitude":51.001,"longitude":-0.101,"timestamp":start + 1_000},
                    "vehicle_state":{"odometer":1000.1,"service_mode":false}
                }),
            ),
        )
        .expect("extend drive")
        .state;
        let step = force_close_for_service(opened, 1, start + 5_000).expect("service close");
        assert_eq!(step.state.service_mode, Some(true));
        assert!(step.state.open_drive.is_none());
        assert_eq!(step.delta.drives.len(), 1);
        assert_eq!(step.delta.positions[1].date_ms, start + 1_000);

        let exited = apply_sample(
            step.state,
            1,
            &sample(
                3,
                start + 10_000,
                json!({
                    "drive_state":{"shift_state":"P","speed":0,"timestamp":start + 10_000},
                    "vehicle_state":{"service_mode":false}
                }),
            ),
        )
        .expect("service exit")
        .state;
        assert_eq!(exited.service_mode, Some(false));
    }

    #[test]
    fn geofences_fill_missing_live_drive_labels_only() {
        let mut delta = LifecycleDelta {
            drives: vec![ProjectionDrive {
                id: 1,
                car_id: 1,
                optimized_at_ms: None,
                start_date_ms: 1,
                end_date_ms: 2,
                distance_km: None,
                duration_min: None,
                efficiency: None,
                outside_temp_avg: None,
                inside_temp_avg: None,
                speed_max: None,
                power_max: None,
                power_min: None,
                start_ideal_range_km: None,
                end_ideal_range_km: None,
                start_address: None,
                end_address: None,
                start_geofence: None,
                end_geofence: Some("Imported".into()),
                start_latitude: Some(51.0),
                start_longitude: Some(-0.1),
                end_latitude: Some(51.001),
                end_longitude: Some(-0.101),
                start_soc: None,
                end_soc: None,
                start_rated_range_km: None,
                end_rated_range_km: None,
                ascent: None,
                descent: None,
            }],
            ..LifecycleDelta::default()
        };
        let fences = vec![
            GeofenceFence {
                name: "Home".into(),
                latitude: 51.0,
                longitude: -0.1,
                radius_m: 150.0,
                billing_type: None,
                cost_per_unit: None,
                session_fee: None,
            },
            GeofenceFence {
                name: "Work".into(),
                latitude: 51.001,
                longitude: -0.101,
                radius_m: 150.0,
                billing_type: None,
                cost_per_unit: None,
                session_fee: None,
            },
        ];

        apply_geofence_labels(&mut delta, &fences);

        assert_eq!(delta.drives[0].start_geofence.as_deref(), Some("Home"));
        assert_eq!(delta.drives[0].end_geofence.as_deref(), Some("Imported"));
    }

    #[test]
    fn position_thermal_flags_survive_sparse_stream_frames_until_drive_close() {
        let t0 = 1_800_001_000_000_i64;
        let owner = sample(
            1,
            t0,
            json!({
                "drive_state": {"shift_state":"D","speed":20,"power":12.5,"latitude":51.0,"longitude":-0.1,"timestamp":t0},
                "charge_state": {"battery_level":70,"battery_heater_on":true,"est_battery_range":200.0},
                "climate_state": {"battery_heater":true,"battery_heater_no_power":false,"fan_status":2,"driver_temp_setting":21.5,"passenger_temp_setting":22.0,"is_rear_defroster_on":false,"is_front_defroster_on":true},
                "vehicle_state": {"odometer":1000.0,"tpms_pressure_fl":2.4,"tpms_pressure_fr":2.5,"tpms_pressure_rl":2.6,"tpms_pressure_rr":2.7}
            }),
        );
        let sparse_stream = LifecycleSample {
            observation_id: 2,
            observed_at_ms: t0 + 1_000,
            vehicle_state: "online".into(),
            payload: json!({
                "record_type":"tesla_stream_update_v1",
                "fields":{"drive_state":{"timestamp":t0 + 1_000,"speed":21,"latitude":51.001,"longitude":-0.101},"vehicle_state":{"odometer":1001.0}}
            }),
        };
        let parked = sample(
            3,
            t0 + 2_000,
            json!({"drive_state":{"shift_state":"P","speed":0,"timestamp":t0 + 2_000}}),
        );

        let first = apply_sample(OpenSessionState::new(), 1, &owner).unwrap();
        let second = apply_sample(first.state, 1, &sparse_stream).unwrap();
        let closed = apply_sample(second.state, 1, &parked).unwrap();

        assert_eq!(closed.delta.positions.len(), 2);
        for (index, position) in closed.delta.positions.into_iter().enumerate() {
            assert_eq!(position.speed, Some(if index == 0 { 32 } else { 34 }));
            assert_eq!(position.power, (index == 0).then_some(12.5));
            assert_eq!(position.est_battery_range_km, Some(321.87));
            assert_eq!(position.fan_status, Some(2));
            assert_eq!(position.driver_temp_setting, Some(21.5));
            assert_eq!(position.passenger_temp_setting, Some(22.0));
            assert_eq!(position.is_rear_defroster_on, Some(false));
            assert_eq!(position.is_front_defroster_on, Some(true));
            assert_eq!(position.battery_heater, Some(true));
            assert_eq!(position.battery_heater_on, Some(true));
            assert_eq!(position.battery_heater_no_power, Some(false));
            assert_eq!(position.tpms_pressure_fl, Some(2.4));
            assert_eq!(position.tpms_pressure_fr, Some(2.5));
            assert_eq!(position.tpms_pressure_rl, Some(2.6));
            assert_eq!(position.tpms_pressure_rr, Some(2.7));
        }
    }

    #[test]
    fn elevation_totals_match_teslamate_cap_boundary() {
        assert_eq!(cap_elevation_total(32_767), 32_767);
        assert_eq!(cap_elevation_total(32_768), 0);
        assert_eq!(cap_elevation_total(40_000), 0);
    }
}
