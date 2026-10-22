// SPDX-License-Identifier: AGPL-3.0-only

//! C6 Part C spec test of the four history serialisers (C6 ST-S45, C6 R58). Test
//! ids carry their unit prefix (C6 R65). Synthetic data only.
//!
//! Kept keys (C6 R58 (c)): drive `efficiency` keeps its name (no Hub producer
//! assigns its unit); charge sample `charge_cable` is the car's charge-cable
//! text passed through and keeps its name.

use std::collections::BTreeSet;

use serde::{
    Deserialize, Deserializer, Serialize,
    de::{IgnoredAny, MapAccess, Visitor},
};
use serde_json::{Map, Value};

use super::*;
use crate::{
    db::{HubStore, SourceDescriptor, VehicleDescriptor},
    hub_pack::GeofenceBillingType,
};

// ------------------------------------------------------------- the contract

/// The 33 renames of C6 R58 (a): item, 1.2.0 key (the alias), new key.
const PAIRS: [(&str, &str, &str); 33] = [
    ("drive", "start_date_ms", "started_at_ms"),
    ("drive", "end_date_ms", "ended_at_ms"),
    (
        "drive",
        "start_ideal_range_km",
        "start_ideal_battery_range_km",
    ),
    ("drive", "end_ideal_range_km", "end_ideal_battery_range_km"),
    ("drive", "start_rated_range_km", "start_battery_range_km"),
    ("drive", "end_rated_range_km", "end_battery_range_km"),
    ("drive", "start_geofence", "start_geofence_name"),
    ("drive", "end_geofence", "end_geofence_name"),
    ("drive", "speed_max", "speed_max_kmh"),
    ("drive", "power_max", "power_max_kw"),
    ("drive", "power_min", "power_min_kw"),
    ("drive", "outside_temp_avg", "outside_temp_avg_c"),
    ("drive", "inside_temp_avg", "inside_temp_avg_c"),
    ("drive", "ascent", "ascent_m"),
    ("drive", "descent", "descent_m"),
    ("charge", "start_date_ms", "started_at_ms"),
    ("charge", "end_date_ms", "ended_at_ms"),
    (
        "charge",
        "start_ideal_range_km",
        "start_ideal_battery_range_km",
    ),
    ("charge", "end_ideal_range_km", "end_ideal_battery_range_km"),
    ("charge", "start_rated_range_km", "start_battery_range_km"),
    ("charge", "end_rated_range_km", "end_battery_range_km"),
    ("charge", "geofence", "geofence_name"),
    ("charge", "billing_type", "tariff_type"),
    ("charge", "cost_per_unit", "tariff_rate"),
    ("charge", "session_fee", "tariff_session_fee"),
    ("charge", "charge_energy_added", "charge_energy_added_kwh"),
    ("charge", "outside_temp_avg", "outside_temp_avg_c"),
    ("position", "date_ms", "timestamp_ms"),
    ("position", "speed", "speed_kmh"),
    ("position", "odometer", "odometer_km"),
    ("position", "rated_battery_range_km", "battery_range_km"),
    ("charge_sample", "ideal_range_km", "ideal_battery_range_km"),
    ("charge_sample", "rated_range_km", "battery_range_km"),
];

/// The renamed keys that 1.2.0 requires non-null (the epoch times).
const REQUIRED_RENAMED: [(&str, &str); 5] = [
    ("drive", "start_date_ms"),
    ("drive", "end_date_ms"),
    ("charge", "start_date_ms"),
    ("charge", "end_date_ms"),
    ("position", "date_ms"),
];

/// C6 ST-S45 (a) as amended for the served keys: the served keys
/// of each item, in ascending byte order (drive 43, charge 39, position 31,
/// charge sample 21; the address-provenance members are not served).
const DRIVE_KEYS: [&str; 43] = [
    "ascent",
    "ascent_m",
    "descent",
    "descent_m",
    "distance_km",
    "duration_min",
    "efficiency",
    "end_address",
    "end_battery_range_km",
    "end_date_ms",
    "end_geofence",
    "end_geofence_name",
    "end_ideal_battery_range_km",
    "end_ideal_range_km",
    "end_latitude",
    "end_longitude",
    "end_rated_range_km",
    "end_soc",
    "ended_at_ms",
    "id",
    "inside_temp_avg",
    "inside_temp_avg_c",
    "outside_temp_avg",
    "outside_temp_avg_c",
    "power_max",
    "power_max_kw",
    "power_min",
    "power_min_kw",
    "speed_max",
    "speed_max_kmh",
    "start_address",
    "start_battery_range_km",
    "start_date_ms",
    "start_geofence",
    "start_geofence_name",
    "start_ideal_battery_range_km",
    "start_ideal_range_km",
    "start_latitude",
    "start_longitude",
    "start_rated_range_km",
    "start_soc",
    "started_at_ms",
    "vehicle_id",
];

const CHARGE_KEYS: [&str; 39] = [
    "address",
    "billing_type",
    "charge_energy_added",
    "charge_energy_added_kwh",
    "charge_energy_used_kwh",
    "charge_rate_km_per_hour",
    "cost",
    "cost_per_unit",
    "duration_min",
    "end_battery_level",
    "end_battery_range_km",
    "end_date_ms",
    "end_ideal_battery_range_km",
    "end_ideal_range_km",
    "end_rated_range_km",
    "ended_at_ms",
    "fast_charger_type",
    "geofence",
    "geofence_name",
    "id",
    "is_dc",
    "location_name",
    "max_charger_power_kw",
    "outside_temp_avg",
    "outside_temp_avg_c",
    "session_fee",
    "start_battery_level",
    "start_battery_range_km",
    "start_date_ms",
    "start_ideal_battery_range_km",
    "start_ideal_range_km",
    "start_latitude",
    "start_longitude",
    "start_rated_range_km",
    "started_at_ms",
    "tariff_rate",
    "tariff_session_fee",
    "tariff_type",
    "vehicle_id",
];

