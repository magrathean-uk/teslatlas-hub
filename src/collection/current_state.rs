// SPDX-License-Identifier: AGPL-3.0-only

//! The current-vehicle summary (C6 Part S1, with Part S2's fields still to
//! come): one pure function of the stored current rows, the stored car, the
//! lifecycle record, a geofence label, the Hub's clock and the configured
//! silence tolerance (C6 R21). Statements, merge, field readers and
//! transient visibility are in `summary_fields`.

use serde::Serialize;
use uuid::Uuid;

use crate::{
    db::{LifecycleStateRecord, ObservationRecord},
    hub_pack::ProjectionCar,
    lifecycle::{miles_to_km, miles_to_km_rounded, mph_to_kmh, round_to_precision},
};

mod summary_fields;
use summary_fields::{
    Class, Fields, Statements, Visibility, accepted_rows, activity_state, exact_integer,
    lifecycle_view, newest_observed_at_ms, number, shift_state, state_since_ms,
};

/// The activity vocabulary of the summary (C6 R29).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivityState {
    Online,
    Asleep,
    Offline,
    Driving,
    Charging,
    Updating,
}

impl ActivityState {
    /// The lowercase vocabulary word.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Asleep => "asleep",
            Self::Offline => "offline",
            Self::Driving => "driving",
            Self::Charging => "charging",
            Self::Updating => "updating",
        }
    }
}

/// The settings view of the stored car under its served names (C6 R49).
/// Filled by C6-S2 with the car view.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CurrentCarSettingsView {
    pub collection_enabled: bool,
    pub streaming_enabled: bool,
    pub sleep_window_min: i64,
    pub sleep_after_idle_min: i64,
    pub sleep_window_min_resolved: bool,
    pub sleep_requires_locked: bool,
    pub supercharging_free: bool,
    pub battery_lfp: bool,
}

/// The stored car as the summary shows it (C6 R40, R48). Filled by C6-S2.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CurrentCarView {
    pub id: i64,
    pub name: String,
    pub vin: Option<String>,
    pub model_code: Option<String>,
    /// The stored model text, served under the alias `car.model` (C6 R50).
    #[serde(skip)]
    pub stored_model_text: String,
    pub model_name: Option<&'static str>,
    pub model_year: Option<i32>,
    pub trim_badging: Option<String>,
    pub variant_name: Option<String>,
    pub exterior_color: Option<String>,
    pub wheel_type: Option<String>,
    pub spoiler_type: Option<String>,
    pub firmware_version: Option<String>,
    pub api_vehicle_id: Option<i64>,
    pub stream_vehicle_id: Option<i64>,
    pub efficiency_wh_per_km: Option<f64>,
    pub settings: CurrentCarSettingsView,
}

