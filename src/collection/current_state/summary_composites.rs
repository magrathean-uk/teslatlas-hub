// SPDX-License-Identifier: AGPL-3.0-only

//! Composite and descriptor fields of the current-vehicle summary (C6 Part S2):
//! doors, windows and trunks (C6 R33), the charging state and cable (C6 R34 and
//! C6 R35), the software version and update (C6 R36 and C6 R37), the service
//! flag (C6 R38), the display name (C6 R39), the descriptor and the `car` view
//! (C6 R40, C6 R48 and C6 R49).
//!
//! Everything here is a pure function of its arguments: no clock, storage,
//! configuration or randomness, and nothing is logged (C6 R21, R45). Every
//! field of this file is a class P field: it has no age rule (C6 R27, R28).

use serde_json::Value;

use super::{
    ActivityState, CurrentCarSettingsView, CurrentCarView,
    summary_fields::{Class, Fields, K_TEXT_MAX, exact_integer, number, text_str},
};
use crate::{
    hub_pack::{
        ProjectionCar, ProjectionCarSettings, live_config_text, vehicle_model_code,
        vehicle_model_name, vehicle_model_year,
    },
    lifecycle::{OpenSessionState, normalise_enum_text, usable_car_version},
};

/// Statuses that mean an update has been offered or is installing (C6 R37,
/// C1 R29): the Offered and Installing classes.
const UPDATE_OFFERED: [&str; 6] = [
    "available",
    "scheduled",
    "downloading",
    "downloading_wifi_wait",
    "wifi_wait",
    "installing",
];

/// The charging states the summary names in canonical spelling (C6 R34).
const CHARGING_STATE_NAMES: [&str; 6] = [
    "Charging",
    "Starting",
    "Stopped",
    "Complete",
    "Disconnected",
    "NoPower",
];

/// The text a stored value may serve in the `car` view (C6 R40): kept as
/// stored, `None` when longer than [`K_TEXT_MAX`] scalar values.
fn bounded(value: Option<&str>) -> Option<String> {
    value
        .filter(|text| text.chars().count() <= K_TEXT_MAX)
        .map(str::to_owned)
}

// ---------------------------------------------------------------------------
// Doors, windows and trunks (C6 R33)
// ---------------------------------------------------------------------------

/// The twelve opening fields (C6 R33).
pub(super) struct Openings {
    pub(super) any_door_open: Option<bool>,
    pub(super) door_driver_front_open: Option<bool>,
    pub(super) door_driver_rear_open: Option<bool>,
    pub(super) door_passenger_front_open: Option<bool>,
    pub(super) door_passenger_rear_open: Option<bool>,
    pub(super) any_window_open: Option<bool>,
    pub(super) window_driver_front_open: Option<bool>,
    pub(super) window_driver_rear_open: Option<bool>,
    pub(super) window_passenger_front_open: Option<bool>,
    pub(super) window_passenger_rear_open: Option<bool>,
    pub(super) front_trunk_open: Option<bool>,
    pub(super) rear_trunk_open: Option<bool>,
}

/// A door or trunk value has no named form: only a boolean or an integer code.
fn no_word(_: &str) -> Option<bool> {
    None
}

/// A window's Fleet Telemetry name (C6 R33): the type prefix `WindowState` is
/// stripped once and the rest is compared ASCII case-insensitively. `Closed` is
/// false, `PartiallyOpen` and `Opened` are true, `Unknown` and anything else
/// are null.
fn window_word(raw: &str) -> Option<bool> {
    const PREFIX: &str = "WindowState";
    let trimmed = raw.trim();
    let rest = match trimmed.get(..PREFIX.len()) {
        Some(head) if head.eq_ignore_ascii_case(PREFIX) => &trimmed[PREFIX.len()..],
        _ => trimmed,
    };
    if rest.eq_ignore_ascii_case("closed") {
        Some(false)
    } else if rest.eq_ignore_ascii_case("partiallyopen") || rest.eq_ignore_ascii_case("opened") {
        Some(true)
    } else {
        None
    }
}