const POSITION_KEYS: [&str; 31] = [
    "battery_heater",
    "battery_heater_on",
    "battery_level",
    "battery_range_km",
    "date_ms",
    "drive_id",
    "driver_temp_setting",
    "elevation",
    "elevation_provenance",
    "est_battery_range_km",
    "fan_status",
    "id",
    "ideal_battery_range_km",
    "inside_temp",
    "is_climate_on",
    "latitude",
    "longitude",
    "odometer",
    "odometer_km",
    "outside_temp",
    "passenger_temp_setting",
    "power",
    "rated_battery_range_km",
    "speed",
    "speed_kmh",
    "timestamp_ms",
    "tpms_pressure_fl",
    "tpms_pressure_fr",
    "tpms_pressure_rl",
    "tpms_pressure_rr",
    "usable_battery_level",
];

const CHARGE_SAMPLE_KEYS: [&str; 21] = [
    "battery_heater_on",
    "battery_level",
    "battery_range_km",
    "charge_cable",
    "charge_energy_added_kwh",
    "charge_id",
    "charger_actual_current",
    "charger_phases",
    "charger_pilot_current",
    "charger_power_kw",
    "charger_voltage",
    "fast_charger_brand",
    "fast_charger_present",
    "fast_charger_type",
    "id",
    "ideal_battery_range_km",
    "ideal_range_km",
    "outside_temp_c",
    "rated_range_km",
    "timestamp_ms",
    "usable_battery_level",
];

// ---------------------------------------------------------------- fixtures

fn vehicle() -> Uuid {
    Uuid::from_u128(0x6c6c_0001_0000_4000_8000_0000_0000_0001)
}

/// A drive whose every 1.2.0 field holds a distinct non-null value of its type.
fn drive_distinct() -> ProjectionDrive {
    ProjectionDrive {
        id: 11,
        car_id: 1,
        optimized_at_ms: Some(1_760_000_999_001),
        start_date_ms: 1_760_000_100_002,
        end_date_ms: 1_760_000_900_003,
        distance_km: Some(12.5),
        duration_min: Some(13),
        efficiency: Some(0.1875),
        outside_temp_avg: Some(14.25),
        inside_temp_avg: Some(15.5),
        speed_max: Some(116),
        power_max: Some(117.75),
        power_min: Some(-18.5),
        start_ideal_range_km: Some(319.25),
        end_ideal_range_km: Some(220.5),
        start_address: Some("Start address 21".to_owned()),
        end_address: Some("End address 22".to_owned()),
        start_geofence: Some("Start fence 23".to_owned()),
        end_geofence: Some("End fence 24".to_owned()),
        start_latitude: Some(47.125),
        start_longitude: Some(19.375),
        end_latitude: Some(47.625),
        end_longitude: Some(19.875),
        start_soc: Some(88),
        end_soc: Some(61),
        start_rated_range_km: Some(301.75),
        end_rated_range_km: Some(202.25),
        ascent: Some(131),
        descent: Some(132),
    }
}

fn drive_null() -> ProjectionDrive {
    ProjectionDrive {
        id: 12,
        car_id: 1,
        optimized_at_ms: None,
        start_date_ms: 1_760_001_100_002,
        end_date_ms: 1_760_001_900_003,
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
        end_geofence: None,
        start_latitude: None,
        start_longitude: None,
        end_latitude: None,
        end_longitude: None,
        start_soc: None,
        end_soc: None,
        start_rated_range_km: None,
        end_rated_range_km: None,
        ascent: None,
        descent: None,
    }
}

fn charge_distinct() -> ProjectionCharge {
    ProjectionCharge {
        id: 21,
        car_id: 1,
        start_date_ms: 1_760_002_100_002,
        end_date_ms: Some(1_760_002_900_003),
        charge_energy_added: Some(41.5),
        charge_energy_used_kwh: Some(42.25),
        start_ideal_range_km: Some(143.75),
        end_ideal_range_km: Some(244.5),
        cost: Some(7.125),
        fast_charger_type: Some("Charger type 47".to_owned()),
        billing_type: Some(GeofenceBillingType::PerMinute),
        cost_per_unit: Some(0.3125),
        session_fee: Some(1.0625),
        start_latitude: Some(48.125),
        start_longitude: Some(20.375),
        start_battery_level: Some(22),
        end_battery_level: Some(93),
        duration_min: Some(54),
        address: Some("Charge address 55".to_owned()),
        location_name: Some("Location 56".to_owned()),
        geofence: Some("Charge fence 57".to_owned()),
        is_dc: Some(true),
        charge_rate_km_per_hour: Some(58.5),
        max_charger_power_kw: Some(159.75),
        outside_temp_avg: Some(8.625),
        start_rated_range_km: Some(130.25),
        end_rated_range_km: Some(231.5),
    }
}

