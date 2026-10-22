// SPDX-License-Identifier: AGPL-3.0-only

//! C6 Part S2 spec tests (synthetic data only): doors, windows and trunks,
//! charging state and cable, software version and update, service mode,
//! display name, descriptor, `car` view and geofence. Test ids carry their
//! unit prefix (C6 R65). The fixtures are those of `b3_c6_s1_tests.rs`; the
//! S2 blocks of the tests marked † in C6 section 8 (C6 ST-S1, C6 ST-S2, C6
//! ST-S7, C6 ST-S8 and C6 ST-S9) are asserted there, beside the S1 fields they
//! extend.

use serde_json::{Value, json};

use super::{
    CurrentCarSettingsView, CurrentVehicleSummary,
    b3_c6_s1::{
        T0, V3, discovery, fd, fd_sparse, l_on, lifecycle_record, open_drive, p0, session, stream,
        vd, vehicle, with, without,
    },
    build_current_vehicle_summary,
};
use crate::{
    db::{LifecycleStateRecord, ObservationRecord},
    hub_pack::ProjectionCar,
};

/// The default `transient_max_age_ms` of the spec tests (C6 section 8).
const T: i64 = 600_000;
const VS: &str = "5YJSA1E20HFTEST02";
/// A VIN of a Tesla WMI that is not filed (C6 R3): only its model letter reads.
const VL: &str = "LRW3E7EK0MCTEST07";

/// The stored car of C6 ST-S12 (`id` 1, name "Stored", model "3", VIN `V3`, trim
/// "74d", no firmware), with `extra` keys set over it. Built through serde, so
/// the fixture does not name the fields a later part adds to the type.
fn car(extra: Value) -> ProjectionCar {
    let mut stored = json!({
        "id": 1,
        "name": "Stored",
        "model": "3",
        "vin": V3,
        "trim_badging": "74d",
        "firmware_version": null,
        "efficiency_wh_per_km": null
    });
    for (key, value) in extra.as_object().expect("extra keys") {
        stored[key] = value.clone();
    }
    serde_json::from_value(stored).expect("stored car")
}

/// The summary at `now` = the newest row's `observed_at_ms` and `T` = 600,000,
/// with a stored car.
fn summary_with(
    rows: &[ObservationRecord],
    lifecycle: Option<&LifecycleStateRecord>,
    car: Option<ProjectionCar>,
) -> CurrentVehicleSummary {
    let now = rows
        .iter()
        .map(|row| row.observed_at_ms)
        .max()
        .unwrap_or(T0);
    build_current_vehicle_summary(vehicle(), rows, car, lifecycle, None, now, T)
}

/// C6 ST-S29's "awake, fresh" setting: one vehicle-data row stating only `group`
/// as `members`, no lifecycle and no stored car.
fn stated(group: &str, members: Value) -> CurrentVehicleSummary {
    summary_with(&[vd(1, T0, json!({ group: members }))], None, None)
}

fn vehicle_state(members: Value) -> CurrentVehicleSummary {
    stated("vehicle_state", members)
}

fn charge_state(members: Value) -> CurrentVehicleSummary {
    stated("charge_state", members)
}

fn software_update(members: Value) -> CurrentVehicleSummary {
    vehicle_state(json!({ "software_update": members }))
}

/// The top-level descriptor of `s` reads as `code`, `name` and `year`.
fn identity(s: &CurrentVehicleSummary, code: Option<&str>, name: Option<&str>, year: Option<i32>) {
    assert_eq!(s.model_code.as_deref(), code, "model_code");
    assert_eq!(s.model_name, name, "model_name");
    assert_eq!(s.model_year, year, "model_year");
}

/// The keys of a JSON object, as the map yields them (serde_json's map keeps them
/// sorted, so this pins the key set, not the order the serialiser wrote).
fn keys(value: &Value) -> Vec<&str> {
    value
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect()
}

type Element = fn(&CurrentVehicleSummary) -> Option<bool>;

