// SPDX-License-Identifier: AGPL-3.0-only

//! The hand-written serialisers of the current summary, its `car` object and
//! the car settings (C6 Part C, C6 R50). Each value is held once in its struct
//! under its 1.3.0 name; the serialiser writes that key and, for each rename of
//! C6 R47 to C6 R49, the 1.2.0 key as an alias with the same JSON value. Keys are
//! written in ascending byte order, so the same struct always serialises to the
//! same bytes (C6 R21, C6 R50). The two exceptions of C6 R50 are the only aliases
//! that do not read their replacement's field: `car.model` is the stored model
//! text, and `active_route_miles_to_arrival` is Tesla's miles value.

use serde::{Serialize, Serializer, ser::SerializeMap};

use super::{CurrentCarSettingsView, CurrentCarView, CurrentVehicleSummary};

/// Top-level keys of the 1.3.0 summary (C6 R47: 85 + 28 + 2).
pub(super) const SUMMARY_KEY_COUNT: usize = 115;
/// Keys of the `car` object (C6 R48: 14 + 4 + 2).
pub(super) const CAR_KEY_COUNT: usize = 20;
/// Keys of `car.settings` (C6 R49: 8 + 8).
pub(super) const SETTINGS_KEY_COUNT: usize = 16;

impl Serialize for CurrentVehicleSummary {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(SUMMARY_KEY_COUNT))?;
        map.serialize_entry("active_route_destination", &self.active_route_destination)?;
        map.serialize_entry(
            "active_route_energy_at_arrival",
            &self.active_route_energy_at_arrival,
        )?;
        map.serialize_entry(
            "active_route_km_to_arrival",
            &self.active_route_km_to_arrival,
        )?;
        map.serialize_entry("active_route_latitude", &self.active_route_latitude)?;
        map.serialize_entry("active_route_longitude", &self.active_route_longitude)?;
        map.serialize_entry(
            "active_route_miles_to_arrival",
            &self.active_route_miles_to_arrival_raw,
        )?;
        map.serialize_entry(
            "active_route_minutes_to_arrival",
            &self.active_route_minutes_to_arrival,
        )?;
        map.serialize_entry(
            "active_route_traffic_minutes_delay",
            &self.active_route_traffic_minutes_delay,
        )?;
        map.serialize_entry("activity_state", &self.activity_state)?;
        map.serialize_entry("any_door_open", &self.any_door_open)?;
        map.serialize_entry("any_window_open", &self.any_window_open)?;
        map.serialize_entry("battery_level", &self.battery_level)?;
        map.serialize_entry("battery_range_km", &self.battery_range_km)?;
        map.serialize_entry("car", &self.car)?;
        map.serialize_entry("center_display_state", &self.center_display_state)?;
        map.serialize_entry("charge_cable_connected", &self.charge_cable_connected)?;
        map.serialize_entry("charge_current_request", &self.charge_current_request)?;
        map.serialize_entry(
            "charge_current_request_max",
            &self.charge_current_request_max,
        )?;
        map.serialize_entry("charge_energy_added", &self.charge_energy_added)?;
        map.serialize_entry("charge_limit_soc", &self.charge_limit_soc)?;
        map.serialize_entry("charge_port_door_open", &self.charge_port_door_open)?;
        map.serialize_entry("charger_actual_current", &self.charger_actual_current)?;
        map.serialize_entry("charger_phases", &self.charger_phases)?;
        map.serialize_entry("charger_power", &self.charger_power)?;
        map.serialize_entry("charger_voltage", &self.charger_voltage)?;
        map.serialize_entry("charging_state", &self.charging_state)?;
        map.serialize_entry("climate_keeper_mode", &self.climate_keeper_mode)?;
        map.serialize_entry("display_name", &self.display_name)?;
        map.serialize_entry("door_driver_front_open", &self.door_driver_front_open)?;
        map.serialize_entry("door_driver_rear_open", &self.door_driver_rear_open)?;
        map.serialize_entry("door_passenger_front_open", &self.door_passenger_front_open)?;
        map.serialize_entry("door_passenger_rear_open", &self.door_passenger_rear_open)?;
        map.serialize_entry("doors_open", &self.any_door_open)?;
        map.serialize_entry("download_perc", &self.software_update_download_perc)?;
        map.serialize_entry("driver_front_door_open", &self.door_driver_front_open)?;
        map.serialize_entry("driver_front_window_open", &self.window_driver_front_open)?;
        map.serialize_entry("driver_rear_door_open", &self.door_driver_rear_open)?;
        map.serialize_entry("driver_rear_window_open", &self.window_driver_rear_open)?;
        map.serialize_entry("elevation", &self.elevation)?;
        map.serialize_entry("est_battery_range_km", &self.est_battery_range_km)?;
        map.serialize_entry("exterior_color", &self.exterior_color)?;
        map.serialize_entry("front_trunk_open", &self.front_trunk_open)?;
        map.serialize_entry("frunk_open", &self.front_trunk_open)?;
        map.serialize_entry("geofence", &self.geofence_name)?;
        map.serialize_entry("geofence_name", &self.geofence_name)?;
        map.serialize_entry("heading", &self.heading)?;
        map.serialize_entry("healthy", &self.processing_healthy)?;
        map.serialize_entry("ideal_battery_range_km", &self.ideal_battery_range_km)?;
        map.serialize_entry("inside_temp", &self.inside_temp)?;
        map.serialize_entry("install_perc", &self.software_update_install_perc)?;
        map.serialize_entry("is_climate_on", &self.is_climate_on)?;
        map.serialize_entry("is_preconditioning", &self.is_preconditioning)?;
        map.serialize_entry("is_user_present", &self.is_user_present)?;
        map.serialize_entry("latitude", &self.latitude)?;
        map.serialize_entry("locked", &self.locked)?;
        map.serialize_entry("longitude", &self.longitude)?;
        map.serialize_entry("model", &self.model_code)?;
        map.serialize_entry("model_code", &self.model_code)?;
        map.serialize_entry("model_name", &self.model_name)?;
        map.serialize_entry("model_year", &self.model_year)?;
        map.serialize_entry("observed_at_ms", &self.observed_at_ms)?;
        map.serialize_entry("odometer", &self.odometer_km)?;
        map.serialize_entry("odometer_km", &self.odometer_km)?;
        map.serialize_entry("outside_temp", &self.outside_temp)?;
        map.serialize_entry("passenger_front_door_open", &self.door_passenger_front_open)?;
        map.serialize_entry(
            "passenger_front_window_open",
            &self.window_passenger_front_open,
        )?;
        map.serialize_entry("passenger_rear_door_open", &self.door_passenger_rear_open)?;
        map.serialize_entry(
            "passenger_rear_window_open",
            &self.window_passenger_rear_open,
        )?;
        map.serialize_entry("plugged_in", &self.charge_cable_connected)?;
        map.serialize_entry("power", &self.power)?;
        map.serialize_entry("processing_healthy", &self.processing_healthy)?;
        map.serialize_entry("rated_battery_range_km", &self.battery_range_km)?;
        map.serialize_entry("rear_trunk_open", &self.rear_trunk_open)?;
        map.serialize_entry(
            "scheduled_charging_start_time",
            &self.scheduled_charging_start_time,
        )?;
        map.serialize_entry("sentry_mode", &self.sentry_mode)?;
        map.serialize_entry("service_mode", &self.service_mode)?;
        map.serialize_entry("shift_state", &self.shift_state)?;
        map.serialize_entry("since", &self.state_since_ms)?;
        map.serialize_entry("software_update_available", &self.software_update_available)?;
        map.serialize_entry(
            "software_update_download_perc",
            &self.software_update_download_perc,
        )?;
        map.serialize_entry(
            "software_update_install_perc",
            &self.software_update_install_perc,
        )?;
        map.serialize_entry("software_update_status", &self.software_update_status)?;
        map.serialize_entry("software_update_version", &self.software_update_version)?;
        map.serialize_entry("software_version", &self.software_version)?;
        map.serialize_entry("speed", &self.speed_kmh)?;
        map.serialize_entry("speed_kmh", &self.speed_kmh)?;
        map.serialize_entry("spoiler_type", &self.spoiler_type)?;
        map.serialize_entry("state", &self.activity_state)?;
        map.serialize_entry("state_since_ms", &self.state_since_ms)?;
        map.serialize_entry("sun_roof_installed", &self.sun_roof_installed)?;
        map.serialize_entry("sun_roof_percent_open", &self.sun_roof_percent_open)?;
        map.serialize_entry("sun_roof_state", &self.sun_roof_state)?;
        map.serialize_entry("time_to_full_charge", &self.time_to_full_charge)?;
        map.serialize_entry("tpms_pressure_fl", &self.tpms_pressure_fl)?;
        map.serialize_entry("tpms_pressure_fr", &self.tpms_pressure_fr)?;
        map.serialize_entry("tpms_pressure_rl", &self.tpms_pressure_rl)?;
        map.serialize_entry("tpms_pressure_rr", &self.tpms_pressure_rr)?;
        map.serialize_entry("tpms_soft_warning_fl", &self.tpms_soft_warning_fl)?;
        map.serialize_entry("tpms_soft_warning_fr", &self.tpms_soft_warning_fr)?;
        map.serialize_entry("tpms_soft_warning_rl", &self.tpms_soft_warning_rl)?;
        map.serialize_entry("tpms_soft_warning_rr", &self.tpms_soft_warning_rr)?;
        map.serialize_entry("trim_badging", &self.trim_badging)?;
        map.serialize_entry("trunk_open", &self.rear_trunk_open)?;
        map.serialize_entry("update_available", &self.software_update_available)?;
        map.serialize_entry("update_status", &self.software_update_status)?;
        map.serialize_entry("update_version", &self.software_update_version)?;
        map.serialize_entry("usable_battery_level", &self.usable_battery_level)?;
        map.serialize_entry("vehicle_id", &self.vehicle_id)?;
        map.serialize_entry("version", &self.software_version)?;
        map.serialize_entry("wheel_type", &self.wheel_type)?;
        map.serialize_entry("window_driver_front_open", &self.window_driver_front_open)?;
        map.serialize_entry("window_driver_rear_open", &self.window_driver_rear_open)?;
        map.serialize_entry(
            "window_passenger_front_open",
            &self.window_passenger_front_open,
        )?;
        map.serialize_entry(
            "window_passenger_rear_open",
            &self.window_passenger_rear_open,
        )?;
        map.serialize_entry("windows_open", &self.any_window_open)?;
        map.end()
    }
}