fn charge_null() -> ProjectionCharge {
    ProjectionCharge {
        id: 22,
        car_id: 1,
        start_date_ms: 1_760_003_100_002,
        end_date_ms: Some(1_760_003_900_003),
        charge_energy_added: None,
        charge_energy_used_kwh: None,
        start_ideal_range_km: None,
        end_ideal_range_km: None,
        cost: None,
        fast_charger_type: None,
        billing_type: None,
        cost_per_unit: None,
        session_fee: None,
        start_latitude: None,
        start_longitude: None,
        start_battery_level: None,
        end_battery_level: None,
        duration_min: None,
        address: None,
        location_name: None,
        geofence: None,
        is_dc: None,
        charge_rate_km_per_hour: None,
        max_charger_power_kw: None,
        outside_temp_avg: None,
        start_rated_range_km: None,
        end_rated_range_km: None,
    }
}

fn position_distinct() -> ProjectionPosition {
    ProjectionPosition {
        id: 31,
        drive_id: Some(11),
        car_id: 1,
        date_ms: 1_760_000_500_004,
        latitude: 47.375,
        longitude: 19.625,
        speed: Some(65),
        power: Some(66.5),
        battery_level: Some(67),
        usable_battery_level: Some(68),
        elevation: Some(169),
        odometer: Some(1_070.125),
        ideal_battery_range_km: Some(271.25),
        est_battery_range_km: Some(272.5),
        rated_battery_range_km: Some(273.75),
        fan_status: Some(74),
        driver_temp_setting: Some(21.5),
        passenger_temp_setting: Some(22.5),
        is_climate_on: Some(true),
        is_rear_defroster_on: Some(true),
        is_front_defroster_on: Some(false),
        inside_temp: Some(23.25),
        outside_temp: Some(9.75),
        battery_heater: Some(true),
        battery_heater_on: Some(false),
        battery_heater_no_power: Some(true),
        tpms_pressure_fl: Some(2.8125),
        tpms_pressure_fr: Some(2.875),
        tpms_pressure_rl: Some(2.9375),
        tpms_pressure_rr: Some(3.0),
    }
}

fn position_null() -> ProjectionPosition {
    ProjectionPosition {
        id: 32,
        drive_id: Some(12),
        car_id: 1,
        date_ms: 1_760_001_500_004,
        latitude: 47.5,
        longitude: 19.5,
        speed: None,
        power: None,
        battery_level: None,
        usable_battery_level: None,
        elevation: None,
        odometer: None,
        ideal_battery_range_km: None,
        est_battery_range_km: None,
        rated_battery_range_km: None,
        fan_status: None,
        driver_temp_setting: None,
        passenger_temp_setting: None,
        is_climate_on: None,
        is_rear_defroster_on: None,
        is_front_defroster_on: None,
        inside_temp: None,
        outside_temp: None,
        battery_heater: None,
        battery_heater_on: None,
        battery_heater_no_power: None,
        tpms_pressure_fl: None,
        tpms_pressure_fr: None,
        tpms_pressure_rl: None,
        tpms_pressure_rr: None,
    }
}

fn terrain() -> PositionTerrainLookup {
    PositionTerrainLookup {
        dataset: Some("Terrain dataset 81".to_owned()),
        dataset_version: Some("v82".to_owned()),
        tile: Some("tile-83".to_owned()),
    }
}

fn sample_distinct() -> ProjectionChargeSample {
    ProjectionChargeSample {
        id: 41,
        charge_process_id: 21,
        timestamp_ms: 1_760_002_500_005,
        battery_level: Some(52),
        usable_battery_level: Some(53),
        charge_energy_added_kwh: Some(5.4375),
        charger_power_kw: Some(55.5),
        charger_voltage: Some(396.25),
        charger_actual_current: Some(57.75),
        charger_pilot_current: Some(58.25),
        charger_phases: Some(3),
        ideal_range_km: Some(159.5),
        rated_range_km: Some(160.75),
        outside_temp_c: Some(6.125),
        battery_heater_on: Some(true),
        battery_heater: Some(false),
        battery_heater_no_power: Some(true),
        not_enough_power_to_heat: Some(false),
        fast_charger_present: Some(true),
        fast_charger_brand: Some("Brand 64".to_owned()),
        fast_charger_type: Some("Sample type 65".to_owned()),
        charge_cable: Some("Cable 66".to_owned()),
    }
}

fn sample_null() -> ProjectionChargeSample {
    ProjectionChargeSample {
        id: 42,
        charge_process_id: 22,
        timestamp_ms: 1_760_003_500_005,
        battery_level: None,
        usable_battery_level: None,
        charge_energy_added_kwh: None,
        charger_power_kw: None,
        charger_voltage: None,
        charger_actual_current: None,
        charger_pilot_current: None,
        charger_phases: None,
        ideal_range_km: None,
        rated_range_km: None,
        outside_temp_c: None,
        battery_heater_on: None,
        battery_heater: None,
        battery_heater_no_power: None,
        not_enough_power_to_heat: None,
        fast_charger_present: None,
        fast_charger_brand: None,
        fast_charger_type: None,
        charge_cable: None,
    }
}

// --------------------------------------------- the 1.2.0 serialiser (golden)

/// The four items as 1.2.0 served them: derived serialisers over the 1.2.0
/// field names, built straight from the stored rows (not through the new
/// types), the golden body of C6 ST-S45 (c).
mod golden_1_2_0 {
    use super::*;

