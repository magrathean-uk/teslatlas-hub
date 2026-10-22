// SPDX-License-Identifier: AGPL-3.0-only

//! The hand-written serialisers of the four history item types (C6 R58 (d)).
//! Each value is held once in its struct under its new name; the serialiser
//! writes that key and, for each of the 33 renames of C6 R58 (a), the 1.2.0 key
//! as an alias with the same JSON value (no history alias differs from its
//! replacement). Keys are written in ascending byte order. A key that 1.2.0
//! requires is always written, null as null; the position's
//! `elevation_provenance` is an object or null, as in 1.2.0; the drive and
//! charge items carry no address-provenance member (the Hub keeps no truthful
//! record yet). The stored `ProjectionDrive`, `ProjectionCharge`,
//! `ProjectionPosition` and `ProjectionChargeSample` types, the packs and the
//! sync payloads keep their serde names; only the served item changes.

use serde::{Serialize, Serializer, ser::SerializeMap};

use super::{PublicCharge, PublicChargeSample, PublicDrive, PublicPosition};

/// Keys served for one drive item.
pub(super) const DRIVE_KEY_COUNT: usize = 43;
/// Keys served for one charge item.
pub(super) const CHARGE_KEY_COUNT: usize = 39;
/// Keys served for one position item.
pub(super) const POSITION_KEY_COUNT: usize = 31;
/// Keys served for one charge sample item.
pub(super) const CHARGE_SAMPLE_KEY_COUNT: usize = 21;

impl Serialize for PublicDrive {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(DRIVE_KEY_COUNT))?;
        map.serialize_entry("ascent", &self.ascent_m)?;
        map.serialize_entry("ascent_m", &self.ascent_m)?;
        map.serialize_entry("descent", &self.descent_m)?;
        map.serialize_entry("descent_m", &self.descent_m)?;
        map.serialize_entry("distance_km", &self.distance_km)?;
        map.serialize_entry("duration_min", &self.duration_min)?;
        map.serialize_entry("efficiency", &self.efficiency)?;
        map.serialize_entry("end_address", &self.end_address)?;
        map.serialize_entry("end_battery_range_km", &self.end_battery_range_km)?;
        map.serialize_entry("end_date_ms", &self.ended_at_ms)?;
        map.serialize_entry("end_geofence", &self.end_geofence_name)?;
        map.serialize_entry("end_geofence_name", &self.end_geofence_name)?;
        map.serialize_entry(
            "end_ideal_battery_range_km",
            &self.end_ideal_battery_range_km,
        )?;
        map.serialize_entry("end_ideal_range_km", &self.end_ideal_battery_range_km)?;
        map.serialize_entry("end_latitude", &self.end_latitude)?;
        map.serialize_entry("end_longitude", &self.end_longitude)?;
        map.serialize_entry("end_rated_range_km", &self.end_battery_range_km)?;
        map.serialize_entry("end_soc", &self.end_soc)?;
        map.serialize_entry("ended_at_ms", &self.ended_at_ms)?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("inside_temp_avg", &self.inside_temp_avg_c)?;
        map.serialize_entry("inside_temp_avg_c", &self.inside_temp_avg_c)?;
        map.serialize_entry("outside_temp_avg", &self.outside_temp_avg_c)?;
        map.serialize_entry("outside_temp_avg_c", &self.outside_temp_avg_c)?;
        map.serialize_entry("power_max", &self.power_max_kw)?;
        map.serialize_entry("power_max_kw", &self.power_max_kw)?;
        map.serialize_entry("power_min", &self.power_min_kw)?;
        map.serialize_entry("power_min_kw", &self.power_min_kw)?;
        map.serialize_entry("speed_max", &self.speed_max_kmh)?;
        map.serialize_entry("speed_max_kmh", &self.speed_max_kmh)?;
        map.serialize_entry("start_address", &self.start_address)?;
        map.serialize_entry("start_battery_range_km", &self.start_battery_range_km)?;
        map.serialize_entry("start_date_ms", &self.started_at_ms)?;
        map.serialize_entry("start_geofence", &self.start_geofence_name)?;
        map.serialize_entry("start_geofence_name", &self.start_geofence_name)?;
        map.serialize_entry(
            "start_ideal_battery_range_km",
            &self.start_ideal_battery_range_km,
        )?;
        map.serialize_entry("start_ideal_range_km", &self.start_ideal_battery_range_km)?;
        map.serialize_entry("start_latitude", &self.start_latitude)?;
        map.serialize_entry("start_longitude", &self.start_longitude)?;
        map.serialize_entry("start_rated_range_km", &self.start_battery_range_km)?;
        map.serialize_entry("start_soc", &self.start_soc)?;
        map.serialize_entry("started_at_ms", &self.started_at_ms)?;
        map.serialize_entry("vehicle_id", &self.vehicle_id)?;
        map.end()
    }
}

