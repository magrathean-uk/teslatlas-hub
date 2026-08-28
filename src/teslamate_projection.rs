// SPDX-License-Identifier: AGPL-3.0-only

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
    GeofenceBillingType, ProjectionAddressV2_2, ProjectionCar, ProjectionCarSettings,
    ProjectionCarSettingsV2_2, ProjectionCarV2_2, ProjectionCharge, ProjectionChargeSample,
    ProjectionChargeV2_2, ProjectionChargingProcessV2_2, ProjectionDrive, ProjectionDriveV2_2,
    ProjectionFixedNumericV2_2, ProjectionFloat64BitsV2_2, ProjectionGeofenceV2_2,
    ProjectionGlobalSettingsV2_2, ProjectionPosition, ProjectionPositionV2_2,
    ProjectionPreferredRangeV2_2, ProjectionSnapshot, ProjectionState, ProjectionStateStatusV2_2,
    ProjectionStateV2_2, ProjectionUnitOfLengthV2_2, ProjectionUnitOfPressureV2_2,
    ProjectionUnitOfTemperatureV2_2, ProjectionUpdate, ProjectionUpdateV2_2,
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

/// Exact selected-car `car_settings` source values for the bounded schema-2.2
/// local candidate. It is intentionally separate from the legacy settings
/// projection, which resolves defaults and drops the physical source identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeslaMateCarSettingsPhysicalV2_2 {
    pub id: i64,
    pub suspend_min: i32,
    pub suspend_after_idle_min: i32,
    pub req_not_unlocked: bool,
    pub free_supercharging: bool,
    pub use_streaming_api: bool,
    pub enabled: bool,
    pub lfp_battery: bool,
}

impl From<TeslaMateCarSettingsPhysicalV2_2> for ProjectionCarSettingsV2_2 {
    fn from(value: TeslaMateCarSettingsPhysicalV2_2) -> Self {
        Self {
            id: value.id,
            suspend_min: value.suspend_min,
            suspend_after_idle_min: value.suspend_after_idle_min,
            req_not_unlocked: value.req_not_unlocked,
            free_supercharging: value.free_supercharging,
            use_streaming_api: value.use_streaming_api,
            enabled: value.enabled,
            lfp_battery: value.lfp_battery,
        }
    }
}

/// Exact source-wide TeslaMate `settings` singleton for the bounded schema-2.2
/// candidate. It is intentionally separate from selected-car settings and all
/// legacy import shapes: source enum labels, opaque URLs, physical text, and
/// raw timestamp(0) payloads survive without policy conversion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeslaMateSettingsPhysicalV2_2 {
    pub id: i64,
    pub unit_of_length: ProjectionUnitOfLengthV2_2,
    pub unit_of_temperature: ProjectionUnitOfTemperatureV2_2,
    pub unit_of_pressure: ProjectionUnitOfPressureV2_2,
    pub preferred_range: ProjectionPreferredRangeV2_2,
    pub base_url: Option<String>,
    pub grafana_url: Option<String>,
    pub language: String,
    pub theme_mode: String,
    pub inserted_at_pg_us: i64,
    pub updated_at_pg_us: i64,
}

impl From<TeslaMateSettingsPhysicalV2_2> for ProjectionGlobalSettingsV2_2 {
    fn from(value: TeslaMateSettingsPhysicalV2_2) -> Self {
        Self {
            id: value.id,
            unit_of_length: value.unit_of_length,
            unit_of_temperature: value.unit_of_temperature,
            unit_of_pressure: value.unit_of_pressure,
            preferred_range: value.preferred_range,
            base_url: value.base_url,
            grafana_url: value.grafana_url,
            language: value.language,
            theme_mode: value.theme_mode,
            inserted_at_pg_us: value.inserted_at_pg_us,
            updated_at_pg_us: value.updated_at_pg_us,
        }
    }
}

/// Exact selected-car source values for the bounded schema-2.2 local
/// candidate. The legacy `TeslaMateCar` remains the compatibility/import
/// shape and must not be widened with these physical fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeslaMateCarPhysicalV2_2 {
    pub id: i16,
    pub eid: i64,
    pub vid: i64,
    pub vin: Option<String>,
    pub name: Option<String>,
    pub model: Option<String>,
    pub efficiency: Option<f64>,
    pub trim_badging: Option<String>,
    pub marketing_name: Option<String>,
    pub exterior_color: Option<String>,
    pub wheel_type: Option<String>,
    pub spoiler_type: Option<String>,
    pub display_priority: i16,
    pub inserted_at_pg_us: i64,
    pub updated_at_pg_us: i64,
    pub settings_id: i64,
}

impl From<TeslaMateCarPhysicalV2_2> for ProjectionCarV2_2 {
    fn from(value: TeslaMateCarPhysicalV2_2) -> Self {
        Self {
            id: value.id,
            eid: value.eid,
            vid: value.vid,
            vin: value.vin,
            name: value.name,
            model: value.model,
            efficiency: value.efficiency,
            trim_badging: value.trim_badging,
            marketing_name: value.marketing_name,
            exterior_color: value.exterior_color,
            wheel_type: value.wheel_type,
            spoiler_type: value.spoiler_type,
            display_priority: value.display_priority,
            inserted_at_pg_us: value.inserted_at_pg_us,
            updated_at_pg_us: value.updated_at_pg_us,
            settings_id: value.settings_id,
        }
    }
}

/// Exact selected-car `states` source row for the bounded schema-2.2 local
/// candidate. Timestamp payloads are raw PostgreSQL binary timestamp i64
/// microseconds since 2000-01-01, so their finite/infinite domain is not
/// narrowed through a wall-clock conversion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeslaMateStatePhysicalV2_2 {
    pub id: i32,
    pub car_id: i16,
    pub state: ProjectionStateStatusV2_2,
    pub start_date_pg_us: i64,
    pub end_date_pg_us: Option<i64>,
}

impl From<TeslaMateStatePhysicalV2_2> for ProjectionStateV2_2 {
    fn from(value: TeslaMateStatePhysicalV2_2) -> Self {
        Self {
            id: value.id,
            car_id: value.car_id,
            state: value.state,
            start_date_pg_us: value.start_date_pg_us,
            end_date_pg_us: value.end_date_pg_us,
        }
    }
}

