use serde::Serialize;
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::{
    db::{LifecycleStateRecord, ObservationRecord},
    hub_pack::{ProjectionCar, normalize_tesla_model_code},
    lifecycle::OpenSessionState,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CurrentVehicleSummary {
    pub vehicle_id: Uuid,
    pub observed_at_ms: Option<i64>,
    pub car: Option<ProjectionCar>,
    pub display_name: Option<String>,
    pub state: Option<String>,
    pub since: Option<i64>,
    pub healthy: Option<bool>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub heading: Option<f64>,
    pub battery_level: Option<i64>,
    pub charging_state: Option<String>,
    pub usable_battery_level: Option<i64>,
    pub ideal_battery_range_km: Option<f64>,
    pub est_battery_range_km: Option<f64>,
    pub rated_battery_range_km: Option<f64>,
    pub charge_energy_added: Option<f64>,
    pub speed: Option<i64>,
    pub outside_temp: Option<f64>,
    pub inside_temp: Option<f64>,
    pub is_climate_on: Option<bool>,
    pub is_preconditioning: Option<bool>,
    pub locked: Option<bool>,
    pub sentry_mode: Option<bool>,
    pub plugged_in: Option<bool>,
    pub scheduled_charging_start_time: Option<i64>,
    pub charge_limit_soc: Option<i64>,
    pub charger_power: Option<f64>,
    pub windows_open: Option<bool>,
    pub driver_front_window_open: Option<bool>,
    pub driver_rear_window_open: Option<bool>,
    pub passenger_front_window_open: Option<bool>,
    pub passenger_rear_window_open: Option<bool>,
    pub doors_open: Option<bool>,
    pub driver_front_door_open: Option<bool>,
    pub driver_rear_door_open: Option<bool>,
    pub passenger_front_door_open: Option<bool>,
    pub passenger_rear_door_open: Option<bool>,
    pub odometer: Option<f64>,
    pub shift_state: Option<String>,
    pub charge_port_door_open: Option<bool>,
    pub time_to_full_charge: Option<f64>,
    pub charger_phases: Option<i64>,
    pub charger_actual_current: Option<f64>,
    pub charger_voltage: Option<f64>,
    pub version: Option<String>,
    pub update_available: Option<bool>,
    pub update_version: Option<String>,
    pub update_status: Option<String>,
    pub is_user_present: Option<bool>,
    pub geofence: Option<String>,
    pub model: Option<String>,
    pub trim_badging: Option<String>,
    pub exterior_color: Option<String>,
    pub wheel_type: Option<String>,
    pub spoiler_type: Option<String>,
    pub trunk_open: Option<bool>,
    pub frunk_open: Option<bool>,
    pub elevation: Option<f64>,
    pub power: Option<f64>,
    pub charge_current_request: Option<i64>,
    pub charge_current_request_max: Option<i64>,
    pub tpms_pressure_fl: Option<f64>,
    pub tpms_pressure_fr: Option<f64>,
    pub tpms_pressure_rl: Option<f64>,
    pub tpms_pressure_rr: Option<f64>,
    pub tpms_soft_warning_fl: Option<bool>,
    pub tpms_soft_warning_fr: Option<bool>,
    pub tpms_soft_warning_rl: Option<bool>,
    pub tpms_soft_warning_rr: Option<bool>,
    pub climate_keeper_mode: Option<String>,
    pub active_route_destination: Option<String>,
    pub active_route_latitude: Option<f64>,
    pub active_route_longitude: Option<f64>,
    pub active_route_energy_at_arrival: Option<f64>,
    pub active_route_miles_to_arrival: Option<f64>,
    pub active_route_minutes_to_arrival: Option<f64>,
    pub active_route_traffic_minutes_delay: Option<f64>,
    pub center_display_state: Option<i64>,
    pub service_mode: Option<bool>,
    pub sun_roof_state: Option<String>,
    pub sun_roof_installed: Option<bool>,
    pub sun_roof_percent_open: Option<i64>,
    pub download_perc: Option<i64>,
    pub install_perc: Option<i64>,
}

pub fn build_current_vehicle_summary(
    vehicle_id: Uuid,
    observations: &[ObservationRecord],
    car: Option<ProjectionCar>,
    lifecycle: Option<&LifecycleStateRecord>,
    geofence: Option<String>,
) -> CurrentVehicleSummary {
    let owner = latest_observation_of_types(
        observations,
        &["owner_api_vehicle_data_v1", "fleet_api_vehicle_data_v1"],
    );
    let stream = observation(observations, "tesla_stream_update_v1");
    let discovery = latest_observation_of_types(
        observations,
        &["owner_api_discovery_v1", "fleet_api_discovery_v1"],
    );
    let mut vehicle_data = owner
        .and_then(|record| provider_vehicle_data(&record.payload))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if stream.is_some_and(|stream| {
        owner.is_none_or(|owner| stream.observed_at_ms >= owner.observed_at_ms)
    }) && let Some(fields) = stream
        .and_then(|record| record.payload.get("fields"))
        .and_then(Value::as_object)
    {
        merge_non_null_objects(&mut vehicle_data, fields);
    }

    let drive = object(Some(&vehicle_data), "drive_state");
    let charge = object(Some(&vehicle_data), "charge_state");
    let climate = object(Some(&vehicle_data), "climate_state");
    let vehicle = object(Some(&vehicle_data), "vehicle_state");
    let config = object(Some(&vehicle_data), "vehicle_config");
    let update = vehicle.and_then(|value| object(Some(value), "software_update"));
    let open =
        lifecycle.and_then(|record| OpenSessionState::decode(&record.open_session_json).ok());
    let newest = observations
        .iter()
        .max_by_key(|record| (record.observed_at_ms, record.observation_id));
    let source_state = newest
        .or(owner)
        .or(discovery)
        .and_then(|record| text(record.payload.as_object(), "source_vehicle_state"));
    let state = open
        .as_ref()
        .and_then(|state| state.open_state.as_ref().map(|state| state.state.clone()))
        .or(source_state)
        .or_else(|| Some("unavailable".to_owned()));
    let since = open
        .as_ref()
        .and_then(|state| state.open_state.as_ref().map(|state| state.start_date_ms));

    let windows = ["fd_window", "rd_window", "fp_window", "rp_window"]
        .map(|field| open_indicator(vehicle, field));
    let doors = ["df", "dr", "pf", "pr"].map(|field| open_indicator(vehicle, field));
    let charging_state = text(charge, "charging_state");
    let software_status = text(update, "status");
    let raw_model = text(config, "car_type");
    let model = car
        .as_ref()
        .map(|car| car.model.clone())
        .or_else(|| raw_model.as_deref().map(normalize_tesla_model_code));
    let display_name = owner
        .and_then(|record| text(record.payload.as_object(), "display_name"))
        .or_else(|| text(Some(&vehicle_data), "display_name"))
        .or_else(|| car.as_ref().map(|car| car.name.clone()));
    let car_version = text(vehicle, "car_version");
    let update_version = text(update, "version").and_then(first_word);
    let latitude = number(drive, "latitude").or_else(|| number(drive, "est_lat"));
    let longitude = number(drive, "longitude").or_else(|| number(drive, "est_lng"));

    CurrentVehicleSummary {
        vehicle_id,
        observed_at_ms: newest.map(|record| record.observed_at_ms),
        car: car.clone(),
        display_name,
        state,
        since,
        healthy: Some(lifecycle.is_some_and(|record| !record.quarantined)),
        latitude,
        longitude,
        heading: number(drive, "heading").or_else(|| number(drive, "est_heading")),
        battery_level: integer(charge, "battery_level"),
        charging_state: charging_state.clone(),
        usable_battery_level: integer(charge, "usable_battery_level"),
        ideal_battery_range_km: miles_to_km(number(charge, "ideal_battery_range")),
        est_battery_range_km: miles_to_km(number(charge, "est_battery_range")),
        rated_battery_range_km: miles_to_km(number(charge, "battery_range")),
        charge_energy_added: number(charge, "charge_energy_added"),
        speed: number(drive, "speed").map(mph_to_kmh),
        outside_temp: number(climate, "outside_temp"),
        inside_temp: number(climate, "inside_temp"),
        is_climate_on: boolean(climate, "is_climate_on"),
        is_preconditioning: boolean(climate, "is_preconditioning"),
        locked: boolean(vehicle, "locked"),
        sentry_mode: boolean(vehicle, "sentry_mode"),
        plugged_in: charging_state.as_deref().map(|state| {
            !state.eq_ignore_ascii_case("disconnected") && !state.eq_ignore_ascii_case("unplugged")
        }),
        scheduled_charging_start_time: integer(charge, "scheduled_charging_start_time"),
        charge_limit_soc: integer(charge, "charge_limit_soc"),
        charger_power: number(charge, "charger_power"),
        windows_open: all_known_any_open(windows),
        driver_front_window_open: windows[0],
        driver_rear_window_open: windows[1],
        passenger_front_window_open: windows[2],
        passenger_rear_window_open: windows[3],
        doors_open: all_known_any_open(doors),
        driver_front_door_open: doors[0],
        driver_rear_door_open: doors[1],
        passenger_front_door_open: doors[2],
        passenger_rear_door_open: doors[3],
        odometer: miles_to_km(number(vehicle, "odometer")),
        shift_state: text(drive, "shift_state"),
        charge_port_door_open: boolean(charge, "charge_port_door_open"),
        time_to_full_charge: number(charge, "time_to_full_charge"),
        charger_phases: integer(charge, "charger_phases").filter(|phases| *phases > 0),
        charger_actual_current: number(charge, "charger_actual_current"),
        charger_voltage: number(charge, "charger_voltage"),
        version: car_version.and_then(first_word),
        update_available: software_status.as_deref().map(|status| {
            matches!(
                status,
                "available" | "downloading" | "downloading_wifi_wait" | "scheduled" | "installing"
            )
        }),
        update_version,
        update_status: software_status,
        is_user_present: boolean(vehicle, "is_user_present"),
        geofence,
        model,
        trim_badging: car
            .as_ref()
            .and_then(|car| car.trim_badging.clone())
            .or_else(|| text(config, "trim_badging")),
        exterior_color: car
            .as_ref()
            .and_then(|car| car.exterior_color.clone())
            .or_else(|| text(config, "exterior_color")),
        wheel_type: car
            .as_ref()
            .and_then(|car| car.wheel_type.clone())
            .or_else(|| text(config, "wheel_type")),
        spoiler_type: car
            .as_ref()
            .and_then(|car| car.spoiler_type.clone())
            .or_else(|| text(config, "spoiler_type")),
        trunk_open: open_indicator(vehicle, "rt"),
        frunk_open: open_indicator(vehicle, "ft"),
        elevation: number(drive, "native_location_elevation")
            .or_else(|| number(drive, "elevation")),
        power: number(drive, "power"),
        charge_current_request: integer(charge, "charge_current_request"),
        charge_current_request_max: integer(charge, "charge_current_request_max"),
        tpms_pressure_fl: number(vehicle, "tpms_pressure_fl"),
        tpms_pressure_fr: number(vehicle, "tpms_pressure_fr"),
        tpms_pressure_rl: number(vehicle, "tpms_pressure_rl"),
        tpms_pressure_rr: number(vehicle, "tpms_pressure_rr"),
        tpms_soft_warning_fl: boolean(vehicle, "tpms_soft_warning_fl"),
        tpms_soft_warning_fr: boolean(vehicle, "tpms_soft_warning_fr"),
        tpms_soft_warning_rl: boolean(vehicle, "tpms_soft_warning_rl"),
        tpms_soft_warning_rr: boolean(vehicle, "tpms_soft_warning_rr"),
        climate_keeper_mode: text(climate, "climate_keeper_mode"),
        active_route_destination: text(drive, "active_route_destination"),
        active_route_latitude: number(drive, "active_route_latitude"),
        active_route_longitude: number(drive, "active_route_longitude"),
        active_route_energy_at_arrival: number(drive, "active_route_energy_at_arrival"),
        active_route_miles_to_arrival: number(drive, "active_route_miles_to_arrival"),
        active_route_minutes_to_arrival: number(drive, "active_route_minutes_to_arrival"),
        active_route_traffic_minutes_delay: number(drive, "active_route_traffic_minutes_delay"),
        center_display_state: integer(vehicle, "center_display_state"),
        service_mode: boolean(vehicle, "service_mode")
            .or_else(|| open.as_ref().and_then(|state| state.service_mode)),
        sun_roof_state: text(vehicle, "sun_roof_state"),
        sun_roof_installed: integer(config, "sun_roof_installed").map(|value| value > 0),
        sun_roof_percent_open: integer(vehicle, "sun_roof_percent_open"),
        download_perc: integer(update, "download_perc"),
        install_perc: integer(update, "install_perc"),
    }
}

fn observation<'a>(
    observations: &'a [ObservationRecord],
    record_type: &str,
) -> Option<&'a ObservationRecord> {
    observations.iter().find(|record| {
        record.payload.get("record_type").and_then(Value::as_str) == Some(record_type)
    })
}