#[test]
fn c6_st_s13_to_s15_openings() {
    // C6 ST-S13: doors. Known true wins, all four known false is false, a
    // partly unknown set is null.
    let s = vehicle_state(json!({"df": 0, "dr": 0, "pf": 0}));
    assert_eq!(s.door_driver_front_open, Some(false));
    assert_eq!(s.door_driver_rear_open, Some(false));
    assert_eq!(s.door_passenger_front_open, Some(false));
    assert_eq!(s.door_passenger_rear_open, None);
    assert_eq!(s.any_door_open, None);
    let s = vehicle_state(json!({"df": 0, "dr": 0, "pf": 0, "pr": 1}));
    assert_eq!(s.door_passenger_rear_open, Some(true));
    assert_eq!(s.any_door_open, Some(true));
    let s = vehicle_state(json!({"df": 0, "dr": null, "pf": 2}));
    assert_eq!(s.door_driver_rear_open, None);
    assert_eq!(s.any_door_open, Some(true));
    let s = vehicle_state(json!({"df": 0, "dr": 0, "pf": 0, "pr": 0}));
    assert_eq!(s.any_door_open, Some(false));
    // Each Tesla key reaches its own field, and only that one.
    let doors: [(&str, Element); 4] = [
        ("df", |s| s.door_driver_front_open),
        ("dr", |s| s.door_driver_rear_open),
        ("pf", |s| s.door_passenger_front_open),
        ("pr", |s| s.door_passenger_rear_open),
    ];
    for (key, _) in doors {
        let mut members = json!({"df": 0, "dr": 0, "pf": 0, "pr": 0});
        members[key] = json!(1);
        let s = vehicle_state(members);
        for (other, field) in doors {
            assert_eq!(field(&s), Some(other == key), "door {key} read as {other}");
        }
        assert_eq!(s.any_door_open, Some(true));
    }
    // The value forms of an element.
    let forms = [
        (json!("open"), None),
        (json!("1"), Some(true)),
        (json!("0"), Some(false)),
        (json!(-1), Some(true)),
        (json!(0.5), None),
        (json!("0.5"), None),
        (json!(true), Some(true)),
        (json!(false), Some(false)),
        (json!(null), None),
        (json!([1]), None),
    ];
    for (value, expected) in forms {
        let s = vehicle_state(json!({ "df": value.clone() }));
        assert_eq!(s.door_driver_front_open, expected, "df {value}");
    }
    // The newest statement wins, and a door is a class P field: it is shown
    // while the car sleeps.
    let rows = [
        vd(1, T0, with(p0(), "vehicle_state", "df", json!(1))),
        fd(2, T0 + 60_000, with(p0(), "vehicle_state", "df", json!(0))),
    ];
    assert_eq!(
        summary_with(&rows, None, None).door_driver_front_open,
        Some(false)
    );
    let rows = [
        vd(1, T0, with(p0(), "vehicle_state", "df", json!(1))),
        discovery(2, T0 + 7_200_000, "asleep"),
    ];
    let asleep = lifecycle_record(&session("asleep", Some(("asleep", T0 + 7_200_000))));
    let s = summary_with(&rows, Some(&asleep), None);
    assert_eq!(s.door_driver_front_open, Some(true));
    assert_eq!(s.any_door_open, Some(true));

    // C6 ST-S14: windows. Each Tesla key reaches its own field.
    let s = vehicle_state(json!({"fd_window": 0, "fp_window": 1, "rd_window": 0, "rp_window": 0}));
    assert_eq!(s.window_passenger_front_open, Some(true));
    assert_eq!(s.any_window_open, Some(true));
    let windows: [(&str, Element); 4] = [
        ("fd_window", |s| s.window_driver_front_open),
        ("fp_window", |s| s.window_passenger_front_open),
        ("rd_window", |s| s.window_driver_rear_open),
        ("rp_window", |s| s.window_passenger_rear_open),
    ];
    for (key, _) in windows {
        let mut members = json!({"fd_window": 0, "fp_window": 0, "rd_window": 0, "rp_window": 0});
        members[key] = json!(1);
        let s = vehicle_state(members);
        for (other, field) in windows {
            assert_eq!(
                field(&s),
                Some(other == key),
                "window {key} read as {other}"
            );
        }
        assert_eq!(s.any_window_open, Some(true));
    }
    // Fleet Telemetry's window names: the prefix is optional, the case is not
    // significant, anything but `Closed` and `Unknown` is open.
    let names = [
        ("WindowStateClosed", Some(false)),
        ("Closed", Some(false)),
        ("closed", Some(false)),
        ("PartiallyOpen", Some(true)),
        ("partiallyopen", Some(true)),
        ("WindowStatePartiallyOpen", Some(true)),
        ("Opened", Some(true)),
        ("WindowStateOpened", Some(true)),
        ("WindowStateUnknown", None),
        ("Unknown", None),
        ("ajar", None),
        ("WindowState", None),
        ("1", Some(true)),
        ("0", Some(false)),
        ("1.5", None),
    ];
    for (name, expected) in names {
        let s = vehicle_state(json!({ "fd_window": name }));
        assert_eq!(s.window_driver_front_open, expected, "fd_window {name}");
    }
    assert_eq!(
        vehicle_state(json!({"fd_window": 1.5})).window_driver_front_open,
        None
    );
    // The window aggregate is false only when all four are known closed.
    let s = vehicle_state(json!({
        "fd_window": "Closed", "fp_window": "WindowStateClosed", "rd_window": 0,
        "rp_window": false
    }));
    assert_eq!(s.any_window_open, Some(false));
    let s = vehicle_state(json!({
        "fd_window": "Closed", "fp_window": "Closed", "rd_window": "Closed",
        "rp_window": "WindowStateUnknown"
    }));
    assert_eq!(s.any_window_open, None);
    // A door does not read a window's names.
    assert_eq!(
        vehicle_state(json!({"df": "Closed"})).door_driver_front_open,
        None
    );

    // C6 ST-S15: trunks. Their keys reach their own fields and are not in the
    // door aggregate.
    let s = vehicle_state(json!({"ft": 0, "rt": 2}));
    assert_eq!(s.front_trunk_open, Some(false));
    assert_eq!(s.rear_trunk_open, Some(true));
    let s = vehicle_state(json!({"ft": 1, "rt": 0}));
    assert_eq!(s.front_trunk_open, Some(true));
    assert_eq!(s.rear_trunk_open, Some(false));
    assert_eq!(vehicle_state(json!({"rt": -1})).rear_trunk_open, Some(true));
    assert_eq!(vehicle_state(json!({"ft": 1.5})).front_trunk_open, None);
    let s = vehicle_state(json!({"df": 0, "dr": 0, "pf": 0, "pr": 0, "rt": 2}));
    assert_eq!(s.rear_trunk_open, Some(true));
    assert_eq!(s.any_door_open, Some(false));
    // A stream-only car states none of them.
    let rows = [stream(1, T0, |update| update.soc = Some(60.0))];
    let s = summary_with(&rows, None, None);
    assert_eq!(s.front_trunk_open, None);
    assert_eq!(s.any_window_open, None);
}

