// SPDX-License-Identifier: AGPL-3.0-only

//! C6 Part C spec tests of the current-summary serialisers (C6 ST-S30 and
//! C6 ST-S43, with the C6 ST-S31 JSON half and the C6 ST-S30 car and model keys). Test ids carry their unit prefix (C6 R65). Synthetic
//! data only.
//!
//! The renames of C6 R47 to C6 R49 are restated here from the specification, not
//! read from the production serialiser. A probe sets one field of an otherwise
//! empty struct and asserts which keys change and to what: it kills an alias
//! read from another field, an omitted alias and an omitted new key for every
//! field, whatever its type or value.

use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    ActivityState, CurrentCarSettingsView, CurrentCarView, CurrentVehicleSummary,
    b3_c6_s1::{
        T, T0, V3, discovery, import_position, l_on, lifecycle_record, open_drive, p0, session,
        stream, vd, vehicle, with,
    },
    build_current_vehicle_summary,
};
use crate::{
    db::{HubStore, ObservationInput, ObservationRecord, SourceDescriptor, VehicleDescriptor},
    hub_pack::ProjectionCar,
};

/// One field of a served struct: the keys it writes and the way to set it.
struct Probe<T> {
    field: &'static str,
    keys: &'static [&'static str],
    set: fn(&mut T) -> Value,
}