fn latest_observation_of_types<'a>(
    observations: &'a [ObservationRecord],
    record_types: &[&str],
) -> Option<&'a ObservationRecord> {
    observations
        .iter()
        .filter(|record| {
            record
                .payload
                .get("record_type")
                .and_then(Value::as_str)
                .is_some_and(|record_type| record_types.contains(&record_type))
        })
        .max_by_key(|record| (record.observed_at_ms, record.observation_id))
}

fn provider_vehicle_data(payload: &Value) -> Option<&Value> {
    payload.get("vehicle_data").or_else(|| {
        payload
            .get("provider_raw_json")
            .and_then(|raw| raw.get("response"))
    })
}

fn merge_non_null_objects(target: &mut Map<String, Value>, source: &Map<String, Value>) {
    for (key, value) in source {
        if value.is_null() {
            continue;
        }
        if let (Some(target), Some(source)) = (
            target.get_mut(key).and_then(Value::as_object_mut),
            value.as_object(),
        ) {
            merge_non_null_objects(target, source);
        } else {
            target.insert(key.clone(), value.clone());
        }
    }
}

fn object<'a>(map: Option<&'a Map<String, Value>>, key: &str) -> Option<&'a Map<String, Value>> {
    map.and_then(|map| map.get(key)).and_then(Value::as_object)
}