#[test]
fn c6_st_s16_to_s19_composites() {
    // C6 ST-S16: the charging state and the cable. Canonical names, with the
    // cable from the state.
    for (state, cable) in [
        ("Disconnected", false),
        ("Charging", true),
        ("Starting", true),
        ("Stopped", true),
        ("Complete", true),
        ("NoPower", true),
    ] {
        let s = charge_state(json!({ "charging_state": state }));
        assert_eq!(s.charging_state.as_deref(), Some(state));
        assert_eq!(s.charge_cable_connected, Some(cable), "{state}");
    }
    // Fleet Telemetry spellings and case read as the canonical name.
    for (raw, canonical) in [
        ("ChargeStateCharging", "Charging"),
        ("charging", "Charging"),
        ("DetailedChargeStateStopped", "Stopped"),
        ("  NOPOWER ", "NoPower"),
        ("ChargeStateDisconnected", "Disconnected"),
    ] {
        let s = charge_state(json!({ "charging_state": raw }));
        assert_eq!(s.charging_state.as_deref(), Some(canonical), "{raw}");
    }
    // `Unknown` is no state; the cable then comes from its own text.
    for (state, cable_text, cable) in [
        ("Unknown", "IEC", Some(true)),
        ("Unknown", "<invalid>", Some(false)),
        ("ChargeStateUnknown", "IEC", Some(true)),
        ("unknown", "SAE", Some(true)),
    ] {
        let s = charge_state(json!({
            "charging_state": state, "conn_charge_cable": cable_text
        }));
        assert_eq!(s.charging_state, None, "{state}");
        assert_eq!(s.charge_cable_connected, cable, "{state} with {cable_text}");
    }
    // Neither stated: both null. A null state is no state either.
    let s = charge_state(json!({"battery_level": 80}));
    assert_eq!((s.charging_state, s.charge_cable_connected), (None, None));
    let s = charge_state(json!({"charging_state": null, "conn_charge_cable": "IEC"}));
    assert_eq!(
        (s.charging_state, s.charge_cable_connected),
        (None, Some(true))
    );
    let s = charge_state(json!({"conn_charge_cable": "<invalid>"}));
    assert_eq!(
        (s.charging_state, s.charge_cable_connected),
        (None, Some(false))
    );
    let s = charge_state(json!({"conn_charge_cable": " "}));
    assert_eq!(s.charge_cable_connected, None);
    // The cable text is trimmed before the R35 comparison.
    let s = charge_state(json!({"conn_charge_cable": " <invalid> "}));
    assert_eq!(s.charge_cable_connected, Some(false));
    // An unrecognised name is passed through trimmed, and the cable is read
    // from its own text.
    for (cable_text, cable) in [
        (Some("IEC"), Some(true)),
        (Some("<invalid>"), Some(false)),
        (None, None),
    ] {
        let mut members = json!({"charging_state": " Balancing "});
        if let Some(text) = cable_text {
            members["conn_charge_cable"] = json!(text);
        }
        let s = charge_state(members);
        assert_eq!(s.charging_state.as_deref(), Some("Balancing"));
        assert_eq!(s.charge_cable_connected, cable);
    }
    // A shift gear is no charging state: it passes through as an unrecognised
    // text, and names no cable.
    let s = charge_state(json!({"charging_state": "D"}));
    assert_eq!(s.charging_state.as_deref(), Some("D"));
    assert_eq!(s.charge_cable_connected, None);
    // The newest statement wins even when it names no state.
    let rows = [
        vd(
            1,
            T0,
            with(p0(), "charge_state", "charging_state", json!("Charging")),
        ),
        fd(
            2,
            T0 + 60_000,
            json!({"charge_state": {"charging_state": "Unknown"}}),
        ),
    ];
    let s = summary_with(&rows, None, None);
    assert_eq!(s.charging_state, None);
    // The newer row names no state, so the cable text decides; the only cable
    // text is the older row's `"<invalid>"`.
    assert_eq!(s.charge_cable_connected, Some(false));
    // A sleeping car is not charging, whatever the last state said; the plug
    // and end states persist. An unreachable one may still be charging.
    let asleep = lifecycle_record(&session("asleep", Some(("asleep", T0 + 7_200_000))));
    let offline = lifecycle_record(&session("offline", Some(("offline", T0 + 7_200_000))));
    for (state, when_asleep, when_offline) in [
        ("Charging", None, Some("Charging")),
        ("Starting", None, Some("Starting")),
        ("Stopped", Some("Stopped"), Some("Stopped")),
        ("Complete", Some("Complete"), Some("Complete")),
        ("NoPower", Some("NoPower"), Some("NoPower")),
        ("Disconnected", Some("Disconnected"), Some("Disconnected")),
    ] {
        let rows = [
            vd(
                1,
                T0,
                with(p0(), "charge_state", "charging_state", json!(state)),
            ),
            discovery(2, T0 + 7_200_000, "asleep"),
        ];
        let s = summary_with(&rows, Some(&asleep), None);
        assert_eq!(s.charging_state.as_deref(), when_asleep, "{state} asleep");
        assert_eq!(s.charge_cable_connected, Some(state != "Disconnected"));
        let s = summary_with(&rows, Some(&offline), None);
        assert_eq!(s.charging_state.as_deref(), when_offline, "{state} offline");
    }

    // C6 ST-S17: the update fields.
    let s = software_update(json!({"status": "available", "version": "2024.20.1 cc"}));
    assert_eq!(s.software_update_status.as_deref(), Some("available"));
    assert_eq!(s.software_update_available, Some(true));
    assert_eq!(s.software_update_version.as_deref(), Some("2024.20.1"));
    for status in [
        "installing",
        "downloading_wifi_wait",
        "scheduled",
        "downloading",
        "wifi_wait",
    ] {
        let s = software_update(json!({ "status": status, "version": "2024.20.1" }));
        assert_eq!(s.software_update_available, Some(true), "{status}");
        assert_eq!(s.software_update_status.as_deref(), Some(status));
        assert_eq!(s.software_update_version.as_deref(), Some("2024.20.1"));
    }
    // Idle: no status, not available, and no version without an offer.
    let s = software_update(json!({"status": "", "version": "2024.20.1 cc"}));
    assert_eq!(s.software_update_status, None);
    assert_eq!(s.software_update_available, Some(false));
    assert_eq!(s.software_update_version, None);
    let s = software_update(json!({"status": "   "}));
    assert_eq!(s.software_update_status, None);
    assert_eq!(s.software_update_available, Some(false));
    // Trimmed and verbatim, matched without regard to case.
    let s = software_update(json!({"status": " Available ", "version": "2024.14"}));
    assert_eq!(s.software_update_status.as_deref(), Some("Available"));
    assert_eq!(s.software_update_available, Some(true));
    assert_eq!(s.software_update_version.as_deref(), Some("2024.14"));
    // An unknown status is shown and gives no availability, however long.
    let s = software_update(json!({"status": "weird", "version": "2024.20.1"}));
    assert_eq!(s.software_update_status.as_deref(), Some("weird"));
    assert_eq!(s.software_update_available, None);
    assert_eq!(s.software_update_version, None);
    let long = "x".repeat(300);
    let s = software_update(json!({ "status": long.clone() }));
    assert_eq!(s.software_update_status.as_deref(), Some(long.as_str()));
    assert_eq!(s.software_update_available, None);
    // An offered version that is no version is not served.
    let s = software_update(json!({"status": "available", "version": "develop"}));
    assert_eq!(s.software_update_available, Some(true));
    assert_eq!(s.software_update_version, None);
    // Absent, null and non-string statuses and a missing object give nothing.
    for members in [
        json!({"download_perc": 1}),
        json!({"status": null, "version": "2024.20.1"}),
        json!({"status": 5, "version": "2024.20.1"}),
    ] {
        let s = software_update(members.clone());
        assert_eq!(
            (
                s.software_update_status,
                s.software_update_available,
                s.software_update_version
            ),
            (None, None, None),
            "{members}"
        );
    }
    let s = vehicle_state(json!({"locked": true}));
    assert_eq!(
        (
            s.software_update_status,
            s.software_update_available,
            s.software_update_version
        ),
        (None, None, None)
    );

    // C6 ST-S18: the version sources, first usable wins.
    let lifecycle_with = |version: &str| {
        let mut state = session("online", Some(("online", T0 - 3_600_000)));
        state.last_car_version = Some(version.to_owned());
        lifecycle_record(&state)
    };
    let rows = [vd(1, T0, p0())];
    let lifecycle = lifecycle_with("2024.20.1 bb");
    let s = summary_with(&rows, Some(&lifecycle), None);
    assert_eq!(s.software_version.as_deref(), Some("2024.20.1"));
    let s = summary_with(&rows, None, None);
    assert_eq!(s.software_version.as_deref(), Some("2024.14.3"));
    // An unusable lifecycle value is skipped.
    let lifecycle = lifecycle_with("dev build");
    let s = summary_with(&rows, Some(&lifecycle), None);
    assert_eq!(s.software_version.as_deref(), Some("2024.14.3"));
    // The stored firmware is the last source.
    let firmware = || car(json!({"firmware_version": "2023.44.30.8 x"}));
    let develop = [vd(
        1,
        T0,
        with(p0(), "vehicle_state", "car_version", json!("develop")),
    )];
    let s = summary_with(&develop, None, Some(firmware()));
    assert_eq!(s.software_version.as_deref(), Some("2023.44.30.8"));
    let s = summary_with(&develop, None, None);
    assert_eq!(s.software_version, None);
    let s = summary_with(&[], None, Some(firmware()));
    assert_eq!(s.software_version.as_deref(), Some("2023.44.30.8"));
    // A sparse source's version is the available one, not the installed one.
    for feed in ["fleet_telemetry", "provider_stream"] {
        let mut row = fd_sparse(1, T0, json!({"vehicle_state": {"car_version": "2025.2.6"}}));
        row.payload["provider_feed"] = json!(feed);
        let s = summary_with(&[row.clone()], None, None);
        assert_eq!(s.software_version, None, "{feed}");
        let s = summary_with(&[row], None, Some(firmware()));
        assert_eq!(
            s.software_version.as_deref(),
            Some("2023.44.30.8"),
            "{feed}"
        );
    }
    let rows = [
        vd(
            1,
            T0,
            with(p0(), "vehicle_state", "car_version", json!("2024.14.3 a")),
        ),
        fd_sparse(
            2,
            T0 + 60_000,
            json!({"vehicle_state": {"car_version": "2025.2.6"}}),
        ),
    ];
    let s = summary_with(&rows, None, None);
    assert_eq!(s.software_version.as_deref(), Some("2024.14.3"));
    // A stream row carries no version, and the newest full statement is the
    // source: an unusable newest one is not replaced by an older usable one.
    let rows = [
        vd(
            1,
            T0,
            with(p0(), "vehicle_state", "car_version", json!("2024.14.3")),
        ),
        fd(
            2,
            T0 + 60_000,
            json!({"vehicle_state": {"car_version": "develop"}}),
        ),
    ];
    assert_eq!(summary_with(&rows, None, None).software_version, None);
    // The grammar: at least two groups of one to nine digits, at most 255
    // characters, the token before the first whitespace.
    for (raw, expected) in [
        ("2024.14", Some("2024.14")),
        ("2024.14.3 aa1bb2", Some("2024.14.3")),
        ("  2025.2.6\tx  ", Some("2025.2.6")),
        ("123456789.1", Some("123456789.1")),
        ("2024", None),
        ("2024.x", None),
        ("2024..3", None),
        ("1234567890.1", None),
        ("", None),
    ] {
        let s = software_update(json!({"status": "available", "version": raw}));
        assert_eq!(s.software_update_version.as_deref(), expected, "{raw:?}");
        let rows = [vd(1, T0, json!({"vehicle_state": {"car_version": raw}}))];
        let s = summary_with(&rows, None, None);
        assert_eq!(s.software_version.as_deref(), expected, "{raw:?}");
    }
    let long_version = format!("2024.14.3{}", " pad".repeat(80));
    assert!(long_version.chars().count() > 255);
    let rows = [vd(
        1,
        T0,
        json!({"vehicle_state": {"car_version": long_version}}),
    )];
    assert_eq!(summary_with(&rows, None, None).software_version, None);
    let at_limit = format!("2024.14.3 {}", "p".repeat(245));
    assert_eq!(at_limit.chars().count(), 255);
    let rows = [vd(
        1,
        T0,
        json!({"vehicle_state": {"car_version": at_limit}}),
    )];
    assert_eq!(
        summary_with(&rows, None, None).software_version.as_deref(),
        Some("2024.14.3")
    );

    // C6 ST-S19: the service flag. The lifecycle's flag wins when it has one.
    let lifecycle_service = |flag: Option<bool>| {
        let mut state = session("online", Some(("online", T0 - 3_600_000)));
        state.service_mode = flag;
        lifecycle_record(&state)
    };
    let stated_false = [vd(1, T0, p0())];
    let lifecycle = lifecycle_service(Some(true));
    let s = summary_with(&stated_false, Some(&lifecycle), None);
    assert_eq!(s.service_mode, Some(true));
    let stated_true = [vd(
        1,
        T0,
        with(p0(), "vehicle_state", "service_mode", json!(true)),
    )];
    let lifecycle = lifecycle_service(Some(false));
    let s = summary_with(&stated_true, Some(&lifecycle), None);
    assert_eq!(s.service_mode, Some(false));
    let lifecycle = lifecycle_service(None);
    assert_eq!(
        summary_with(&stated_true, Some(&lifecycle), None).service_mode,
        Some(true)
    );
    assert_eq!(
        summary_with(&stated_true, None, None).service_mode,
        Some(true)
    );
    let neither = [vd(1, T0, without(p0(), "vehicle_state", "service_mode"))];
    assert_eq!(summary_with(&neither, None, None).service_mode, None);
    let lifecycle = lifecycle_service(None);
    assert_eq!(
        summary_with(&neither, Some(&lifecycle), None).service_mode,
        None
    );
    // Only a boolean is a statement of it.
    let rows = [vd(
        1,
        T0,
        with(p0(), "vehicle_state", "service_mode", json!("yes")),
    )];
    assert_eq!(summary_with(&rows, None, None).service_mode, None);
}

