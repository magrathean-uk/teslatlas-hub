// SPDX-License-Identifier: AGPL-3.0-only

//! C6 Part S1 spec tests (synthetic data only): the summary engine, merge,
//! visibility, activity and configured time. Test ids carry their unit prefix
//! (C6 R65). The tests marked † in C6 section 8 assert here only the fields
//! that C6-S1 owns; C6-S2 extends them with its own fields.

use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    ActivityState, CurrentVehicleSummary, build_current_vehicle_summary, offline_drive_timeout_ms,
    tests::record,
};
use crate::{
    collection::teslamate_current::IMPORT_POSITION_RECORD_TYPE,
    db::{
        HubStore, LifecycleStateRecord, ObservationInput, ObservationRecord, SourceDescriptor,
        VehicleDescriptor,
    },
    lifecycle::{
        OpenCharge, OpenDrive, OpenSessionState, OpenState, OpenUpdate, VehiclePhase,
        stream_observation_payload,
    },
    tesla_stream::StreamUpdate,
};

const T0: i64 = 1_760_000_000_000;
/// The default `transient_max_age_ms` of the spec tests (C6 section 8).
const T: i64 = 600_000;
const V3: &str = "5YJ3E1EA0KFTEST01";
const GROUPS: [&str; 6] = [
    "drive_state",
    "charge_state",
    "climate_state",
    "vehicle_state",
    "vehicle_config",
    "gui_settings",
];

fn vehicle() -> Uuid {
    Uuid::from_u128(2)
}

/// `P0` (C6 section 8): a parked vehicle-data response.
fn p0() -> Value {
    json!({
        "display_name": "Test Car",
        "vin": V3,
        "drive_state": {
            "shift_state": null, "speed": null, "power": 0,
            "latitude": 10.1234567, "longitude": 20.7654321,
            "heading": 90, "elevation": 50.4
        },
        "charge_state": {
            "battery_level": 80, "usable_battery_level": 79,
            "battery_range": 200.0, "ideal_battery_range": 210.0, "est_battery_range": 150.5,
            "charge_limit_soc": 90, "charging_state": "Disconnected",
            "conn_charge_cable": "<invalid>", "charger_power": 0,
            "charge_energy_added": 12.346, "charge_port_door_open": false,
            "time_to_full_charge": 0.0
        },
        "climate_state": {"inside_temp": 21.25, "outside_temp": 10.04, "is_climate_on": false},
        "vehicle_state": {
            "odometer": 1000.0, "car_version": "2024.14.3 aa1bb2", "locked": true,
            "sentry_mode": false, "df": 0, "dr": 0, "pf": 0, "pr": 0, "ft": 0, "rt": 0,
            "fd_window": 0, "fp_window": 0, "rd_window": 0, "rp_window": 0,
            "service_mode": false, "tpms_pressure_fl": 2.875,
            "software_update": {
                "status": "", "version": "", "download_perc": 0, "install_perc": 1
            }
        },
        "vehicle_config": {
            "car_type": "model3", "trim_badging": "74d", "exterior_color": "MidnightSilver",
            "wheel_type": "Pinwheel18", "spoiler_type": "None", "sun_roof_installed": 0
        }
    })
}

/// `response` with `group.key` set (a JSON null stays a stated key).
fn with(mut response: Value, group: &str, key: &str, value: Value) -> Value {
    response[group][key] = value;
    response
}

/// `response` without `group.key`.
fn without(mut response: Value, group: &str, key: &str) -> Value {
    response[group]
        .as_object_mut()
        .expect("group object")
        .remove(key);
    response
}

/// Every group of `response` stamped `time_ms`, unless a group states its own.
fn stamped(mut response: Value, time_ms: i64) -> Value {
    for group in GROUPS {
        if let Some(members) = response.get_mut(group).and_then(Value::as_object_mut) {
            members.entry("timestamp").or_insert(json!(time_ms));
        }
    }
    response
}

/// `VD(id, t, {..})`: an Owner-wire vehicle-data row, every group stamped `t`.
fn vd(id: i64, t: i64, response: Value) -> ObservationRecord {
    vd_stamped(id, t, t, response)
}

/// A vehicle-data row whose groups are stamped `group_time_ms`.
fn vd_stamped(id: i64, t: i64, group_time_ms: i64, response: Value) -> ObservationRecord {
    record(
        id,
        t,
        json!({
            "record_type": "owner_api_vehicle_data_v1",
            "source_vehicle_id": "1",
            "source_vehicle_state": "online",
            "provider_raw_json": {"response": stamped(response, group_time_ms)}
        }),
    )
}

/// `FD(id, t, {..})`: the Fleet variant.
fn fd(id: i64, t: i64, response: Value) -> ObservationRecord {
    let mut row = vd(id, t, response);
    row.payload["record_type"] = json!("fleet_api_vehicle_data_v1");
    row
}

/// A Fleet row from a sparse source (a non-empty `provider_feed`).
fn fd_sparse(id: i64, t: i64, response: Value) -> ObservationRecord {
    let mut row = fd(id, t, response);
    row.payload["provider_feed"] = json!("provider_stream");
    row
}

/// `S(id, t, {..})`: a stream payload in the stored shape, group times `t`,
/// unlisted keys null.
fn stream(id: i64, t: i64, edit: impl FnOnce(&mut StreamUpdate)) -> ObservationRecord {
    let mut update = StreamUpdate {
        tag: "1".to_owned(),
        timestamp_ms: t,
        elevation: None,
        est_heading: None,
        est_lat: None,
        est_lng: None,
        est_range: None,
        heading: None,
        odometer: None,
        power: None,
        range: None,
        shift_state: None,
        soc: None,
        speed: None,
    };
    edit(&mut update);
    record(id, t, stream_observation_payload(&update))
}

/// `D(id, t, s)`: a discovery row.
fn discovery(id: i64, t: i64, state: &str) -> ObservationRecord {
    record(
        id,
        t,
        json!({
            "record_type": "owner_api_discovery_v1",
            "source_vehicle_id": "1",
            "source_vehicle_state": state
        }),
    )
}