/// One element (C6 R33): a boolean as given; a numeric value (a JSON number or a
/// numeric string, C1 R4a) that is an exact integer, 0 for closed and any other
/// code for open; a fraction is null; a non-numeric string reads through
/// `word`; anything else is null.
fn opening(value: &Value, word: fn(&str) -> Option<bool>) -> Option<bool> {
    match value {
        Value::Bool(open) => Some(*open),
        Value::Number(_) => exact_integer(value).map(|code| code != 0.0),
        Value::String(raw) => {
            if number(value).is_some() {
                exact_integer(value).map(|code| code != 0.0)
            } else {
                word(raw)
            }
        }
        _ => None,
    }
}

/// Three-valued OR over four elements (C6 R33): true if any is true, false only
/// if all four are known false, otherwise null.
fn any_open(elements: [Option<bool>; 4]) -> Option<bool> {
    if elements.contains(&Some(true)) {
        Some(true)
    } else if elements.iter().all(|element| *element == Some(false)) {
        Some(false)
    } else {
        None
    }
}

pub(super) fn openings(fields: &Fields<'_, '_>) -> Openings {
    let persistent = Class::Persistent;
    let door = |key: &str| fields.with(persistent, &[key], |s| opening(s.value, no_word));
    let window = |key: &str| fields.with(persistent, &[key], |s| opening(s.value, window_word));
    let (driver_front, driver_rear) = (door("vehicle_state.df"), door("vehicle_state.dr"));
    let (passenger_front, passenger_rear) = (door("vehicle_state.pf"), door("vehicle_state.pr"));
    let (window_df, window_pf) = (
        window("vehicle_state.fd_window"),
        window("vehicle_state.fp_window"),
    );
    let (window_dr, window_pr) = (
        window("vehicle_state.rd_window"),
        window("vehicle_state.rp_window"),
    );
    Openings {
        any_door_open: any_open([driver_front, driver_rear, passenger_front, passenger_rear]),
        door_driver_front_open: driver_front,
        door_driver_rear_open: driver_rear,
        door_passenger_front_open: passenger_front,
        door_passenger_rear_open: passenger_rear,
        any_window_open: any_open([window_df, window_dr, window_pf, window_pr]),
        window_driver_front_open: window_df,
        window_driver_rear_open: window_dr,
        window_passenger_front_open: window_pf,
        window_passenger_rear_open: window_pr,
        // Trunks are not part of `any_door_open` (C6 R33).
        front_trunk_open: door("vehicle_state.ft"),
        rear_trunk_open: door("vehicle_state.rt"),
    }
}

// ---------------------------------------------------------------------------
// Charging state and cable (C6 R34, R35)
// ---------------------------------------------------------------------------

/// `charging_state` and `charge_cable_connected`.
pub(super) struct Charging {
    pub(super) state: Option<String>,
    pub(super) cable_connected: Option<bool>,
}

/// The canonical charging name of a text (C6 R34): C1 R40a normalisation, kept
/// only when it names one of the six charging states. A shift gear or
/// `Invalid` is canonical to the shared normaliser but is no charging state.
fn canonical_charging(raw: &str) -> Option<&'static str> {
    normalise_enum_text(raw).filter(|name| CHARGING_STATE_NAMES.contains(name))
}

/// The cable from a `conn_charge_cable` text (C6 R35): `"<invalid>"` is no
/// cable, any other non-empty text names the connected cable. The text is
/// trimmed first.
fn cable_from_text(raw: &str) -> Option<bool> {
    match raw.trim() {
        "" => None,
        "<invalid>" => Some(false),
        _ => Some(true),
    }
}

pub(super) fn charging(fields: &Fields<'_, '_>, activity: Option<ActivityState>) -> Charging {
    let raw = fields
        .stated(Class::Persistent, "charge_state.charging_state")
        .and_then(|statement| statement.value.as_str());
    let canonical = raw.and_then(canonical_charging);
    let state = match (raw, canonical) {
        // A sleeping car is not charging (C1 R26): a pre-sleep `Charging` or
        // `Starting` is no longer known. `offline` has no exception (C1 R27).
        (_, Some(name)) => {
            let sleeping = activity == Some(ActivityState::Asleep);
            (!(sleeping && matches!(name, "Charging" | "Starting"))).then(|| name.to_owned())
        }
        // `Unknown` is no state; any other string is passed through trimmed
        // and verbatim under the text rule.
        (Some(raw), None) if normalise_enum_text(raw) != Some("Unknown") => text_str(raw),
        _ => None,
    };
    // The cable reads the newest charging-state statement before the sleep
    // exception; when that names no state it reads the cable text (C6 R35).
    let cable_connected = match canonical {
        Some("Disconnected") => Some(false),
        Some(_) => Some(true),
        None => fields
            .stated(Class::Persistent, "charge_state.conn_charge_cable")
            .and_then(|statement| statement.value.as_str())
            .and_then(cable_from_text),
    };
    Charging {
        state,
        cable_connected,
    }
}