#[test]
fn c6_st_s12_telemetry_only() {
    // C6 ST-S12: a stream-only car. The fields the stream does not carry are
    // null; the descriptor comes from the stored car.
    let rows = [stream(1, T0, |update| {
        update.speed = Some(30.0);
        update.shift_state = Some("D".to_owned());
        update.soc = Some(60.0);
        update.range = Some(100.0);
    })];
    let mut state = session("driving", Some(("online", T0 - 60_000)));
    state.open_drive = Some(open_drive(T0));
    let lifecycle = lifecycle_record(&state);
    let s = summary_with(&rows, Some(&lifecycle), Some(car(json!({}))));
    assert_eq!(s.battery_level, Some(60));
    assert_eq!(s.ideal_battery_range_km, Some(160.93));
    assert_eq!(s.est_battery_range_km, None);
    assert_eq!(s.locked, None);
    assert_eq!(s.model_code.as_deref(), Some("3"));
    assert_eq!(s.model_name, Some("Model 3"));
    assert_eq!(s.model_year, Some(2019));
    assert_eq!(s.trim_badging.as_deref(), Some("74d"));
    assert_eq!(s.any_door_open, None);
    assert_eq!(s.any_window_open, None);
    assert_eq!(s.charging_state, None);
    assert_eq!(s.charge_cable_connected, None);
    assert_eq!(s.software_version, None);
    assert_eq!(s.software_update_available, None);
    assert_eq!(s.service_mode, None);
    assert_eq!(s.display_name.as_deref(), Some("Stored"));
    let view = s.car.as_ref().expect("stored car view");
    assert_eq!(view.model_code.as_deref(), Some("3"));

    // The `car` view is built from the stored car only (C6 R40, R48): the
    // renamed names, the identity facts of the code and the VIN, and every
    // other value as stored.
    let stored = car(json!({
        "source_eid": 11, "source_vid": 22, "marketing_name": "Long Range",
        "exterior_color": "  Red ", "wheel_type": "Pinwheel18", "spoiler_type": "None",
        "firmware_version": "2024.14.3 aa1bb2", "efficiency_wh_per_km": 151.5
    }));
    let s = summary_with(&[], None, Some(stored));
    let view = s.car.as_ref().expect("stored car view");
    assert_eq!(view.id, 1);
    assert_eq!(view.name, "Stored");
    assert_eq!(view.vin.as_deref(), Some(V3));
    assert_eq!(view.model_code.as_deref(), Some("3"));
    assert_eq!(view.model_name, Some("Model 3"));
    assert_eq!(view.model_year, Some(2019));
    assert_eq!(view.stored_model_text, "3");
    assert_eq!(view.trim_badging.as_deref(), Some("74d"));
    assert_eq!(view.variant_name.as_deref(), Some("Long Range"));
    assert_eq!(view.exterior_color.as_deref(), Some("  Red "));
    assert_eq!(view.wheel_type.as_deref(), Some("Pinwheel18"));
    assert_eq!(view.spoiler_type.as_deref(), Some("None"));
    assert_eq!(view.firmware_version.as_deref(), Some("2024.14.3 aa1bb2"));
    assert_eq!(view.api_vehicle_id, Some(11));
    assert_eq!(view.stream_vehicle_id, Some(22));
    assert_eq!(view.efficiency_wh_per_km, Some(151.5));
    // The top level reads the stored value under the text rule, the view as
    // stored; the stored firmware is also the version when no other source is.
    assert_eq!(s.exterior_color.as_deref(), Some("Red"));
    assert_eq!(s.software_version.as_deref(), Some("2024.14.3"));
    // No stored car, no view.
    assert!(summary_with(&[], None, None).car.is_none());

    // The served car (C6 R13a, R50): until C6-C1 writes the aliased serialiser
    // it is the 1.2.0 shape, whose `model` is the stored model text. The text an
    // import kept in `source_model_text` is not served, at any depth.
    let imported = car(json!({
        "source_eid": 11, "source_vid": 22, "marketing_name": "Long Range",
        "source_model_text": "Model 3 Long Range"
    }));
    assert_eq!(
        imported.source_model_text.as_deref(),
        Some("Model 3 Long Range")
    );
    let s = summary_with(&[], None, Some(imported));
    let served = serde_json::to_value(&s).expect("summary JSON");
    assert_eq!(
        keys(&served["car"]),
        [
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
            "wheel_type"
        ]
    );
    assert_eq!(
        keys(&served["car"]["settings"]),
        [
            "enabled",
            "free_supercharging",
            "lfp_battery",
            "req_not_unlocked",
            "suspend_after_idle_min",
            "suspend_min",
            "suspend_min_resolved",
            "use_streaming_api"
        ]
    );
    assert_eq!(served["car"]["model"], "3");
    assert_eq!(served["car"]["marketing_name"], "Long Range");
    assert_eq!(served["car"]["source_eid"], 11);
    assert_eq!(served["car"]["source_vid"], 22);
    assert!(!served.to_string().contains("source_model_text"));
    assert!(!served.to_string().contains("Model 3 Long Range"));

    // The settings of the view carry the stored settings under their served
    // names: each stored setting reaches its own field and no other.
    let minutes = car(json!({"settings": {"suspend_min": 34, "suspend_after_idle_min": 12}}));
    let s = summary_with(&[], None, Some(minutes));
    let settings = &s.car.as_ref().expect("view").settings;
    assert_eq!(settings.sleep_window_min, 34);
    assert_eq!(settings.sleep_after_idle_min, 12);
    type Setting = fn(&CurrentCarSettingsView) -> bool;
    let flags: [(&str, Setting); 6] = [
        ("enabled", |s| s.collection_enabled),
        ("use_streaming_api", |s| s.streaming_enabled),
        ("suspend_min_resolved", |s| s.sleep_window_min_resolved),
        ("req_not_unlocked", |s| s.sleep_requires_locked),
        ("free_supercharging", |s| s.supercharging_free),
        ("lfp_battery", |s| s.battery_lfp),
    ];
    for (key, _) in flags {
        let mut settings = json!({
            "enabled": false, "use_streaming_api": false, "suspend_min_resolved": false,
            "req_not_unlocked": false, "free_supercharging": false, "lfp_battery": false
        });
        settings[key] = json!(true);
        let s = summary_with(&[], None, Some(car(json!({ "settings": settings }))));
        let view = &s.car.as_ref().expect("view").settings;
        for (other, field) in flags {
            assert_eq!(field(view), other == key, "setting {key} read as {other}");
        }
    }

    // The live configuration is newer than the stored car, and the view keeps
    // reporting the stored one (C6 R40). A statement that reads as null is not
    // replaced by the stored value; no statement is.
    let live = json!({"vehicle_config": {"trim_badging": "  p74d ", "wheel_type": null}});
    let stored = car(json!({"exterior_color": "  Red ", "wheel_type": "Pinwheel18"}));
    let s = summary_with(&[vd(1, T0, live)], None, Some(stored));
    assert_eq!(s.trim_badging.as_deref(), Some("p74d"));
    assert_eq!(s.wheel_type, None);
    assert_eq!(s.exterior_color.as_deref(), Some("Red"));
    let view = s.car.as_ref().expect("view");
    assert_eq!(view.trim_badging.as_deref(), Some("74d"));
    assert_eq!(view.wheel_type.as_deref(), Some("Pinwheel18"));
    assert_eq!(view.exterior_color.as_deref(), Some("  Red "));
    let blank = json!({"vehicle_config": {"trim_badging": "   "}});
    let s = summary_with(&[vd(1, T0, blank)], None, Some(car(json!({}))));
    assert_eq!(s.trim_badging, None);
    assert_eq!(
        s.car.as_ref().expect("view").trim_badging.as_deref(),
        Some("74d")
    );
    // The stored car's VIN decides over a statement of another VIN; without a
    // stored VIN the statement is the VIN.
    let mut other_vin = p0();
    other_vin["vin"] = json!(VS);
    let s = summary_with(&[vd(1, T0, other_vin.clone())], None, Some(car(json!({}))));
    assert_eq!(s.model_year, Some(2019));
    let s = summary_with(
        &[vd(1, T0, other_vin)],
        None,
        Some(car(json!({"vin": null}))),
    );
    assert_eq!(s.model_year, Some(2017));
    // A stored text longer than the contract's bound is null in both places.
    let long = "é".repeat(65_537);
    let exact = "é".repeat(65_536);
    let s = summary_with(&[], None, Some(car(json!({"wheel_type": long}))));
    assert_eq!(s.wheel_type, None);
    assert_eq!(s.car.as_ref().expect("view").wheel_type, None);
    let s = summary_with(&[], None, Some(car(json!({"wheel_type": exact.clone()}))));
    assert_eq!(s.wheel_type.as_deref(), Some(exact.as_str()));
    assert_eq!(
        s.car.as_ref().expect("view").wheel_type.as_deref(),
        Some(exact.as_str())
    );

    // The top-level code is `vehicle_model_code(VIN, text)` (C6 R40, R6) with
    // the VIN the stored car's, else the newest root `vin` statement, and the
    // text the newest `vehicle_config.car_type` statement, else the stored
    // car's model. Each case names the one input that decides it.
    // (a) No stored car: the root VIN statement of a not-filed WMI reads as
    // the VIN, and a recognised `car_type` text wins over its model letter.
    let row = vd(
        1,
        T0,
        json!({"vin": VL, "vehicle_config": {"car_type": "modely"}}),
    );
    let s = summary_with(&[row], None, None);
    identity(&s, Some("Y"), Some("Model Y"), None);
    // Without a car_type statement or a stored car, the VIN's model letter decides.
    let s = summary_with(&[vd(1, T0, json!({"vin": VL}))], None, None);
    identity(&s, Some("3"), Some("Model 3"), None);
    // (b) A filed stored VIN beats the text of the statement; the year is its.
    let row = vd(1, T0, p0());
    let s = summary_with(&[row], None, Some(car(json!({"vin": VS}))));
    identity(&s, Some("S"), Some("Model S"), Some(2017));
    // (c) No VIN anywhere: the stored model text is the code, verbatim when
    // unrecognised, with no name and no year.
    let row = vd(1, T0, json!({"vehicle_state": {"locked": true}}));
    let stored = car(json!({"vin": null, "model": "lychee"}));
    let s = summary_with(&[row], None, Some(stored));
    identity(&s, Some("lychee"), None, None);
    // (d) The newest `car_type` statement is the text, not the stored model.
    let row = vd(1, T0, json!({"vehicle_config": {"car_type": "modelx"}}));
    let stored = car(json!({"vin": null, "model": "3"}));
    let s = summary_with(&[row], None, Some(stored.clone()));
    identity(&s, Some("X"), Some("Model X"), None);
    // A statement of null is a statement: it gives no text, and the stored
    // model does not stand in for it (the recorded reading of C6 R40).
    let row = vd(1, T0, json!({"vehicle_config": {"car_type": null}}));
    let s = summary_with(&[row], None, Some(stored));
    identity(&s, None, None, None);
    // (e) A blank stored VIN is no VIN: the root VIN statement is read, and a
    // filed one beats the stored model text.
    let row = vd(1, T0, json!({"vin": VS}));
    let stored = car(json!({"vin": "", "model": "3"}));
    let s = summary_with(&[row], None, Some(stored));
    identity(&s, Some("S"), Some("Model S"), Some(2017));
}