/// `IP(id, t, {..})`: an import-source position row; its type comes from the
/// one definition (C6 R64), never from a literal.
fn import_position(id: i64, t: i64, keys: Value) -> ObservationRecord {
    let mut payload = json!({"record_type": IMPORT_POSITION_RECORD_TYPE, "position_id": 7});
    for (key, value) in keys.as_object().expect("keys") {
        payload[key] = value.clone();
    }
    record(id, t, payload)
}

fn open_state(state: &str, start_date_ms: i64) -> OpenState {
    serde_json::from_value(
        json!({"id": 1, "car_id": 1, "state": state, "start_date_ms": start_date_ms}),
    )
    .expect("open state")
}

fn open_drive(start_date_ms: i64) -> OpenDrive {
    serde_json::from_value(json!({
        "id": 1, "car_id": 1, "start_date_ms": start_date_ms,
        "outside_temp_sum": 0.0, "outside_temp_count": 0
    }))
    .expect("open drive")
}

fn open_charge(start_date_ms: i64) -> OpenCharge {
    serde_json::from_value(json!({
        "id": 1, "car_id": 1, "start_date_ms": start_date_ms,
        "outside_temp_sum": 0.0, "outside_temp_count": 0
    }))
    .expect("open charge")
}

fn open_update(start_date_ms: i64) -> OpenUpdate {
    serde_json::from_value(json!({"id": 1, "car_id": 1, "start_date_ms": start_date_ms}))
        .expect("open update")
}

/// An open-session state in `phase` (the stored vocabulary) with its interval.
fn session(phase: &str, interval: Option<(&str, i64)>) -> OpenSessionState {
    let mut state = OpenSessionState::new();
    state.phase = serde_json::from_value::<VehiclePhase>(json!(phase)).expect("phase");
    state.open_state = interval.map(|(name, start)| open_state(name, start));
    state
}

fn lifecycle_record(state: &OpenSessionState) -> LifecycleStateRecord {
    LifecycleStateRecord {
        vehicle_id: vehicle(),
        car_id: 1,
        last_observation_id: 0,
        open_session_json: state.encode().expect("encode"),
        quarantined: false,
        updated_at_ms: 0,
    }
}

/// `L_on`: online, its interval open from `t0 - 3,600,000`.
fn l_on() -> LifecycleStateRecord {
    lifecycle_record(&session("online", Some(("online", T0 - 3_600_000))))
}

/// The summary at `now` = the newest row's `observed_at_ms` (`t0` without
/// rows) and `T` = 600,000, the default of the spec tests.
fn summary(
    rows: &[ObservationRecord],
    lifecycle: Option<&LifecycleStateRecord>,
) -> CurrentVehicleSummary {
    let now = rows
        .iter()
        .map(|row| row.observed_at_ms)
        .max()
        .unwrap_or(T0);
    summary_at(rows, lifecycle, now, T)
}

fn summary_at(
    rows: &[ObservationRecord],
    lifecycle: Option<&LifecycleStateRecord>,
    now_ms: i64,
    max_age_ms: i64,
) -> CurrentVehicleSummary {
    build_current_vehicle_summary(vehicle(), rows, None, lifecycle, None, now_ms, max_age_ms)
}

/// The summary of one fresh vehicle-data row that states only `group.key`
/// (ST-S29's "awake, fresh" setting: no lifecycle, so nothing hides a value).
fn one(group: &str, key: &str, value: Value) -> CurrentVehicleSummary {
    summary(&[vd(1, T0, json!({ group: { key: value } }))], None)
}

/// Equal to within the last digits of a conversion product.
fn close(actual: Option<f64>, expected: f64) -> bool {
    actual.is_some_and(|value| (value - expected).abs() < 1e-9)
}

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

#[test]
fn c6_st_s01_s02_basics() {
    // C6 ST-S1 (C6-S1 fields): a parked response under L_on.
    let rows = [vd(1, T0, p0())];
    let lifecycle = l_on();
    let s = summary(&rows, Some(&lifecycle));
    assert_eq!(s.vehicle_id, vehicle());
    assert_eq!(s.activity_state, Some(ActivityState::Online));
    assert_eq!(s.state_since_ms, Some(T0 - 3_600_000));
    assert_eq!(s.processing_healthy, Some(true));
    assert_eq!(s.observed_at_ms, Some(T0));
    assert_eq!(s.latitude, Some(10.123457));
    assert_eq!(s.longitude, Some(20.765432));
    assert_eq!(s.heading, Some(90.0));
    assert_eq!(s.elevation, Some(50.0));
    assert_eq!(s.speed_kmh, None);
    assert_eq!(s.power, Some(0.0));
    assert_eq!(s.shift_state, None);
    assert_eq!(s.battery_level, Some(80));
    assert_eq!(s.usable_battery_level, Some(79));
    assert_eq!(s.battery_range_km, Some(321.87));
    assert_eq!(s.ideal_battery_range_km, Some(337.96));
    assert_eq!(s.est_battery_range_km, Some(242.21));
    assert!(close(s.odometer_km, 1609.344));
    assert_eq!(s.charge_energy_added, Some(12.35));
    assert_eq!(s.charge_limit_soc, Some(90));
    assert_eq!(s.charge_port_door_open, Some(false));
    assert_eq!(s.time_to_full_charge, Some(0.0));
    assert_eq!(s.charger_power, Some(0.0));
    assert_eq!(s.inside_temp, Some(21.3));
    assert_eq!(s.outside_temp, Some(10.0));
    assert_eq!(s.is_climate_on, Some(false));
    assert_eq!(s.tpms_pressure_fl, Some(2.9));
    assert_eq!(s.locked, Some(true));
    assert_eq!(s.sun_roof_installed, Some(false));
    assert_eq!(s.software_update_download_perc, Some(0));
    assert_eq!(s.software_update_install_perc, Some(1));
    assert_eq!(s.geofence_name, None);

    // C6 ST-S2 (C6-S1 fields): the newest statement of each field wins across
    // the vehicle-data row and the stream row.
    let rows = [
        vd(1, T0, p0()),
        stream(2, T0 + 60_000, |update| {
            update.speed = Some(30.0);
            update.shift_state = Some("D".to_owned());
            update.soc = Some(79.0);
            update.est_lat = Some(10.2);
            update.est_lng = Some(20.8);
            update.power = Some(15);
            update.odometer = Some(1000.5);
        }),
    ];
    let mut state = session("driving", Some(("online", T0 - 3_600_000)));
    state.open_drive = Some(open_drive(T0 + 30_000));
    let lifecycle = lifecycle_record(&state);
    let s = summary(&rows, Some(&lifecycle));
    assert_eq!(s.activity_state, Some(ActivityState::Driving));
    assert_eq!(s.state_since_ms, Some(T0 + 30_000));
    assert_eq!(s.speed_kmh, Some(48));
    assert_eq!(s.shift_state.as_deref(), Some("D"));
    assert_eq!(s.power, Some(15.0));
    assert_eq!(s.battery_level, Some(79));
    assert_eq!(s.usable_battery_level, Some(79));
    assert_eq!(s.latitude, Some(10.2));
    assert_eq!(s.longitude, Some(20.8));
    assert!(close(s.odometer_km, 1610.148672));
    assert_eq!(s.inside_temp, Some(21.3));
}