/// Exact selected-car `updates` source row for the bounded schema-2.2 local
/// candidate. Nullable end/version values are preserved without legacy
/// interval, trim, or default policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeslaMateUpdatePhysicalV2_2 {
    pub id: i32,
    pub car_id: i16,
    pub start_date_pg_us: i64,
    pub end_date_pg_us: Option<i64>,
    pub version: Option<String>,
}

impl From<TeslaMateUpdatePhysicalV2_2> for ProjectionUpdateV2_2 {
    fn from(value: TeslaMateUpdatePhysicalV2_2) -> Self {
        Self {
            id: value.id,
            car_id: value.car_id,
            start_date_pg_us: value.start_date_pg_us,
            end_date_pg_us: value.end_date_pg_us,
            version: value.version,
        }
    }
}

/// Exact non-sensitive physical TeslaMate address values for the bounded
/// schema-2.2 local candidate. This deliberately sits beside the legacy
/// `TeslaMateAddress`: the compatibility/import shape remains unchanged.
///
/// Source numerics retain a tagged finite-scaled-or-NaN representation, while
/// `timestamp(0)` values retain PostgreSQL binary microseconds, including its
/// infinity sentinels. The source JSON payload has no representation here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeslaMateAddressPhysicalV2_2 {
    pub id: i32,
    pub display_name: Option<String>,
    pub latitude_e6: Option<ProjectionFixedNumericV2_2>,
    pub longitude_e6: Option<ProjectionFixedNumericV2_2>,
    pub name: Option<String>,
    pub house_number: Option<String>,
    pub road: Option<String>,
    pub neighbourhood: Option<String>,
    pub city: Option<String>,
    pub county: Option<String>,
    pub postcode: Option<String>,
    pub state: Option<String>,
    pub state_district: Option<String>,
    pub country: Option<String>,
    pub inserted_at_pg_us: i64,
    pub updated_at_pg_us: i64,
    pub osm_id: Option<i64>,
    pub osm_type: Option<String>,
}

impl From<TeslaMateAddressPhysicalV2_2> for ProjectionAddressV2_2 {
    fn from(value: TeslaMateAddressPhysicalV2_2) -> Self {
        Self {
            id: value.id,
            display_name: value.display_name,
            latitude_e6: value.latitude_e6,
            longitude_e6: value.longitude_e6,
            name: value.name,
            house_number: value.house_number,
            road: value.road,
            neighbourhood: value.neighbourhood,
            city: value.city,
            county: value.county,
            postcode: value.postcode,
            state: value.state,
            state_district: value.state_district,
            country: value.country,
            inserted_at_pg_us: value.inserted_at_pg_us,
            updated_at_pg_us: value.updated_at_pg_us,
            osm_id: value.osm_id,
            osm_type: value.osm_type,
        }
    }
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
            && radius_m > 0.0
            && radius_m < 5_000.0)
            .then_some((latitude, longitude, radius_m))
    }
}

/// Exact physical TeslaMate geofence values for the bounded schema-2.2 local
/// candidate.  This deliberately sits beside the legacy `TeslaMateGeofence`:
/// the latter remains the compatibility/import shape and must not be widened
/// or reinterpreted by the candidate work.
///
/// Decimal source values retain tagged finite-scaled-or-NaN values: latitude
/// and longitude are e6, cost-per-unit is e4, and session-fee is e2. The source
/// `smallint` radius is preserved verbatim, including zero and its signed
/// extremes. `timestamp(0)` values retain PostgreSQL binary microseconds,
/// including the server's infinity sentinels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeslaMateGeofencePhysicalV2_2 {
    pub id: i32,
    pub name: String,
    pub latitude_e6: ProjectionFixedNumericV2_2,
    pub longitude_e6: ProjectionFixedNumericV2_2,
    pub radius: i16,
    pub billing_type: GeofenceBillingType,
    pub cost_per_unit_e4: Option<ProjectionFixedNumericV2_2>,
    pub session_fee_e2: Option<ProjectionFixedNumericV2_2>,
    pub inserted_at_pg_us: i64,
    pub updated_at_pg_us: i64,
}

impl From<TeslaMateGeofencePhysicalV2_2> for ProjectionGeofenceV2_2 {
    fn from(value: TeslaMateGeofencePhysicalV2_2) -> Self {
        Self {
            id: value.id,
            name: value.name,
            latitude_e6: value.latitude_e6,
            longitude_e6: value.longitude_e6,
            radius: value.radius,
            billing_type: value.billing_type,
            cost_per_unit_e4: value.cost_per_unit_e4,
            session_fee_e2: value.session_fee_e2,
            inserted_at_pg_us: value.inserted_at_pg_us,
            updated_at_pg_us: value.updated_at_pg_us,
        }
    }
}

/// Exact physical TeslaMate `drives` values for the bounded schema-2.2 local
/// candidate. This remains separate from `TeslaMateDrive`, whose legacy path
/// narrows source values and filters completed intervals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeslaMateDrivePhysicalV2_2 {
    pub id: i32,
    pub car_id: i16,
    pub start_date_pg_us: i64,
    pub end_date_pg_us: Option<i64>,
    pub start_position_id: Option<i32>,
    pub end_position_id: Option<i32>,
    pub start_address_id: Option<i32>,
    pub end_address_id: Option<i32>,
    pub start_geofence_id: Option<i32>,
    pub end_geofence_id: Option<i32>,
    pub outside_temp_avg_e1: Option<ProjectionFixedNumericV2_2>,
    pub inside_temp_avg_e1: Option<ProjectionFixedNumericV2_2>,
    pub speed_max: Option<i16>,
    pub power_max: Option<i16>,
    pub power_min: Option<i16>,
    pub start_ideal_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub end_ideal_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub start_rated_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub end_rated_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub start_km: Option<ProjectionFloat64BitsV2_2>,
    pub end_km: Option<ProjectionFloat64BitsV2_2>,
    pub distance: Option<ProjectionFloat64BitsV2_2>,
    pub duration_min: Option<i16>,
    pub ascent: Option<i16>,
    pub descent: Option<i16>,
}