fn text(map: Option<&Map<String, Value>>, key: &str) -> Option<String> {
    map.and_then(|map| map.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 512)
        .map(str::to_owned)
}

fn number(map: Option<&Map<String, Value>>, key: &str) -> Option<f64> {
    map.and_then(|map| map.get(key))
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
}

fn integer(map: Option<&Map<String, Value>>, key: &str) -> Option<i64> {
    let value = map.and_then(|map| map.get(key))?;
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
}

fn boolean(map: Option<&Map<String, Value>>, key: &str) -> Option<bool> {
    map.and_then(|map| map.get(key)).and_then(Value::as_bool)
}

fn open_indicator(map: Option<&Map<String, Value>>, key: &str) -> Option<bool> {
    number(map, key).map(|value| value > 0.0)
}

fn all_known_any_open(values: [Option<bool>; 4]) -> Option<bool> {
    values
        .iter()
        .all(Option::is_some)
        .then(|| values.into_iter().flatten().any(|value| value))
}

fn miles_to_km(value: Option<f64>) -> Option<f64> {
    value.map(|value| (value * 1.609_344 * 100.0).round() / 100.0)
}

fn mph_to_kmh(value: f64) -> i64 {
    (value / 0.621_371_192_237_33).round() as i64
}

fn first_word(value: String) -> Option<String> {
    value.split_whitespace().next().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{db::ObservationRecord, protocol::Sha256Digest};

    fn record(id: i64, observed_at_ms: i64, payload: Value) -> ObservationRecord {
        ObservationRecord {
            observation_id: id,
            source_id: Uuid::from_u128(1),
            vehicle_id: Uuid::from_u128(2),
            observed_at_ms,
            received_at_ms: observed_at_ms,
            payload_sha256: Sha256Digest::of_bytes(b"test"),
            payload,
        }
    }

    #[test]
    fn v4_1_1_summary_maps_owner_fields_and_non_null_stream_overlay() {
        let owner = record(
            1,
            1_000,
            json!({
                "record_type": "owner_api_vehicle_data_v1",
                "source_vehicle_state": "online",
                "vehicle_data": {
                    "drive_state": {"latitude": 1.0, "longitude": 2.0, "speed": 10},
                    "charge_state": {"battery_level": 80, "charger_phases": 0},
                    "climate_state": {"inside_temp": 21.0},
                    "vehicle_state": {
                        "fd_window": 0, "rd_window": 0, "fp_window": 1, "rp_window": 0,
                        "df": 0, "dr": 0, "pf": 0, "pr": 0,
                        "software_update": {"status": "downloading", "download_perc": 42},
                        "tpms_soft_warning_fl": true,
                        "sun_roof_percent_open": 5,
                        "service_mode": true
                    },
                    "vehicle_config": {"car_type": "model3", "sun_roof_installed": 1}
                }
            }),
        );
        let stream = record(
            2,
            2_000,
            json!({
                "record_type": "tesla_stream_update_v1",
                "source_vehicle_state": "online",
                "fields": {
                    "drive_state": {"latitude": 3.0, "longitude": null, "power": 12.0},
                    "charge_state": {"battery_level": 79}
                }
            }),
        );
        let summary = build_current_vehicle_summary(
            Uuid::from_u128(2),
            &[owner, stream],
            None,
            None,
            Some("Home".to_owned()),
        );
        assert_eq!(summary.latitude, Some(3.0));
        assert_eq!(summary.longitude, Some(2.0));
        assert_eq!(summary.battery_level, Some(79));
        assert_eq!(summary.windows_open, Some(true));
        assert_eq!(summary.doors_open, Some(false));
        assert_eq!(summary.charger_phases, None);
        assert_eq!(summary.download_perc, Some(42));
        assert_eq!(summary.tpms_soft_warning_fl, Some(true));
        assert_eq!(summary.sun_roof_installed, Some(true));
        assert_eq!(summary.sun_roof_percent_open, Some(5));
        assert_eq!(summary.service_mode, Some(true));
        assert_eq!(summary.geofence.as_deref(), Some("Home"));
    }

    #[test]
    fn stream_range_overlays_owner_ideal_battery_range() {
        let owner = record(
            1,
            1_000,
            json!({
                "record_type": "owner_api_vehicle_data_v1",
                "source_vehicle_state": "online",
                "vehicle_data": {
                    "charge_state": {"battery_level": 80, "ideal_battery_range": 250.0}
                }
            }),
        );
        let stream = record(
            2,
            2_000,
            crate::lifecycle::stream_observation_payload(&crate::tesla_stream::StreamUpdate {
                tag: "9".into(),
                timestamp_ms: 2_000,
                speed: Some(10),
                odometer: Some(100.0),
                soc: Some(79),
                elevation: Some(10),
                est_heading: Some(90),
                est_lat: Some(1.0),
                est_lng: Some(2.0),
                power: Some(5),
                shift_state: Some("D".into()),
                range: Some(200),
                est_range: Some(190),
                heading: Some(90),
            }),
        );
        let summary =
            build_current_vehicle_summary(Uuid::from_u128(2), &[owner, stream], None, None, None);
        assert_eq!(summary.battery_level, Some(79));
        assert_eq!(summary.ideal_battery_range_km, Some(321.87));
    }

    #[test]
    fn fleet_provider_raw_response_builds_the_same_current_summary() {
        let stale_owner = record(
            1,
            1_000,
            json!({
                "record_type": "owner_api_vehicle_data_v1",
                "vehicle_data": {"charge_state": {"battery_level": 10}}
            }),
        );
        let fleet = record(
            2,
            2_000,
            json!({
                "record_type": "fleet_api_vehicle_data_v1",
                "source_vehicle_state": "online",
                "provider_raw_json": {
                    "response": {
                        "drive_state": {"latitude": 47.0, "longitude": 19.0},
                        "charge_state": {"battery_level": 81}
                    },
                    "provider_trace": "kept"
                }
            }),
        );

        let summary = build_current_vehicle_summary(
            Uuid::from_u128(2),
            &[stale_owner, fleet],
            None,
            None,
            None,
        );
        assert_eq!(summary.latitude, Some(47.0));
        assert_eq!(summary.longitude, Some(19.0));
        assert_eq!(summary.battery_level, Some(81));
        assert_eq!(summary.plugged_in, None);
    }

    #[test]
    fn plugged_in_requires_charging_state_and_ignores_bare_charge_state() {
        let stream = record(
            1,
            1_000,
            json!({
                "record_type": "tesla_stream_update_v1",
                "source_vehicle_state": "online",
                "fields": {
                    "charge_state": {"battery_level": 80, "ideal_battery_range": 200}
                }
            }),
        );
        let stream_summary =
            build_current_vehicle_summary(Uuid::from_u128(2), &[stream], None, None, None);
        assert_eq!(stream_summary.plugged_in, None);
        assert_eq!(stream_summary.battery_level, Some(80));

        let charging = record(
            2,
            2_000,
            json!({
                "record_type": "owner_api_vehicle_data_v1",
                "source_vehicle_state": "online",
                "vehicle_data": {
                    "charge_state": {"charging_state": "Charging", "battery_level": 50}
                }
            }),
        );
        assert_eq!(
            build_current_vehicle_summary(Uuid::from_u128(2), &[charging], None, None, None)
                .plugged_in,
            Some(true)
        );

        let disconnected = record(
            3,
            3_000,
            json!({
                "record_type": "owner_api_vehicle_data_v1",
                "source_vehicle_state": "online",
                "vehicle_data": {
                    "charge_state": {"charging_state": "Disconnected", "battery_level": 50}
                }
            }),
        );
        assert_eq!(
            build_current_vehicle_summary(Uuid::from_u128(2), &[disconnected], None, None, None)
                .plugged_in,
            Some(false)
        );
    }
}