#[test]
fn c6_st_s03_to_s06_merge() {
    // C6 ST-S3: the stream frame's nulls are no statements.
    let rows = [
        vd(
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
        ),
        stream(2, T0 + 60_000, |update| update.soc = Some(78.0)),
    ];
    let mut state = session("driving", Some(("online", T0 - 60_000)));
    state.open_drive = Some(open_drive(T0));
    let lifecycle = lifecycle_record(&state);
    let s = summary(&rows, Some(&lifecycle));
    assert_eq!(s.battery_level, Some(78));
    assert_eq!(s.speed_kmh, Some(48));
    assert_eq!(s.shift_state.as_deref(), Some("D"));
    assert_eq!(s.latitude, Some(10.15));

    // C6 ST-S4: a null in a full response is a statement and beats an older
    // stream value.
    let rows = [
        stream(1, T0 + 60_000, |update| {
            update.speed = Some(30.0);
            update.shift_state = Some("D".to_owned());
            update.est_lat = Some(10.2);
            update.est_lng = Some(20.8);
        }),
        vd(
            2,
            T0 + 180_000,
            with(
                with(p0(), "drive_state", "latitude", json!(10.3)),
                "drive_state",
                "longitude",
                json!(20.9),
            ),
        ),
    ];
    let lifecycle = lifecycle_record(&session("online", Some(("online", T0))));
    let s = summary(&rows, Some(&lifecycle));
    assert_eq!(s.shift_state, None);
    assert_eq!(s.speed_kmh, None);
    assert_eq!(s.latitude, Some(10.3));

    // C6 ST-S5: a response without location access states no coordinates, so
    // the newest stream position stands.
    let response = without(
        without(p0(), "drive_state", "latitude"),
        "drive_state",
        "longitude",
    );
    let rows = [
        stream(1, T0, |update| {
            update.est_lat = Some(10.2);
            update.est_lng = Some(20.8);
        }),
        vd(2, T0 + 60_000, response),
    ];
    let s = summary(&rows, None);
    assert_eq!((s.latitude, s.longitude), (Some(10.2), Some(20.8)));

    // C6 ST-S6: an invalid newest pair gives two nulls, and an invalid newest
    // value gives null, never an older valid value.
    let stream_position = stream(1, T0, |update| {
        update.est_lat = Some(10.2);
        update.est_lng = Some(20.8);
        update.soc = Some(70.0);
    });
    let rows = [
        stream_position.clone(),
        vd(
            2,
            T0 + 60_000,
            with(p0(), "drive_state", "latitude", json!(95.0)),
        ),
    ];
    let s = summary(&rows, None);
    assert_eq!((s.latitude, s.longitude), (None, None));
    let rows = [
        stream_position,
        vd(
            2,
            T0 + 60_000,
            with(p0(), "charge_state", "battery_level", json!(140)),
        ),
    ];
    assert_eq!(summary(&rows, None).battery_level, None);
    // A pair is one group that states both keys: a newer response that states
    // only the latitude gives no half, and the older complete pair stands.
    let rows = [
        stream(1, T0, |update| {
            update.est_lat = Some(10.2);
            update.est_lng = Some(20.8);
        }),
        vd(
            2,
            T0 + 60_000,
            without(
                with(p0(), "drive_state", "latitude", json!(10.5)),
                "drive_state",
                "longitude",
            ),
        ),
    ];
    let s = summary(&rows, None);
    assert_eq!((s.latitude, s.longitude), (Some(10.2), Some(20.8)));

    // C6 ST-S12 (C6-S1 fields): a stream-only car. The fields the stream does
    // not carry are null; the stream's `soc` and `range` fold to the battery
    // level and the ideal range (C1 R37).
    let rows = [stream(1, T0, |update| {
        update.speed = Some(30.0);
        update.shift_state = Some("D".to_owned());
        update.soc = Some(60.0);
        update.range = Some(100.0);
    })];
    let mut state = session("driving", Some(("online", T0 - 60_000)));
    state.open_drive = Some(open_drive(T0));
    let lifecycle = lifecycle_record(&state);
    let s = summary(&rows, Some(&lifecycle));
    assert_eq!(s.activity_state, Some(ActivityState::Driving));
    assert_eq!(s.battery_level, Some(60));
    assert_eq!(s.ideal_battery_range_km, Some(160.93));
    assert_eq!(s.est_battery_range_km, None);
    assert_eq!(s.locked, None);
}