impl From<TeslaMateDrivePhysicalV2_2> for ProjectionDriveV2_2 {
    fn from(value: TeslaMateDrivePhysicalV2_2) -> Self {
        Self {
            id: value.id,
            car_id: value.car_id,
            start_date_pg_us: value.start_date_pg_us,
            end_date_pg_us: value.end_date_pg_us,
            start_position_id: value.start_position_id,
            end_position_id: value.end_position_id,
            start_address_id: value.start_address_id,
            end_address_id: value.end_address_id,
            start_geofence_id: value.start_geofence_id,
            end_geofence_id: value.end_geofence_id,
            outside_temp_avg_e1: value.outside_temp_avg_e1,
            inside_temp_avg_e1: value.inside_temp_avg_e1,
            speed_max: value.speed_max,
            power_max: value.power_max,
            power_min: value.power_min,
            start_ideal_range_km_e2: value.start_ideal_range_km_e2,
            end_ideal_range_km_e2: value.end_ideal_range_km_e2,
            start_rated_range_km_e2: value.start_rated_range_km_e2,
            end_rated_range_km_e2: value.end_rated_range_km_e2,
            start_km: value.start_km,
            end_km: value.end_km,
            distance: value.distance,
            duration_min: value.duration_min,
            ascent: value.ascent,
            descent: value.descent,
        }
    }
}

/// Exact physical TeslaMate `positions` values for the bounded schema-2.2
/// local candidate. This remains separate from `TeslaMatePosition`: no legacy
/// time conversion, coordinate policy, relation closure, or FLOAT8 narrowing
/// applies at this local-only boundary. Rust selected-car admission enforces
/// the source `car_id` relation; V3 local SQLite intentionally has no FKs, and
/// a source `drive_id` may target an extant cross-car drive omitted from scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeslaMatePositionPhysicalV2_2 {
    pub id: i32,
    pub car_id: i16,
    pub drive_id: Option<i32>,
    pub date_pg_us: i64,
    pub latitude_e6: ProjectionFixedNumericV2_2,
    pub longitude_e6: ProjectionFixedNumericV2_2,
    pub elevation: Option<i16>,
    pub speed: Option<i16>,
    pub power: Option<i16>,
    pub odometer: Option<ProjectionFloat64BitsV2_2>,
    pub ideal_battery_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub est_battery_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub rated_battery_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub battery_level: Option<i16>,
    pub usable_battery_level: Option<i16>,
    pub battery_heater: Option<bool>,
    pub battery_heater_on: Option<bool>,
    pub battery_heater_no_power: Option<bool>,
    pub outside_temp_e1: Option<ProjectionFixedNumericV2_2>,
    pub inside_temp_e1: Option<ProjectionFixedNumericV2_2>,
    pub fan_status: Option<i32>,
    pub driver_temp_setting_e1: Option<ProjectionFixedNumericV2_2>,
    pub passenger_temp_setting_e1: Option<ProjectionFixedNumericV2_2>,
    pub is_climate_on: Option<bool>,
    pub is_rear_defroster_on: Option<bool>,
    pub is_front_defroster_on: Option<bool>,
    pub tpms_pressure_fl_e1: Option<ProjectionFixedNumericV2_2>,
    pub tpms_pressure_fr_e1: Option<ProjectionFixedNumericV2_2>,
    pub tpms_pressure_rl_e1: Option<ProjectionFixedNumericV2_2>,
    pub tpms_pressure_rr_e1: Option<ProjectionFixedNumericV2_2>,
}

impl From<TeslaMatePositionPhysicalV2_2> for ProjectionPositionV2_2 {
    fn from(value: TeslaMatePositionPhysicalV2_2) -> Self {
        Self {
            id: value.id,
            car_id: value.car_id,
            drive_id: value.drive_id,
            date_pg_us: value.date_pg_us,
            latitude_e6: value.latitude_e6,
            longitude_e6: value.longitude_e6,
            elevation: value.elevation,
            speed: value.speed,
            power: value.power,
            odometer: value.odometer,
            ideal_battery_range_km_e2: value.ideal_battery_range_km_e2,
            est_battery_range_km_e2: value.est_battery_range_km_e2,
            rated_battery_range_km_e2: value.rated_battery_range_km_e2,
            battery_level: value.battery_level,
            usable_battery_level: value.usable_battery_level,
            battery_heater: value.battery_heater,
            battery_heater_on: value.battery_heater_on,
            battery_heater_no_power: value.battery_heater_no_power,
            outside_temp_e1: value.outside_temp_e1,
            inside_temp_e1: value.inside_temp_e1,
            fan_status: value.fan_status,
            driver_temp_setting_e1: value.driver_temp_setting_e1,
            passenger_temp_setting_e1: value.passenger_temp_setting_e1,
            is_climate_on: value.is_climate_on,
            is_rear_defroster_on: value.is_rear_defroster_on,
            is_front_defroster_on: value.is_front_defroster_on,
            tpms_pressure_fl_e1: value.tpms_pressure_fl_e1,
            tpms_pressure_fr_e1: value.tpms_pressure_fr_e1,
            tpms_pressure_rl_e1: value.tpms_pressure_rl_e1,
            tpms_pressure_rr_e1: value.tpms_pressure_rr_e1,
        }
    }
}

/// Exact physical TeslaMate `charging_processes` row for the unlinked
/// schema-2.2 candidate. Source IDs stay signed, timestamps retain their raw
/// PostgreSQL microseconds, and NUMERIC fields keep finite/NaN distinction.
/// The source `position_id` relation is deliberately not made a V3 SQLite FK:
/// its valid target can be omitted from the selected-car local subset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeslaMateChargingProcessPhysicalV2_2 {
    pub id: i32,
    pub car_id: i16,
    pub position_id: i32,
    pub address_id: Option<i32>,
    pub geofence_id: Option<i32>,
    pub start_date_pg_us: i64,
    pub end_date_pg_us: Option<i64>,
    pub charge_energy_added_e2: Option<ProjectionFixedNumericV2_2>,
    pub charge_energy_used_e2: Option<ProjectionFixedNumericV2_2>,
    pub start_ideal_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub end_ideal_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub start_rated_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub end_rated_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub start_battery_level: Option<i16>,
    pub end_battery_level: Option<i16>,
    pub duration_min: Option<i16>,
    pub outside_temp_avg_e1: Option<ProjectionFixedNumericV2_2>,
    pub cost_e2: Option<ProjectionFixedNumericV2_2>,
}