/// The summary. Every field is one key of the published contract and is
/// emitted as JSON null when unknown (C6 R42). Fields are held under their
/// 1.3.0 names (C6 R47); until C6-C1 writes the hand-made serialiser with the
/// aliases (C6 R50), the derived `Serialize` below keeps serving the 1.2.0 key
/// names and shape, so the HTTP body stays a valid 1.2.0 body in between. The
/// keys that 1.2.0 has no name for are skipped.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CurrentVehicleSummary {
    pub vehicle_id: Uuid,
    pub observed_at_ms: Option<i64>,
    #[serde(rename = "state")]
    pub activity_state: Option<ActivityState>,
    #[serde(rename = "since")]
    pub state_since_ms: Option<i64>,
    #[serde(rename = "healthy")]
    pub processing_healthy: Option<bool>,
    #[serde(rename = "geofence")]
    pub geofence_name: Option<String>,
    // Descriptor and car view: C6-S2.
    pub display_name: Option<String>,
    #[serde(rename = "model")]
    pub model_code: Option<String>,
    #[serde(skip)]
    pub model_name: Option<&'static str>,
    #[serde(skip)]
    pub model_year: Option<i32>,
    pub trim_badging: Option<String>,
    pub exterior_color: Option<String>,
    pub wheel_type: Option<String>,
    pub spoiler_type: Option<String>,
    pub car: Option<CurrentCarView>,
    // Drive state.
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub heading: Option<f64>,
    pub elevation: Option<f64>,
    #[serde(rename = "speed")]
    pub speed_kmh: Option<i64>,
    pub power: Option<f64>,
    pub shift_state: Option<String>,
    pub active_route_destination: Option<String>,
    pub active_route_latitude: Option<f64>,
    pub active_route_longitude: Option<f64>,
    #[serde(skip)]
    pub active_route_km_to_arrival: Option<f64>,
    /// Tesla's miles value, the alias value of C6 R50.
    #[serde(rename = "active_route_miles_to_arrival")]
    pub active_route_miles_to_arrival_raw: Option<f64>,
    pub active_route_minutes_to_arrival: Option<f64>,
    pub active_route_traffic_minutes_delay: Option<f64>,
    pub active_route_energy_at_arrival: Option<f64>,
    // Charge state.
    pub battery_level: Option<i64>,
    pub usable_battery_level: Option<i64>,
    #[serde(rename = "rated_battery_range_km")]
    pub battery_range_km: Option<f64>,
    pub ideal_battery_range_km: Option<f64>,
    pub est_battery_range_km: Option<f64>,
    pub charge_limit_soc: Option<i64>,
    pub charge_energy_added: Option<f64>,
    pub charge_current_request: Option<i64>,
    pub charge_current_request_max: Option<i64>,
    pub scheduled_charging_start_time: Option<i64>,
    pub charge_port_door_open: Option<bool>,
    // C6-S2.
    pub charging_state: Option<String>,
    #[serde(rename = "plugged_in")]
    pub charge_cable_connected: Option<bool>,
    pub charger_power: Option<f64>,
    pub charger_voltage: Option<f64>,
    pub charger_actual_current: Option<f64>,
    pub charger_phases: Option<i64>,
    pub time_to_full_charge: Option<f64>,
    // Climate state.
    pub inside_temp: Option<f64>,
    pub outside_temp: Option<f64>,
    pub is_climate_on: Option<bool>,
    pub is_preconditioning: Option<bool>,
    pub climate_keeper_mode: Option<String>,
    // Vehicle state.
    #[serde(rename = "odometer")]
    pub odometer_km: Option<f64>,
    pub locked: Option<bool>,
    pub tpms_soft_warning_fl: Option<bool>,
    pub tpms_soft_warning_fr: Option<bool>,
    pub tpms_soft_warning_rl: Option<bool>,
    pub tpms_soft_warning_rr: Option<bool>,
    pub sentry_mode: Option<bool>,
    pub is_user_present: Option<bool>,
    pub center_display_state: Option<i64>,
    pub sun_roof_percent_open: Option<i64>,
    pub sun_roof_state: Option<String>,
    pub tpms_pressure_fl: Option<f64>,
    pub tpms_pressure_fr: Option<f64>,
    pub tpms_pressure_rl: Option<f64>,
    pub tpms_pressure_rr: Option<f64>,
    pub sun_roof_installed: Option<bool>,
    // Doors, windows and trunks: C6-S2.
    #[serde(rename = "doors_open")]
    pub any_door_open: Option<bool>,
    #[serde(rename = "driver_front_door_open")]
    pub door_driver_front_open: Option<bool>,
    #[serde(rename = "driver_rear_door_open")]
    pub door_driver_rear_open: Option<bool>,
    #[serde(rename = "passenger_front_door_open")]
    pub door_passenger_front_open: Option<bool>,
    #[serde(rename = "passenger_rear_door_open")]
    pub door_passenger_rear_open: Option<bool>,
    #[serde(rename = "windows_open")]
    pub any_window_open: Option<bool>,
    #[serde(rename = "driver_front_window_open")]
    pub window_driver_front_open: Option<bool>,
    #[serde(rename = "driver_rear_window_open")]
    pub window_driver_rear_open: Option<bool>,
    #[serde(rename = "passenger_front_window_open")]
    pub window_passenger_front_open: Option<bool>,
    #[serde(rename = "passenger_rear_window_open")]
    pub window_passenger_rear_open: Option<bool>,
    #[serde(rename = "frunk_open")]
    pub front_trunk_open: Option<bool>,
    #[serde(rename = "trunk_open")]
    pub rear_trunk_open: Option<bool>,
    // Software and service: C6-S2, except the two progress values.
    #[serde(rename = "version")]
    pub software_version: Option<String>,
    #[serde(rename = "update_available")]
    pub software_update_available: Option<bool>,
    #[serde(rename = "update_status")]
    pub software_update_status: Option<String>,
    #[serde(rename = "update_version")]
    pub software_update_version: Option<String>,
    #[serde(rename = "download_perc")]
    pub software_update_download_perc: Option<i64>,
    #[serde(rename = "install_perc")]
    pub software_update_install_perc: Option<i64>,
    pub service_mode: Option<bool>,
}