#[test]
fn c6_st_s24_to_s26_order() {
    let soc_79 = stream(2, T0 + 30_000, |update| update.soc = Some(79.0));

    // C6 ST-S24: the group time, not the arrival time, says how current a value is.
    let rows = [
        vd(
            1,
            T0 + 60_000,
            with(p0(), "charge_state", "timestamp", json!(T0)),
        ),
        soc_79.clone(),
    ];
    assert_eq!(summary(&rows, None).battery_level, Some(79));
    // A group time later than the row time plus the skew is timed at the row
    // time; exactly the skew is kept.
    let rows = [
        vd(
            1,
            T0,
            with(p0(), "charge_state", "timestamp", json!(T0 + 60_001)),
        ),
        soc_79.clone(),
    ];
    assert_eq!(summary(&rows, None).battery_level, Some(79));
    let rows = [
        vd(
            1,
            T0,
            with(p0(), "charge_state", "timestamp", json!(T0 + 60_000)),
        ),
        soc_79.clone(),
    ];
    assert_eq!(summary(&rows, None).battery_level, Some(80));
    // C1's group-time reader accepts a numeric string and an integral float.
    for stated in [json!("1760000000000"), json!(1_760_000_000_000.0)] {
        let rows = [
            vd(
                1,
                T0 + 60_000,
                with(p0(), "charge_state", "timestamp", stated),
            ),
            soc_79.clone(),
        ];
        assert_eq!(summary(&rows, None).battery_level, Some(79));
    }

    // C6 ST-S25: equal statement times are settled by the observation id.
    let rows = [
        vd(5, T0, p0()),
        fd(
            6,
            T0,
            with(p0(), "charge_state", "battery_level", json!(81)),
        ),
    ];
    assert_eq!(summary(&rows, None).battery_level, Some(81));

    // C6 ST-S26: the order of the rows changes nothing.
    let mut state = session("driving", Some(("online", T0 - 60_000)));
    state.open_drive = Some(open_drive(T0));
    let lifecycle = lifecycle_record(&state);
    let drive_response = with(
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
    );
    let rows = [
        vd(1, T0, drive_response),
        stream(2, T0 + 60_000, |update| update.soc = Some(78.0)),
        discovery(3, T0 + 10, "online"),
        import_position(4, T0 + 5, json!({"battery_level": 70})),
    ];
    let expected = summary(&rows, Some(&lifecycle));
    assert_eq!(expected.battery_level, Some(78));
    let orders = permutations(&rows);
    assert_eq!(orders.len(), 24);
    for order in orders {
        assert_eq!(summary(&order, Some(&lifecycle)), expected);
    }
}

#[test]
fn c6_st_s07_to_s11_visibility() {
    // C6 ST-S7 (C6-S1 fields): an asleep vehicle keeps its persistent values and
    // shows no transient one.
    let awake = {
        let mut response = with(p0(), "climate_state", "is_climate_on", json!(true));
        response = with(response, "charge_state", "charger_power", json!(7.4));
        response = with(
            response,
            "charge_state",
            "charging_state",
            json!("Charging"),
        );
        response = with(response, "vehicle_state", "sentry_mode", json!(true));
        with(response, "drive_state", "speed", json!(0))
    };
    let rows = [
        vd(1, T0, awake.clone()),
        discovery(2, T0 + 7_200_000, "asleep"),
    ];
    let lifecycle = lifecycle_record(&session("asleep", Some(("asleep", T0 + 7_200_000))));
    let s = summary(&rows, Some(&lifecycle));
    assert_eq!(s.activity_state, Some(ActivityState::Asleep));
    assert_eq!(s.state_since_ms, Some(T0 + 7_200_000));
    assert_eq!(s.observed_at_ms, Some(T0 + 7_200_000));
    assert_eq!(s.battery_level, Some(80));
    assert_eq!(s.latitude, Some(10.123457));
    assert_eq!(s.locked, Some(true));
    assert_eq!(s.is_climate_on, None);
    assert_eq!(s.charger_power, None);
    assert_eq!(s.inside_temp, None);
    assert_eq!(s.sentry_mode, None);
    assert_eq!(s.speed_kmh, None);

    // C6 ST-S8 (C6-S1 fields): the same under `offline`.
    let rows = [vd(1, T0, awake), discovery(2, T0 + 7_200_000, "offline")];
    let lifecycle = lifecycle_record(&session("offline", Some(("offline", T0 + 7_200_000))));
    let s = summary(&rows, Some(&lifecycle));
    assert_eq!(s.activity_state, Some(ActivityState::Offline));
    assert_eq!(s.charger_power, None);
    assert_eq!(s.inside_temp, None);
    assert_eq!(s.battery_level, Some(80));

    // A fresh reading hidden by the reachability alone (C6 R28 (a)): the
    // vehicle-data row is a minute older than the newest row, far inside the
    // age bound, so only `asleep` or `offline` can hide its transient fields.
    // The same rows under an online interval show them.
    let fresh = with(p0(), "climate_state", "is_climate_on", json!(true));
    for phase in ["asleep", "offline"] {
        let rows = [vd(1, T0, fresh.clone()), discovery(2, T0 + 60_000, phase)];
        let hidden = lifecycle_record(&session(phase, Some((phase, T0 + 60_000))));
        let s = summary(&rows, Some(&hidden));
        assert_eq!(s.is_climate_on, None, "{phase}");
        assert_eq!(s.inside_temp, None, "{phase}");
        assert_eq!(s.battery_level, Some(80), "{phase}");
        let shown = summary(&rows, Some(&l_on()));
        assert_eq!(shown.is_climate_on, Some(true), "{phase} control");
        assert_eq!(shown.inside_temp, Some(21.3), "{phase} control");
    }

    // C6 ST-S9: a group older than T_gap against the newest row is stale.
    let rows = [
        vd(
            1,
            T0,
            with(p0(), "charge_state", "charger_power", json!(7.4)),
        ),
        stream(2, T0 + 900_000, |update| {
            update.speed = Some(30.0);
            update.shift_state = Some("D".to_owned());
        }),
    ];
    let mut state = session("driving", Some(("online", T0 - 3_600_000)));
    state.open_drive = Some(open_drive(T0 + 890_000));
    let lifecycle = lifecycle_record(&state);
    let s = summary(&rows, Some(&lifecycle));
    assert_eq!(s.charger_power, None);
    assert_eq!(s.inside_temp, None);
    assert_eq!(s.speed_kmh, Some(48));

    // C6 ST-S10: the age bound is inclusive, against the newest row.
    let lifecycle = l_on();
    for (stream_at, expected) in [(T0 + 600_000, Some(21.3)), (T0 + 600_001, None)] {
        let rows = [
            vd(1, T0, p0()),
            stream(2, stream_at, |update| update.speed = Some(0.0)),
        ];
        assert_eq!(summary(&rows, Some(&lifecycle)).inside_temp, expected);
    }
    // ... and against the caller's clock, where `T` is the configured bound and
    // a bound of zero reads as the default.
    let rows = [vd(1, T0, p0())];
    assert_eq!(
        summary_at(&rows, Some(&lifecycle), T0 + 600_000, T).inside_temp,
        Some(21.3)
    );
    assert_eq!(
        summary_at(&rows, Some(&lifecycle), T0 + 599_999, T).inside_temp,
        Some(21.3)
    );
    let late = summary_at(&rows, Some(&lifecycle), T0 + 600_001, T);
    assert_eq!(late.inside_temp, None);
    assert_eq!(late.battery_level, Some(80));
    assert_eq!(
        summary_at(&rows, Some(&lifecycle), T0 + 600_001, 1_200_000).inside_temp,
        Some(21.3)
    );
    for (now_ms, expected) in [
        (T0 + 1_199_999, Some(21.3)),
        (T0 + 1_200_000, Some(21.3)),
        (T0 + 1_200_001, None),
    ] {
        assert_eq!(
            summary_at(&rows, Some(&lifecycle), now_ms, 1_200_000).inside_temp,
            expected
        );
    }
    assert_eq!(
        summary_at(&rows, Some(&lifecycle), T0 + 600_000, 0).inside_temp,
        Some(21.3)
    );
    assert_eq!(
        summary_at(&rows, Some(&lifecycle), T0 + 600_001, 0).inside_temp,
        None
    );
    // A Hub clock behind the newest row (a vehicle clock ahead, C1 R7) is no
    // reason to show a value that is stale against that row.
    let rows = [
        vd(1, T0, p0()),
        stream(2, T0 + 700_000, |update| update.speed = Some(0.0)),
    ];
    assert_eq!(summary_at(&rows, Some(&lifecycle), T0, T).inside_temp, None);
    assert_eq!(
        summary_at(&rows, Some(&lifecycle), T0 + 700_000, T).inside_temp,
        None
    );

    // C6 ST-S11: a reading from before the open `online` interval is not
    // known to still hold.
    let rows = [
        vd(
            1,
            T0 + 3_300_000,
            with(p0(), "climate_state", "is_climate_on", json!(true)),
        ),
        discovery(2, T0 + 3_600_000, "online"),
    ];
    let lifecycle = lifecycle_record(&session("online", Some(("online", T0 + 3_600_000))));
    let s = summary(&rows, Some(&lifecycle));
    assert_eq!(s.is_climate_on, None);
    assert_eq!(s.battery_level, Some(80));
}