impl From<TeslaMateChargingProcessPhysicalV2_2> for ProjectionChargingProcessV2_2 {
    fn from(value: TeslaMateChargingProcessPhysicalV2_2) -> Self {
        Self {
            id: value.id,
            car_id: value.car_id,
            position_id: value.position_id,
            address_id: value.address_id,
            geofence_id: value.geofence_id,
            start_date_pg_us: value.start_date_pg_us,
            end_date_pg_us: value.end_date_pg_us,
            charge_energy_added_e2: value.charge_energy_added_e2,
            charge_energy_used_e2: value.charge_energy_used_e2,
            start_ideal_range_km_e2: value.start_ideal_range_km_e2,
            end_ideal_range_km_e2: value.end_ideal_range_km_e2,
            start_rated_range_km_e2: value.start_rated_range_km_e2,
            end_rated_range_km_e2: value.end_rated_range_km_e2,
            start_battery_level: value.start_battery_level,
            end_battery_level: value.end_battery_level,
            duration_min: value.duration_min,
            outside_temp_avg_e1: value.outside_temp_avg_e1,
            cost_e2: value.cost_e2,
        }
    }
}

/// Exact physical TeslaMate `charges` row for the unlinked schema-2.2
/// candidate. It represents a source sample, not a normalized charge session:
/// its nullable booleans remain tri-state and varchar/numeric source contracts
/// are retained without defaults, finite-only conversion, or policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeslaMateChargePhysicalV2_2 {
    pub id: i32,
    pub charging_process_id: i32,
    pub date_pg_us: i64,
    pub battery_heater: Option<bool>,
    pub battery_heater_on: Option<bool>,
    pub battery_heater_no_power: Option<bool>,
    pub battery_level: Option<i16>,
    pub usable_battery_level: Option<i16>,
    pub charge_energy_added_e2: ProjectionFixedNumericV2_2,
    pub charger_actual_current: Option<i16>,
    pub charger_phases: Option<i16>,
    pub charger_pilot_current: Option<i16>,
    pub charger_power: i16,
    pub charger_voltage: Option<i16>,
    pub conn_charge_cable: Option<String>,
    pub fast_charger_present: Option<bool>,
    pub fast_charger_brand: Option<String>,
    pub fast_charger_type: Option<String>,
    pub ideal_battery_range_km_e2: ProjectionFixedNumericV2_2,
    pub rated_battery_range_km_e2: Option<ProjectionFixedNumericV2_2>,
    pub not_enough_power_to_heat: Option<bool>,
    pub outside_temp_e1: Option<ProjectionFixedNumericV2_2>,
}