impl Serialize for CurrentCarView {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(CAR_KEY_COUNT))?;
        map.serialize_entry("api_vehicle_id", &self.api_vehicle_id)?;
        map.serialize_entry("efficiency_wh_per_km", &self.efficiency_wh_per_km)?;
        map.serialize_entry("exterior_color", &self.exterior_color)?;
        map.serialize_entry("firmware_version", &self.firmware_version)?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("marketing_name", &self.variant_name)?;
        map.serialize_entry("model", &self.stored_model_text)?;
        map.serialize_entry("model_code", &self.model_code)?;
        map.serialize_entry("model_name", &self.model_name)?;
        map.serialize_entry("model_year", &self.model_year)?;
        map.serialize_entry("name", &self.name)?;
        map.serialize_entry("settings", &self.settings)?;
        map.serialize_entry("source_eid", &self.api_vehicle_id)?;
        map.serialize_entry("source_vid", &self.stream_vehicle_id)?;
        map.serialize_entry("spoiler_type", &self.spoiler_type)?;
        map.serialize_entry("stream_vehicle_id", &self.stream_vehicle_id)?;
        map.serialize_entry("trim_badging", &self.trim_badging)?;
        map.serialize_entry("variant_name", &self.variant_name)?;
        map.serialize_entry("vin", &self.vin)?;
        map.serialize_entry("wheel_type", &self.wheel_type)?;
        map.end()
    }
}