#[test]
fn c6_st_s40_to_s42_activity() {
    // C6 ST-S40: a sparse source's null is no statement.
    let rows = [
        vd(
            1,
            T0,
            with(
                with(p0(), "drive_state", "shift_state", json!("D")),
                "drive_state",
                "speed",
                json!(30),
            ),
        ),
        fd_sparse(
            2,
            T0 + 60_000,
            json!({"drive_state": {
                "timestamp": T0 + 60_000, "shift_state": null, "speed": 20
            }}),
        ),
    ];
    let mut state = session("driving", Some(("online", T0 - 60_000)));
    state.open_drive = Some(open_drive(T0));
    let lifecycle = lifecycle_record(&state);
    let s = summary(&rows, Some(&lifecycle));
    assert_eq!(s.shift_state.as_deref(), Some("D"));
    assert_eq!(s.speed_kmh, Some(32));

    // C6 ST-S41: the phase has no age rule; a stale value is still hidden.
    let rows = [stream(1, T0, |update| {
        update.speed = Some(30.0);
        update.shift_state = Some("D".to_owned());
    })];
    let s = summary_at(&rows, Some(&lifecycle), T0 + 3_600_000, T);
    assert_eq!(s.activity_state, Some(ActivityState::Driving));
    assert_eq!(s.state_since_ms, Some(T0));
    assert_eq!(s.speed_kmh, None);
    assert_eq!(s.shift_state, None);
    assert_eq!(s.observed_at_ms, Some(T0));

    // C6 ST-S42: a waking response's own readings, timed before the interval
    // start, still show (C1 R5 allows group times up to the skew earlier).
    let awake = with(p0(), "climate_state", "is_climate_on", json!(true));
    let rows = [vd_stamped(1, T0, T0 - 30_000, awake.clone())];
    let lifecycle = lifecycle_record(&session("online", Some(("online", T0))));
    let s = summary(&rows, Some(&lifecycle));
    assert_eq!(s.is_climate_on, Some(true));
    assert_eq!(s.inside_temp, Some(21.3));
    // The row time decides against the interval start minus the skew.
    for (row_at, expected) in [(T0 - 60_000, Some(true)), (T0 - 60_001, None)] {
        let rows = [discovery(1, T0, "online"), vd(2, row_at, awake.clone())];
        assert_eq!(summary(&rows, Some(&lifecycle)).is_climate_on, expected);
    }
}