/// The configured silence tolerance, `[collector] offline_drive_timeout_seconds`,
/// as the `transient_max_age_ms` argument of
/// [`build_current_vehicle_summary`] (C6 R21, R71): seconds times 1,000,
/// saturating. Every caller that holds the configuration converts through this
/// one function.
pub fn offline_drive_timeout_ms(offline_drive_timeout_seconds: u64) -> i64 {
    i64::try_from(offline_drive_timeout_seconds)
        .unwrap_or(i64::MAX)
        .saturating_mul(1_000)
}

/// Build the current summary of one vehicle (C6 R21). Pure: it reads no clock,
/// storage, configuration or randomness and logs nothing, and its output does
/// not depend on the order of `observations`. `now_ms` is the Hub's wall clock;
/// `transient_max_age_ms` is the configured `T_gap` (a value of 0 or less reads
/// as the default, C1's `DEFAULT_OFFLINE_DRIVE_TIMEOUT`). `car` is read by C6-S2.
/// The label in `geofence` is kept only for a known coordinate pair (C6 R41).
pub fn build_current_vehicle_summary(
    vehicle_id: Uuid,
    observations: &[ObservationRecord],
    _car: Option<ProjectionCar>,
    lifecycle: Option<&LifecycleStateRecord>,
    geofence: Option<String>,
    now_ms: i64,
    transient_max_age_ms: i64,
) -> CurrentVehicleSummary {
    let rows = accepted_rows(vehicle_id, observations);
    let statements = Statements::collect(&rows);
    let lifecycle = lifecycle_view(lifecycle);
    let activity = activity_state(lifecycle.state.as_ref(), &rows);
    let observed_at_ms = newest_observed_at_ms(&rows);
    let fields = Fields::new(
        &statements,
        Visibility::new(
            activity,
            lifecycle.state.as_ref(),
            observed_at_ms,
            now_ms,
            transient_max_age_ms,
        ),
    );
    let (persistent, transient) = (Class::Persistent, Class::Transient);

    let coordinates = fields.pair(persistent, "drive_state.latitude", "drive_state.longitude");
    let route = fields.pair(
        transient,
        "drive_state.active_route_latitude",
        "drive_state.active_route_longitude",
    );
    // Rated, ideal and estimated range: Tesla's miles, or the import source's
    // kilometres, at two decimals (C6 R27).
    let range_km = |candidates: [&str; 2]| {
        fields.with(persistent, &candidates, |statement| {
            let value = number(statement.value)?;
            let km = if statement.is_import() {
                round_to_precision(value, 2)
            } else {
                miles_to_km_rounded(value, 2)
            };
            (0.0..=9_999.99).contains(&km).then_some(km + 0.0)
        })
    };
    let temperature = |candidate: &str| fields.scaled(transient, candidate, 1, -999.9..=999.9);
    let pressure = |candidate: &str| fields.scaled(persistent, candidate, 1, 0.0..=999.9);
    let non_negative = 0.0..=f64::MAX;

    CurrentVehicleSummary {
        vehicle_id,
        observed_at_ms,
        activity_state: activity,
        state_since_ms: state_since_ms(activity, lifecycle.state.as_ref()),
        processing_healthy: lifecycle.processing_healthy,
        geofence_name: coordinates.and(geofence),
        display_name: None,
        model_code: None,
        model_name: None,
        model_year: None,
        trim_badging: None,
        exterior_color: None,
        wheel_type: None,
        spoiler_type: None,
        car: None,
        latitude: coordinates.map(|(latitude, _)| latitude),
        longitude: coordinates.map(|(_, longitude)| longitude),
        heading: fields.plain(persistent, "drive_state.heading", 0.0..=360.0),
        elevation: fields.elevation(),
        speed_kmh: fields.with(transient, &["drive_state.speed"], |statement| {
            let kmh = mph_to_kmh(number(statement.value)?);
            (0..=32_767).contains(&kmh).then_some(kmh)
        }),
        power: fields.int_number(transient, "drive_state.power", -32_768, 32_767),
        shift_state: fields.with(transient, &["drive_state.shift_state"], |statement| {
            shift_state(statement.value)
        }),
        active_route_destination: fields.text(transient, "drive_state.active_route_destination"),
        active_route_latitude: route.map(|(latitude, _)| latitude),
        active_route_longitude: route.map(|(_, longitude)| longitude),
        active_route_km_to_arrival: fields.with(
            transient,
            &["drive_state.active_route_miles_to_arrival"],
            |statement| {
                let miles = number(statement.value).filter(|miles| *miles >= 0.0)?;
                // A finite input can still overflow the conversion (C6 R27).
                let km = miles_to_km_rounded(miles, 2);
                km.is_finite().then_some(km)
            },
        ),
        active_route_miles_to_arrival_raw: fields.plain(
            transient,
            "drive_state.active_route_miles_to_arrival",
            non_negative.clone(),
        ),
        active_route_minutes_to_arrival: fields.plain(
            transient,
            "drive_state.active_route_minutes_to_arrival",
            non_negative.clone(),
        ),
        active_route_traffic_minutes_delay: fields.plain(
            transient,
            "drive_state.active_route_traffic_minutes_delay",
            non_negative.clone(),
        ),
        active_route_energy_at_arrival: fields.plain(
            transient,
            "drive_state.active_route_energy_at_arrival",
            f64::MIN..=f64::MAX,
        ),
        battery_level: fields.with(
            persistent,
            &[
                "charge_state.battery_level",
                "import_position.battery_level",
            ],
            |statement| summary_fields::integer(statement.value, 0, 100),
        ),
        usable_battery_level: fields.int(persistent, "charge_state.usable_battery_level", 0, 100),
        battery_range_km: range_km([
            "charge_state.battery_range",
            "import_position.rated_battery_range_km",
        ]),
        ideal_battery_range_km: range_km([
            "charge_state.ideal_battery_range",
            "import_position.ideal_battery_range_km",
        ]),
        est_battery_range_km: range_km([
            "charge_state.est_battery_range",
            "import_position.est_battery_range_km",
        ]),
        charge_limit_soc: fields.int(persistent, "charge_state.charge_limit_soc", 0, 100),
        charge_energy_added: fields.scaled(
            persistent,
            "charge_state.charge_energy_added",
            2,
            0.0..=999_999.99,
        ),
        charge_current_request: fields.int(
            persistent,
            "charge_state.charge_current_request",
            0,
            i64::MAX,
        ),
        charge_current_request_max: fields.int(
            persistent,
            "charge_state.charge_current_request_max",
            0,
            i64::MAX,
        ),
        scheduled_charging_start_time: fields.int(
            persistent,
            "charge_state.scheduled_charging_start_time",
            0,
            i64::MAX,
        ),
        charge_port_door_open: fields.flag(persistent, "charge_state.charge_port_door_open"),
        charging_state: None,
        charge_cable_connected: None,
        charger_power: fields.int_number(transient, "charge_state.charger_power", -32_768, 32_767),
        charger_voltage: fields.int_number(
            transient,
            "charge_state.charger_voltage",
            -32_768,
            32_767,
        ),
        charger_actual_current: fields.int_number(
            transient,
            "charge_state.charger_actual_current",
            -32_768,
            32_767,
        ),
        charger_phases: fields.int(transient, "charge_state.charger_phases", 0, 32_767),
        time_to_full_charge: fields.plain(
            transient,
            "charge_state.time_to_full_charge",
            non_negative,
        ),
        inside_temp: temperature("climate_state.inside_temp"),
        outside_temp: temperature("climate_state.outside_temp"),
        is_climate_on: fields.flag(transient, "climate_state.is_climate_on"),
        is_preconditioning: fields.flag(transient, "climate_state.is_preconditioning"),
        climate_keeper_mode: fields.text(transient, "climate_state.climate_keeper_mode"),
        odometer_km: fields.with(
            persistent,
            &["vehicle_state.odometer", "import_position.odometer_km"],
            |statement| {
                let value = number(statement.value)?;
                let km = if statement.is_import() {
                    value
                } else {
                    miles_to_km(value)
                };
                (km.is_finite() && km >= 0.0).then_some(km + 0.0)
            },
        ),
        locked: fields.flag(persistent, "vehicle_state.locked"),
        tpms_soft_warning_fl: fields.flag(persistent, "vehicle_state.tpms_soft_warning_fl"),
        tpms_soft_warning_fr: fields.flag(persistent, "vehicle_state.tpms_soft_warning_fr"),
        tpms_soft_warning_rl: fields.flag(persistent, "vehicle_state.tpms_soft_warning_rl"),
        tpms_soft_warning_rr: fields.flag(persistent, "vehicle_state.tpms_soft_warning_rr"),
        sentry_mode: fields.flag(transient, "vehicle_state.sentry_mode"),
        is_user_present: fields.flag(transient, "vehicle_state.is_user_present"),
        center_display_state: fields.with(
            transient,
            &["vehicle_state.center_display_state"],
            |statement| {
                let code = exact_integer(statement.value)?;
                (0.0..i64::MAX as f64)
                    .contains(&code)
                    .then_some(code as i64)
            },
        ),
        sun_roof_percent_open: fields.int(
            persistent,
            "vehicle_state.sun_roof_percent_open",
            0,
            100,
        ),
        sun_roof_state: fields.text(persistent, "vehicle_state.sun_roof_state"),
        tpms_pressure_fl: pressure("vehicle_state.tpms_pressure_fl"),
        tpms_pressure_fr: pressure("vehicle_state.tpms_pressure_fr"),
        tpms_pressure_rl: pressure("vehicle_state.tpms_pressure_rl"),
        tpms_pressure_rr: pressure("vehicle_state.tpms_pressure_rr"),
        // A boolean as given; Tesla's equipment code reads 0 as absent and any
        // other exact integer as fitted (C6 R27).
        sun_roof_installed: fields.with(
            persistent,
            &["vehicle_config.sun_roof_installed"],
            |statement| match statement.value.as_bool() {
                Some(fitted) => Some(fitted),
                None => exact_integer(statement.value).map(|code| code != 0.0),
            },
        ),
        any_door_open: None,
        door_driver_front_open: None,
        door_driver_rear_open: None,
        door_passenger_front_open: None,
        door_passenger_rear_open: None,
        any_window_open: None,
        window_driver_front_open: None,
        window_driver_rear_open: None,
        window_passenger_front_open: None,
        window_passenger_rear_open: None,
        front_trunk_open: None,
        rear_trunk_open: None,
        software_version: None,
        software_update_available: None,
        software_update_status: None,
        software_update_version: None,
        software_update_download_perc: fields.int(
            transient,
            "vehicle_state.software_update.download_perc",
            0,
            100,
        ),
        software_update_install_perc: fields.int(
            transient,
            "vehicle_state.software_update.install_perc",
            0,
            100,
        ),
        service_mode: None,
    }
}

#[cfg(test)]
#[path = "current_state/b3_c6_s1_tests.rs"]
mod b3_c6_s1;

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::ObservationRecord;
    use crate::protocol::Sha256Digest;

    pub(super) fn record(
        id: i64,
        observed_at_ms: i64,
        payload: serde_json::Value,
    ) -> ObservationRecord {
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
}