#[test]
fn c6_st_s37_name() {
    // C6 ST-S37: the display name. Root `display_name` first, then the
    // vehicle's own name, then the stored car's name.
    let named = |display_name: Value, vehicle_name: Value| {
        let mut response = with(p0(), "vehicle_state", "vehicle_name", vehicle_name);
        match display_name {
            Value::Null => {
                response
                    .as_object_mut()
                    .expect("response")
                    .remove("display_name");
            }
            value => response["display_name"] = value,
        }
        response
    };
    // Absent root name: the vehicle's own name.
    let s = summary_with(
        &[vd(1, T0, named(Value::Null, json!("Garage Car")))],
        None,
        None,
    );
    assert_eq!(s.display_name.as_deref(), Some("Garage Car"));
    // A blank root name gives null, so the vehicle's own name.
    let s = summary_with(
        &[vd(1, T0, named(json!("  "), json!("Garage Car")))],
        None,
        None,
    );
    assert_eq!(s.display_name.as_deref(), Some("Garage Car"));
    // Both stated: the root name, trimmed.
    let s = summary_with(
        &[vd(1, T0, named(json!(" Root "), json!("Garage Car")))],
        None,
        None,
    );
    assert_eq!(s.display_name.as_deref(), Some("Root"));
    // Both absent or blank: the stored car's name, then null.
    let mut response = named(Value::Null, Value::Null);
    response["vehicle_state"]
        .as_object_mut()
        .expect("vehicle state")
        .remove("vehicle_name");
    let rows = [vd(1, T0, response)];
    let s = summary_with(&rows, None, Some(car(json!({"name": "Stored"}))));
    assert_eq!(s.display_name.as_deref(), Some("Stored"));
    let s = summary_with(&rows, None, Some(car(json!({"name": ""}))));
    assert_eq!(s.display_name, None);
    let s = summary_with(&rows, None, Some(car(json!({"name": "   "}))));
    assert_eq!(s.display_name, None);
    let s = summary_with(&rows, None, None);
    assert_eq!(s.display_name, None);
    // A stored name is trimmed like any other, and a statement beats it.
    let s = summary_with(&rows, None, Some(car(json!({"name": " Stored "}))));
    assert_eq!(s.display_name.as_deref(), Some("Stored"));
    let s = summary_with(
        &[vd(1, T0, named(json!("Root"), Value::Null))],
        None,
        Some(car(json!({"name": "Stored"}))),
    );
    assert_eq!(s.display_name.as_deref(), Some("Root"));
    // No rows and a stored car: the car's name, never an invented observation.
    let s = summary_with(&[], None, Some(car(json!({}))));
    assert_eq!(s.display_name.as_deref(), Some("Stored"));
    assert_eq!(s.observed_at_ms, None);
    // The stored name is not read through the text length bound twice: a name
    // longer than the contract's bound is not served.
    let long = "é".repeat(65_537);
    let s = summary_with(&[], None, Some(car(json!({ "name": long }))));
    assert_eq!(s.display_name, None);
    assert_eq!(s.car.as_ref().expect("view").name, "");
}