impl Serialize for CurrentCarSettingsView {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(SETTINGS_KEY_COUNT))?;
        map.serialize_entry("battery_lfp", &self.battery_lfp)?;
        map.serialize_entry("collection_enabled", &self.collection_enabled)?;
        map.serialize_entry("enabled", &self.collection_enabled)?;
        map.serialize_entry("free_supercharging", &self.supercharging_free)?;
        map.serialize_entry("lfp_battery", &self.battery_lfp)?;
        map.serialize_entry("req_not_unlocked", &self.sleep_requires_locked)?;
        map.serialize_entry("sleep_after_idle_min", &self.sleep_after_idle_min)?;
        map.serialize_entry("sleep_requires_locked", &self.sleep_requires_locked)?;
        map.serialize_entry("sleep_window_min", &self.sleep_window_min)?;
        map.serialize_entry("sleep_window_min_resolved", &self.sleep_window_min_resolved)?;
        map.serialize_entry("streaming_enabled", &self.streaming_enabled)?;
        map.serialize_entry("supercharging_free", &self.supercharging_free)?;
        map.serialize_entry("suspend_after_idle_min", &self.sleep_after_idle_min)?;
        map.serialize_entry("suspend_min", &self.sleep_window_min)?;
        map.serialize_entry("suspend_min_resolved", &self.sleep_window_min_resolved)?;
        map.serialize_entry("use_streaming_api", &self.streaming_enabled)?;
        map.end()
    }
}