    #[derive(Serialize)]
    pub(super) struct Drive {
        id: i64,
        vehicle_id: Uuid,
        start_date_ms: i64,
        end_date_ms: i64,
        distance_km: Option<f64>,
        duration_min: Option<i64>,
        efficiency: Option<f64>,
        outside_temp_avg: Option<f64>,
        inside_temp_avg: Option<f64>,
        speed_max: Option<i64>,
        power_max: Option<f64>,
        power_min: Option<f64>,
        start_ideal_range_km: Option<f64>,
        end_ideal_range_km: Option<f64>,
        start_address: Option<String>,
        end_address: Option<String>,
        start_geofence: Option<String>,
        end_geofence: Option<String>,
        start_latitude: Option<f64>,
        start_longitude: Option<f64>,
        end_latitude: Option<f64>,
        end_longitude: Option<f64>,
        start_soc: Option<i64>,
        end_soc: Option<i64>,
        start_rated_range_km: Option<f64>,
        end_rated_range_km: Option<f64>,
        ascent: Option<i64>,
        descent: Option<i64>,
    }

    impl Drive {
        pub(super) fn of(vehicle_id: Uuid, d: &ProjectionDrive) -> Self {
            let d = d.clone();
            Self {
                id: d.id,
                vehicle_id,
                start_date_ms: d.start_date_ms,
                end_date_ms: d.end_date_ms,
                distance_km: d.distance_km,
                duration_min: d.duration_min,
                efficiency: d.efficiency,
                outside_temp_avg: d.outside_temp_avg,
                inside_temp_avg: d.inside_temp_avg,
                speed_max: d.speed_max,
                power_max: d.power_max,
                power_min: d.power_min,
                start_ideal_range_km: d.start_ideal_range_km,
                end_ideal_range_km: d.end_ideal_range_km,
                start_address: d.start_address,
                end_address: d.end_address,
                start_geofence: d.start_geofence,
                end_geofence: d.end_geofence,
                start_latitude: d.start_latitude,
                start_longitude: d.start_longitude,
                end_latitude: d.end_latitude,
                end_longitude: d.end_longitude,
                start_soc: d.start_soc,
                end_soc: d.end_soc,
                start_rated_range_km: d.start_rated_range_km,
                end_rated_range_km: d.end_rated_range_km,
                ascent: d.ascent,
                descent: d.descent,
            }
        }
    }

    #[derive(Serialize)]
    pub(super) struct Charge {
        id: i64,
        vehicle_id: Uuid,
        start_date_ms: i64,
        end_date_ms: i64,
        duration_min: Option<i64>,
        charge_energy_added: Option<f64>,
        charge_energy_used_kwh: Option<f64>,
        start_battery_level: Option<i64>,
        end_battery_level: Option<i64>,
        start_ideal_range_km: Option<f64>,
        end_ideal_range_km: Option<f64>,
        start_rated_range_km: Option<f64>,
        end_rated_range_km: Option<f64>,
        start_latitude: Option<f64>,
        start_longitude: Option<f64>,
        address: Option<String>,
        location_name: Option<String>,
        geofence: Option<String>,
        fast_charger_type: Option<String>,
        is_dc: Option<bool>,
        charge_rate_km_per_hour: Option<f64>,
        max_charger_power_kw: Option<f64>,
        outside_temp_avg: Option<f64>,
        cost: Option<f64>,
        billing_type: Option<GeofenceBillingType>,
        cost_per_unit: Option<f64>,
        session_fee: Option<f64>,
    }

    impl Charge {
        pub(super) fn of(vehicle_id: Uuid, c: &ProjectionCharge) -> Self {
            let c = c.clone();
            Self {
                id: c.id,
                vehicle_id,
                start_date_ms: c.start_date_ms,
                end_date_ms: c.end_date_ms.expect("closed charge"),
                duration_min: c.duration_min,
                charge_energy_added: c.charge_energy_added,
                charge_energy_used_kwh: c.charge_energy_used_kwh,
                start_battery_level: c.start_battery_level,
                end_battery_level: c.end_battery_level,
                start_ideal_range_km: c.start_ideal_range_km,
                end_ideal_range_km: c.end_ideal_range_km,
                start_rated_range_km: c.start_rated_range_km,
                end_rated_range_km: c.end_rated_range_km,
                start_latitude: c.start_latitude,
                start_longitude: c.start_longitude,
                address: c.address,
                location_name: c.location_name,
                geofence: c.geofence,
                fast_charger_type: c.fast_charger_type,
                is_dc: c.is_dc,
                charge_rate_km_per_hour: c.charge_rate_km_per_hour,
                max_charger_power_kw: c.max_charger_power_kw,
                outside_temp_avg: c.outside_temp_avg,
                cost: c.cost,
                billing_type: c.billing_type,
                cost_per_unit: c.cost_per_unit,
                session_fee: c.session_fee,
            }
        }
    }

    #[derive(Serialize)]
    pub(super) struct Position {
        id: i64,
        drive_id: i64,
        date_ms: i64,
        latitude: f64,
        longitude: f64,
        speed: Option<i64>,
        power: Option<f64>,
        battery_level: Option<i64>,
        usable_battery_level: Option<i64>,
        elevation: Option<i64>,
        odometer: Option<f64>,
        ideal_battery_range_km: Option<f64>,
        est_battery_range_km: Option<f64>,
        rated_battery_range_km: Option<f64>,
        inside_temp: Option<f64>,
        outside_temp: Option<f64>,
        is_climate_on: Option<bool>,
        fan_status: Option<i64>,
        driver_temp_setting: Option<f64>,
        passenger_temp_setting: Option<f64>,
        battery_heater: Option<bool>,
        battery_heater_on: Option<bool>,
        tpms_pressure_fl: Option<f64>,
        tpms_pressure_fr: Option<f64>,
        tpms_pressure_rl: Option<f64>,
        tpms_pressure_rr: Option<f64>,
        elevation_provenance: Option<PublicElevationProvenance>,
    }