#[test]
fn c6_st_s31_geofence() {
    // C6 ST-S31: the label is kept only for a known coordinate pair.
    let rows = [vd(1, T0, p0())];
    let lifecycle = l_on();
    let named = |rows: &[ObservationRecord], lifecycle: Option<&LifecycleStateRecord>| {
        build_current_vehicle_summary(
            vehicle(),
            rows,
            None,
            lifecycle,
            Some("Home".to_owned()),
            T0,
            T,
        )
    };
    let s = named(&rows, Some(&lifecycle));
    assert_eq!(s.geofence_name.as_deref(), Some("Home"));
    // The old key serves the same name. CARRY to C6-C1: the JSON half of C6
    // ST-S31 (a non-null label is written as both `geofence_name` and
    // `geofence`, each equal to "Home") is asserted by C6-C1 in
    // `c6_st_s30_aliases` on this fixture, with a mutant that writes
    // `geofence_name` as null; the C6 ST-S30 and C6 ST-S33 fixtures carry no label.
    let body = serde_json::to_value(&s).expect("summary JSON");
    assert_eq!(body["geofence"], "Home");
    // A position is class P: the label is also kept while the car sleeps.
    let asleep = lifecycle_record(&session("asleep", Some(("asleep", T0 + 7_200_000))));
    let rows_asleep = [vd(1, T0, p0()), discovery(2, T0 + 7_200_000, "asleep")];
    assert_eq!(
        named(&rows_asleep, Some(&asleep)).geofence_name.as_deref(),
        Some("Home")
    );
    // No coordinate pair, no name: a stream-only car without a position, an
    // invalid pair, one half, and a route-only pair.
    let no_position = [stream(1, T0, |update| update.soc = Some(60.0))];
    assert_eq!(named(&no_position, None).geofence_name, None);
    let invalid = [vd(
        1,
        T0,
        with(p0(), "drive_state", "latitude", json!(95.0)),
    )];
    let s = named(&invalid, Some(&lifecycle));
    assert_eq!(
        (s.latitude, s.longitude, s.geofence_name),
        (None, None, None)
    );
    let half = [vd(1, T0, json!({"drive_state": {"latitude": 10.5}}))];
    assert_eq!(named(&half, None).geofence_name, None);
    let route_only = [vd(
        1,
        T0,
        json!({"drive_state": {
            "active_route_latitude": 10.5, "active_route_longitude": 20.5
        }}),
    )];
    let s = named(&route_only, None);
    assert_eq!(s.active_route_latitude, Some(10.5));
    assert_eq!(s.geofence_name, None);
    // Without a label argument there is no name, whatever the position.
    let s = build_current_vehicle_summary(vehicle(), &rows, None, Some(&lifecycle), None, T0, T);
    assert_eq!(s.geofence_name, None);
    // Without rows: no position and no name.
    assert_eq!(named(&[], None).geofence_name, None);
}