#[test]
fn c6_st_s20_to_s29_fields() {
    // C6 ST-S20: the phase mapping and each phase's start.
    let mut driving = session("driving", Some(("online", T0 - 500)));
    driving.open_drive = Some(open_drive(T0 + 1));
    let mut charging = session("charging", Some(("online", T0 - 500)));
    charging.open_charge = Some(open_charge(T0 + 2));
    let mut updating = session("updating", Some(("online", T0 - 500)));
    updating.open_update = Some(open_update(T0 + 3));
    let cases = [
        (session("unknown", None), None, None),
        (
            session("online", Some(("online", T0 + 4))),
            Some(ActivityState::Online),
            Some(T0 + 4),
        ),
        (
            session("suspended", Some(("online", T0 + 5))),
            Some(ActivityState::Online),
            Some(T0 + 5),
        ),
        (
            session("asleep", Some(("asleep", T0 + 6))),
            Some(ActivityState::Asleep),
            Some(T0 + 6),
        ),
        (
            session("offline", Some(("offline", T0 + 7))),
            Some(ActivityState::Offline),
            Some(T0 + 7),
        ),
        (driving, Some(ActivityState::Driving), Some(T0 + 1)),
        (charging, Some(ActivityState::Charging), Some(T0 + 2)),
        (updating, Some(ActivityState::Updating), Some(T0 + 3)),
        // The interval must be the one the phase names.
        (
            session("online", Some(("asleep", T0 + 8))),
            Some(ActivityState::Online),
            None,
        ),
        (session("online", None), Some(ActivityState::Online), None),
    ];
    for (state, activity, since) in cases {
        let s = summary(&[vd(1, T0, p0())], Some(&lifecycle_record(&state)));
        assert_eq!(s.activity_state, activity, "{:?}", state.phase);
        assert_eq!(s.state_since_ms, since, "{:?}", state.phase);
    }

    // C6 ST-S21: without a lifecycle record the newest reachability row decides;
    // an import-source position is no reachability evidence.
    let s = summary(&[vd(1, T0, p0()), discovery(2, T0 + 10, "asleep")], None);
    assert_eq!(s.activity_state, Some(ActivityState::Asleep));
    assert_eq!((s.state_since_ms, s.processing_healthy), (None, None));
    let s = summary(
        &[discovery(1, T0 + 10, "asleep"), vd(2, T0 + 20, p0())],
        None,
    );
    assert_eq!(s.activity_state, Some(ActivityState::Online));
    let s = summary(
        &[
            vd(1, T0, p0()),
            import_position(2, T0 + 60_000, json!({"battery_level": 75})),
        ],
        None,
    );
    assert_eq!(s.activity_state, Some(ActivityState::Online));
    let s = summary(&[discovery(1, T0, "unavailable")], None);
    assert_eq!(s.activity_state, None);

    // C6 ST-S22: a quarantined or undecodable record reports unhealthy.
    let mut quarantined = l_on();
    quarantined.quarantined = true;
    let s = summary(&[vd(1, T0, p0())], Some(&quarantined));
    assert_eq!(s.processing_healthy, Some(false));
    assert_eq!(s.activity_state, Some(ActivityState::Online));
    let mut broken = l_on();
    broken.open_session_json = b"not json".to_vec();
    let s = summary(&[vd(1, T0, p0())], Some(&broken));
    assert_eq!(s.processing_healthy, Some(false));
    assert_eq!(s.activity_state, Some(ActivityState::Online));
    assert_eq!(s.state_since_ms, None);

    // C6 ST-S23: an import-source row states its already-metric values.
    let rows = [
        vd(1, T0, p0()),
        import_position(
            2,
            T0 + 60_000,
            json!({"battery_level": 75, "rated_battery_range_km": 300.004, "odometer_km": 1700.5}),
        ),
    ];
    let s = summary(&rows, None);
    assert_eq!(s.battery_level, Some(75));
    assert_eq!(s.battery_range_km, Some(300.0));
    assert_eq!(s.odometer_km, Some(1700.5));
    assert_eq!(s.ideal_battery_range_km, Some(337.96));

    // C6 ST-S27: rows that are not the vehicle's accepted rows change nothing,
    // also not `observed_at_ms`.
    let base = [vd(1, T0, p0())];
    let mut other_vehicle = vd(2, T0 + 1, json!({"charge_state": {"battery_level": 10}}));
    other_vehicle.vehicle_id = Uuid::from_u128(3);
    let mut unknown_type = record(3, T0 + 1, json!({"record_type": "x", "fields": {}}));
    unknown_type.payload["charge_state"] = json!({"battery_level": 10});
    let not_an_object = record(4, T0 + 1, json!("broken"));
    let no_type = record(
        5,
        T0 + 1,
        json!({"vehicle_data": {"charge_state": {"battery_level": 10}}}),
    );
    let hub_event = record(6, T0 + 1, json!({"record_type": "hub_lifecycle_event_v1"}));
    let mut noisy = base.to_vec();
    noisy.extend([
        other_vehicle,
        unknown_type,
        not_an_object,
        no_type,
        hub_event,
    ]);
    let expected = summary_at(&base, None, T0, T);
    assert_eq!(summary_at(&noisy, None, T0, T), expected);
    assert_eq!(expected.observed_at_ms, Some(T0));

    // C6 ST-S28: a broken data object states nothing but still counts as
    // received; a broken group is skipped alone.
    let mut broken_object = fd(2, T0 + 60_000, json!({}));
    broken_object.payload["provider_raw_json"] = json!({"response": "x"});
    let rows = [vd(1, T0, p0()), broken_object.clone()];
    let lifecycle = l_on();
    let s = summary(&rows, Some(&lifecycle));
    // Every field stays as in ST-S1 at the same clock; only `observed_at_ms`
    // moves to the broken row's time (C6 ST-S28).
    let mut expected = summary_at(&rows[..1], Some(&lifecycle), T0 + 60_000, T);
    assert_eq!(expected.observed_at_ms, Some(T0));
    expected.observed_at_ms = Some(T0 + 60_000);
    assert_eq!(s, expected);
    assert_eq!(s.battery_level, Some(80));
    assert!(close(s.odometer_km, 1609.344));
    assert_eq!(
        summary(&rows, None).activity_state,
        Some(ActivityState::Online)
    );
    let mut broken_group = fd(1, T0, json!({"charge_state": {"battery_level": 50}}));
    broken_group.payload["provider_raw_json"]["response"]["vehicle_state"] = json!("x");
    let s = summary(&[broken_group], None);
    assert_eq!(s.battery_level, Some(50));
    assert_eq!(s.odometer_km, None);

    // C6 ST-S29: conversions, ranges and readable forms.
    assert_eq!(one("drive_state", "speed", json!(60)).speed_kmh, Some(97));
    assert_eq!(one("drive_state", "speed", json!("60")).speed_kmh, Some(97));
    assert_eq!(
        one("charge_state", "charger_power", json!(7.4)).charger_power,
        Some(7.0)
    );
    assert_eq!(
        one("charge_state", "charger_voltage", json!(230.6)).charger_voltage,
        Some(231.0)
    );
    let route = one("drive_state", "active_route_miles_to_arrival", json!(12.5));
    assert_eq!(route.active_route_km_to_arrival, Some(20.12));
    assert_eq!(route.active_route_miles_to_arrival_raw, Some(12.5));
    // A finite input whose conversion overflows is no kilometre value (C6 R27).
    let huge = one("drive_state", "active_route_miles_to_arrival", json!(1e307));
    assert_eq!(huge.active_route_km_to_arrival, None);
    for (stated, expected) in [
        (json!(2), Some(true)),
        (json!(0), Some(false)),
        (json!(true), Some(true)),
        (json!(1.5), None),
    ] {
        assert_eq!(
            one("vehicle_config", "sun_roof_installed", stated).sun_roof_installed,
            expected
        );
    }
    assert_eq!(
        one(
            "charge_state",
            "scheduled_charging_start_time",
            json!(1_760_003_600)
        )
        .scheduled_charging_start_time,
        Some(1_760_003_600)
    );
    assert_eq!(
        one("drive_state", "heading", json!(360)).heading,
        Some(360.0)
    );
    assert_eq!(one("drive_state", "heading", json!(361)).heading, None);
    // 2^63, one above `i64::MAX`, is the wrong type (C6 R27).
    for (stated, expected) in [
        (json!(7), Some(7)),
        (json!(2.5), None),
        (json!(-1), None),
        (json!(9_223_372_036_854_775_808_u64), None),
        (json!(1e19), None),
    ] {
        assert_eq!(
            one("vehicle_state", "center_display_state", stated).center_display_state,
            expected
        );
    }
    assert_eq!(
        one("charge_state", "charge_current_request", json!("16")).charge_current_request,
        Some(16)
    );
    assert_eq!(
        one("charge_state", "charge_current_request", json!(-1)).charge_current_request,
        None
    );
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
        .register_source(&SourceDescriptor::new("owner_api", "c6-s1-test"), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "1").with_tesla_identity(Some(1), None),
            1_000,
        )
        .expect("vehicle");
    (temp, store, source.source_id, vehicle.vehicle_id)
}