    impl Position {
        pub(super) fn of(
            drive_id: i64,
            p: &ProjectionPosition,
            provenance: Option<PublicElevationProvenance>,
        ) -> Self {
            let p = p.clone();
            Self {
                id: p.id,
                drive_id,
                date_ms: p.date_ms,
                latitude: p.latitude,
                longitude: p.longitude,
                speed: p.speed,
                power: p.power,
                battery_level: p.battery_level,
                usable_battery_level: p.usable_battery_level,
                elevation: p.elevation,
                odometer: p.odometer,
                ideal_battery_range_km: p.ideal_battery_range_km,
                est_battery_range_km: p.est_battery_range_km,
                rated_battery_range_km: p.rated_battery_range_km,
                inside_temp: p.inside_temp,
                outside_temp: p.outside_temp,
                is_climate_on: p.is_climate_on,
                fan_status: p.fan_status,
                driver_temp_setting: p.driver_temp_setting,
                passenger_temp_setting: p.passenger_temp_setting,
                battery_heater: p.battery_heater,
                battery_heater_on: p.battery_heater_on,
                tpms_pressure_fl: p.tpms_pressure_fl,
                tpms_pressure_fr: p.tpms_pressure_fr,
                tpms_pressure_rl: p.tpms_pressure_rl,
                tpms_pressure_rr: p.tpms_pressure_rr,
                elevation_provenance: provenance,
            }
        }
    }

    #[derive(Serialize)]
    pub(super) struct ChargeSample {
        id: i64,
        charge_id: i64,
        timestamp_ms: i64,
        battery_level: Option<i64>,
        usable_battery_level: Option<i64>,
        charge_energy_added_kwh: Option<f64>,
        charger_power_kw: Option<f64>,
        charger_voltage: Option<f64>,
        charger_actual_current: Option<f64>,
        charger_pilot_current: Option<f64>,
        charger_phases: Option<i64>,
        ideal_range_km: Option<f64>,
        rated_range_km: Option<f64>,
        outside_temp_c: Option<f64>,
        battery_heater_on: Option<bool>,
        fast_charger_present: Option<bool>,
        fast_charger_brand: Option<String>,
        fast_charger_type: Option<String>,
        charge_cable: Option<String>,
    }

    impl ChargeSample {
        pub(super) fn of(s: &ProjectionChargeSample) -> Self {
            let s = s.clone();
            Self {
                id: s.id,
                charge_id: s.charge_process_id,
                timestamp_ms: s.timestamp_ms,
                battery_level: s.battery_level,
                usable_battery_level: s.usable_battery_level,
                charge_energy_added_kwh: s.charge_energy_added_kwh,
                charger_power_kw: s.charger_power_kw,
                charger_voltage: s.charger_voltage,
                charger_actual_current: s.charger_actual_current,
                charger_pilot_current: s.charger_pilot_current,
                charger_phases: s.charger_phases,
                ideal_range_km: s.ideal_range_km,
                rated_range_km: s.rated_range_km,
                outside_temp_c: s.outside_temp_c,
                battery_heater_on: s.battery_heater_on,
                fast_charger_present: s.fast_charger_present,
                fast_charger_brand: s.fast_charger_brand,
                fast_charger_type: s.fast_charger_type,
                charge_cable: s.charge_cable,
            }
        }
    }
}

// ------------------------------------------------------------------ helpers

/// The keys of a serialised JSON object in the order the bytes carry them.
struct KeyOrder(Vec<String>);

impl<'de> Deserialize<'de> for KeyOrder {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Keys;
        impl<'de> Visitor<'de> for Keys {
            type Value = KeyOrder;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<KeyOrder, A::Error> {
                let mut keys = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    map.next_value::<IgnoredAny>()?;
                    keys.push(key);
                }
                Ok(KeyOrder(keys))
            }
        }
        deserializer.deserialize_map(Keys)
    }
}

fn key_order(bytes: &[u8]) -> Vec<String> {
    serde_json::from_slice::<KeyOrder>(bytes)
        .expect("a JSON object")
        .0
}

/// One served item of one fixture: its bytes and the 1.2.0 golden body.
struct Case {
    item: &'static str,
    fixture: &'static str,
    expected_keys: &'static [&'static str],
    bytes: Vec<u8>,
    golden: Value,
}

fn case<T: Serialize, G: Serialize>(
    item: &'static str,
    fixture: &'static str,
    expected_keys: &'static [&'static str],
    served: &T,
    golden: &G,
) -> Case {
    Case {
        item,
        fixture,
        expected_keys,
        bytes: serde_json::to_vec(served).expect("served item"),
        golden: serde_json::to_value(golden).expect("golden item"),
    }
}

fn provenance_of(terrain: PositionTerrainLookup) -> PublicElevationProvenance {
    PublicElevationProvenance {
        source: "terrain",
        dataset: terrain.dataset,
        dataset_version: terrain.dataset_version,
        tile: terrain.tile,
    }
}

