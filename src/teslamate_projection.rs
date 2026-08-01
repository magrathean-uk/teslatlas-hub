//! TeslaMate-history projection into the typed Hub snapshot contract.
//!
//! A PostgreSQL reader will decode only the fixed schema-contract projections
//! into these source values. This module then makes the lossy boundaries
//! explicit: only completed drives are included, while standalone positions
//! remain valid history and an in-progress drive remains for the next
//! snapshot rather than being fabricated as finished history.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::hub_pack::{
    GeofenceBillingType, ProjectionCar, ProjectionCarSettings, ProjectionCharge,
    ProjectionChargeSample, ProjectionDrive, ProjectionPosition, ProjectionSnapshot,
    ProjectionState,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeslaMateHistory {
    pub cars: Vec<TeslaMateCar>,
    pub drives: Vec<TeslaMateDrive>,
    pub positions: Vec<TeslaMatePosition>,
    pub charging_processes: Vec<TeslaMateChargingProcess>,
    pub charges: Vec<TeslaMateCharge>,
    pub addresses: Vec<TeslaMateAddress>,
    pub geofences: Vec<TeslaMateGeofence>,
    pub states: Vec<TeslaMateState>,
    pub updates: Vec<TeslaMateUpdate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TeslaMateSourceWatermark {
    pub max_id: Option<i64>,
    pub max_timestamp_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TeslaMateSourceWatermarks {
    pub drives: TeslaMateSourceWatermark,
    pub positions: TeslaMateSourceWatermark,
    pub charging_processes: TeslaMateSourceWatermark,
    pub charges: TeslaMateSourceWatermark,
    pub states: TeslaMateSourceWatermark,
    pub updates: TeslaMateSourceWatermark,
}

/// Open TeslaMate activity captured from one repeatable-read source snapshot.
/// This is an import bridge type only; completed pack projection does not use it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TeslaMateOpenSession {
    pub car_id: i64,
    pub drive: Option<TeslaMateDrive>,
    pub drive_positions: Vec<TeslaMatePosition>,
    pub charge: Option<TeslaMateChargingProcess>,
    pub charge_samples: Vec<TeslaMateCharge>,
    pub state: Option<TeslaMateState>,
    pub standalone_positions: Vec<TeslaMatePosition>,
    pub watermarks: TeslaMateSourceWatermarks,
}

impl TeslaMateOpenSession {
    pub fn validate(&self) -> Result<(), TeslaMateProjectionError> {
        if self.car_id <= 0 {
            return Err(TeslaMateProjectionError::InvalidId {
                entity: "car",
                id: self.car_id,
            });
        }
        if let Some(drive) = &self.drive {
            require_selected_car("open drive", drive.id, drive.car_id, self.car_id)?;
            if drive.end_date_ms.is_some() {
                return Err(TeslaMateProjectionError::InvalidValue {
                    field: "open_drive.end_date_ms",
                });
            }
        }
        for position in &self.drive_positions {
            require_selected_car("open position", position.id, position.car_id, self.car_id)?;
            if position.drive_id != self.drive.as_ref().map(|drive| drive.id) {
                return Err(TeslaMateProjectionError::MissingRelated {
                    field: "open_drive.position.drive_id",
                    id: position.drive_id.unwrap_or_default(),
                });
            }
        }
        for position in &self.standalone_positions {
            require_selected_car(
                "standalone position",
                position.id,
                position.car_id,
                self.car_id,
            )?;
            if position.drive_id.is_some() {
                return Err(TeslaMateProjectionError::InvalidValue {
                    field: "standalone_position.drive_id",
                });
            }
        }
        if let Some(charge) = &self.charge {
            require_selected_car(
                "open charging process",
                charge.id,
                charge.car_id,
                self.car_id,
            )?;
            if charge.end_date_ms.is_some() {
                return Err(TeslaMateProjectionError::InvalidValue {
                    field: "open_charge.end_date_ms",
                });
            }
        }
        for sample in &self.charge_samples {
            if sample.id <= 0
                || sample.charging_process_id
                    != self
                        .charge
                        .as_ref()
                        .map(|charge| charge.id)
                        .unwrap_or_default()
            {
                return Err(TeslaMateProjectionError::MissingRelated {
                    field: "open_charge.sample.charging_process_id",
                    id: sample.charging_process_id,
                });
            }
        }
        if let Some(state) = &self.state {
            require_selected_car("open state", state.id, state.car_id, self.car_id)?;
            if state.end_date_ms.is_some() {
                return Err(TeslaMateProjectionError::InvalidValue {
                    field: "open_state.end_date_ms",
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeslaMateCar {
    pub id: i64,
    pub eid: i64,
    #[serde(default)]
    pub vid: Option<i64>,
    pub vin: Option<String>,
    pub name: Option<String>,
    pub model: Option<String>,
    pub trim_badging: Option<String>,
    pub marketing_name: Option<String>,
    #[serde(default)]
    pub exterior_color: Option<String>,
    #[serde(default)]
    pub wheel_type: Option<String>,
    #[serde(default)]
    pub spoiler_type: Option<String>,
    pub efficiency_wh_per_km: Option<f64>,
    #[serde(default)]
    pub settings: ProjectionCarSettings,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeslaMateDrive {
    pub id: i64,
    pub car_id: i64,
    pub start_date_ms: i64,
    pub end_date_ms: Option<i64>,
    pub start_position_id: Option<i64>,
    pub end_position_id: Option<i64>,
    pub start_address_id: Option<i64>,
    pub end_address_id: Option<i64>,
    pub start_geofence_id: Option<i64>,
    pub end_geofence_id: Option<i64>,
    pub outside_temp_avg: Option<f64>,
    #[serde(default)]
    pub inside_temp_avg: Option<f64>,
    pub speed_max: Option<i64>,
    #[serde(default)]
    pub power_max: Option<f64>,
    #[serde(default)]
    pub power_min: Option<f64>,
    #[serde(default)]
    pub start_ideal_range_km: Option<f64>,
    #[serde(default)]
    pub end_ideal_range_km: Option<f64>,
    pub start_rated_range_km: Option<f64>,
    pub end_rated_range_km: Option<f64>,
    pub start_km: Option<f64>,
    pub end_km: Option<f64>,
    pub distance_km: Option<f64>,
    pub duration_min: Option<i64>,
    #[serde(default)]
    pub ascent: Option<i64>,
    #[serde(default)]
    pub descent: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeslaMatePosition {
    pub id: i64,
    pub car_id: i64,
    pub drive_id: Option<i64>,
    pub date_ms: i64,
    pub latitude: f64,
    pub longitude: f64,
    pub elevation: Option<i64>,
    pub speed: Option<i64>,
    pub power: Option<f64>,
    pub odometer: Option<f64>,
    pub ideal_battery_range_km: Option<f64>,
    pub est_battery_range_km: Option<f64>,
    pub rated_battery_range_km: Option<f64>,
    pub battery_level: Option<i64>,
    pub usable_battery_level: Option<i64>,
    pub fan_status: Option<i64>,
    pub driver_temp_setting: Option<f64>,
    pub passenger_temp_setting: Option<f64>,
    pub is_climate_on: Option<bool>,
    pub is_rear_defroster_on: Option<bool>,
    pub is_front_defroster_on: Option<bool>,
    pub outside_temp: Option<f64>,
    pub inside_temp: Option<f64>,
    pub battery_heater: Option<bool>,
    pub battery_heater_on: Option<bool>,
    pub battery_heater_no_power: Option<bool>,
    pub tpms_pressure_fl: Option<f64>,
    pub tpms_pressure_fr: Option<f64>,
    pub tpms_pressure_rl: Option<f64>,
    pub tpms_pressure_rr: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeslaMateChargingProcess {
    pub id: i64,
    pub car_id: i64,
    pub position_id: Option<i64>,
    pub address_id: Option<i64>,
    pub geofence_id: Option<i64>,
    pub start_date_ms: i64,
    pub end_date_ms: Option<i64>,
    pub charge_energy_added: Option<f64>,
    #[serde(default)]
    pub charge_energy_used_kwh: Option<f64>,
    #[serde(default)]
    pub start_ideal_range_km: Option<f64>,
    #[serde(default)]
    pub end_ideal_range_km: Option<f64>,
    pub start_battery_level: Option<i64>,
    pub end_battery_level: Option<i64>,
    pub duration_min: Option<i64>,
    pub outside_temp_avg: Option<f64>,
    pub start_rated_range_km: Option<f64>,
    pub end_rated_range_km: Option<f64>,
    #[serde(default)]
    pub cost: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeslaMateCharge {
    pub id: i64,
    pub charging_process_id: i64,
    pub date_ms: i64,
    pub battery_heater: Option<bool>,
    pub battery_heater_on: Option<bool>,
    pub battery_heater_no_power: Option<bool>,
    pub battery_level: Option<i64>,
    pub usable_battery_level: Option<i64>,
    pub charge_energy_added_kwh: Option<f64>,
    pub charger_actual_current: Option<f64>,
    pub charger_phases: Option<i64>,
    pub charger_pilot_current: Option<f64>,
    pub charger_power_kw: Option<f64>,
    pub charger_voltage: Option<f64>,
    pub charge_cable: Option<String>,
    pub fast_charger_present: Option<bool>,
    pub fast_charger_brand: Option<String>,
    pub fast_charger_type: Option<String>,
    pub ideal_range_km: Option<f64>,
    pub rated_range_km: Option<f64>,
    pub not_enough_power_to_heat: Option<bool>,
    pub outside_temp_c: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeslaMateAddress {
    pub id: i64,
    pub display_name: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeslaMateGeofence {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub latitude: Option<f64>,
    #[serde(default)]
    pub longitude: Option<f64>,
    #[serde(default)]
    pub radius_m: Option<f64>,
    #[serde(default)]
    pub billing_type: Option<GeofenceBillingType>,
    #[serde(default)]
    pub cost_per_unit: Option<f64>,
    #[serde(default)]
    pub session_fee: Option<f64>,
}

impl TeslaMateGeofence {
    pub fn valid_geometry(&self) -> Option<(f64, f64, f64)> {
        let (Some(latitude), Some(longitude), Some(radius_m)) =
            (self.latitude, self.longitude, self.radius_m)
        else {
            return None;
        };
        (latitude.is_finite()
            && longitude.is_finite()
            && radius_m.is_finite()
            && (-90.0..=90.0).contains(&latitude)
            && (-180.0..=180.0).contains(&longitude)
            && (0.0..=5_000.0).contains(&radius_m))
        .then_some((latitude, longitude, radius_m))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeslaMateState {
    pub id: i64,
    pub car_id: i64,
    pub state: String,
    pub start_date_ms: i64,
    pub end_date_ms: Option<i64>,
}

/// Map one TeslaMate state for the selected car. The source enum text is
/// intentionally preserved; unknown values are rejected rather than mapped
/// into a different state.
pub fn project_state(
    state: &TeslaMateState,
    selected_car_id: i64,
) -> Result<Option<ProjectionState>, TeslaMateProjectionError> {
    require_selected_car("state", state.id, state.car_id, selected_car_id)?;
    if !matches!(state.state.as_str(), "online" | "offline" | "asleep") {
        return Err(TeslaMateProjectionError::InvalidValue {
            field: "state.state",
        });
    }
    Ok(Some(ProjectionState {
        id: state.id,
        car_id: selected_car_id,
        state: state.state.clone(),
        start_date_ms: state.start_date_ms,
        end_date_ms: state.end_date_ms,
    }))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeslaMateUpdate {
    pub id: i64,
    pub car_id: i64,
    pub start_date_ms: i64,
    pub end_date_ms: Option<i64>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProjectionReport {
    pub completed_drives: u64,
    pub skipped_open_drives: u64,
    pub skipped_unattached_positions: u64,
    pub projected_positions: u64,
    pub projected_charges: u64,
    pub projected_charge_samples: u64,
}

impl ProjectionReport {
    pub fn logical_row_count(&self) -> Option<u64> {
        1_u64
            .checked_add(self.completed_drives)?
            .checked_add(self.projected_positions)?
            .checked_add(self.projected_charges)?
            .checked_add(self.projected_charge_samples)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TeslaMateProjection {
    pub snapshot: ProjectionSnapshot,
    /// State rows live in the first pack fragment rather than the projection
    /// snapshot payload, so callers cannot accidentally omit them at publish.
    pub states: Vec<ProjectionState>,
    pub report: ProjectionReport,
}

/// Order-independent aggregate for a process's staged samples. It keeps only
/// the facts needed by the parent charge row, so a producer never needs an
/// unbounded session vector merely to derive charge metadata.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChargeProjectionFacts {
    is_dc: Option<bool>,
    max_charger_power_kw: Option<f64>,
    fast_charger_type: Option<String>,
    first_sample: Option<(i64, i64)>,
    last_sample: Option<(i64, i64)>,
    first_energy: Option<((i64, i64), f64)>,
    last_energy: Option<((i64, i64), f64)>,
    last_battery_level: Option<((i64, i64), i64)>,
    first_rated_range_km: Option<((i64, i64), f64)>,
    last_rated_range_km: Option<((i64, i64), f64)>,
}

impl ChargeProjectionFacts {
    pub fn observe(&mut self, sample: &TeslaMateCharge) {
        let order = (sample.date_ms, sample.id);
        self.first_sample = Some(
            self.first_sample
                .map_or(order, |current| current.min(order)),
        );
        self.last_sample = Some(self.last_sample.map_or(order, |current| current.max(order)));
        if let Some(value) = sample.fast_charger_present {
            self.is_dc = Some(self.is_dc.unwrap_or(false).max(value));
        }
        if let Some(value) = sample.charger_power_kw {
            self.max_charger_power_kw = Some(
                self.max_charger_power_kw
                    .map_or(value, |current| current.max(value)),
            );
        }
        if sample.charger_power_kw.is_some_and(|value| value > 0.0)
            && sample.fast_charger_type.is_some()
        {
            self.fast_charger_type = sample.fast_charger_type.clone();
        }
        update_first(
            &mut self.first_energy,
            order,
            sample.charge_energy_added_kwh,
        );
        update_last(&mut self.last_energy, order, sample.charge_energy_added_kwh);
        update_last(&mut self.last_battery_level, order, sample.battery_level);
        update_first(&mut self.first_rated_range_km, order, sample.rated_range_km);
        update_last(&mut self.last_rated_range_km, order, sample.rated_range_km);
    }

    pub fn from_samples(samples: &[&TeslaMateCharge]) -> Self {
        let mut facts = Self::default();
        for sample in samples {
            facts.observe(sample);
        }
        facts
    }

    fn energy_added(&self) -> Option<f64> {
        let first = self.first_energy?.1;
        let last = self.last_energy?.1;
        let energy_added = last - first;
        (energy_added.is_finite() && energy_added >= 0.0).then_some(energy_added)
    }
}

/// Map the one source-owned vehicle row that every full-snapshot fragment
/// repeats. Keeping this separate lets the staged producer hold only one
/// source page at a time.
pub fn project_car(
    car: &TeslaMateCar,
    firmware_version: Option<String>,
) -> Result<ProjectionCar, TeslaMateProjectionError> {
    if car.id <= 0 {
        return Err(TeslaMateProjectionError::InvalidId {
            entity: "car",
            id: car.id,
        });
    }
    let name = first_nonblank([car.name.as_deref()]).unwrap_or_else(|| format!("Car {}", car.id));
    let raw_model = first_nonblank([car.model.as_deref()]);
    let model = raw_model
        .as_deref()
        .map(crate::hub_pack::normalize_tesla_model_code)
        .unwrap_or_else(|| "Unknown Tesla".to_owned());
    let trim_badging = car
        .trim_badging
        .as_deref()
        .map(crate::hub_pack::normalize_tesla_trim);
    let marketing_name = car.marketing_name.clone().or_else(|| {
        crate::hub_pack::derive_tesla_marketing_name(
            &model,
            trim_badging.as_deref(),
            raw_model.as_deref(),
        )
    });
    Ok(ProjectionCar {
        id: car.id,
        name,
        model,
        vin: car.vin.clone(),
        source_eid: Some(car.eid),
        source_vid: car.vid,
        trim_badging,
        marketing_name,
        exterior_color: car.exterior_color.clone(),
        wheel_type: car.wheel_type.clone(),
        spoiler_type: car.spoiler_type.clone(),
        firmware_version,
        efficiency_wh_per_km: normalise_efficiency(car.efficiency_wh_per_km)?,
        settings: car.settings.clone(),
    })
}

/// Map one completed drive with its fixed endpoint relationships. An open
/// drive deliberately returns `None`, matching the complete-history contract.
#[derive(Debug, Clone, Copy, Default)]
pub struct DriveRelations<'a> {
    pub start_position: Option<&'a TeslaMatePosition>,
    pub end_position: Option<&'a TeslaMatePosition>,
    pub start_address: Option<&'a TeslaMateAddress>,
    pub end_address: Option<&'a TeslaMateAddress>,
    pub start_geofence: Option<&'a TeslaMateGeofence>,
    pub end_geofence: Option<&'a TeslaMateGeofence>,
}

pub fn project_drive(
    drive: &TeslaMateDrive,
    selected_car_id: i64,
    relations: DriveRelations<'_>,
) -> Result<Option<ProjectionDrive>, TeslaMateProjectionError> {
    require_selected_car("drive", drive.id, drive.car_id, selected_car_id)?;
    let Some(end_date_ms) = drive.end_date_ms else {
        return Ok(None);
    };
    let start_position = related_position_value(
        drive.start_position_id,
        relations.start_position,
        selected_car_id,
        "drive.start_position_id",
    )?;
    let end_position = related_position_value(
        drive.end_position_id,
        relations.end_position,
        selected_car_id,
        "drive.end_position_id",
    )?;
    let start_address = related_address_value(
        drive.start_address_id,
        relations.start_address,
        "drive.start_address_id",
    )?;
    let end_address = related_address_value(
        drive.end_address_id,
        relations.end_address,
        "drive.end_address_id",
    )?;
    let start_geofence = related_geofence_value(
        drive.start_geofence_id,
        relations.start_geofence,
        "drive.start_geofence_id",
    )?;
    let end_geofence = related_geofence_value(
        drive.end_geofence_id,
        relations.end_geofence,
        "drive.end_geofence_id",
    )?;
    Ok(Some(ProjectionDrive {
        id: drive.id,
        car_id: selected_car_id,
        optimized_at_ms: None,
        start_date_ms: drive.start_date_ms,
        end_date_ms,
        distance_km: drive.distance_km,
        duration_min: drive.duration_min,
        efficiency: None,
        outside_temp_avg: drive.outside_temp_avg,
        inside_temp_avg: drive.inside_temp_avg,
        speed_max: drive.speed_max,
        power_max: drive.power_max,
        power_min: drive.power_min,
        start_ideal_range_km: drive.start_ideal_range_km,
        end_ideal_range_km: drive.end_ideal_range_km,
        start_address,
        end_address,
        start_geofence,
        end_geofence,
        start_latitude: start_position.map(|position| position.latitude),
        start_longitude: start_position.map(|position| position.longitude),
        end_latitude: end_position.map(|position| position.latitude),
        end_longitude: end_position.map(|position| position.longitude),
        start_soc: start_position.and_then(|position| position.battery_level),
        end_soc: end_position.and_then(|position| position.battery_level),
        start_rated_range_km: drive.start_rated_range_km,
        end_rated_range_km: drive.end_rated_range_km,
        ascent: drive.ascent,
        descent: drive.descent,
    }))
}

/// Map a position from the selected car. Standalone positions are valid
/// history and retain a NULL drive_id. A non-NULL drive_id must belong to a
/// completed drive selected by the caller.
pub fn project_position(
    position: &TeslaMatePosition,
    selected_car_id: i64,
    drive_is_included: bool,
) -> Result<Option<ProjectionPosition>, TeslaMateProjectionError> {
    require_selected_car("position", position.id, position.car_id, selected_car_id)?;
    if position.drive_id.is_some() && !drive_is_included {
        return Ok(None);
    }
    Ok(Some(ProjectionPosition {
        id: position.id,
        drive_id: position.drive_id,
        car_id: selected_car_id,
        date_ms: position.date_ms,
        latitude: position.latitude,
        longitude: position.longitude,
        speed: position.speed,
        power: position.power,
        battery_level: position.battery_level,
        usable_battery_level: position.usable_battery_level,
        elevation: position.elevation,
        odometer: position.odometer,
        ideal_battery_range_km: position.ideal_battery_range_km,
        est_battery_range_km: position.est_battery_range_km,
        rated_battery_range_km: position.rated_battery_range_km,
        fan_status: position.fan_status,
        driver_temp_setting: position.driver_temp_setting,
        passenger_temp_setting: position.passenger_temp_setting,
        is_climate_on: position.is_climate_on,
        is_rear_defroster_on: position.is_rear_defroster_on,
        is_front_defroster_on: position.is_front_defroster_on,
        inside_temp: position.inside_temp,
        outside_temp: position.outside_temp,
        battery_heater: position.battery_heater,
        battery_heater_on: position.battery_heater_on,
        battery_heater_no_power: position.battery_heater_no_power,
        tpms_pressure_fl: position.tpms_pressure_fl,
        tpms_pressure_fr: position.tpms_pressure_fr,
        tpms_pressure_rl: position.tpms_pressure_rl,
        tpms_pressure_rr: position.tpms_pressure_rr,
    }))
}

/// Map one charge session after callers have bounded and ordered its samples.
/// A fragment producer may scan sample pages twice: once for this aggregate,
/// then again to emit independently resumable sample fragments.
pub fn project_charge(
    process: &TeslaMateChargingProcess,
    selected_car_id: i64,
    position: Option<&TeslaMatePosition>,
    address: Option<&TeslaMateAddress>,
    geofence: Option<&TeslaMateGeofence>,
    sample_facts: &ChargeProjectionFacts,
) -> Result<ProjectionCharge, TeslaMateProjectionError> {
    require_selected_car(
        "charging process",
        process.id,
        process.car_id,
        selected_car_id,
    )?;
    let _ = related_position_value(
        process.position_id,
        position,
        selected_car_id,
        "charging_process.position_id",
    )?;
    let address_value =
        related_address_value(process.address_id, address, "charging_process.address_id")?;
    let location_name = match (process.address_id, address) {
        (Some(id), Some(address)) if address.id == id => address.name.clone(),
        (Some(id), _) => {
            return Err(TeslaMateProjectionError::MissingRelated {
                field: "charging_process.address_id",
                id,
            });
        }
        (None, _) => None,
    };
    let geofence_value = related_geofence_value(
        process.geofence_id,
        geofence,
        "charging_process.geofence_id",
    )?;
    let charge_energy_added = process
        .charge_energy_added
        .or_else(|| sample_facts.energy_added());
    let end_battery_level = process
        .end_battery_level
        .or_else(|| sample_facts.last_battery_level.map(|(_, value)| value));
    let start_rated_range_km = process
        .start_rated_range_km
        .or_else(|| sample_facts.first_rated_range_km.map(|(_, value)| value));
    let end_rated_range_km = process
        .end_rated_range_km
        .or_else(|| sample_facts.last_rated_range_km.map(|(_, value)| value));
    let start_latitude = position.map(|position| position.latitude);
    let start_longitude = position.map(|position| position.longitude);
    let start_date_ms = sample_facts
        .first_sample
        .map_or(process.start_date_ms, |(date_ms, _)| {
            process.start_date_ms.min(date_ms)
        });
    let end_date_ms = process.end_date_ms.map(|end_date_ms| {
        sample_facts
            .last_sample
            .map_or(end_date_ms, |(date_ms, _)| end_date_ms.max(date_ms))
    });
    Ok(ProjectionCharge {
        id: process.id,
        car_id: selected_car_id,
        start_date_ms,
        end_date_ms,
        charge_energy_added,
        charge_energy_used_kwh: process.charge_energy_used_kwh,
        start_ideal_range_km: process.start_ideal_range_km,
        end_ideal_range_km: process.end_ideal_range_km,
        cost: process.cost,
        fast_charger_type: sample_facts.fast_charger_type.clone(),
        billing_type: geofence.and_then(|value| value.billing_type),
        cost_per_unit: geofence.and_then(|value| value.cost_per_unit),
        session_fee: geofence.and_then(|value| value.session_fee),
        start_latitude,
        start_longitude,
        start_battery_level: process.start_battery_level,
        end_battery_level,
        duration_min: process.duration_min,
        address: address_value,
        location_name,
        geofence: geofence_value,
        is_dc: sample_facts.is_dc,
        charge_rate_km_per_hour: None,
        max_charger_power_kw: sample_facts.max_charger_power_kw,
        outside_temp_avg: process.outside_temp_avg,
        start_rated_range_km,
        end_rated_range_km,
    })
}

pub fn project_charge_sample(sample: &TeslaMateCharge) -> ProjectionChargeSample {
    ProjectionChargeSample {
        id: sample.id,
        charge_process_id: sample.charging_process_id,
        timestamp_ms: sample.date_ms,
        battery_level: sample.battery_level,
        usable_battery_level: sample.usable_battery_level,
        charge_energy_added_kwh: sample.charge_energy_added_kwh,
        charger_power_kw: sample.charger_power_kw,
        charger_voltage: sample.charger_voltage,
        charger_actual_current: sample.charger_actual_current,
        charger_pilot_current: sample.charger_pilot_current,
        charger_phases: sample.charger_phases,
        ideal_range_km: sample.ideal_range_km,
        rated_range_km: sample.rated_range_km,
        outside_temp_c: sample.outside_temp_c,
        battery_heater_on: sample.battery_heater_on,
        battery_heater: sample.battery_heater,
        battery_heater_no_power: sample.battery_heater_no_power,
        not_enough_power_to_heat: sample.not_enough_power_to_heat,
        fast_charger_present: sample.fast_charger_present,
        fast_charger_brand: sample.fast_charger_brand.clone(),
        fast_charger_type: sample.fast_charger_type.clone(),
        charge_cable: sample.charge_cable.clone(),
    }
}

/// Build one vehicle's typed Hub history. This does not open a database or
/// retain source credentials. The caller must use `teslamate_schema` before it
/// decodes PostgreSQL rows into the input structures.
pub fn project_vehicle(
    source: &TeslaMateHistory,
    selected_car_id: i64,
) -> Result<TeslaMateProjection, TeslaMateProjectionError> {
    let car = source
        .cars
        .iter()
        .find(|car| car.id == selected_car_id)
        .ok_or(TeslaMateProjectionError::SelectedCarMissing(
            selected_car_id,
        ))?;
    require_unique_ids(source.cars.iter().map(|row| row.id), "car")?;
    require_unique_ids(source.drives.iter().map(|row| row.id), "drive")?;
    require_unique_ids(source.positions.iter().map(|row| row.id), "position")?;
    require_unique_ids(
        source.charging_processes.iter().map(|row| row.id),
        "charging process",
    )?;
    require_unique_ids(source.charges.iter().map(|row| row.id), "charge")?;
    require_unique_ids(source.addresses.iter().map(|row| row.id), "address")?;
    require_unique_ids(source.geofences.iter().map(|row| row.id), "geofence")?;
    require_unique_ids(source.updates.iter().map(|row| row.id), "update")?;

    let positions_by_id = source
        .positions
        .iter()
        .map(|row| (row.id, row))
        .collect::<HashMap<_, _>>();
    let addresses_by_id = source
        .addresses
        .iter()
        .map(|row| (row.id, row))
        .collect::<HashMap<_, _>>();
    let geofences_by_id = source
        .geofences
        .iter()
        .map(|row| (row.id, row))
        .collect::<HashMap<_, _>>();

    let firmware_version = latest_firmware(source, selected_car_id);
    let projected_car = project_car(car, firmware_version)?;

    let mut report = ProjectionReport::default();
    let mut included_drive_ids = HashSet::new();
    let mut drives = Vec::new();
    for drive in source
        .drives
        .iter()
        .filter(|drive| drive.car_id == selected_car_id)
    {
        let Some(_) = drive.end_date_ms else {
            report.skipped_open_drives += 1;
            continue;
        };
        included_drive_ids.insert(drive.id);
        report.completed_drives += 1;
        let projected = project_drive(
            drive,
            selected_car_id,
            DriveRelations {
                start_position: related_position_from_map(
                    drive.start_position_id,
                    &positions_by_id,
                    selected_car_id,
                    "drive.start_position_id",
                )?,
                end_position: related_position_from_map(
                    drive.end_position_id,
                    &positions_by_id,
                    selected_car_id,
                    "drive.end_position_id",
                )?,
                start_address: related_address_from_map(
                    drive.start_address_id,
                    &addresses_by_id,
                    "drive.start_address_id",
                )?,
                end_address: related_address_from_map(
                    drive.end_address_id,
                    &addresses_by_id,
                    "drive.end_address_id",
                )?,
                start_geofence: related_geofence_from_map(
                    drive.start_geofence_id,
                    &geofences_by_id,
                    "drive.start_geofence_id",
                )?,
                end_geofence: related_geofence_from_map(
                    drive.end_geofence_id,
                    &geofences_by_id,
                    "drive.end_geofence_id",
                )?,
            },
        )?
        .expect("completed drive must project");
        drives.push(projected);
    }

    let mut positions = Vec::new();
    for position in source
        .positions
        .iter()
        .filter(|position| position.car_id == selected_car_id)
    {
        let included = position
            .drive_id
            .map_or(true, |drive_id| included_drive_ids.contains(&drive_id));
        let projected = project_position(position, selected_car_id, included)?;
        let Some(projected) = projected else {
            report.skipped_unattached_positions += 1;
            continue;
        };
        positions.push(projected);
        report.projected_positions += 1;
    }

    let mut samples_by_process = HashMap::<i64, Vec<&TeslaMateCharge>>::new();
    for sample in &source.charges {
        samples_by_process
            .entry(sample.charging_process_id)
            .or_default()
            .push(sample);
    }
    let mut charge_ids = HashSet::new();
    let mut charges = Vec::new();
    for process in source
        .charging_processes
        .iter()
        .filter(|process| process.car_id == selected_car_id)
    {
        charge_ids.insert(process.id);
        let mut samples = samples_by_process
            .get(&process.id)
            .cloned()
            .unwrap_or_default();
        samples.sort_unstable_by_key(|sample| (sample.date_ms, sample.id));
        let position = related_position_from_map(
            process.position_id,
            &positions_by_id,
            selected_car_id,
            "charging_process.position_id",
        )?;
        let address = related_address_from_map(
            process.address_id,
            &addresses_by_id,
            "charging_process.address_id",
        )?;
        let geofence = related_geofence_from_map(
            process.geofence_id,
            &geofences_by_id,
            "charging_process.geofence_id",
        )?;
        report.projected_charges += 1;
        charges.push(project_charge(
            process,
            selected_car_id,
            position,
            address,
            geofence,
            &ChargeProjectionFacts::from_samples(&samples),
        )?);
    }

    let mut charge_samples = Vec::new();
    for sample in &source.charges {
        if !charge_ids.contains(&sample.charging_process_id) {
            continue;
        }
        report.projected_charge_samples += 1;
        charge_samples.push(project_charge_sample(sample));
    }

    let mut source_states = source
        .states
        .iter()
        .filter(|state| state.car_id == selected_car_id)
        .collect::<Vec<_>>();
    source_states.sort_unstable_by_key(|state| (state.start_date_ms, state.id));
    let mut states = Vec::with_capacity(source_states.len());
    for state in source_states {
        if let Some(projected) = project_state(state, selected_car_id)? {
            states.push(projected);
        }
    }

    Ok(TeslaMateProjection {
        snapshot: ProjectionSnapshot {
            cars: vec![projected_car],
            drives,
            positions,
            charges,
            charge_samples,
        },
        states,
        report,
    })
}

fn first_nonblank<const N: usize>(values: [Option<&str>; N]) -> Option<String> {
    values.into_iter().find_map(|value| {
        value
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned)
    })
}

fn normalise_efficiency(value: Option<f64>) -> Result<Option<f64>, TeslaMateProjectionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if !value.is_finite() || value < 0.0 {
        return Err(TeslaMateProjectionError::InvalidValue {
            field: "car.efficiency",
        });
    }
    // TeslaMate stores this as kWh/km. Its own UI log multiplies by 1000 for
    // Wh/km; the typed Teslatlas table is explicitly Wh/km.
    Ok(Some(if value > 0.0 && value < 1.0 {
        value * 1_000.0
    } else {
        value
    }))
}

fn latest_firmware(source: &TeslaMateHistory, selected_car_id: i64) -> Option<String> {
    source
        .updates
        .iter()
        .filter(|update| update.car_id == selected_car_id)
        .filter_map(|update| {
            let end_date_ms = update.end_date_ms?;
            let version = update.version.as_deref()?.trim();
            (!version.is_empty())
                .then_some(((end_date_ms, update.start_date_ms, update.id), version))
        })
        .max_by_key(|(order, _)| *order)
        .map(|(_, version)| version.to_owned())
}

fn related_position_from_map<'a>(
    id: Option<i64>,
    positions: &HashMap<i64, &'a TeslaMatePosition>,
    selected_car_id: i64,
    field: &'static str,
) -> Result<Option<&'a TeslaMatePosition>, TeslaMateProjectionError> {
    let Some(id) = id else {
        return Ok(None);
    };
    let position = positions
        .get(&id)
        .copied()
        .ok_or(TeslaMateProjectionError::MissingRelated { field, id })?;
    if position.car_id != selected_car_id {
        return Err(TeslaMateProjectionError::RelatedPositionWrongCar {
            field,
            id,
            expected_car_id: selected_car_id,
            found_car_id: position.car_id,
        });
    }
    Ok(Some(position))
}

fn related_address_from_map<'a>(
    id: Option<i64>,
    addresses: &HashMap<i64, &'a TeslaMateAddress>,
    field: &'static str,
) -> Result<Option<&'a TeslaMateAddress>, TeslaMateProjectionError> {
    let Some(id) = id else {
        return Ok(None);
    };
    let address = addresses
        .get(&id)
        .ok_or(TeslaMateProjectionError::MissingRelated { field, id })?;
    Ok(Some(*address))
}

fn related_geofence_from_map<'a>(
    id: Option<i64>,
    geofences: &HashMap<i64, &'a TeslaMateGeofence>,
    field: &'static str,
) -> Result<Option<&'a TeslaMateGeofence>, TeslaMateProjectionError> {
    let Some(id) = id else {
        return Ok(None);
    };
    let geofence = geofences
        .get(&id)
        .ok_or(TeslaMateProjectionError::MissingRelated { field, id })?;
    Ok(Some(*geofence))
}

fn related_position_value<'a>(
    id: Option<i64>,
    position: Option<&'a TeslaMatePosition>,
    selected_car_id: i64,
    field: &'static str,
) -> Result<Option<&'a TeslaMatePosition>, TeslaMateProjectionError> {
    let Some(id) = id else {
        return Ok(None);
    };
    let position = position.ok_or(TeslaMateProjectionError::MissingRelated { field, id })?;
    if position.id != id {
        return Err(TeslaMateProjectionError::MissingRelated { field, id });
    }
    if position.car_id != selected_car_id {
        return Err(TeslaMateProjectionError::RelatedPositionWrongCar {
            field,
            id,
            expected_car_id: selected_car_id,
            found_car_id: position.car_id,
        });
    }
    Ok(Some(position))
}

fn related_address_value(
    id: Option<i64>,
    address: Option<&TeslaMateAddress>,
    field: &'static str,
) -> Result<Option<String>, TeslaMateProjectionError> {
    let Some(id) = id else {
        return Ok(None);
    };
    let address = address.ok_or(TeslaMateProjectionError::MissingRelated { field, id })?;
    if address.id != id {
        return Err(TeslaMateProjectionError::MissingRelated { field, id });
    }
    Ok(address.display_name.clone())
}

fn related_geofence_value(
    id: Option<i64>,
    geofence: Option<&TeslaMateGeofence>,
    field: &'static str,
) -> Result<Option<String>, TeslaMateProjectionError> {
    let Some(id) = id else {
        return Ok(None);
    };
    let geofence = geofence.ok_or(TeslaMateProjectionError::MissingRelated { field, id })?;
    if geofence.id != id {
        return Err(TeslaMateProjectionError::MissingRelated { field, id });
    }
    Ok(Some(geofence.name.clone()))
}

fn require_selected_car(
    entity: &'static str,
    id: i64,
    found_car_id: i64,
    selected_car_id: i64,
) -> Result<(), TeslaMateProjectionError> {
    if id <= 0 {
        return Err(TeslaMateProjectionError::InvalidId { entity, id });
    }
    if found_car_id != selected_car_id {
        return Err(TeslaMateProjectionError::SelectedCarMismatch {
            entity,
            id,
            expected_car_id: selected_car_id,
            found_car_id,
        });
    }
    Ok(())
}

fn update_first<T: Copy>(
    target: &mut Option<((i64, i64), T)>,
    order: (i64, i64),
    value: Option<T>,
) {
    if let Some(value) = value.filter(|_| target.is_none_or(|(current, _)| order < current)) {
        *target = Some((order, value));
    }
}

fn update_last<T: Copy>(target: &mut Option<((i64, i64), T)>, order: (i64, i64), value: Option<T>) {
    if let Some(value) = value.filter(|_| target.is_none_or(|(current, _)| order > current)) {
        *target = Some((order, value));
    }
}

fn require_unique_ids(
    values: impl IntoIterator<Item = i64>,
    entity: &'static str,
) -> Result<(), TeslaMateProjectionError> {
    let mut ids = HashSet::new();
    for id in values {
        if id <= 0 {
            return Err(TeslaMateProjectionError::InvalidId { entity, id });
        }
        if !ids.insert(id) {
            return Err(TeslaMateProjectionError::DuplicateId { entity, id });
        }
    }
    Ok(())
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TeslaMateProjectionError {
    #[error("selected TeslaMate car {0} is missing")]
    SelectedCarMissing(i64),
    #[error("{entity} id {id} must be positive")]
    InvalidId { entity: &'static str, id: i64 },
    #[error("duplicate {entity} id {id}")]
    DuplicateId { entity: &'static str, id: i64 },
    #[error("{field} references missing source row {id}")]
    MissingRelated { field: &'static str, id: i64 },
    #[error("{field} position {id} belongs to car {found_car_id}, not car {expected_car_id}")]
    RelatedPositionWrongCar {
        field: &'static str,
        id: i64,
        expected_car_id: i64,
        found_car_id: i64,
    },
    #[error("{entity} {id} belongs to car {found_car_id}, not selected car {expected_car_id}")]
    SelectedCarMismatch {
        entity: &'static str,
        id: i64,
        expected_car_id: i64,
        found_car_id: i64,
    },
    #[error("{field} is negative or non-finite")]
    InvalidValue { field: &'static str },
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use uuid::Uuid;

    use crate::{
        hub_pack::{ProjectionBinding, ProjectionPackRequest, ProjectionPackWriter},
        protocol::SequenceRange,
    };

    fn history() -> TeslaMateHistory {
        TeslaMateHistory {
            cars: vec![
                TeslaMateCar {
                    id: 1,
                    eid: 101,
                    vid: Some(201),
                    vin: Some("5YJTESTVIN1234567".into()),
                    name: Some("Road car".into()),
                    model: Some("Model 3".into()),
                    trim_badging: None,
                    marketing_name: Some("Model 3 Highland".into()),
                    exterior_color: Some("Pearl White".into()),
                    wheel_type: Some("Apollo".into()),
                    spoiler_type: Some("None".into()),
                    efficiency_wh_per_km: Some(145.0),
                    settings: Default::default(),
                },
                TeslaMateCar {
                    id: 2,
                    eid: 102,
                    vid: Some(202),
                    vin: None,
                    name: Some("Other".into()),
                    model: Some("Model Y".into()),
                    trim_badging: None,
                    marketing_name: None,
                    exterior_color: None,
                    wheel_type: None,
                    spoiler_type: None,
                    efficiency_wh_per_km: None,
                    settings: Default::default(),
                },
            ],
            drives: vec![
                TeslaMateDrive {
                    id: 10,
                    car_id: 1,
                    start_date_ms: 1_700_000_000_000,
                    end_date_ms: Some(1_700_000_060_000),
                    start_position_id: Some(20),
                    end_position_id: Some(20),
                    start_address_id: Some(100),
                    end_address_id: Some(100),
                    start_geofence_id: Some(200),
                    end_geofence_id: Some(200),
                    outside_temp_avg: Some(18.0),
                    inside_temp_avg: Some(21.0),
                    speed_max: Some(80),
                    power_max: Some(36.0),
                    power_min: Some(-7.0),
                    start_ideal_range_km: Some(338.8),
                    end_ideal_range_km: Some(334.8),
                    start_rated_range_km: Some(400.0),
                    end_rated_range_km: Some(390.0),
                    start_km: Some(10_000.0),
                    end_km: Some(10_012.0),
                    distance_km: Some(12.0),
                    duration_min: Some(10),
                    ascent: Some(60),
                    descent: Some(30),
                },
                TeslaMateDrive {
                    id: 11,
                    car_id: 1,
                    start_date_ms: 1_700_000_100_000,
                    end_date_ms: None,
                    start_position_id: None,
                    end_position_id: None,
                    start_address_id: None,
                    end_address_id: None,
                    start_geofence_id: None,
                    end_geofence_id: None,
                    outside_temp_avg: None,
                    inside_temp_avg: None,
                    speed_max: None,
                    power_max: None,
                    power_min: None,
                    start_ideal_range_km: None,
                    end_ideal_range_km: None,
                    start_rated_range_km: None,
                    end_rated_range_km: None,
                    start_km: None,
                    end_km: None,
                    distance_km: None,
                    duration_min: None,
                    ascent: None,
                    descent: None,
                },
            ],
            positions: vec![
                TeslaMatePosition {
                    id: 20,
                    car_id: 1,
                    drive_id: Some(10),
                    date_ms: 1_700_000_030_000,
                    latitude: 51.5,
                    longitude: -0.1,
                    elevation: Some(20),
                    speed: Some(50),
                    power: Some(10.0),
                    odometer: Some(10_006.0),
                    ideal_battery_range_km: Some(390.0),
                    est_battery_range_km: Some(385.0),
                    rated_battery_range_km: Some(389.0),
                    battery_level: Some(78),
                    usable_battery_level: Some(77),
                    fan_status: Some(2),
                    driver_temp_setting: Some(21.5),
                    passenger_temp_setting: Some(22.0),
                    is_climate_on: Some(false),
                    is_rear_defroster_on: Some(false),
                    is_front_defroster_on: Some(true),
                    outside_temp: Some(18.0),
                    inside_temp: Some(20.0),
                    battery_heater: Some(true),
                    battery_heater_on: Some(true),
                    battery_heater_no_power: Some(false),
                    tpms_pressure_fl: Some(2.4),
                    tpms_pressure_fr: Some(2.5),
                    tpms_pressure_rl: Some(2.6),
                    tpms_pressure_rr: Some(2.7),
                },
                TeslaMatePosition {
                    id: 21,
                    car_id: 1,
                    drive_id: Some(11),
                    date_ms: 1_700_000_110_000,
                    latitude: 51.5,
                    longitude: -0.1,
                    elevation: None,
                    speed: None,
                    power: None,
                    odometer: None,
                    ideal_battery_range_km: None,
                    est_battery_range_km: None,
                    rated_battery_range_km: None,
                    battery_level: None,
                    usable_battery_level: None,
                    fan_status: None,
                    driver_temp_setting: None,
                    passenger_temp_setting: None,
                    is_climate_on: None,
                    is_rear_defroster_on: None,
                    is_front_defroster_on: None,
                    outside_temp: None,
                    inside_temp: None,
                    battery_heater: None,
                    battery_heater_on: None,
                    battery_heater_no_power: None,
                    tpms_pressure_fl: None,
                    tpms_pressure_fr: None,
                    tpms_pressure_rl: None,
                    tpms_pressure_rr: None,
                },
            ],
            charging_processes: vec![TeslaMateChargingProcess {
                id: 30,
                car_id: 1,
                position_id: Some(20),
                address_id: Some(100),
                geofence_id: Some(200),
                start_date_ms: 1_700_001_000_000,
                end_date_ms: Some(1_700_001_360_000),
                charge_energy_added: Some(20.0),
                charge_energy_used_kwh: None,
                start_ideal_range_km: None,
                end_ideal_range_km: None,
                cost: Some(9.99),
                start_battery_level: Some(50),
                end_battery_level: Some(80),
                duration_min: Some(60),
                outside_temp_avg: Some(18.0),
                start_rated_range_km: Some(250.0),
                end_rated_range_km: Some(400.0),
            }],
            charges: vec![TeslaMateCharge {
                id: 40,
                charging_process_id: 30,
                date_ms: 1_700_001_100_000,
                battery_heater: Some(false),
                battery_heater_on: Some(false),
                battery_heater_no_power: Some(false),
                battery_level: Some(60),
                usable_battery_level: Some(59),
                charge_energy_added_kwh: Some(6.0),
                charger_actual_current: Some(30.0),
                charger_phases: Some(1),
                charger_pilot_current: Some(32.0),
                charger_power_kw: Some(7.0),
                charger_voltage: Some(230.0),
                charge_cable: Some("Type 2".into()),
                fast_charger_present: Some(false),
                fast_charger_brand: None,
                fast_charger_type: None,
                ideal_range_km: Some(300.0),
                rated_range_km: Some(298.0),
                not_enough_power_to_heat: Some(false),
                outside_temp_c: Some(18.0),
            }],
            addresses: vec![TeslaMateAddress {
                id: 100,
                display_name: Some("Home, London".into()),
                name: Some("Home".into()),
            }],
            geofences: vec![TeslaMateGeofence {
                id: 200,
                name: "Home".into(),
                latitude: Some(51.0),
                longitude: Some(-0.1),
                radius_m: Some(100.0),
                billing_type: Some(GeofenceBillingType::PerKwh),
                cost_per_unit: Some(0.30),
                session_fee: Some(2.0),
            }],
            states: vec![],
            updates: vec![TeslaMateUpdate {
                id: 300,
                car_id: 1,
                start_date_ms: 1_699_000_000_000,
                end_date_ms: Some(1_699_000_060_000),
                version: Some("2026.20.1".into()),
            }],
        }
    }

    #[test]
    fn projects_only_completed_selected_vehicle_history() {
        let projected = project_vehicle(&history(), 1).unwrap();
        assert_eq!(projected.snapshot.cars.len(), 1);
        assert_eq!(projected.snapshot.drives.len(), 1);
        assert_eq!(projected.snapshot.drives[0].inside_temp_avg, Some(21.0));
        assert_eq!(projected.snapshot.drives[0].power_max, Some(36.0));
        assert_eq!(projected.snapshot.drives[0].power_min, Some(-7.0));
        assert_eq!(
            projected.snapshot.drives[0].start_ideal_range_km,
            Some(338.8)
        );
        assert_eq!(projected.snapshot.drives[0].end_ideal_range_km, Some(334.8));
        assert_eq!(projected.snapshot.drives[0].ascent, Some(60));
        assert_eq!(projected.snapshot.charges[0].cost, Some(9.99));
        assert_eq!(
            projected.snapshot.charges[0].billing_type,
            Some(GeofenceBillingType::PerKwh)
        );
        assert_eq!(projected.snapshot.drives[0].descent, Some(30));
        assert_eq!(projected.snapshot.positions.len(), 1);
        assert_eq!(projected.snapshot.positions[0].battery_heater, Some(true));
        assert_eq!(
            projected.snapshot.positions[0].battery_heater_on,
            Some(true)
        );
        assert_eq!(
            projected.snapshot.positions[0].battery_heater_no_power,
            Some(false)
        );
        assert_eq!(projected.snapshot.charges.len(), 1);
        assert_eq!(projected.snapshot.charge_samples.len(), 1);
        assert_eq!(
            projected.snapshot.charges[0].max_charger_power_kw,
            Some(7.0)
        );
        assert_eq!(projected.snapshot.charges[0].is_dc, Some(false));
        assert_eq!(projected.snapshot.cars[0].model, "3");
        assert_eq!(
            projected.snapshot.cars[0].marketing_name.as_deref(),
            Some("Model 3 Highland")
        );
        assert_eq!(
            projected.snapshot.cars[0].firmware_version.as_deref(),
            Some("2026.20.1")
        );
        assert_eq!(
            projected.snapshot.drives[0].start_address.as_deref(),
            Some("Home, London")
        );
        assert_eq!(projected.snapshot.drives[0].start_soc, Some(78));
        assert_eq!(
            projected.snapshot.charges[0].location_name.as_deref(),
            Some("Home")
        );
        assert_eq!(
            projected.report,
            ProjectionReport {
                completed_drives: 1,
                skipped_open_drives: 1,
                skipped_unattached_positions: 1,
                projected_positions: 1,
                projected_charges: 1,
                projected_charge_samples: 1,
            }
        );
    }

    #[test]
    fn charge_interval_contains_all_source_samples() {
        let mut before = history();
        before.charges[0].date_ms = before.charging_processes[0].start_date_ms - 1_000;
        let projected = project_vehicle(&before, 1).unwrap();
        assert_eq!(
            projected.snapshot.charges[0].start_date_ms,
            before.charges[0].date_ms
        );

        let mut after = history();
        after.charges[0].date_ms = after.charging_processes[0].end_date_ms.unwrap() + 1_000;
        let projected = project_vehicle(&after, 1).unwrap();
        assert_eq!(
            projected.snapshot.charges[0].end_date_ms,
            Some(after.charges[0].date_ms)
        );
    }

    #[test]
    fn refuses_duplicate_source_identity_and_uses_safe_car_fallbacks() {
        let mut source = history();
        source.cars[1].id = 1;
        assert!(matches!(
            project_vehicle(&source, 1),
            Err(TeslaMateProjectionError::DuplicateId {
                entity: "car",
                id: 1
            })
        ));

        let mut source = history();
        source.cars[0].name = None;
        source.cars[0].model = None;
        source.cars[0].marketing_name = None;
        source.cars[0].trim_badging = None;
        let projected = project_vehicle(&source, 1).unwrap();
        assert_eq!(projected.snapshot.cars[0].name, "Car 1");
        assert_eq!(projected.snapshot.cars[0].model, "Unknown Tesla");
    }

    #[test]
    fn refuses_cross_car_endpoint_position() {
        let mut source = history();
        source.positions[0].car_id = 2;
        assert_eq!(
            project_vehicle(&source, 1),
            Err(TeslaMateProjectionError::RelatedPositionWrongCar {
                field: "drive.start_position_id",
                id: 20,
                expected_car_id: 1,
                found_car_id: 2,
            })
        );
    }

    #[test]
    fn validates_a_complete_open_session_fixture_and_rejects_cross_car_rows() {
        let mut source = history();
        source.drives[0].end_date_ms = None;
        source.charging_processes[0].end_date_ms = None;
        let state = TeslaMateState {
            id: 40,
            car_id: 1,
            state: "online".into(),
            start_date_ms: 1_700_000_000_000,
            end_date_ms: None,
        };
        let mut standalone = source.positions[0].clone();
        standalone.id = 99;
        standalone.drive_id = None;
        let open = TeslaMateOpenSession {
            car_id: 1,
            drive: Some(source.drives[0].clone()),
            drive_positions: vec![source.positions[0].clone()],
            charge: Some(source.charging_processes[0].clone()),
            charge_samples: source.charges.clone(),
            state: Some(state),
            standalone_positions: vec![standalone],
            watermarks: TeslaMateSourceWatermarks {
                drives: TeslaMateSourceWatermark {
                    max_id: Some(10),
                    max_timestamp_ms: Some(1_700_000_060_000),
                },
                ..TeslaMateSourceWatermarks::default()
            },
        };
        open.validate().expect("open fixture validates");

        let mut wrong_car = open.clone();
        wrong_car.drive_positions[0].car_id = 2;
        assert!(matches!(
            wrong_car.validate(),
            Err(TeslaMateProjectionError::SelectedCarMismatch { .. })
        ));
    }

    #[test]
    fn mapped_history_is_accepted_by_the_typed_pack_gate() {
        let temporary = TempDir::new().unwrap();
        let projected = project_vehicle(&history(), 1).unwrap();
        let request = ProjectionPackRequest {
            pack_id: Uuid::from_u128(1),
            snapshot_id: Uuid::from_u128(2),
            ordinal: 0,
            binding: ProjectionBinding {
                installation_id: Uuid::from_u128(3),
                account_id: Uuid::from_u128(4),
                vehicle_id: Uuid::from_u128(5),
                generation: 1,
                selected_car_id: 1,
            },
            sequence: SequenceRange {
                from_exclusive: 0,
                to_inclusive: 1,
            },
            snapshot: &projected.snapshot,
        };
        let built = ProjectionPackWriter::new(temporary.path())
            .write_full_snapshot(&request)
            .unwrap();
        assert_eq!(built.metadata.row_count, 5);
    }
}