impl From<TeslaMateChargePhysicalV2_2> for ProjectionChargeV2_2 {
    fn from(value: TeslaMateChargePhysicalV2_2) -> Self {
        Self {
            id: value.id,
            charging_process_id: value.charging_process_id,
            date_pg_us: value.date_pg_us,
            battery_heater: value.battery_heater,
            battery_heater_on: value.battery_heater_on,
            battery_heater_no_power: value.battery_heater_no_power,
            battery_level: value.battery_level,
            usable_battery_level: value.usable_battery_level,
            charge_energy_added_e2: value.charge_energy_added_e2,
            charger_actual_current: value.charger_actual_current,
            charger_phases: value.charger_phases,
            charger_pilot_current: value.charger_pilot_current,
            charger_power: value.charger_power,
            charger_voltage: value.charger_voltage,
            conn_charge_cable: value.conn_charge_cable,
            fast_charger_present: value.fast_charger_present,
            fast_charger_brand: value.fast_charger_brand,
            fast_charger_type: value.fast_charger_type,
            ideal_battery_range_km_e2: value.ideal_battery_range_km_e2,
            rated_battery_range_km_e2: value.rated_battery_range_km_e2,
            not_enough_power_to_heat: value.not_enough_power_to_heat,
            outside_temp_e1: value.outside_temp_e1,
        }
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

/// Map one completed TeslaMate firmware update for the selected car. TeslaMate
/// keeps an in-progress update open by omitting its end time and can create a
/// placeholder before a version is known. Schema 2.1 has no representation for
/// either incomplete form, so leave those source rows for the next import.
pub fn project_update(
    update: &TeslaMateUpdate,
    selected_car_id: i64,
) -> Result<Option<ProjectionUpdate>, TeslaMateProjectionError> {
    require_selected_car("update", update.id, update.car_id, selected_car_id)?;
    if update.start_date_ms <= 0 {
        return Err(TeslaMateProjectionError::InvalidValue {
            field: "update.start_date_ms",
        });
    }
    let Some(end_date_ms) = update.end_date_ms else {
        return Ok(None);
    };
    if end_date_ms < update.start_date_ms {
        return Err(TeslaMateProjectionError::InvalidValue {
            field: "update.end_date_ms",
        });
    }
    let Some(version) = update
        .version
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    Ok(Some(ProjectionUpdate {
        id: update.id,
        car_id: selected_car_id,
        start_date_ms: update.start_date_ms,
        end_date_ms,
        version: version.to_owned(),
    }))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProjectionReport {
    pub completed_drives: u64,
    pub skipped_open_drives: u64,
    pub skipped_unattached_positions: u64,
    pub projected_positions: u64,
    pub projected_charges: u64,
    pub projected_charge_samples: u64,
    pub projected_states: u64,
    pub projected_updates: u64,
    pub skipped_incomplete_updates: u64,
}

impl ProjectionReport {
    pub fn logical_row_count(&self) -> Option<u64> {
        1_u64
            .checked_add(self.completed_drives)?
            .checked_add(self.projected_positions)?
            .checked_add(self.projected_charges)?
            .checked_add(self.projected_charge_samples)?
            .checked_add(self.projected_states)?
            .checked_add(self.projected_updates)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TeslaMateProjection {
    pub snapshot: ProjectionSnapshot,
    /// State rows live in the first pack fragment rather than the projection
    /// snapshot payload, so callers cannot accidentally omit them at publish.
    pub states: Vec<ProjectionState>,
    /// Firmware-update rows are published alongside state rows in schema 2.1.
    pub updates: Vec<ProjectionUpdate>,
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
            car.vin.as_deref(),
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

    let mut report = ProjectionReport::default();
    let mut source_updates = source
        .updates
        .iter()
        .filter(|update| update.car_id == selected_car_id)
        .collect::<Vec<_>>();
    source_updates.sort_unstable_by_key(|update| (update.start_date_ms, update.id));
    let mut updates = Vec::with_capacity(source_updates.len());
    for update in source_updates {
        match project_update(update, selected_car_id)? {
            Some(projected) => {
                updates.push(projected);
                report.projected_updates += 1;
            }
            None => report.skipped_incomplete_updates += 1,
        }
    }
    let projected_car = project_car(car, latest_firmware(&updates))?;

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
            .is_none_or(|drive_id| included_drive_ids.contains(&drive_id));
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
            report.projected_states += 1;
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
        updates,
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

fn latest_firmware(updates: &[ProjectionUpdate]) -> Option<String> {
    updates
        .iter()
        .max_by_key(|update| (update.end_date_ms, update.start_date_ms, update.id))
        .map(|update| update.version.clone())
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
        hub_pack::{
            ProjectionBinding, ProjectionFixedNumericV2_2, ProjectionPackRequest,
            ProjectionPackWriter,
        },
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
    fn address_v2_2_physical_conversion_preserves_every_non_sensitive_field() {
        let projected: ProjectionAddressV2_2 = TeslaMateAddressPhysicalV2_2 {
            id: 100,
            display_name: Some("Home, London".into()),
            latitude_e6: Some(ProjectionFixedNumericV2_2::Finite(51_500_123)),
            longitude_e6: Some(ProjectionFixedNumericV2_2::NaN),
            name: Some("Home".into()),
            house_number: Some("1".into()),
            road: Some("Example Road".into()),
            neighbourhood: Some("Westminster".into()),
            city: Some("London".into()),
            county: Some("Greater London".into()),
            postcode: Some("SW1A 1AA".into()),
            state: Some("England".into()),
            state_district: Some("London".into()),
            country: Some("United Kingdom".into()),
            inserted_at_pg_us: 1_700_000_000_000_000,
            updated_at_pg_us: 1_700_000_100_000_000,
            osm_id: Some(-42),
            osm_type: Some("node".into()),
        }
        .into();
        assert_eq!(
            projected,
            ProjectionAddressV2_2 {
                id: 100,
                display_name: Some("Home, London".into()),
                latitude_e6: Some(ProjectionFixedNumericV2_2::Finite(51_500_123)),
                longitude_e6: Some(ProjectionFixedNumericV2_2::NaN),
                name: Some("Home".into()),
                house_number: Some("1".into()),
                road: Some("Example Road".into()),
                neighbourhood: Some("Westminster".into()),
                city: Some("London".into()),
                county: Some("Greater London".into()),
                postcode: Some("SW1A 1AA".into()),
                state: Some("England".into()),
                state_district: Some("London".into()),
                country: Some("United Kingdom".into()),
                inserted_at_pg_us: 1_700_000_000_000_000,
                updated_at_pg_us: 1_700_000_100_000_000,
                osm_id: Some(-42),
                osm_type: Some("node".into()),
            }
        );
    }

    #[test]
    fn global_settings_v2_2_physical_conversion_preserves_singleton_values() {
        let projected: ProjectionGlobalSettingsV2_2 = TeslaMateSettingsPhysicalV2_2 {
            id: i64::MIN,
            unit_of_length: ProjectionUnitOfLengthV2_2::Miles,
            unit_of_temperature: ProjectionUnitOfTemperatureV2_2::Fahrenheit,
            unit_of_pressure: ProjectionUnitOfPressureV2_2::Psi,
            preferred_range: ProjectionPreferredRangeV2_2::Ideal,
            base_url: Some("é".repeat(255)),
            grafana_url: None,
            language: String::new(),
            theme_mode: String::new(),
            inserted_at_pg_us: i64::MIN,
            updated_at_pg_us: i64::MAX,
        }
        .into();
        assert_eq!(projected.id, i64::MIN);
        assert_eq!(projected.unit_of_length, ProjectionUnitOfLengthV2_2::Miles);
        assert_eq!(
            projected.unit_of_temperature,
            ProjectionUnitOfTemperatureV2_2::Fahrenheit
        );
        assert_eq!(
            projected.unit_of_pressure,
            ProjectionUnitOfPressureV2_2::Psi
        );
        assert_eq!(
            projected.preferred_range,
            ProjectionPreferredRangeV2_2::Ideal
        );
        assert_eq!(projected.base_url.as_deref().unwrap().chars().count(), 255);
        assert_eq!(projected.grafana_url, None);
        assert!(projected.language.is_empty());
        assert!(projected.theme_mode.is_empty());
        assert_eq!(projected.inserted_at_pg_us, i64::MIN);
        assert_eq!(projected.updated_at_pg_us, i64::MAX);
    }

    #[test]
    fn cars_and_car_settings_v2_2_physical_conversion_preserves_source_widths() {
        let settings: ProjectionCarSettingsV2_2 = TeslaMateCarSettingsPhysicalV2_2 {
            id: i64::MIN,
            suspend_min: i32::MIN,
            suspend_after_idle_min: i32::MAX,
            req_not_unlocked: true,
            free_supercharging: false,
            use_streaming_api: true,
            enabled: true,
            lfp_battery: false,
        }
        .into();
        assert_eq!(
            settings,
            ProjectionCarSettingsV2_2 {
                id: i64::MIN,
                suspend_min: i32::MIN,
                suspend_after_idle_min: i32::MAX,
                req_not_unlocked: true,
                free_supercharging: false,
                use_streaming_api: true,
                enabled: true,
                lfp_battery: false,
            }
        );

        let car: ProjectionCarV2_2 = TeslaMateCarPhysicalV2_2 {
            id: i16::MIN,
            eid: i64::MIN,
            vid: i64::MAX,
            vin: None,
            name: None,
            model: Some("Model 3".into()),
            efficiency: Some(-0.145),
            trim_badging: Some("74D".into()),
            marketing_name: Some("LR AWD".into()),
            exterior_color: Some("Pearl White".into()),
            wheel_type: Some("Apollo".into()),
            spoiler_type: Some("None".into()),
            display_priority: i16::MAX,
            inserted_at_pg_us: 1_700_000_000_000_000,
            updated_at_pg_us: 1_700_000_100_000_000,
            settings_id: i64::MIN,
        }
        .into();
        assert_eq!(car.id, i16::MIN);
        assert_eq!(car.eid, i64::MIN);
        assert_eq!(car.vid, i64::MAX);
        assert_eq!(car.efficiency, Some(-0.145));
        assert_eq!(car.display_priority, i16::MAX);
        assert_eq!(car.settings_id, i64::MIN);
    }

    #[test]
    fn states_and_updates_v2_2_physical_conversion_preserves_raw_postgres_timestamps() {
        let state: ProjectionStateV2_2 = TeslaMateStatePhysicalV2_2 {
            id: i32::MIN,
            car_id: i16::MAX,
            state: ProjectionStateStatusV2_2::Asleep,
            start_date_pg_us: i64::MIN,
            end_date_pg_us: Some(i64::MAX),
        }
        .into();
        assert_eq!(
            state,
            ProjectionStateV2_2 {
                id: i32::MIN,
                car_id: i16::MAX,
                state: ProjectionStateStatusV2_2::Asleep,
                start_date_pg_us: i64::MIN,
                end_date_pg_us: Some(i64::MAX),
            }
        );

        let update: ProjectionUpdateV2_2 = TeslaMateUpdatePhysicalV2_2 {
            id: i32::MAX,
            car_id: i16::MIN,
            start_date_pg_us: i64::MAX,
            end_date_pg_us: None,
            version: Some(String::new()),
        }
        .into();
        assert_eq!(
            update,
            ProjectionUpdateV2_2 {
                id: i32::MAX,
                car_id: i16::MIN,
                start_date_pg_us: i64::MAX,
                end_date_pg_us: None,
                version: Some(String::new()),
            }
        );
    }

    #[test]
    fn drives_v2_2_physical_conversion_preserves_every_raw_field() {
        let physical = TeslaMateDrivePhysicalV2_2 {
            id: i32::MIN,
            car_id: i16::MIN,
            start_date_pg_us: i64::MAX,
            end_date_pg_us: Some(i64::MIN),
            start_position_id: Some(i32::MIN),
            end_position_id: Some(i32::MAX),
            start_address_id: Some(-1),
            end_address_id: Some(i32::MAX),
            start_geofence_id: Some(i32::MIN),
            end_geofence_id: None,
            outside_temp_avg_e1: Some(ProjectionFixedNumericV2_2::NaN),
            inside_temp_avg_e1: None,
            speed_max: Some(i16::MIN),
            power_max: Some(i16::MAX),
            power_min: Some(i16::MIN),
            start_ideal_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(999_999)),
            end_ideal_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(-999_999)),
            start_rated_range_km_e2: Some(ProjectionFixedNumericV2_2::NaN),
            end_rated_range_km_e2: None,
            start_km: Some(ProjectionFloat64BitsV2_2((-0.0_f64).to_bits())),
            end_km: Some(ProjectionFloat64BitsV2_2(f64::NEG_INFINITY.to_bits())),
            distance: Some(ProjectionFloat64BitsV2_2(0x7ff8_0000_0000_0042)),
            duration_min: Some(i16::MIN),
            ascent: Some(i16::MAX),
            descent: Some(i16::MIN),
        };
        let projected: ProjectionDriveV2_2 = physical.clone().into();
        assert_eq!(projected.id, physical.id);
        assert_eq!(projected.car_id, physical.car_id);
        assert_eq!(projected.start_date_pg_us, physical.start_date_pg_us);
        assert_eq!(projected.end_date_pg_us, physical.end_date_pg_us);
        assert_eq!(projected.start_position_id, physical.start_position_id);
        assert_eq!(projected.end_position_id, physical.end_position_id);
        assert_eq!(projected.start_address_id, physical.start_address_id);
        assert_eq!(projected.end_address_id, physical.end_address_id);
        assert_eq!(projected.start_geofence_id, physical.start_geofence_id);
        assert_eq!(projected.end_geofence_id, physical.end_geofence_id);
        assert_eq!(projected.outside_temp_avg_e1, physical.outside_temp_avg_e1);
        assert_eq!(projected.inside_temp_avg_e1, physical.inside_temp_avg_e1);
        assert_eq!(projected.speed_max, physical.speed_max);
        assert_eq!(projected.power_max, physical.power_max);
        assert_eq!(projected.power_min, physical.power_min);
        assert_eq!(
            projected.start_ideal_range_km_e2,
            physical.start_ideal_range_km_e2
        );
        assert_eq!(
            projected.end_ideal_range_km_e2,
            physical.end_ideal_range_km_e2
        );
        assert_eq!(
            projected.start_rated_range_km_e2,
            physical.start_rated_range_km_e2
        );
        assert_eq!(
            projected.end_rated_range_km_e2,
            physical.end_rated_range_km_e2
        );
        assert_eq!(projected.start_km, physical.start_km);
        assert_eq!(projected.end_km, physical.end_km);
        assert_eq!(projected.distance, physical.distance);
        assert_eq!(projected.duration_min, physical.duration_min);
        assert_eq!(projected.ascent, physical.ascent);
        assert_eq!(projected.descent, physical.descent);
    }

    #[test]
    fn positions_v2_2_physical_conversion_preserves_every_raw_field() {
        let physical = TeslaMatePositionPhysicalV2_2 {
            id: i32::MIN,
            car_id: i16::MIN,
            drive_id: Some(i32::MAX),
            date_pg_us: i64::MIN,
            latitude_e6: ProjectionFixedNumericV2_2::NaN,
            longitude_e6: ProjectionFixedNumericV2_2::Finite(-999_999_999),
            elevation: Some(i16::MIN),
            speed: Some(i16::MAX),
            power: Some(i16::MIN),
            odometer: Some(ProjectionFloat64BitsV2_2(0x7ff8_0000_0000_0042)),
            ideal_battery_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(999_999)),
            est_battery_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(-999_999)),
            rated_battery_range_km_e2: Some(ProjectionFixedNumericV2_2::NaN),
            battery_level: Some(i16::MIN),
            usable_battery_level: Some(i16::MAX),
            battery_heater: Some(false),
            battery_heater_on: Some(true),
            battery_heater_no_power: None,
            outside_temp_e1: Some(ProjectionFixedNumericV2_2::NaN),
            inside_temp_e1: Some(ProjectionFixedNumericV2_2::Finite(-9_999)),
            fan_status: Some(i32::MIN),
            driver_temp_setting_e1: None,
            passenger_temp_setting_e1: Some(ProjectionFixedNumericV2_2::Finite(9_999)),
            is_climate_on: Some(true),
            is_rear_defroster_on: Some(false),
            is_front_defroster_on: None,
            tpms_pressure_fl_e1: Some(ProjectionFixedNumericV2_2::Finite(-9_999)),
            tpms_pressure_fr_e1: Some(ProjectionFixedNumericV2_2::NaN),
            tpms_pressure_rl_e1: None,
            tpms_pressure_rr_e1: Some(ProjectionFixedNumericV2_2::Finite(9_999)),
        };
        let projected: ProjectionPositionV2_2 = physical.clone().into();
        assert_eq!(projected.id, physical.id);
        assert_eq!(projected.car_id, physical.car_id);
        assert_eq!(projected.drive_id, physical.drive_id);
        assert_eq!(projected.date_pg_us, physical.date_pg_us);
        assert_eq!(projected.latitude_e6, physical.latitude_e6);
        assert_eq!(projected.longitude_e6, physical.longitude_e6);
        assert_eq!(projected.elevation, physical.elevation);
        assert_eq!(projected.speed, physical.speed);
        assert_eq!(projected.power, physical.power);
        assert_eq!(projected.odometer, physical.odometer);
        assert_eq!(
            projected.ideal_battery_range_km_e2,
            physical.ideal_battery_range_km_e2
        );
        assert_eq!(
            projected.est_battery_range_km_e2,
            physical.est_battery_range_km_e2
        );
        assert_eq!(
            projected.rated_battery_range_km_e2,
            physical.rated_battery_range_km_e2
        );
        assert_eq!(projected.battery_level, physical.battery_level);
        assert_eq!(
            projected.usable_battery_level,
            physical.usable_battery_level
        );
        assert_eq!(projected.battery_heater, physical.battery_heater);
        assert_eq!(projected.battery_heater_on, physical.battery_heater_on);
        assert_eq!(
            projected.battery_heater_no_power,
            physical.battery_heater_no_power
        );
        assert_eq!(projected.outside_temp_e1, physical.outside_temp_e1);
        assert_eq!(projected.inside_temp_e1, physical.inside_temp_e1);
        assert_eq!(projected.fan_status, physical.fan_status);
        assert_eq!(
            projected.driver_temp_setting_e1,
            physical.driver_temp_setting_e1
        );
        assert_eq!(
            projected.passenger_temp_setting_e1,
            physical.passenger_temp_setting_e1
        );
        assert_eq!(projected.is_climate_on, physical.is_climate_on);
        assert_eq!(
            projected.is_rear_defroster_on,
            physical.is_rear_defroster_on
        );
        assert_eq!(
            projected.is_front_defroster_on,
            physical.is_front_defroster_on
        );
        assert_eq!(projected.tpms_pressure_fl_e1, physical.tpms_pressure_fl_e1);
        assert_eq!(projected.tpms_pressure_fr_e1, physical.tpms_pressure_fr_e1);
        assert_eq!(projected.tpms_pressure_rl_e1, physical.tpms_pressure_rl_e1);
        assert_eq!(projected.tpms_pressure_rr_e1, physical.tpms_pressure_rr_e1);
    }

    #[test]
    fn charging_v2_2_physical_conversion_preserves_every_raw_field() {
        let process = TeslaMateChargingProcessPhysicalV2_2 {
            id: i32::MIN,
            car_id: i16::MIN,
            position_id: i32::MAX,
            address_id: Some(i32::MIN),
            geofence_id: Some(i32::MAX),
            start_date_pg_us: i64::MIN,
            end_date_pg_us: Some(i64::MAX),
            charge_energy_added_e2: Some(ProjectionFixedNumericV2_2::NaN),
            charge_energy_used_e2: None,
            start_ideal_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(999_999)),
            end_ideal_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(-999_999)),
            start_rated_range_km_e2: Some(ProjectionFixedNumericV2_2::NaN),
            end_rated_range_km_e2: None,
            start_battery_level: Some(i16::MIN),
            end_battery_level: Some(i16::MAX),
            duration_min: Some(i16::MIN),
            outside_temp_avg_e1: Some(ProjectionFixedNumericV2_2::Finite(-9_999)),
            cost_e2: Some(ProjectionFixedNumericV2_2::Finite(999_999)),
        };
        let projected_process: ProjectionChargingProcessV2_2 = process.clone().into();
        assert_eq!(projected_process.id, process.id);
        assert_eq!(projected_process.car_id, process.car_id);
        assert_eq!(projected_process.position_id, process.position_id);
        assert_eq!(projected_process.address_id, process.address_id);
        assert_eq!(projected_process.geofence_id, process.geofence_id);
        assert_eq!(projected_process.start_date_pg_us, process.start_date_pg_us);
        assert_eq!(projected_process.end_date_pg_us, process.end_date_pg_us);
        assert_eq!(
            projected_process.charge_energy_added_e2,
            process.charge_energy_added_e2
        );
        assert_eq!(
            projected_process.charge_energy_used_e2,
            process.charge_energy_used_e2
        );
        assert_eq!(
            projected_process.start_ideal_range_km_e2,
            process.start_ideal_range_km_e2
        );
        assert_eq!(
            projected_process.end_ideal_range_km_e2,
            process.end_ideal_range_km_e2
        );
        assert_eq!(
            projected_process.start_rated_range_km_e2,
            process.start_rated_range_km_e2
        );
        assert_eq!(
            projected_process.end_rated_range_km_e2,
            process.end_rated_range_km_e2
        );
        assert_eq!(
            projected_process.start_battery_level,
            process.start_battery_level
        );
        assert_eq!(
            projected_process.end_battery_level,
            process.end_battery_level
        );
        assert_eq!(projected_process.duration_min, process.duration_min);
        assert_eq!(
            projected_process.outside_temp_avg_e1,
            process.outside_temp_avg_e1
        );
        assert_eq!(projected_process.cost_e2, process.cost_e2);

        let charge = TeslaMateChargePhysicalV2_2 {
            id: i32::MAX,
            charging_process_id: i32::MIN,
            date_pg_us: i64::MAX,
            battery_heater: Some(false),
            battery_heater_on: Some(true),
            battery_heater_no_power: None,
            battery_level: Some(i16::MIN),
            usable_battery_level: Some(i16::MAX),
            charge_energy_added_e2: ProjectionFixedNumericV2_2::NaN,
            charger_actual_current: Some(i16::MIN),
            charger_phases: Some(i16::MAX),
            charger_pilot_current: Some(i16::MIN),
            charger_power: i16::MAX,
            charger_voltage: Some(i16::MIN),
            conn_charge_cable: Some("Type 2".into()),
            fast_charger_present: Some(false),
            fast_charger_brand: Some("Tesla".into()),
            fast_charger_type: Some("Supercharger".into()),
            ideal_battery_range_km_e2: ProjectionFixedNumericV2_2::Finite(-999_999),
            rated_battery_range_km_e2: Some(ProjectionFixedNumericV2_2::NaN),
            not_enough_power_to_heat: Some(false),
            outside_temp_e1: Some(ProjectionFixedNumericV2_2::Finite(9_999)),
        };
        let projected_charge: ProjectionChargeV2_2 = charge.clone().into();
        assert_eq!(projected_charge.id, charge.id);
        assert_eq!(
            projected_charge.charging_process_id,
            charge.charging_process_id
        );
        assert_eq!(projected_charge.date_pg_us, charge.date_pg_us);
        assert_eq!(projected_charge.battery_heater, charge.battery_heater);
        assert_eq!(projected_charge.battery_heater_on, charge.battery_heater_on);
        assert_eq!(
            projected_charge.battery_heater_no_power,
            charge.battery_heater_no_power
        );
        assert_eq!(projected_charge.battery_level, charge.battery_level);
        assert_eq!(
            projected_charge.usable_battery_level,
            charge.usable_battery_level
        );
        assert_eq!(
            projected_charge.charge_energy_added_e2,
            charge.charge_energy_added_e2
        );
        assert_eq!(
            projected_charge.charger_actual_current,
            charge.charger_actual_current
        );
        assert_eq!(projected_charge.charger_phases, charge.charger_phases);
        assert_eq!(
            projected_charge.charger_pilot_current,
            charge.charger_pilot_current
        );
        assert_eq!(projected_charge.charger_power, charge.charger_power);
        assert_eq!(projected_charge.charger_voltage, charge.charger_voltage);
        assert_eq!(projected_charge.conn_charge_cable, charge.conn_charge_cable);
        assert_eq!(
            projected_charge.fast_charger_present,
            charge.fast_charger_present
        );
        assert_eq!(
            projected_charge.fast_charger_brand,
            charge.fast_charger_brand
        );
        assert_eq!(projected_charge.fast_charger_type, charge.fast_charger_type);
        assert_eq!(
            projected_charge.ideal_battery_range_km_e2,
            charge.ideal_battery_range_km_e2
        );
        assert_eq!(
            projected_charge.rated_battery_range_km_e2,
            charge.rated_battery_range_km_e2
        );
        assert_eq!(
            projected_charge.not_enough_power_to_heat,
            charge.not_enough_power_to_heat
        );
        assert_eq!(projected_charge.outside_temp_e1, charge.outside_temp_e1);
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
                projected_states: 0,
                projected_updates: 1,
                skipped_incomplete_updates: 0,
            }
        );
    }

    #[test]
    fn projects_completed_update_history_in_canonical_order_and_skips_placeholders() {
        let mut source = history();
        source.updates.extend([
            TeslaMateUpdate {
                id: 303,
                car_id: 1,
                start_date_ms: 1_800_000_000_000,
                end_date_ms: Some(1_800_000_060_000),
                version: Some(" 2027.4.1 ".into()),
            },
            TeslaMateUpdate {
                id: 302,
                car_id: 1,
                start_date_ms: 1_750_000_000_000,
                end_date_ms: Some(1_750_000_060_000),
                version: Some("   ".into()),
            },
            TeslaMateUpdate {
                id: 301,
                car_id: 1,
                start_date_ms: 1_700_000_000_000,
                end_date_ms: None,
                version: Some("2026.30.1".into()),
            },
        ]);

        let projected = project_vehicle(&source, 1).expect("project updates");
        assert_eq!(
            projected
                .updates
                .iter()
                .map(|update| (update.id, update.version.as_str()))
                .collect::<Vec<_>>(),
            vec![(300, "2026.20.1"), (303, "2027.4.1")]
        );
        assert_eq!(projected.report.projected_updates, 2);
        assert_eq!(projected.report.skipped_incomplete_updates, 2);
        assert_eq!(
            projected.snapshot.cars[0].firmware_version.as_deref(),
            Some("2027.4.1")
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