/// The eight cases: four item types, each with the distinct non-null fixture
/// and the all-null fixture.
fn cases() -> Vec<Case> {
    let v = vehicle();
    let mut all = Vec::new();
    for (fixture, row) in [("distinct", drive_distinct()), ("null", drive_null())] {
        let served = PublicDrive::from_projection(v, row.clone());
        all.push(case(
            "drive",
            fixture,
            &DRIVE_KEYS,
            &served,
            &golden_1_2_0::Drive::of(v, &row),
        ));
    }
    for (fixture, row) in [("distinct", charge_distinct()), ("null", charge_null())] {
        let served = PublicCharge::from_projection(v, row.clone()).expect("closed charge");
        all.push(case(
            "charge",
            fixture,
            &CHARGE_KEYS,
            &served,
            &golden_1_2_0::Charge::of(v, &row),
        ));
    }
    let with_terrain = position_distinct();
    let served = PublicPosition::from_projection(11, with_terrain.clone(), Some(terrain()));
    all.push(case(
        "position",
        "distinct",
        &POSITION_KEYS,
        &served,
        &golden_1_2_0::Position::of(11, &with_terrain, Some(provenance_of(terrain()))),
    ));
    let without = position_null();
    let served = PublicPosition::from_projection(12, without.clone(), None);
    all.push(case(
        "position",
        "null",
        &POSITION_KEYS,
        &served,
        &golden_1_2_0::Position::of(12, &without, None),
    ));
    for (fixture, row) in [("distinct", sample_distinct()), ("null", sample_null())] {
        let served = PublicChargeSample::from_projection(row.clone());
        all.push(case(
            "charge_sample",
            fixture,
            &CHARGE_SAMPLE_KEYS,
            &served,
            &golden_1_2_0::ChargeSample::of(&row),
        ));
    }
    all
}