// ---------------------------------------------------------------------------
// Software version, update and service mode (C6 R36, R37, R38)
// ---------------------------------------------------------------------------

/// `software_version` (C6 R36): the first usable of the lifecycle's
/// `last_car_version`, the newest `vehicle_state.car_version` statement of a
/// vehicle-data row of a full source, and the stored car's `firmware_version`.
/// A source that does not parse is skipped.
pub(super) fn software_version(
    lifecycle: Option<&OpenSessionState>,
    fields: &Fields<'_, '_>,
    car: Option<&ProjectionCar>,
) -> Option<String> {
    lifecycle
        .and_then(|state| state.last_car_version.as_deref())
        .and_then(usable_car_version)
        .or_else(|| {
            fields.with_full_source(
                Class::Persistent,
                &["vehicle_state.car_version"],
                |statement| statement.value.as_str().and_then(usable_car_version),
            )
        })
        .or_else(|| {
            car.and_then(|car| car.firmware_version.as_deref())
                .and_then(usable_car_version)
        })
}

/// The update fields of C6 R37 (the two progress percents are class T and are
/// read with the S1 fields).
pub(super) struct SoftwareUpdate {
    pub(super) status: Option<String>,
    pub(super) available: Option<bool>,
    pub(super) version: Option<String>,
}

pub(super) fn software_update(fields: &Fields<'_, '_>) -> SoftwareUpdate {
    let persistent = Class::Persistent;
    let raw = fields
        .stated(persistent, "vehicle_state.software_update.status")
        .and_then(|statement| statement.value.as_str());
    let lowered = raw.map(|raw| raw.trim().to_ascii_lowercase());
    // `""` is Tesla's idle value; the listed statuses mean an offer; anything
    // else, an absent or a null status gives no information (C6 R37, 6.3).
    let available = match lowered.as_deref() {
        Some("") => Some(false),
        Some(word) if UPDATE_OFFERED.contains(&word) => Some(true),
        _ => None,
    };
    // The offered version means nothing without an offer.
    let version = if available == Some(true) {
        fields.with(
            persistent,
            &["vehicle_state.software_update.version"],
            |statement| statement.value.as_str().and_then(usable_car_version),
        )
    } else {
        None
    };
    SoftwareUpdate {
        status: raw.and_then(text_str),
        available,
        version,
    }
}

/// `service_mode` (C6 R38): the lifecycle's flag when it has one, else the
/// newest `vehicle_state.service_mode` boolean, else null.
pub(super) fn service_mode(
    lifecycle: Option<&OpenSessionState>,
    fields: &Fields<'_, '_>,
) -> Option<bool> {
    lifecycle
        .and_then(|state| state.service_mode)
        .or_else(|| fields.flag(Class::Persistent, "vehicle_state.service_mode"))
}

// ---------------------------------------------------------------------------
// Display name, descriptor and car view (C6 R39, R40, R48, R49)
// ---------------------------------------------------------------------------

/// `display_name` (C6 R39): the newest root `display_name` statement, else the
/// newest `vehicle_state.vehicle_name` statement (each under the text rule),
/// else the stored car's name when it is not empty after trimming.
pub(super) fn display_name(fields: &Fields<'_, '_>, car: Option<&ProjectionCar>) -> Option<String> {
    fields
        .text(Class::Persistent, "root.display_name")
        .or_else(|| fields.text(Class::Persistent, "vehicle_state.vehicle_name"))
        .or_else(|| car.and_then(|car| text_str(&car.name)))
}

/// The top-level descriptor fields (C6 R40).
pub(super) struct Descriptor {
    pub(super) model_code: Option<String>,
    pub(super) model_name: Option<&'static str>,
    pub(super) model_year: Option<i32>,
    pub(super) trim_badging: Option<String>,
    pub(super) exterior_color: Option<String>,
    pub(super) wheel_type: Option<String>,
    pub(super) spoiler_type: Option<String>,
}