impl Serialize for PublicCharge {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(CHARGE_KEY_COUNT))?;
        map.serialize_entry("address", &self.address)?;
        map.serialize_entry("billing_type", &self.tariff_type)?;
        map.serialize_entry("charge_energy_added", &self.charge_energy_added_kwh)?;
        map.serialize_entry("charge_energy_added_kwh", &self.charge_energy_added_kwh)?;
        map.serialize_entry("charge_energy_used_kwh", &self.charge_energy_used_kwh)?;
        map.serialize_entry("charge_rate_km_per_hour", &self.charge_rate_km_per_hour)?;
        map.serialize_entry("cost", &self.cost)?;
        map.serialize_entry("cost_per_unit", &self.tariff_rate)?;
        map.serialize_entry("duration_min", &self.duration_min)?;
        map.serialize_entry("end_battery_level", &self.end_battery_level)?;
        map.serialize_entry("end_battery_range_km", &self.end_battery_range_km)?;
        map.serialize_entry("end_date_ms", &self.ended_at_ms)?;
        map.serialize_entry(
            "end_ideal_battery_range_km",
            &self.end_ideal_battery_range_km,
        )?;
        map.serialize_entry("end_ideal_range_km", &self.end_ideal_battery_range_km)?;
        map.serialize_entry("end_rated_range_km", &self.end_battery_range_km)?;
        map.serialize_entry("ended_at_ms", &self.ended_at_ms)?;
        map.serialize_entry("fast_charger_type", &self.fast_charger_type)?;
        map.serialize_entry("geofence", &self.geofence_name)?;
        map.serialize_entry("geofence_name", &self.geofence_name)?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("is_dc", &self.is_dc)?;
        map.serialize_entry("location_name", &self.location_name)?;
        map.serialize_entry("max_charger_power_kw", &self.max_charger_power_kw)?;
        map.serialize_entry("outside_temp_avg", &self.outside_temp_avg_c)?;
        map.serialize_entry("outside_temp_avg_c", &self.outside_temp_avg_c)?;
        map.serialize_entry("session_fee", &self.tariff_session_fee)?;
        map.serialize_entry("start_battery_level", &self.start_battery_level)?;
        map.serialize_entry("start_battery_range_km", &self.start_battery_range_km)?;
        map.serialize_entry("start_date_ms", &self.started_at_ms)?;
        map.serialize_entry(
            "start_ideal_battery_range_km",
            &self.start_ideal_battery_range_km,
        )?;
        map.serialize_entry("start_ideal_range_km", &self.start_ideal_battery_range_km)?;
        map.serialize_entry("start_latitude", &self.start_latitude)?;
        map.serialize_entry("start_longitude", &self.start_longitude)?;
        map.serialize_entry("start_rated_range_km", &self.start_battery_range_km)?;
        map.serialize_entry("started_at_ms", &self.started_at_ms)?;
        map.serialize_entry("tariff_rate", &self.tariff_rate)?;
        map.serialize_entry("tariff_session_fee", &self.tariff_session_fee)?;
        map.serialize_entry("tariff_type", &self.tariff_type)?;
        map.serialize_entry("vehicle_id", &self.vehicle_id)?;
        map.end()
    }
}