fn stored_summary(store: &HubStore, vehicle_id: Uuid, now_ms: i64) -> CurrentVehicleSummary {
    let rows = store
        .current_observations_for_vehicle(vehicle_id)
        .expect("current rows");
    let lifecycle = store.load_lifecycle_state(vehicle_id).expect("lifecycle");
    build_current_vehicle_summary(vehicle_id, &rows, None, lifecycle.as_ref(), None, now_ms, T)
}

#[test]
fn c6_st_s34_to_s39_storage_text() {
    // C6 ST-S34: a restart rebuilds an equal summary from storage.
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
    let before = stored_summary(&store, vehicle_id, T0 + 60_000);
    assert_eq!(before.battery_level, Some(79));
    assert_eq!(before.speed_kmh, Some(48));
    drop(store);
    let reopened = HubStore::initialize(temp.path()).expect("reopened store");
    let after = stored_summary(&reopened, vehicle_id, T0 + 60_000);
    assert_eq!(after, before);
    drop(reopened);
    drop(temp);

    // C6 ST-S35: a stream frame replaces the previous frame's row entirely, and
    // the nulls it sent state nothing.
    let (_temp, store, source_id, vehicle_id) = store_with_vehicle();
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
    store
        .accept_owner_observation_and_lifecycle(
            &owner_input(source_id, vehicle_id, &owner),
            T0 + 1,
            1,
        )
        .expect("owner observation");
    let first = stream(2, T0 + 60_000, |update| {
        update.speed = Some(40.0);
        update.shift_state = Some("D".to_owned());
        update.est_lat = Some(10.2);
        update.est_lng = Some(20.8);
        update.soc = Some(79.0);
    });
    let second = stream(3, T0 + 120_000, |update| update.soc = Some(78.0));
    for (frame, received) in [(&first, T0 + 60_001), (&second, T0 + 120_001)] {
        store
            .accept_stream_observation_and_lifecycle(
                &owner_input(source_id, vehicle_id, frame),
                received,
                1,
            )
            .expect("stream observation");
    }
    let rows = store
        .current_observations_for_vehicle(vehicle_id)
        .expect("rows");
    assert_eq!(rows.len(), 2);
    let stored_frame = rows
        .iter()
        .find(|row| row.payload["record_type"] == "tesla_stream_update_v1")
        .expect("stream row");
    assert_eq!(stored_frame.observed_at_ms, T0 + 120_000);
    let s = stored_summary(&store, vehicle_id, T0 + 120_000);
    assert_eq!(s.battery_level, Some(78));
    assert_eq!(s.speed_kmh, Some(48));
    assert_eq!(s.shift_state.as_deref(), Some("D"));
    assert_eq!(s.latitude, Some(10.15));
    assert_eq!(s.activity_state, Some(ActivityState::Driving));

    // C6 ST-S36: a late older row never replaces a newer one, and a repeated
    // row leaves the row and the summary byte-identical.
    let (_temp, store, source_id, vehicle_id) = store_with_vehicle();
    let newer = vd(1, T0 + 60_000, p0());
    let older = vd(
        2,
        T0,
        with(p0(), "charge_state", "battery_level", json!(50)),
    );
    store
        .accept_owner_observation_and_lifecycle(
            &owner_input(source_id, vehicle_id, &newer),
            T0 + 60_001,
            1,
        )
        .expect("newer observation");
    store
        .accept_owner_observation_and_lifecycle(
            &owner_input(source_id, vehicle_id, &older),
            T0 + 60_002,
            1,
        )
        .expect("late observation");
    let rows = store
        .current_observations_for_vehicle(vehicle_id)
        .expect("rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].observed_at_ms, T0 + 60_000);
    let s = stored_summary(&store, vehicle_id, T0 + 60_000);
    assert_eq!(s.battery_level, Some(80));
    let bytes = serde_json::to_vec(&s).expect("summary JSON");
    store
        .accept_owner_observation_and_lifecycle(
            &owner_input(source_id, vehicle_id, &newer),
            T0 + 60_003,
            1,
        )
        .expect("repeated observation");
    assert_eq!(
        store
            .current_observations_for_vehicle(vehicle_id)
            .expect("rows"),
        rows
    );
    let again = stored_summary(&store, vehicle_id, T0 + 60_000);
    assert_eq!(serde_json::to_vec(&again).expect("summary JSON"), bytes);

    // C6 ST-S38: the text rule is a trimmed string within K_TEXT_MAX scalar values.
    let climate =
        |value: Value| one("climate_state", "climate_keeper_mode", value).climate_keeper_mode;
    assert_eq!(climate(json!(" dog ")).as_deref(), Some("dog"));
    assert_eq!(climate(json!("")), None);
    assert_eq!(
        climate(json!("ClimateKeeperModeStateDog")).as_deref(),
        Some("ClimateKeeperModeStateDog")
    );
    assert_eq!(
        one("vehicle_state", "sun_roof_state", json!("closed"))
            .sun_roof_state
            .as_deref(),
        Some("closed")
    );
    let destination = |value: String| {
        one("drive_state", "active_route_destination", json!(value)).active_route_destination
    };
    assert_eq!(destination("a".repeat(65_537)), None);
    assert_eq!(
        destination("é".repeat(65_536)).map(|text| text.chars().count()),
        Some(65_536)
    );

    // C6 ST-S39: `elevation` precedence follows C1 R38a, per observation.
    let elevation = |response: Value| summary(&[vd(1, T0, response)], None).elevation;
    let drive = |members: Value| json!({"drive_state": members});
    assert_eq!(
        elevation(drive(
            json!({"elevation": null, "native_location_elevation": 120.4})
        )),
        Some(120.0)
    );
    assert_eq!(
        elevation(drive(
            json!({"elevation": 50.4, "native_location_elevation": 120})
        )),
        Some(50.0)
    );
    assert_eq!(elevation(drive(json!({"elevation": null}))), None);
    let rows = [
        stream(1, T0, |update| update.elevation = Some(30.0)),
        vd(
            2,
            T0 + 60_000,
            drive(json!({"latitude": 10.0, "longitude": 20.0})),
        ),
    ];
    assert_eq!(summary(&rows, None).elevation, Some(30.0));
    // The newest observation wins even when it holds no valid reading.
    let rows = [
        stream(1, T0, |update| update.elevation = Some(30.0)),
        vd(2, T0 + 60_000, drive(json!({"elevation": null}))),
    ];
    assert_eq!(summary(&rows, None).elevation, None);
}

#[test]
fn c6_st_s44_route() {
    // C6 ST-S44: the route pair is one pair; its text and numbers follow the rules.
    let route = |members: Value| {
        let mut response = p0();
        for (key, value) in members.as_object().expect("members") {
            response["drive_state"][key] = value.clone();
        }
        vd(1, T0, response)
    };
    let s = summary(
        &[route(json!({
            "active_route_latitude": 10.5, "active_route_longitude": 20.5,
            "active_route_destination": " Home "
        }))],
        None,
    );
    assert_eq!(
        (
            s.active_route_latitude,
            s.active_route_longitude,
            s.active_route_destination.as_deref()
        ),
        (Some(10.5), Some(20.5), Some("Home"))
    );
    let s = summary(
        &[route(
            json!({"active_route_latitude": 95.0, "active_route_longitude": 20.5}),
        )],
        None,
    );
    assert_eq!(
        (s.active_route_latitude, s.active_route_longitude),
        (None, None)
    );
    let s = summary(&[route(json!({"active_route_latitude": 10.5}))], None);
    assert_eq!(
        (s.active_route_latitude, s.active_route_longitude),
        (None, None)
    );
    // The route is a transient value: an asleep vehicle shows none of it.
    let lifecycle = lifecycle_record(&session("asleep", Some(("asleep", T0))));
    let s = summary(
        &[route(json!({
            "active_route_latitude": 10.5, "active_route_longitude": 20.5,
            "active_route_destination": "Home"
        }))],
        Some(&lifecycle),
    );
    assert_eq!(
        (
            s.active_route_latitude,
            s.active_route_longitude,
            s.active_route_destination
        ),
        (None, None, None)
    );
}

#[test]
fn c6_st_s52_fractional_stream() {
    // C6 ST-S52: the integer rule rounds half away from zero, then range-checks;
    // a value beyond the i64 range is the wrong type.
    for (soc, expected) in [
        (79.6, Some(80)),
        (79.4, Some(79)),
        (79.5, Some(80)),
        (-0.4, Some(0)),
        (100.4, Some(100)),
        (100.6, None),
        (1e20, None),
    ] {
        let rows = [stream(1, T0, |update| update.soc = Some(soc))];
        assert_eq!(summary(&rows, None).battery_level, expected, "soc {soc}");
    }
    assert_eq!(offline_drive_timeout_ms(120), 120_000);
    assert_eq!(offline_drive_timeout_ms(u64::MAX), i64::MAX);
}