const SUMMARY_PROBES: [Probe<CurrentVehicleSummary>; 88] = [
    Probe {
        field: "vehicle_id",
        keys: &["vehicle_id"],
        set: |s| {
            s.vehicle_id = Uuid::from_u128(3);
            json!(Uuid::from_u128(3).to_string())
        },
    },
    Probe {
        field: "observed_at_ms",
        keys: &["observed_at_ms"],
        set: |s| {
            s.observed_at_ms = Some(1001);
            json!(1001)
        },
    },
    Probe {
        field: "activity_state",
        keys: &["activity_state", "state"],
        set: |s| {
            s.activity_state = Some(ActivityState::Charging);
            json!("charging")
        },
    },
    Probe {
        field: "state_since_ms",
        keys: &["state_since_ms", "since"],
        set: |s| {
            s.state_since_ms = Some(1003);
            json!(1003)
        },
    },
    Probe {
        field: "processing_healthy",
        keys: &["processing_healthy", "healthy"],
        set: |s| {
            s.processing_healthy = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "geofence_name",
        keys: &["geofence_name", "geofence"],
        set: |s| {
            s.geofence_name = Some("value of geofence_name".to_owned());
            json!("value of geofence_name")
        },
    },
    Probe {
        field: "display_name",
        keys: &["display_name"],
        set: |s| {
            s.display_name = Some("value of display_name".to_owned());
            json!("value of display_name")
        },
    },
    Probe {
        field: "model_code",
        keys: &["model_code", "model"],
        set: |s| {
            s.model_code = Some("value of model_code".to_owned());
            json!("value of model_code")
        },
    },
    Probe {
        field: "model_name",
        keys: &["model_name"],
        set: |s| {
            s.model_name = Some("Model 3");
            json!("Model 3")
        },
    },
    Probe {
        field: "model_year",
        keys: &["model_year"],
        set: |s| {
            s.model_year = Some(2019);
            json!(2019)
        },
    },
    Probe {
        field: "trim_badging",
        keys: &["trim_badging"],
        set: |s| {
            s.trim_badging = Some("value of trim_badging".to_owned());
            json!("value of trim_badging")
        },
    },
    Probe {
        field: "exterior_color",
        keys: &["exterior_color"],
        set: |s| {
            s.exterior_color = Some("value of exterior_color".to_owned());
            json!("value of exterior_color")
        },
    },
    Probe {
        field: "wheel_type",
        keys: &["wheel_type"],
        set: |s| {
            s.wheel_type = Some("value of wheel_type".to_owned());
            json!("value of wheel_type")
        },
    },
    Probe {
        field: "spoiler_type",
        keys: &["spoiler_type"],
        set: |s| {
            s.spoiler_type = Some("value of spoiler_type".to_owned());
            json!("value of spoiler_type")
        },
    },
    Probe {
        field: "car",
        keys: &["car"],
        set: |s| {
            s.car = Some(distinct_car());
            serde_json::to_value(distinct_car()).expect("car JSON")
        },
    },
    Probe {
        field: "latitude",
        keys: &["latitude"],
        set: |s| {
            s.latitude = Some(15.5);
            json!(15.5)
        },
    },
    Probe {
        field: "longitude",
        keys: &["longitude"],
        set: |s| {
            s.longitude = Some(16.5);
            json!(16.5)
        },
    },
    Probe {
        field: "heading",
        keys: &["heading"],
        set: |s| {
            s.heading = Some(17.5);
            json!(17.5)
        },
    },
    Probe {
        field: "elevation",
        keys: &["elevation"],
        set: |s| {
            s.elevation = Some(18.5);
            json!(18.5)
        },
    },
    Probe {
        field: "speed_kmh",
        keys: &["speed_kmh", "speed"],
        set: |s| {
            s.speed_kmh = Some(1019);
            json!(1019)
        },
    },
    Probe {
        field: "power",
        keys: &["power"],
        set: |s| {
            s.power = Some(20.5);
            json!(20.5)
        },
    },
    Probe {
        field: "shift_state",
        keys: &["shift_state"],
        set: |s| {
            s.shift_state = Some("value of shift_state".to_owned());
            json!("value of shift_state")
        },
    },
    Probe {
        field: "active_route_destination",
        keys: &["active_route_destination"],
        set: |s| {
            s.active_route_destination = Some("value of active_route_destination".to_owned());
            json!("value of active_route_destination")
        },
    },
    Probe {
        field: "active_route_latitude",
        keys: &["active_route_latitude"],
        set: |s| {
            s.active_route_latitude = Some(23.5);
            json!(23.5)
        },
    },
    Probe {
        field: "active_route_longitude",
        keys: &["active_route_longitude"],
        set: |s| {
            s.active_route_longitude = Some(24.5);
            json!(24.5)
        },
    },
    Probe {
        field: "active_route_km_to_arrival",
        keys: &["active_route_km_to_arrival"],
        set: |s| {
            s.active_route_km_to_arrival = Some(25.5);
            json!(25.5)
        },
    },
    Probe {
        field: "active_route_miles_to_arrival_raw",
        keys: &["active_route_miles_to_arrival"],
        set: |s| {
            s.active_route_miles_to_arrival_raw = Some(26.5);
            json!(26.5)
        },
    },
    Probe {
        field: "active_route_minutes_to_arrival",
        keys: &["active_route_minutes_to_arrival"],
        set: |s| {
            s.active_route_minutes_to_arrival = Some(27.5);
            json!(27.5)
        },
    },
    Probe {
        field: "active_route_traffic_minutes_delay",
        keys: &["active_route_traffic_minutes_delay"],
        set: |s| {
            s.active_route_traffic_minutes_delay = Some(28.5);
            json!(28.5)
        },
    },
    Probe {
        field: "active_route_energy_at_arrival",
        keys: &["active_route_energy_at_arrival"],
        set: |s| {
            s.active_route_energy_at_arrival = Some(29.5);
            json!(29.5)
        },
    },
    Probe {
        field: "battery_level",
        keys: &["battery_level"],
        set: |s| {
            s.battery_level = Some(1030);
            json!(1030)
        },
    },
    Probe {
        field: "usable_battery_level",
        keys: &["usable_battery_level"],
        set: |s| {
            s.usable_battery_level = Some(1031);
            json!(1031)
        },
    },
    Probe {
        field: "battery_range_km",
        keys: &["battery_range_km", "rated_battery_range_km"],
        set: |s| {
            s.battery_range_km = Some(32.5);
            json!(32.5)
        },
    },
    Probe {
        field: "ideal_battery_range_km",
        keys: &["ideal_battery_range_km"],
        set: |s| {
            s.ideal_battery_range_km = Some(33.5);
            json!(33.5)
        },
    },
    Probe {
        field: "est_battery_range_km",
        keys: &["est_battery_range_km"],
        set: |s| {
            s.est_battery_range_km = Some(34.5);
            json!(34.5)
        },
    },
    Probe {
        field: "charge_limit_soc",
        keys: &["charge_limit_soc"],
        set: |s| {
            s.charge_limit_soc = Some(1035);
            json!(1035)
        },
    },
    Probe {
        field: "charge_energy_added",
        keys: &["charge_energy_added"],
        set: |s| {
            s.charge_energy_added = Some(36.5);
            json!(36.5)
        },
    },
    Probe {
        field: "charge_current_request",
        keys: &["charge_current_request"],
        set: |s| {
            s.charge_current_request = Some(1037);
            json!(1037)
        },
    },
    Probe {
        field: "charge_current_request_max",
        keys: &["charge_current_request_max"],
        set: |s| {
            s.charge_current_request_max = Some(1038);
            json!(1038)
        },
    },
    Probe {
        field: "scheduled_charging_start_time",
        keys: &["scheduled_charging_start_time"],
        set: |s| {
            s.scheduled_charging_start_time = Some(1039);
            json!(1039)
        },
    },
    Probe {
        field: "charge_port_door_open",
        keys: &["charge_port_door_open"],
        set: |s| {
            s.charge_port_door_open = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "charging_state",
        keys: &["charging_state"],
        set: |s| {
            s.charging_state = Some("value of charging_state".to_owned());
            json!("value of charging_state")
        },
    },
    Probe {
        field: "charge_cable_connected",
        keys: &["charge_cable_connected", "plugged_in"],
        set: |s| {
            s.charge_cable_connected = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "charger_power",
        keys: &["charger_power"],
        set: |s| {
            s.charger_power = Some(43.5);
            json!(43.5)
        },
    },
    Probe {
        field: "charger_voltage",
        keys: &["charger_voltage"],
        set: |s| {
            s.charger_voltage = Some(44.5);
            json!(44.5)
        },
    },
    Probe {
        field: "charger_actual_current",
        keys: &["charger_actual_current"],
        set: |s| {
            s.charger_actual_current = Some(45.5);
            json!(45.5)
        },
    },
    Probe {
        field: "charger_phases",
        keys: &["charger_phases"],
        set: |s| {
            s.charger_phases = Some(1046);
            json!(1046)
        },
    },
    Probe {
        field: "time_to_full_charge",
        keys: &["time_to_full_charge"],
        set: |s| {
            s.time_to_full_charge = Some(47.5);
            json!(47.5)
        },
    },
    Probe {
        field: "inside_temp",
        keys: &["inside_temp"],
        set: |s| {
            s.inside_temp = Some(48.5);
            json!(48.5)
        },
    },
    Probe {
        field: "outside_temp",
        keys: &["outside_temp"],
        set: |s| {
            s.outside_temp = Some(49.5);
            json!(49.5)
        },
    },
    Probe {
        field: "is_climate_on",
        keys: &["is_climate_on"],
        set: |s| {
            s.is_climate_on = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "is_preconditioning",
        keys: &["is_preconditioning"],
        set: |s| {
            s.is_preconditioning = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "climate_keeper_mode",
        keys: &["climate_keeper_mode"],
        set: |s| {
            s.climate_keeper_mode = Some("value of climate_keeper_mode".to_owned());
            json!("value of climate_keeper_mode")
        },
    },
    Probe {
        field: "odometer_km",
        keys: &["odometer_km", "odometer"],
        set: |s| {
            s.odometer_km = Some(53.5);
            json!(53.5)
        },
    },
    Probe {
        field: "locked",
        keys: &["locked"],
        set: |s| {
            s.locked = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "tpms_soft_warning_fl",
        keys: &["tpms_soft_warning_fl"],
        set: |s| {
            s.tpms_soft_warning_fl = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "tpms_soft_warning_fr",
        keys: &["tpms_soft_warning_fr"],
        set: |s| {
            s.tpms_soft_warning_fr = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "tpms_soft_warning_rl",
        keys: &["tpms_soft_warning_rl"],
        set: |s| {
            s.tpms_soft_warning_rl = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "tpms_soft_warning_rr",
        keys: &["tpms_soft_warning_rr"],
        set: |s| {
            s.tpms_soft_warning_rr = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "sentry_mode",
        keys: &["sentry_mode"],
        set: |s| {
            s.sentry_mode = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "is_user_present",
        keys: &["is_user_present"],
        set: |s| {
            s.is_user_present = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "center_display_state",
        keys: &["center_display_state"],
        set: |s| {
            s.center_display_state = Some(1061);
            json!(1061)
        },
    },
    Probe {
        field: "sun_roof_percent_open",
        keys: &["sun_roof_percent_open"],
        set: |s| {
            s.sun_roof_percent_open = Some(1062);
            json!(1062)
        },
    },
    Probe {
        field: "sun_roof_state",
        keys: &["sun_roof_state"],
        set: |s| {
            s.sun_roof_state = Some("value of sun_roof_state".to_owned());
            json!("value of sun_roof_state")
        },
    },
    Probe {
        field: "tpms_pressure_fl",
        keys: &["tpms_pressure_fl"],
        set: |s| {
            s.tpms_pressure_fl = Some(64.5);
            json!(64.5)
        },
    },
    Probe {
        field: "tpms_pressure_fr",
        keys: &["tpms_pressure_fr"],
        set: |s| {
            s.tpms_pressure_fr = Some(65.5);
            json!(65.5)
        },
    },
    Probe {
        field: "tpms_pressure_rl",
        keys: &["tpms_pressure_rl"],
        set: |s| {
            s.tpms_pressure_rl = Some(66.5);
            json!(66.5)
        },
    },
    Probe {
        field: "tpms_pressure_rr",
        keys: &["tpms_pressure_rr"],
        set: |s| {
            s.tpms_pressure_rr = Some(67.5);
            json!(67.5)
        },
    },
    Probe {
        field: "sun_roof_installed",
        keys: &["sun_roof_installed"],
        set: |s| {
            s.sun_roof_installed = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "any_door_open",
        keys: &["any_door_open", "doors_open"],
        set: |s| {
            s.any_door_open = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "door_driver_front_open",
        keys: &["door_driver_front_open", "driver_front_door_open"],
        set: |s| {
            s.door_driver_front_open = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "door_driver_rear_open",
        keys: &["door_driver_rear_open", "driver_rear_door_open"],
        set: |s| {
            s.door_driver_rear_open = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "door_passenger_front_open",
        keys: &["door_passenger_front_open", "passenger_front_door_open"],
        set: |s| {
            s.door_passenger_front_open = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "door_passenger_rear_open",
        keys: &["door_passenger_rear_open", "passenger_rear_door_open"],
        set: |s| {
            s.door_passenger_rear_open = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "any_window_open",
        keys: &["any_window_open", "windows_open"],
        set: |s| {
            s.any_window_open = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "window_driver_front_open",
        keys: &["window_driver_front_open", "driver_front_window_open"],
        set: |s| {
            s.window_driver_front_open = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "window_driver_rear_open",
        keys: &["window_driver_rear_open", "driver_rear_window_open"],
        set: |s| {
            s.window_driver_rear_open = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "window_passenger_front_open",
        keys: &["window_passenger_front_open", "passenger_front_window_open"],
        set: |s| {
            s.window_passenger_front_open = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "window_passenger_rear_open",
        keys: &["window_passenger_rear_open", "passenger_rear_window_open"],
        set: |s| {
            s.window_passenger_rear_open = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "front_trunk_open",
        keys: &["front_trunk_open", "frunk_open"],
        set: |s| {
            s.front_trunk_open = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "rear_trunk_open",
        keys: &["rear_trunk_open", "trunk_open"],
        set: |s| {
            s.rear_trunk_open = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "software_version",
        keys: &["software_version", "version"],
        set: |s| {
            s.software_version = Some("value of software_version".to_owned());
            json!("value of software_version")
        },
    },
    Probe {
        field: "software_update_available",
        keys: &["software_update_available", "update_available"],
        set: |s| {
            s.software_update_available = Some(true);
            json!(true)
        },
    },
    Probe {
        field: "software_update_status",
        keys: &["software_update_status", "update_status"],
        set: |s| {
            s.software_update_status = Some("value of software_update_status".to_owned());
            json!("value of software_update_status")
        },
    },
    Probe {
        field: "software_update_version",
        keys: &["software_update_version", "update_version"],
        set: |s| {
            s.software_update_version = Some("value of software_update_version".to_owned());
            json!("value of software_update_version")
        },
    },
    Probe {
        field: "software_update_download_perc",
        keys: &["software_update_download_perc", "download_perc"],
        set: |s| {
            s.software_update_download_perc = Some(1085);
            json!(1085)
        },
    },
    Probe {
        field: "software_update_install_perc",
        keys: &["software_update_install_perc", "install_perc"],
        set: |s| {
            s.software_update_install_perc = Some(1086);
            json!(1086)
        },
    },
    Probe {
        field: "service_mode",
        keys: &["service_mode"],
        set: |s| {
            s.service_mode = Some(true);
            json!(true)
        },
    },
];

const CAR_PROBES: [Probe<CurrentCarView>; 16] = [
    Probe {
        field: "id",
        keys: &["id"],
        set: |s| {
            s.id = 5;
            json!(5)
        },
    },
    Probe {
        field: "name",
        keys: &["name"],
        set: |s| {
            s.name = "Car name".to_owned();
            json!("Car name")
        },
    },
    Probe {
        field: "vin",
        keys: &["vin"],
        set: |s| {
            s.vin = Some("value of vin".to_owned());
            json!("value of vin")
        },
    },
    Probe {
        field: "model_code",
        keys: &["model_code"],
        set: |s| {
            s.model_code = Some("value of model_code".to_owned());
            json!("value of model_code")
        },
    },
    Probe {
        field: "stored_model_text",
        keys: &["model"],
        set: |s| {
            s.stored_model_text = "stored model text".to_owned();
            json!("stored model text")
        },
    },
    Probe {
        field: "model_name",
        keys: &["model_name"],
        set: |s| {
            s.model_name = Some("Model 3");
            json!("Model 3")
        },
    },
    Probe {
        field: "model_year",
        keys: &["model_year"],
        set: |s| {
            s.model_year = Some(2019);
            json!(2019)
        },
    },
    Probe {
        field: "trim_badging",
        keys: &["trim_badging"],
        set: |s| {
            s.trim_badging = Some("value of trim_badging".to_owned());
            json!("value of trim_badging")
        },
    },
    Probe {
        field: "variant_name",
        keys: &["variant_name", "marketing_name"],
        set: |s| {
            s.variant_name = Some("value of variant_name".to_owned());
            json!("value of variant_name")
        },
    },
    Probe {
        field: "exterior_color",
        keys: &["exterior_color"],
        set: |s| {
            s.exterior_color = Some("value of exterior_color".to_owned());
            json!("value of exterior_color")
        },
    },
    Probe {
        field: "wheel_type",
        keys: &["wheel_type"],
        set: |s| {
            s.wheel_type = Some("value of wheel_type".to_owned());
            json!("value of wheel_type")
        },
    },
    Probe {
        field: "spoiler_type",
        keys: &["spoiler_type"],
        set: |s| {
            s.spoiler_type = Some("value of spoiler_type".to_owned());
            json!("value of spoiler_type")
        },
    },
    Probe {
        field: "firmware_version",
        keys: &["firmware_version"],
        set: |s| {
            s.firmware_version = Some("value of firmware_version".to_owned());
            json!("value of firmware_version")
        },
    },
    Probe {
        field: "api_vehicle_id",
        keys: &["api_vehicle_id", "source_eid"],
        set: |s| {
            s.api_vehicle_id = Some(1013);
            json!(1013)
        },
    },
    Probe {
        field: "stream_vehicle_id",
        keys: &["stream_vehicle_id", "source_vid"],
        set: |s| {
            s.stream_vehicle_id = Some(1014);
            json!(1014)
        },
    },
    Probe {
        field: "efficiency_wh_per_km",
        keys: &["efficiency_wh_per_km"],
        set: |s| {
            s.efficiency_wh_per_km = Some(15.5);
            json!(15.5)
        },
    },
];

const SETTINGS_PROBES: [Probe<CurrentCarSettingsView>; 8] = [
    Probe {
        field: "collection_enabled",
        keys: &["collection_enabled", "enabled"],
        set: |s| {
            s.collection_enabled = true;
            json!(true)
        },
    },
    Probe {
        field: "streaming_enabled",
        keys: &["streaming_enabled", "use_streaming_api"],
        set: |s| {
            s.streaming_enabled = true;
            json!(true)
        },
    },
    Probe {
        field: "sleep_window_min",
        keys: &["sleep_window_min", "suspend_min"],
        set: |s| {
            s.sleep_window_min = 9;
            json!(9)
        },
    },
    Probe {
        field: "sleep_after_idle_min",
        keys: &["sleep_after_idle_min", "suspend_after_idle_min"],
        set: |s| {
            s.sleep_after_idle_min = 10;
            json!(10)
        },
    },
    Probe {
        field: "sleep_window_min_resolved",
        keys: &["sleep_window_min_resolved", "suspend_min_resolved"],
        set: |s| {
            s.sleep_window_min_resolved = true;
            json!(true)
        },
    },
    Probe {
        field: "sleep_requires_locked",
        keys: &["sleep_requires_locked", "req_not_unlocked"],
        set: |s| {
            s.sleep_requires_locked = true;
            json!(true)
        },
    },
    Probe {
        field: "supercharging_free",
        keys: &["supercharging_free", "free_supercharging"],
        set: |s| {
            s.supercharging_free = true;
            json!(true)
        },
    },
    Probe {
        field: "battery_lfp",
        keys: &["battery_lfp", "lfp_battery"],
        set: |s| {
            s.battery_lfp = true;
            json!(true)
        },
    },
];

const CURRENT_1_2_0_KEYS: [&str; 85] = [
    "active_route_destination",
    "active_route_energy_at_arrival",
    "active_route_latitude",
    "active_route_longitude",
    "active_route_miles_to_arrival",
    "active_route_minutes_to_arrival",
    "active_route_traffic_minutes_delay",
    "battery_level",
    "car",
    "center_display_state",
    "charge_current_request",
    "charge_current_request_max",
    "charge_energy_added",
    "charge_limit_soc",
    "charge_port_door_open",
    "charger_actual_current",
    "charger_phases",
    "charger_power",
    "charger_voltage",
    "charging_state",
    "climate_keeper_mode",
    "display_name",
    "doors_open",
    "download_perc",
    "driver_front_door_open",
    "driver_front_window_open",
    "driver_rear_door_open",
    "driver_rear_window_open",
    "elevation",
    "est_battery_range_km",
    "exterior_color",
    "frunk_open",
    "geofence",
    "heading",
    "healthy",
    "ideal_battery_range_km",
    "inside_temp",
    "install_perc",
    "is_climate_on",
    "is_preconditioning",
    "is_user_present",
    "latitude",
    "locked",
    "longitude",
    "model",
    "observed_at_ms",
    "odometer",
    "outside_temp",
    "passenger_front_door_open",
    "passenger_front_window_open",
    "passenger_rear_door_open",
    "passenger_rear_window_open",
    "plugged_in",
    "power",
    "rated_battery_range_km",
    "scheduled_charging_start_time",
    "sentry_mode",
    "service_mode",
    "shift_state",
    "since",
    "speed",
    "spoiler_type",
    "state",
    "sun_roof_installed",
    "sun_roof_percent_open",
    "sun_roof_state",
    "time_to_full_charge",
    "tpms_pressure_fl",
    "tpms_pressure_fr",
    "tpms_pressure_rl",
    "tpms_pressure_rr",
    "tpms_soft_warning_fl",
    "tpms_soft_warning_fr",
    "tpms_soft_warning_rl",
    "tpms_soft_warning_rr",
    "trim_badging",
    "trunk_open",
    "update_available",
    "update_status",
    "update_version",
    "usable_battery_level",
    "vehicle_id",
    "version",
    "wheel_type",
    "windows_open",
];

const NEW_SUMMARY_KEYS: [&str; 30] = [
    "active_route_km_to_arrival",
    "activity_state",
    "any_door_open",
    "any_window_open",
    "battery_range_km",
    "charge_cable_connected",
    "door_driver_front_open",
    "door_driver_rear_open",
    "door_passenger_front_open",
    "door_passenger_rear_open",
    "front_trunk_open",
    "geofence_name",
    "model_code",
    "model_name",
    "model_year",
    "odometer_km",
    "processing_healthy",
    "rear_trunk_open",
    "software_update_available",
    "software_update_download_perc",
    "software_update_install_perc",
    "software_update_status",
    "software_update_version",
    "software_version",
    "speed_kmh",
    "state_since_ms",
    "window_driver_front_open",
    "window_driver_rear_open",
    "window_passenger_front_open",
    "window_passenger_rear_open",
];

/// C6 R47: the 28 renames, 1.2.0 key then 1.3.0 key.
const RENAMES: [(&str, &str); 28] = [
    ("state", "activity_state"),
    ("since", "state_since_ms"),
    ("healthy", "processing_healthy"),
    ("geofence", "geofence_name"),
    ("model", "model_code"),
    ("version", "software_version"),
    ("update_available", "software_update_available"),
    ("update_status", "software_update_status"),
    ("update_version", "software_update_version"),
    ("download_perc", "software_update_download_perc"),
    ("install_perc", "software_update_install_perc"),
    ("speed", "speed_kmh"),
    ("odometer", "odometer_km"),
    ("rated_battery_range_km", "battery_range_km"),
    (
        "active_route_miles_to_arrival",
        "active_route_km_to_arrival",
    ),
    ("plugged_in", "charge_cable_connected"),
    ("doors_open", "any_door_open"),
    ("windows_open", "any_window_open"),
    ("driver_front_door_open", "door_driver_front_open"),
    ("driver_rear_door_open", "door_driver_rear_open"),
    ("passenger_front_door_open", "door_passenger_front_open"),
    ("passenger_rear_door_open", "door_passenger_rear_open"),
    ("driver_front_window_open", "window_driver_front_open"),
    ("driver_rear_window_open", "window_driver_rear_open"),
    ("passenger_front_window_open", "window_passenger_front_open"),
    ("passenger_rear_window_open", "window_passenger_rear_open"),
    ("frunk_open", "front_trunk_open"),
    ("trunk_open", "rear_trunk_open"),
];

// ---------------------------------------------------------------- fixtures

/// The summary of a vehicle with no rows: every field null.
fn null_summary() -> CurrentVehicleSummary {
    build_current_vehicle_summary(vehicle(), &[], None, None, None, T0, T)
}

fn null_settings() -> CurrentCarSettingsView {
    CurrentCarSettingsView {
        collection_enabled: false,
        streaming_enabled: false,
        sleep_window_min: 0,
        sleep_after_idle_min: 0,
        sleep_window_min_resolved: false,
        sleep_requires_locked: false,
        supercharging_free: false,
        battery_lfp: false,
    }
}

fn null_car() -> CurrentCarView {
    CurrentCarView {
        id: 1,
        name: String::new(),
        vin: None,
        model_code: None,
        stored_model_text: String::new(),
        model_name: None,
        model_year: None,
        trim_badging: None,
        variant_name: None,
        exterior_color: None,
        wheel_type: None,
        spoiler_type: None,
        firmware_version: None,
        api_vehicle_id: None,
        stream_vehicle_id: None,
        efficiency_wh_per_km: None,
        settings: null_settings(),
    }
}

/// A car view with every field set to its own value: the probes applied in turn.
fn distinct_car() -> CurrentCarView {
    let mut car = null_car();
    for probe in &CAR_PROBES {
        (probe.set)(&mut car);
    }
    for probe in &SETTINGS_PROBES {
        (probe.set)(&mut car.settings);
    }
    car
}

/// A summary with every field set to its own value and a distinct car.
fn distinct_summary() -> CurrentVehicleSummary {
    let mut summary = null_summary();
    for probe in &SUMMARY_PROBES {
        (probe.set)(&mut summary);
    }
    summary
}

/// The stored car of C6 ST-S12 (id 1, name "Stored", model "3", VIN `V3`, trim
/// "74d", no firmware), with `extra` keys set over it.
fn stored_car(extra: Value) -> ProjectionCar {
    let mut stored = json!({
        "id": 1, "name": "Stored", "model": "3", "vin": V3, "trim_badging": "74d",
        "firmware_version": null, "efficiency_wh_per_km": null
    });
    for (key, value) in extra.as_object().expect("extra keys") {
        stored[key] = value.clone();
    }
    serde_json::from_value(stored).expect("stored car")
}

/// C6 ST-S1: a parked response under `L_on`.
fn s1(car: Option<ProjectionCar>, geofence: Option<&str>) -> CurrentVehicleSummary {
    let lifecycle = l_on();
    build_current_vehicle_summary(
        vehicle(),
        &[vd(1, T0, p0())],
        car,
        Some(&lifecycle),
        geofence.map(str::to_owned),
        T0,
        T,
    )
}

/// C6 ST-S29's awake, fresh setting: one row stating the conversion cases.
fn s29(car: Option<ProjectionCar>) -> CurrentVehicleSummary {
    let response = json!({
        "drive_state": {
            "speed": 60, "heading": 360, "active_route_miles_to_arrival": 12.5
        },
        "charge_state": {
            "charger_power": 7.4, "charger_voltage": 230.6,
            "scheduled_charging_start_time": 1_760_003_600_i64, "charge_current_request": "16"
        },
        "vehicle_state": {"center_display_state": 7},
        "vehicle_config": {"sun_roof_installed": 2}
    });
    build_current_vehicle_summary(vehicle(), &[vd(1, T0, response)], car, None, None, T0, T)
}

fn bytes_of<T: serde::Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).expect("served bytes")
}

/// The keys of a JSON object in the order the bytes carry them.
struct Keys(Vec<String>);

impl<'de> serde::Deserialize<'de> for Keys {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{IgnoredAny, MapAccess, Visitor};
        struct Collect;
        impl<'de> Visitor<'de> for Collect {
            type Value = Keys;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Keys, A::Error> {
                let mut keys = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    map.next_value::<IgnoredAny>()?;
                    keys.push(key);
                }
                Ok(Keys(keys))
            }
        }
        deserializer.deserialize_map(Collect)
    }
}

/// The keys of the serialised JSON object `bytes`, in the order they carry them.
fn key_order(bytes: &[u8]) -> Vec<String> {
    serde_json::from_slice::<Keys>(bytes)
        .expect("a JSON object")
        .0
}

/// The keys, in the order the bytes carry them, of the first object that
/// follows `"key":` inside `bytes` (the nested `car` and `settings` objects:
/// parsing them into a `Value` first would sort their keys).
fn nested_key_order(bytes: &[u8], key: &str) -> Vec<String> {
    let needle = format!("\"{key}\":");
    let start = bytes
        .windows(needle.len())
        .position(|window| window == needle.as_bytes())
        .expect("the nested key is written")
        + needle.len();
    serde_json::Deserializer::from_slice(&bytes[start..])
        .into_iter::<Keys>()
        .next()
        .expect("a value")
        .expect("an object")
        .0
}

fn ascending(keys: &[String]) -> bool {
    keys.windows(2)
        .all(|pair| pair[0].as_bytes() < pair[1].as_bytes())
}

fn sorted(keys: &[&str]) -> Vec<String> {
    let mut sorted: Vec<String> = keys.iter().map(|key| (*key).to_owned()).collect();
    sorted.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    sorted
}

fn without(object: &Value, keys: &[&str]) -> Value {
    let mut copy = object.as_object().expect("object").clone();
    for key in keys {
        copy.remove(*key);
    }
    Value::Object(copy)
}

fn key_set(object: &Value) -> Vec<String> {
    let mut keys: Vec<String> = object
        .as_object()
        .expect("object")
        .keys()
        .cloned()
        .collect();
    keys.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    keys
}

/// A value of the served summary against a JSON-Schema fragment of the
/// published 1.2.0 `current` definition: `anyOf`, `type`, `maxLength`, the
/// int64 bounds, the uuid pattern and nested `properties` and `required`.
fn schema_allows(schema: &Value, value: &Value) -> bool {
    if let Some(options) = schema.get("anyOf").and_then(Value::as_array) {
        return options.iter().any(|option| schema_allows(option, value));
    }
    match schema["type"].as_str() {
        Some("null") => value.is_null(),
        Some("boolean") => value.is_boolean(),
        Some("number") => value.is_number(),
        Some("integer") => value.is_i64() || value.is_u64(),
        Some("string") => value.as_str().is_some_and(|text| {
            text.chars().count() as u64 <= schema["maxLength"].as_u64().unwrap_or(u64::MAX)
                && (schema.get("pattern").is_none()
                    || Uuid::parse_str(text).is_ok_and(|id| id.to_string() == text))
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
                        .is_none_or(|property| schema_allows(property, member))
                })
        }
        _ => false,
    }
}

fn current_schema_1_2_0() -> Value {
    let path = format!(
        "{}/protocol/profiles/hub-http-v1/1.2.0/resources.schema.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(path).expect("committed hub-http-v1@1.2.0 schema");
    let schema: Value = serde_json::from_str(&text).expect("schema JSON");
    schema["$defs"]["current"].clone()
}

// ------------------------------------------------------ the probe machinery

/// Set one field of an empty struct and compare the served body with the empty
/// struct's: exactly the keys of the field change, each to the field's value.
fn assert_probe<T: serde::Serialize + Clone>(
    scope: &str,
    base: &T,
    probe: &Probe<T>,
    key_count: usize,
) {
    let empty = serde_json::to_value(base).expect("empty body");
    let mut changed = base.clone();
    let value = (probe.set)(&mut changed);
    let bytes = bytes_of(&changed);
    let body: Value = serde_json::from_slice(&bytes).expect("body");
    assert_eq!(
        key_order(&bytes).len(),
        key_count,
        "{scope}.{}: key count",
        probe.field
    );
    let mut differing: Vec<&str> = body
        .as_object()
        .expect("body object")
        .iter()
        .filter(|(key, member)| empty.get(key.as_str()) != Some(*member))
        .map(|(key, _)| key.as_str())
        .collect();
    differing.sort_unstable();
    let mut expected: Vec<&str> = probe.keys.to_vec();
    expected.sort_unstable();
    assert_eq!(
        differing, expected,
        "{scope}.{}: keys that change",
        probe.field
    );
    for key in probe.keys {
        assert_eq!(body[*key], value, "{scope}.{}: {key}", probe.field);
    }
}

/// The 115 served keys in ascending byte order.
fn expected_top() -> Vec<String> {
    let all: Vec<&str> = CURRENT_1_2_0_KEYS
        .iter()
        .chain(NEW_SUMMARY_KEYS.iter())
        .copied()
        .collect();
    sorted(&all)
}

/// The 20 served `car` keys in ascending byte order.
fn expected_car() -> Vec<String> {
    sorted(&[
        "api_vehicle_id",
        "efficiency_wh_per_km",
        "exterior_color",
        "firmware_version",
        "id",
        "marketing_name",
        "model",
        "model_code",
        "model_name",
        "model_year",
        "name",
        "settings",
        "source_eid",
        "source_vid",
        "spoiler_type",
        "stream_vehicle_id",
        "trim_badging",
        "variant_name",
        "vin",
        "wheel_type",
    ])
}

/// C6 R48: the three renames that have an alias (`model_code` has none: the
/// alias `car.model` is the stored text, C6 R50).
const CAR_RENAMES: [(&str, &str); 3] = [
    ("marketing_name", "variant_name"),
    ("source_eid", "api_vehicle_id"),
    ("source_vid", "stream_vehicle_id"),
];
/// C6 R48: the six keys 1.3.0 adds to `car`.
const NEW_CAR_KEYS: [&str; 6] = [
    "model_code",
    "model_name",
    "model_year",
    "variant_name",
    "api_vehicle_id",
    "stream_vehicle_id",
];
/// C6 R49: the eight settings renames, 1.2.0 key then 1.3.0 key.
const SETTINGS_RENAMES: [(&str, &str); 8] = [
    ("enabled", "collection_enabled"),
    ("use_streaming_api", "streaming_enabled"),
    ("suspend_min", "sleep_window_min"),
    ("suspend_after_idle_min", "sleep_after_idle_min"),
    ("suspend_min_resolved", "sleep_window_min_resolved"),
    ("req_not_unlocked", "sleep_requires_locked"),
    ("free_supercharging", "supercharging_free"),
    ("lfp_battery", "battery_lfp"),
];

/// Every assertion of C6 ST-S30 that holds for any summary body: 115 keys in
/// ascending byte order, every C6 R47 pair equal except the miles alias, the
/// 1.2.0 key set once the 30 new keys are removed, and the 1.2.0 schema.
fn check_summary(label: &str, summary: &CurrentVehicleSummary) -> Value {
    let bytes = bytes_of(summary);
    let order = key_order(&bytes);
    assert_eq!(
        order,
        expected_top(),
        "{label}: 115 keys in ascending byte order"
    );
    assert!(ascending(&order), "{label}: ascending byte order");
    let body: Value = serde_json::from_slice(&bytes).expect("body");
    for (old, new) in RENAMES {
        if old == "active_route_miles_to_arrival" {
            // C6 R50: the alias is Tesla's miles value, not the kilometres.
            assert_eq!(
                body[old],
                json!(summary.active_route_miles_to_arrival_raw),
                "{label}: {old}"
            );
            assert_eq!(
                body[new],
                json!(summary.active_route_km_to_arrival),
                "{label}: {new}"
            );
        } else {
            assert_eq!(body[old], body[new], "{label}: {old} equals {new}");
        }
    }
    let restored = without(&body, &NEW_SUMMARY_KEYS);
    assert_eq!(
        key_set(&restored),
        sorted(&CURRENT_1_2_0_KEYS),
        "{label}: removing the 30 new keys leaves the 85 keys of 1.2.0"
    );
    assert!(
        schema_allows(&current_schema_1_2_0(), &body),
        "{label}: the body validates against the 1.2.0 current definition"
    );
    match summary.car.as_ref() {
        Some(car) => check_car(label, car, &bytes, &body["car"]),
        None => assert!(body["car"].is_null(), "{label}: no car"),
    }
    body
}

fn check_car(label: &str, car: &CurrentCarView, bytes: &[u8], body: &Value) {
    let order = nested_key_order(bytes, "car");
    assert_eq!(
        order,
        expected_car(),
        "{label}: 20 car keys in ascending byte order"
    );
    assert!(ascending(&order));
    for (old, new) in CAR_RENAMES {
        assert_eq!(body[old], body[new], "{label}: car.{old} equals car.{new}");
    }
    // C6 R50: `car.model` is the stored text as stored, whatever the code is.
    assert_eq!(
        body["model"],
        json!(car.stored_model_text),
        "{label}: car.model"
    );
    assert_eq!(
        key_set(&without(body, &NEW_CAR_KEYS)),
        sorted(&[
            "efficiency_wh_per_km",
            "exterior_color",
            "firmware_version",
            "id",
            "marketing_name",
            "model",
            "name",
            "settings",
            "source_eid",
            "source_vid",
            "spoiler_type",
            "trim_badging",
            "vin",
            "wheel_type",
        ]),
        "{label}: removing the 6 new car keys leaves the 14 keys of 1.2.0"
    );
    let settings = &body["settings"];
    let order = nested_key_order(bytes, "settings");
    assert_eq!(order.len(), 16, "{label}: 16 settings keys");
    assert!(
        ascending(&order),
        "{label}: settings in ascending byte order"
    );
    for (old, new) in SETTINGS_RENAMES {
        assert_eq!(
            settings[old], settings[new],
            "{label}: settings.{old} equals {new}"
        );
    }
    let new_settings: Vec<&str> = SETTINGS_RENAMES.iter().map(|pair| pair.1).collect();
    let old_settings: Vec<&str> = SETTINGS_RENAMES.iter().map(|pair| pair.0).collect();
    assert_eq!(
        key_set(&without(settings, &new_settings)),
        sorted(&old_settings),
        "{label}: removing the 8 new settings keys leaves the 8 keys of 1.2.0"
    );
}

#[test]
fn c6_st_s30_aliases() {
    // The restated counts: 85 + 28 + 2 = 115 top-level keys, 14 + 4 + 2 = 20
    // `car` keys and 8 + 8 = 16 settings keys (C6 R47 to C6 R49); the 1.2.0 list
    // is the published schema's property set.
    assert_eq!(CURRENT_1_2_0_KEYS.len(), 85);
    assert_eq!(RENAMES.len(), 28);
    assert_eq!(NEW_SUMMARY_KEYS.len(), 30);
    assert_eq!(expected_top().len(), 115);
    assert_eq!(expected_car().len(), 20);
    assert_eq!(
        key_set(&current_schema_1_2_0()["properties"]),
        sorted(&CURRENT_1_2_0_KEYS)
    );

    // (1) Every field alone sets exactly its own keys: an alias read from
    // another field, an omitted alias and an omitted new key each change what
    // some probe sees.
    for probe in &SUMMARY_PROBES {
        assert_probe("summary", &null_summary(), probe, 115);
    }
    for probe in &CAR_PROBES {
        assert_probe("car", &null_car(), probe, 20);
    }
    for probe in &SETTINGS_PROBES {
        assert_probe("settings", &null_settings(), probe, 16);
    }

    // (2) The distinct fixture: every field and every car and settings field
    // holds its own value, so no key is null.
    let distinct = distinct_summary();
    let body = check_summary("distinct", &distinct);
    assert!(
        body.as_object()
            .expect("object")
            .values()
            .all(|value| !value.is_null()),
        "the distinct fixture states every top-level key"
    );
    assert_ne!(
        body["active_route_miles_to_arrival"],
        body["active_route_km_to_arrival"]
    );
    assert_ne!(body["car"]["model"], body["car"]["model_code"]);
    for (key, value) in body["car"].as_object().expect("car") {
        assert!(!value.is_null(), "distinct car.{key}");
    }
    for (key, value) in body["car"]["settings"].as_object().expect("settings") {
        assert!(!value.is_null(), "distinct settings.{key}");
    }

    // (3) The null fixture: a summary without rows, and one with a null car.
    let null = null_summary();
    let body = check_summary("null", &null);
    assert_eq!(body["vehicle_id"], json!(vehicle().to_string()));
    let mut null_with_car = null_summary();
    null_with_car.car = Some(null_car());
    let body = check_summary("null with car", &null_with_car);
    assert_eq!(
        body["car"]["model"],
        json!(""),
        "an empty stored text is served as is"
    );
    assert!(body["car"]["model_code"].is_null());

    // (4) C6 ST-S1 and C6 ST-S29, each without a car and with the car of C6
    // C6 ST-S12.
    for (label, car) in [
        ("without car", None),
        ("with car", Some(stored_car(json!({})))),
    ] {
        let body = check_summary(&format!("C6 ST-S1 {label}"), &s1(car.clone(), None));
        assert_eq!(body["battery_level"], 80);
        assert_eq!(body["state"], "online");
        assert_eq!(body["activity_state"], "online");
        assert_eq!(body["since"], json!(T0 - 3_600_000));
        assert_eq!(body["geofence"], Value::Null);
        assert_eq!(body["geofence_name"], Value::Null);
        let body = check_summary(&format!("C6 ST-S29 {label}"), &s29(car));
        // C6 R50 and C6 ST-S29: Tesla's miles value under the old name, the
        // converted value under the new one.
        assert_eq!(body["active_route_miles_to_arrival"], json!(12.5));
        assert_eq!(body["active_route_km_to_arrival"], json!(20.12));
        assert_eq!(body["speed"], 97);
        assert_eq!(body["speed_kmh"], 97);
        assert_eq!(body["sun_roof_installed"], true);
    }

    // C6 ST-S30 car and model keys: the car view now served.
    // The code, name and year of the stored car's VIN, on the car and on the
    // summary; `car.model` stays the stored text, even when it differs or is
    // empty.
    let body = check_summary("C6 ST-S12 car", &s1(Some(stored_car(json!({}))), None));
    for scope in [&body, &body["car"]] {
        assert_eq!(scope["model_code"], "3");
        assert_eq!(scope["model_name"], "Model 3");
        assert_eq!(scope["model_year"], 2019);
    }
    assert_eq!(body["model"], "3");
    assert_eq!(body["car"]["model"], "3");
    assert_eq!(body["car"]["trim_badging"], "74d");
    assert!(body["car"]["variant_name"].is_null());
    assert!(body["car"]["marketing_name"].is_null());
    for text in ["Model 3 Long Range", ""] {
        let body = check_summary(
            "stored text",
            &s1(Some(stored_car(json!({ "model": text }))), None),
        );
        assert_eq!(
            body["car"]["model"],
            json!(text),
            "car.model is the stored text"
        );
        assert_eq!(body["car"]["model_code"], "3", "the VIN decides the code");
        assert_eq!(body["model"], "3");
    }
    let body = check_summary(
        "named",
        &s1(
            Some(stored_car(json!({
                "marketing_name": "Variant", "source_eid": 7_i64, "source_vid": 8_i64
            }))),
            None,
        ),
    );
    assert_eq!(body["car"]["variant_name"], "Variant");
    assert_eq!(body["car"]["marketing_name"], "Variant");
    assert_eq!(body["car"]["api_vehicle_id"], 7);
    assert_eq!(body["car"]["source_eid"], 7);
    assert_eq!(body["car"]["stream_vehicle_id"], 8);
    assert_eq!(body["car"]["source_vid"], 8);

    // The JSON half of C6 ST-S31: with a non-null label the
    // serialiser writes `geofence_name` and `geofence`, both "Home".
    let body = check_summary("C6 ST-S31", &s1(None, Some("Home")));
    assert_eq!(body["geofence_name"], "Home");
    assert_eq!(body["geofence"], "Home");
    let body = check_summary(
        "C6 ST-S31 car",
        &s1(Some(stored_car(json!({}))), Some("Home")),
    );
    assert_eq!(body["geofence_name"], "Home");
    assert_eq!(body["geofence"], "Home");
}

// ----------------------------------------------------------------- C6 ST-S43

fn permutations<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut all = Vec::new();
    for index in 0..items.len() {
        let mut rest = items.to_vec();
        let head = rest.remove(index);
        for mut tail in permutations(&rest) {
            tail.insert(0, head.clone());
            all.push(tail);
        }
    }
    all
}

fn owner_input(source_id: Uuid, vehicle_id: Uuid, row: &ObservationRecord) -> ObservationInput {
    ObservationInput {
        source_id,
        vehicle_id,
        observed_at_ms: row.observed_at_ms,
        payload: row.payload.clone(),
    }
}

/// A store with one registered vehicle (through `HubStore`).
fn store_with_vehicle() -> (tempfile::TempDir, HubStore, Uuid, Uuid) {
    let temp = crate::private_tempdir().expect("temporary store directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let source = store
        .register_source(&SourceDescriptor::new("owner_api", "c6-c1-test"), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "1").with_tesla_identity(Some(1), None),
            1_000,
        )
        .expect("vehicle");
    (temp, store, source.source_id, vehicle.vehicle_id)
}

/// The served bytes of the summary built from what `store` holds.
fn stored_bytes(store: &HubStore, vehicle_id: Uuid, now_ms: i64) -> Vec<u8> {
    let rows = store
        .current_observations_for_vehicle(vehicle_id)
        .expect("current rows");
    let lifecycle = store.load_lifecycle_state(vehicle_id).expect("lifecycle");
    let car = store
        .materialised_car_for_vehicle(vehicle_id)
        .expect("stored car");
    assert!(car.is_some(), "the observation stored a car");
    let summary =
        build_current_vehicle_summary(vehicle_id, &rows, car, lifecycle.as_ref(), None, now_ms, T);
    assert!(summary.car.is_some());
    bytes_of(&summary)
}

#[test]
fn c6_st_s43_stable_bytes() {
    // The exact bytes of the empty summary: the 115 keys in ascending byte
    // order, no whitespace, null written as null.
    let expected = format!(
        "{{{}}}",
        expected_top()
            .iter()
            .map(|key| {
                if key == "vehicle_id" {
                    format!("\"{key}\":\"{}\"", vehicle())
                } else {
                    format!("\"{key}\":null")
                }
            })
            .collect::<Vec<_>>()
            .join(",")
    );
    assert_eq!(bytes_of(&null_summary()), expected.into_bytes());

    // The 24 orders of C6 ST-S26 (C6 ST-S3's rows plus a discovery row and an
    // import-source row) serialise to identical bytes.
    let owner = vd(
        1,
        T0,
        with(
            with(
                with(
                    with(p0(), "drive_state", "shift_state", json!("D")),
                    "drive_state",
                    "speed",
                    json!(30),
                ),
                "drive_state",
                "latitude",
                json!(10.15),
            ),
            "drive_state",
            "longitude",
            json!(20.75),
        ),
    );
    let frame = stream(2, T0 + 60_000, |update| update.soc = Some(78.0));
    let rows = [
        owner,
        frame,
        discovery(3, T0 + 10, "online"),
        import_position(4, T0 + 5, json!({"battery_level": 70})),
    ];
    let mut state = session("driving", Some(("online", T0 - 60_000)));
    state.open_drive = Some(open_drive(T0));
    let lifecycle = lifecycle_record(&state);
    let serialise = |order: &[ObservationRecord]| {
        bytes_of(&build_current_vehicle_summary(
            vehicle(),
            order,
            Some(stored_car(json!({}))),
            Some(&lifecycle),
            None,
            T0 + 60_000,
            T,
        ))
    };
    let first = serialise(&rows);
    let body: Value = serde_json::from_slice(&first).expect("body");
    assert_eq!(body["battery_level"], 78);
    assert_eq!(body["speed"], 48);
    assert_eq!(body["car"]["model_code"], "3");
    let orders = permutations(&rows);
    assert_eq!(orders.len(), 24);
    for order in &orders {
        assert_eq!(serialise(order), first, "every order gives the same bytes");
    }
    // Serialising the same struct twice gives the same bytes.
    let summary = distinct_summary();
    assert_eq!(bytes_of(&summary), bytes_of(&summary.clone()));

    // C6 ST-S34: a rebuild after a store restart serialises to the bytes before
    // it, with the stored car in place.
    let (temp, store, source_id, vehicle_id) = store_with_vehicle();
    let owner = vd(1, T0, p0());
    let frame = stream(2, T0 + 60_000, |update| {
        update.speed = Some(30.0);
        update.shift_state = Some("D".to_owned());
        update.soc = Some(79.0);
        update.est_lat = Some(10.2);
        update.est_lng = Some(20.8);
        update.power = Some(15);
        update.odometer = Some(1000.5);
    });
    store
        .accept_owner_observation_and_lifecycle(
            &owner_input(source_id, vehicle_id, &owner),
            T0 + 1,
            1,
        )
        .expect("owner observation");
    store
        .accept_stream_observation_and_lifecycle(
            &owner_input(source_id, vehicle_id, &frame),
            T0 + 60_001,
            1,
        )
        .expect("stream observation");
    let before = stored_bytes(&store, vehicle_id, T0 + 60_000);
    let body: Value = serde_json::from_slice(&before).expect("body");
    assert_eq!(body["battery_level"], 79);
    assert_eq!(body["state"], "driving");
    assert_eq!(body["car"]["model_code"], "3");
    assert_eq!(
        before,
        stored_bytes(&store, vehicle_id, T0 + 60_000),
        "repeat bytes"
    );
    drop(store);
    let reopened = HubStore::initialize(temp.path()).expect("reopened store");
    assert_eq!(
        stored_bytes(&reopened, vehicle_id, T0 + 60_000),
        before,
        "bytes after the restart"
    );
}