/// The descriptor (C6 R40). The VIN is the stored car's, else the newest root
/// `vin` statement. The model text is the newest `vehicle_config.car_type`
/// statement when one exists (a value that is not a string is no text), else
/// the stored car's model. Each configuration text is the newest
/// `vehicle_config` statement through `live_config_text`, and only when no
/// statement exists the stored car's value under the text rule.
pub(super) fn descriptor(fields: &Fields<'_, '_>, car: Option<&ProjectionCar>) -> Descriptor {
    let persistent = Class::Persistent;
    let vin = car
        .and_then(|car| car.vin.as_deref())
        .filter(|vin| !vin.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| fields.text(persistent, "root.vin"));
    let model_text = match fields.stated(persistent, "vehicle_config.car_type") {
        Some(statement) => statement.value.as_str().map(str::to_owned),
        None => car.map(|car| car.model.clone()),
    };
    let model_code = vehicle_model_code(vin.as_deref(), model_text.as_deref())
        .filter(|code| code.chars().count() <= K_TEXT_MAX);
    let configuration = |key: &str, stored: Option<&str>| match fields.stated(persistent, key) {
        Some(statement) => live_config_text(statement.value.as_str()),
        None => stored.and_then(text_str),
    };
    Descriptor {
        model_name: model_code.as_deref().and_then(vehicle_model_name),
        model_year: vehicle_model_year(vin.as_deref()),
        model_code,
        trim_badging: configuration(
            "vehicle_config.trim_badging",
            car.and_then(|car| car.trim_badging.as_deref()),
        ),
        exterior_color: configuration(
            "vehicle_config.exterior_color",
            car.and_then(|car| car.exterior_color.as_deref()),
        ),
        wheel_type: configuration(
            "vehicle_config.wheel_type",
            car.and_then(|car| car.wheel_type.as_deref()),
        ),
        spoiler_type: configuration(
            "vehicle_config.spoiler_type",
            car.and_then(|car| car.spoiler_type.as_deref()),
        ),
    }
}

/// The settings of the stored car under their served names (C6 R49).
fn settings_view(settings: &ProjectionCarSettings) -> CurrentCarSettingsView {
    CurrentCarSettingsView {
        collection_enabled: settings.enabled,
        streaming_enabled: settings.use_streaming_api,
        sleep_window_min: settings.suspend_min,
        sleep_after_idle_min: settings.suspend_after_idle_min,
        sleep_window_min_resolved: settings.suspend_min_resolved,
        sleep_requires_locked: settings.req_not_unlocked,
        supercharging_free: settings.free_supercharging,
        battery_lfp: settings.lfp_battery,
    }
}

/// The `car` object (C6 R40, R48): `None` without a stored car, otherwise built
/// from the stored `ProjectionCar` only. `model_code` is recomputed from the
/// stored VIN and model (so a row written by the replaced code is normalised),
/// `model_name` and `model_year` follow from it, and the other descriptor
/// fields are as stored. A text value longer than [`K_TEXT_MAX`] is null in the
/// view; the two text values that are not nullable, `name` and the stored model
/// text served as `model`, read as the empty string then.
pub(super) fn car_view(car: Option<&ProjectionCar>) -> Option<CurrentCarView> {
    let car = car?;
    let vin = car.vin.as_deref();
    let model_code =
        vehicle_model_code(vin, Some(&car.model)).filter(|code| code.chars().count() <= K_TEXT_MAX);
    Some(CurrentCarView {
        id: car.id,
        name: bounded(Some(&car.name)).unwrap_or_default(),
        vin: bounded(vin),
        model_name: model_code.as_deref().and_then(vehicle_model_name),
        model_year: vehicle_model_year(vin),
        model_code,
        stored_model_text: bounded(Some(&car.model)).unwrap_or_default(),
        trim_badging: bounded(car.trim_badging.as_deref()),
        variant_name: bounded(car.marketing_name.as_deref()),
        exterior_color: bounded(car.exterior_color.as_deref()),
        wheel_type: bounded(car.wheel_type.as_deref()),
        spoiler_type: bounded(car.spoiler_type.as_deref()),
        firmware_version: bounded(car.firmware_version.as_deref()),
        api_vehicle_id: car.source_eid,
        stream_vehicle_id: car.source_vid,
        efficiency_wh_per_km: car.efficiency_wh_per_km,
        settings: settings_view(&car.settings),
    })
}