impl Serialize for PublicPosition {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(POSITION_KEY_COUNT))?;
        map.serialize_entry("battery_heater", &self.battery_heater)?;
        map.serialize_entry("battery_heater_on", &self.battery_heater_on)?;
        map.serialize_entry("battery_level", &self.battery_level)?;
        map.serialize_entry("battery_range_km", &self.battery_range_km)?;
        map.serialize_entry("date_ms", &self.timestamp_ms)?;
        map.serialize_entry("drive_id", &self.drive_id)?;
        map.serialize_entry("driver_temp_setting", &self.driver_temp_setting)?;
        map.serialize_entry("elevation", &self.elevation)?;
        map.serialize_entry("elevation_provenance", &self.elevation_provenance)?;
        map.serialize_entry("est_battery_range_km", &self.est_battery_range_km)?;
        map.serialize_entry("fan_status", &self.fan_status)?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("ideal_battery_range_km", &self.ideal_battery_range_km)?;
        map.serialize_entry("inside_temp", &self.inside_temp)?;
        map.serialize_entry("is_climate_on", &self.is_climate_on)?;
        map.serialize_entry("latitude", &self.latitude)?;
        map.serialize_entry("longitude", &self.longitude)?;
        map.serialize_entry("odometer", &self.odometer_km)?;
        map.serialize_entry("odometer_km", &self.odometer_km)?;
        map.serialize_entry("outside_temp", &self.outside_temp)?;
        map.serialize_entry("passenger_temp_setting", &self.passenger_temp_setting)?;
        map.serialize_entry("power", &self.power)?;
        map.serialize_entry("rated_battery_range_km", &self.battery_range_km)?;
        map.serialize_entry("speed", &self.speed_kmh)?;
        map.serialize_entry("speed_kmh", &self.speed_kmh)?;
        map.serialize_entry("timestamp_ms", &self.timestamp_ms)?;
        map.serialize_entry("tpms_pressure_fl", &self.tpms_pressure_fl)?;
        map.serialize_entry("tpms_pressure_fr", &self.tpms_pressure_fr)?;
        map.serialize_entry("tpms_pressure_rl", &self.tpms_pressure_rl)?;
        map.serialize_entry("tpms_pressure_rr", &self.tpms_pressure_rr)?;
        map.serialize_entry("usable_battery_level", &self.usable_battery_level)?;
        map.end()
    }
}

impl Serialize for PublicChargeSample {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(CHARGE_SAMPLE_KEY_COUNT))?;
        map.serialize_entry("battery_heater_on", &self.battery_heater_on)?;
        map.serialize_entry("battery_level", &self.battery_level)?;
        map.serialize_entry("battery_range_km", &self.battery_range_km)?;
        map.serialize_entry("charge_cable", &self.charge_cable)?;
        map.serialize_entry("charge_energy_added_kwh", &self.charge_energy_added_kwh)?;
        map.serialize_entry("charge_id", &self.charge_id)?;
        map.serialize_entry("charger_actual_current", &self.charger_actual_current)?;
        map.serialize_entry("charger_phases", &self.charger_phases)?;
        map.serialize_entry("charger_pilot_current", &self.charger_pilot_current)?;
        map.serialize_entry("charger_power_kw", &self.charger_power_kw)?;
        map.serialize_entry("charger_voltage", &self.charger_voltage)?;
        map.serialize_entry("fast_charger_brand", &self.fast_charger_brand)?;
        map.serialize_entry("fast_charger_present", &self.fast_charger_present)?;
        map.serialize_entry("fast_charger_type", &self.fast_charger_type)?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("ideal_battery_range_km", &self.ideal_battery_range_km)?;
        map.serialize_entry("ideal_range_km", &self.ideal_battery_range_km)?;
        map.serialize_entry("outside_temp_c", &self.outside_temp_c)?;
        map.serialize_entry("rated_range_km", &self.battery_range_km)?;
        map.serialize_entry("timestamp_ms", &self.timestamp_ms)?;
        map.serialize_entry("usable_battery_level", &self.usable_battery_level)?;
        map.end()
    }
}