/// The published 1.2.0 `resources.schema.json` (the item definitions of the
/// first half of C6 ST-S46: a served item stays valid against them).
fn resources_schema_1_2_0() -> Value {
    let path = format!(
        "{}/protocol/profiles/hub-http-v1/1.2.0/resources.schema.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(path).expect("committed hub-http-v1@1.2.0 schema");
    serde_json::from_str(&text).expect("schema JSON")
}

/// A value against a JSON-Schema fragment of the 1.2.0 resources: `$ref` into
/// `$defs`, `anyOf`, `enum`, `type`, `maxLength`, the int64 bounds and nested
/// `properties` and `required`; members a schema does not list are open.
fn schema_allows(defs: &Value, schema: &Value, value: &Value) -> bool {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let name = reference.strip_prefix("#/$defs/").expect("local $defs ref");
        return schema_allows(defs, &defs[name], value);
    }
    if let Some(options) = schema.get("anyOf").and_then(Value::as_array) {
        return options
            .iter()
            .any(|option| schema_allows(defs, option, value));
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array) {
        return options.contains(value);
    }
    match schema["type"].as_str() {
        Some("null") => value.is_null(),
        Some("boolean") => value.is_boolean(),
        Some("number") => value.is_number(),
        Some("integer") => value.is_i64() || value.is_u64(),
        Some("string") => value.as_str().is_some_and(|text| {
            text.chars().count() as u64 <= schema["maxLength"].as_u64().unwrap_or(u64::MAX)
        }),
        Some("object") => {
            let Some(object) = value.as_object() else {
                return false;
            };
            let properties = schema["properties"].as_object().expect("properties");
            let required = schema["required"].as_array().expect("required");
            required
                .iter()
                .all(|name| object.contains_key(name.as_str().expect("name")))
                && object.iter().all(|(key, member)| {
                    properties
                        .get(key)
                        .is_none_or(|property| schema_allows(defs, property, member))
                })
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------- test

#[test]
fn c6_st_s45_history_aliases() {
    // The table itself: 15 + 12 + 4 + 2 = 33 pairs, no duplicate, and the key
    // lists are strictly ascending in byte order with the stated counts.
    let count = |item: &str| PAIRS.iter().filter(|pair| pair.0 == item).count();
    assert_eq!(
        [
            count("drive"),
            count("charge"),
            count("position"),
            count("charge_sample")
        ],
        [15, 12, 4, 2]
    );
    assert_eq!(
        PAIRS
            .iter()
            .map(|pair| (pair.0, pair.1))
            .collect::<BTreeSet<_>>()
            .len(),
        33
    );
    for keys in [
        &DRIVE_KEYS[..],
        &CHARGE_KEYS[..],
        &POSITION_KEYS[..],
        &CHARGE_SAMPLE_KEYS[..],
    ] {
        assert!(
            keys.windows(2)
                .all(|pair| pair[0].as_bytes() < pair[1].as_bytes()),
            "ascending byte order"
        );
    }
    assert_eq!(
        [
            DRIVE_KEYS.len(),
            CHARGE_KEYS.len(),
            POSITION_KEYS.len(),
            CHARGE_SAMPLE_KEYS.len()
        ],
        [43, 39, 31, 21]
    );

    let cases = cases();
    assert_eq!(cases.len(), 8);
    for case in &cases {
        let label = format!("{} ({})", case.item, case.fixture);
        let body: Value = serde_json::from_slice(&case.bytes).expect("item JSON");
        let object = body.as_object().expect("item object");

        // (a) The exact served keys, in the order the bytes carry them.
        assert_eq!(
            key_order(&case.bytes),
            case.expected_keys,
            "{label}: exact ordered key list"
        );
        assert_eq!(object.len(), case.expected_keys.len(), "{label}: key count");

        // (b) Each of the 33 pairs: the alias equals the new key, in both
        // fixtures, and the new key carries the value 1.2.0 served under the old
        // key (so neither side reads another field).
        let pairs: Vec<_> = PAIRS.iter().filter(|pair| pair.0 == case.item).collect();
        for (_, old, new) in &pairs {
            assert!(object.contains_key(*new), "{label}: new key {new} written");
            assert!(object.contains_key(*old), "{label}: alias {old} written");
            assert_eq!(object[*old], object[*new], "{label}: {old} equals {new}");
            assert_eq!(
                object[*new], case.golden[*old],
                "{label}: {new} carries the 1.2.0 value of {old}"
            );
            // The null fixture nulls every nullable field; the epoch times a
            // 1.2.0 item requires stay non-null (null as null only where
            // nullable).
            let required = REQUIRED_RENAMED.contains(&(case.item, *old));
            if case.fixture == "distinct" || required {
                assert!(!object[*new].is_null(), "{label}: {new} non-null");
            } else {
                assert!(object[*new].is_null(), "{label}: {new} null");
            }
        }
        if case.fixture == "distinct" {
            // Every pair of the distinct fixture holds its own value: no two
            // renamed keys of one item share a value.
            let values: BTreeSet<String> = pairs
                .iter()
                .map(|(_, _, new)| object[*new].to_string())
                .collect();
            let distinct_news: BTreeSet<&str> = pairs.iter().map(|pair| pair.2).collect();
            assert_eq!(
                values.len(),
                distinct_news.len(),
                "{label}: distinct values"
            );
        }

        // (c) Removing the new keys restores exactly the 1.2.0 keys and values.
        let mut restored: Map<String, Value> = object.clone();
        for (_, _, new) in &pairs {
            restored.remove(*new);
        }
        assert_eq!(
            Value::Object(restored),
            case.golden,
            "{label}: the 1.2.0 body"
        );
        assert_eq!(
            case.golden.as_object().expect("golden").len(),
            case.expected_keys.len() - pairs.len(),
            "{label}: 1.2.0 key count"
        );
    }
    // (e) Every served item validates against the published 1.2.0 item schema
    // (C6 ST-S46, the 1.2.0 half; the new minor's schemas are C6-C2's). The
    // schema accepts the served body and refuses a body that lacks a required key
    // or holds a wrongly typed one, so the check cannot pass vacuously.
    let schema = resources_schema_1_2_0();
    let defs = &schema["$defs"];
    for case in &cases {
        let label = format!("{} ({})", case.item, case.fixture);
        let body: Value = serde_json::from_slice(&case.bytes).expect("item JSON");
        let item_schema = &defs[case.item];
        assert!(
            schema_allows(defs, item_schema, &body),
            "{label}: valid against the 1.2.0 item schema"
        );
        let required = item_schema["required"].as_array().expect("required");
        assert!(required.len() >= 19, "{label}: the schema requires keys");
        for name in required {
            let name = name.as_str().expect("required name");
            let mut missing = body.clone();
            missing.as_object_mut().expect("item").remove(name);
            assert!(
                !schema_allows(defs, item_schema, &missing),
                "{label}: refused without {name}"
            );
            let mut wrong = body.clone();
            wrong[name] = Value::from("not the stored type");
            if !schema_allows(
                defs,
                &item_schema["properties"][name],
                &Value::from("not the stored type"),
            ) {
                assert!(
                    !schema_allows(defs, item_schema, &wrong),
                    "{label}: refused with a wrongly typed {name}"
                );
            }
        }
    }

    // The served 1.2.0 key counts of the four items.
    let counts: Vec<usize> = cases
        .iter()
        .filter(|case| case.fixture == "distinct")
        .map(|case| case.golden.as_object().expect("golden").len())
        .collect();
    assert_eq!(counts, [28, 27, 27, 19]);

    // The non-null position carries its provenance object, the null one null,
    // and the drive and charge items carry no address-provenance member.
    let position = |fixture: &str| -> Value {
        let case = cases
            .iter()
            .find(|case| case.item == "position" && case.fixture == fixture)
            .expect("position case");
        serde_json::from_slice(&case.bytes).expect("position JSON")
    };
    assert_eq!(
        position("distinct")["elevation_provenance"],
        serde_json::json!({
            "source": "terrain",
            "dataset": "Terrain dataset 81",
            "dataset_version": "v82",
            "tile": "tile-83"
        })
    );
    assert!(position("null")["elevation_provenance"].is_null());
    for case in &cases {
        for member in [
            "start_address_provenance",
            "end_address_provenance",
            "address_provenance",
        ] {
            assert!(
                !case.expected_keys.contains(&member),
                "{} serves no {member}",
                case.item
            );
        }
    }

    // (d) The same item serialises to the same bytes twice, and an item rebuilt
    // from its stored row after a store reopen serialises to the bytes before the
    // reopen. The stored shapes keep the 1.2.0 names.
    let again = self::cases();
    for (first, second) in cases.iter().zip(&again) {
        assert_eq!(first.bytes, second.bytes, "{}: repeat bytes", first.item);
    }
    assert_stored_shapes_and_reopen_bytes();
}

/// The stored rows keep their serde names (C6 R58 (d)), and the served bytes of
/// rows read back from a store before and after a reopen are equal.
fn assert_stored_shapes_and_reopen_bytes() {
    let stored_keys = |value: &Value| -> BTreeSet<String> {
        value
            .as_object()
            .expect("stored row")
            .keys()
            .cloned()
            .collect()
    };
    let drive = serde_json::to_value(drive_distinct()).expect("stored drive");
    let charge = serde_json::to_value(charge_distinct()).expect("stored charge");
    let position = serde_json::to_value(position_distinct()).expect("stored position");
    let sample = serde_json::to_value(sample_distinct()).expect("stored sample");
    for (item, stored) in [
        ("drive", stored_keys(&drive)),
        ("charge", stored_keys(&charge)),
        ("position", stored_keys(&position)),
        ("charge_sample", stored_keys(&sample)),
    ] {
        for (_, old, new) in PAIRS.iter().filter(|pair| pair.0 == item) {
            assert!(stored.contains(*old), "{item}: stored key {old}");
            assert!(!stored.contains(*new), "{item}: {new} is not a stored key");
        }
    }
    assert_eq!(
        serde_json::from_value::<ProjectionDrive>(drive).expect("decode"),
        drive_distinct()
    );
    assert_eq!(
        serde_json::from_value::<ProjectionCharge>(charge).expect("decode"),
        charge_distinct()
    );
    assert_eq!(
        serde_json::from_value::<ProjectionPosition>(position).expect("decode"),
        position_distinct()
    );
    assert_eq!(
        serde_json::from_value::<ProjectionChargeSample>(sample).expect("decode"),
        sample_distinct()
    );

    let temp = crate::private_tempdir().expect("temporary store directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let source = store
        .register_source(&SourceDescriptor::new("owner_api", "c6-c1-history"), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "7").with_tesla_identity(Some(7), None),
            1_000,
        )
        .expect("vehicle")
        .vehicle_id;
    let connection = store.open().expect("open store");
    let vehicle_text = vehicle.to_string();
    connection
        .execute(
            "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
             VALUES (?1, 11, 1, ?2)",
            rusqlite::params![
                vehicle_text,
                serde_json::to_string(&drive_distinct()).unwrap()
            ],
        )
        .expect("seed drive");
    connection
        .execute(
            "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
             VALUES (?1, 21, 1, ?2)",
            rusqlite::params![
                vehicle_text,
                serde_json::to_string(&charge_distinct()).unwrap()
            ],
        )
        .expect("seed charge");
    connection
        .execute(
            "INSERT INTO materialised_positions(vehicle_id, position_id, drive_id, car_id, position_json)
             VALUES (?1, 31, 11, 1, ?2)",
            rusqlite::params![vehicle_text, serde_json::to_string(&position_distinct()).unwrap()],
        )
        .expect("seed position");
    connection
        .execute(
            "INSERT INTO materialised_charge_samples(vehicle_id, sample_id, charge_id, sample_json)
             VALUES (?1, 41, 21, ?2)",
            rusqlite::params![
                vehicle_text,
                serde_json::to_string(&sample_distinct()).unwrap()
            ],
        )
        .expect("seed sample");
    connection
        .execute(
            "INSERT INTO terrain_elevation_provenance(
                 vehicle_id, position_id, drive_id, latitude, longitude, elevation_m,
                 tile_name, tile_hash, dataset_source, dataset_version, status,
                 error_code, attempts, attempted_at_ms, retry_after_ms
             ) VALUES (?1, 31, 11, 47.375, 19.625, NULL, 'tile-83', NULL,
                       'Terrain dataset 81', 'v82', 'success', NULL, 1, 1000, 0)",
            rusqlite::params![vehicle_text],
        )
        .expect("seed terrain provenance");
    drop(connection);

    let served_bytes = |store: &HubStore| -> Vec<Vec<u8>> {
        let drive = store
            .materialised_closed_drive_for_vehicle(vehicle, 11)
            .expect("read drive")
            .expect("drive");
        let charge = store
            .materialised_charge_for_vehicle(vehicle, 21)
            .expect("read charge")
            .expect("charge");
        let position = store
            .drive_positions_window_page(vehicle, 11, 0, i64::MAX, None, 10)
            .expect("read positions")
            .into_iter()
            .next()
            .expect("position");
        let sample = store
            .charge_samples_page(vehicle, 21, 0, i64::MAX, None, 10)
            .expect("read samples")
            .into_iter()
            .next()
            .expect("sample");
        vec![
            serde_json::to_vec(&PublicDrive::from_projection(vehicle, drive)).unwrap(),
            serde_json::to_vec(&PublicCharge::from_projection(vehicle, charge).unwrap()).unwrap(),
            serde_json::to_vec(&PublicPosition::from_projection(
                11,
                position.position,
                position.terrain,
            ))
            .unwrap(),
            serde_json::to_vec(&PublicChargeSample::from_projection(sample)).unwrap(),
        ]
    };
    let before = served_bytes(&store);
    assert_eq!(before, served_bytes(&store), "repeat bytes from the store");
    // The stored position carries provenance through the terrain table.
    let position: Value = serde_json::from_slice(&before[2]).expect("position JSON");
    assert_eq!(position["elevation_provenance"]["tile"], "tile-83");
    drop(store);
    let reopened = HubStore::initialize(temp.path()).expect("reopened store");
    assert_eq!(
        before,
        served_bytes(&reopened),
        "bytes after a store reopen"
    );
    // The same bytes as items built from the in-memory rows of the fixtures.
    let cases = cases();
    for (index, item) in ["drive", "charge", "position", "charge_sample"]
        .into_iter()
        .enumerate()
    {
        let fixture = cases
            .iter()
            .find(|case| case.item == item && case.fixture == "distinct")
            .expect("distinct case");
        // The store-read item belongs to the registered vehicle.
        let rebind = |mut value: Value| {
            if let Some(slot) = value.get_mut("vehicle_id") {
                *slot = Value::String(vehicle.to_string());
            }
            value
        };
        let expected = rebind(serde_json::from_slice(&fixture.bytes).expect("fixture JSON"));
        let stored = rebind(serde_json::from_slice(&before[index]).expect("stored JSON"));
        assert_eq!(stored, expected, "{item}: store read equals the fixture");
    }
}
