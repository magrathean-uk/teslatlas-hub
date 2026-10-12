// SPDX-License-Identifier: AGPL-3.0-only

//! Part D spec tests (C1 specification section 8): drives ST23-ST44, the
//! geofence rule ST45-ST50, ST77 (drive half), ST88 and the merge of step
//! deltas (R48, R48a). Synthetic data only. `T0` is 1_760_000_000_000 ms and
//! `MIN` is one minute. Coordinates are in the latitude 10.0, longitude 20.0
//! region, where 0.001 degrees of latitude is 111.195 m. Odometer values are
//! miles and speeds mph, as Tesla reports them. Observation shorthands: `full`
//! is an owner `vehicle_data` with the three groups the drive rules read,
//! `frame` a legacy stream frame, `event` a Hub event and `disc` a discovery.

use serde_json::{Map, json};

use super::tests::imported_position_fixture;
use super::*;
use crate::tesla_stream::StreamUpdate;

pub(crate) const T0: i64 = 1_760_000_000_000;
pub(crate) const MIN: i64 = 60_000;
pub(crate) const SEC: i64 = 1_000;

fn vd(id: i64, t: i64, data: Value) -> LifecycleSample {
    LifecycleSample {
        observation_id: id,
        observed_at_ms: t,
        vehicle_state: "online".to_owned(),
        payload: json!({
            "record_type": "owner_api_vehicle_data_v1",
            "source_vehicle_id": "9",
            "source_vehicle_state": "online",
            "provider_raw_json": {"response": data},
        }),
    }
}

/// An owner `vehicle_data` with a drive group (gear and speed as given, `null`
/// meaning the key is present with JSON null), a charge group with the state
/// of charge and a vehicle group with the odometer. All group times are `t`.
pub(crate) fn full(
    id: i64,
    t: i64,
    gear: Value,
    speed: Value,
    latitude: f64,
    odometer_mi: Option<f64>,
    soc: Option<i64>,
) -> LifecycleSample {
    let mut data = Map::new();
    data.insert(
        "drive_state".to_owned(),
        json!({"timestamp": t, "shift_state": gear, "speed": speed,
               "latitude": latitude, "longitude": 20.0}),
    );
    if let Some(soc) = soc {
        data.insert(
            "charge_state".to_owned(),
            json!({"timestamp": t, "battery_level": soc}),
        );
    }
    if let Some(odometer) = odometer_mi {
        data.insert(
            "vehicle_state".to_owned(),
            json!({"timestamp": t, "odometer": odometer}),
        );
    }
    vd(id, t, Value::Object(data))
}

/// Gear D at 30 mph.
pub(crate) fn driving(id: i64, t: i64, latitude: f64, odometer_mi: Option<f64>) -> LifecycleSample {
    full(id, t, json!("D"), json!(30), latitude, odometer_mi, None)
}

/// Gear key present with JSON null and no speed: Park for a full source.
pub(crate) fn parked(id: i64, t: i64, latitude: f64, odometer_mi: Option<f64>) -> LifecycleSample {
    full(id, t, Value::Null, Value::Null, latitude, odometer_mi, None)
}

fn disc(id: i64, t: i64, state: &str) -> LifecycleSample {
    LifecycleSample {
        observation_id: id,
        observed_at_ms: t,
        vehicle_state: state.to_owned(),
        payload: json!({
            "record_type": "owner_api_discovery_v1",
            "source_vehicle_id": "9",
            "source_vehicle_state": state,
        }),
    }
}

fn event(id: i64, t: i64, kind: &str) -> LifecycleSample {
    LifecycleSample {
        observation_id: id,
        observed_at_ms: t,
        vehicle_state: "unknown".to_owned(),
        payload: json!({"record_type": "hub_lifecycle_event_v1", "event": kind}),
    }
}

/// A stream frame built the way production builds it (`StreamUpdate`, then
/// the stored payload): `gear` `None` is the empty wire column, `speed` mph.
pub(crate) fn frame(
    id: i64,
    t: i64,
    gear: Option<&str>,
    speed: Option<i64>,
    latitude: f64,
    odometer_mi: Option<f64>,
) -> LifecycleSample {
    let update = StreamUpdate {
        tag: "9".to_owned(),
        timestamp_ms: t,
        speed,
        odometer: odometer_mi,
        soc: Some(80),
        elevation: None,
        est_heading: None,
        est_lat: Some(latitude),
        est_lng: Some(20.0),
        power: None,
        shift_state: gear.map(str::to_owned),
        range: None,
        est_range: None,
        heading: None,
    };
    LifecycleSample {
        observation_id: id,
        observed_at_ms: t,
        vehicle_state: "online".to_owned(),
        payload: stream_observation_payload(&update),
    }
}

fn run(samples: &[LifecycleSample]) -> LifecycleStep {
    apply_samples(OpenSessionState::new(), 1, samples).expect("apply samples")
}

fn step_one(state: OpenSessionState, sample: &LifecycleSample) -> LifecycleStep {
    apply_sample(state, 1, sample).expect("apply sample")
}

fn close_to(left: f64, right: f64) -> bool {
    (left - right).abs() < 1e-9
}

fn dates(positions: &[ProjectionPosition]) -> Vec<i64> {
    positions.iter().map(|position| position.date_ms).collect()
}

/// A moving drive of `count` observations one minute apart from `t`, each
/// 0.001 degrees of latitude and 0.5 mile further on.
pub(crate) fn moving(first_id: i64, t: i64, count: i64) -> Vec<LifecycleSample> {
    (0..count)
        .map(|k| {
            driving(
                first_id + k,
                t + k * MIN,
                10.0 + 0.001 * k as f64,
                Some(1000.0 + 0.5 * k as f64),
            )
        })
        .collect()
}

// ------------------------------------------------------------------ drives

#[test]
fn st23_basic_drive_with_a_lead_from_the_parked_spot() {
    let samples = [
        full(
            1,
            T0,
            Value::Null,
            Value::Null,
            10.0,
            Some(1000.0),
            Some(80),
        ),
        full(
            2,
            T0 + MIN,
            json!("D"),
            json!(30),
            10.005,
            Some(1000.5),
            Some(80),
        ),
        full(
            3,
            T0 + 2 * MIN,
            json!("D"),
            json!(60),
            10.010,
            Some(1001.5),
            Some(79),
        ),
        full(
            4,
            T0 + 3 * MIN,
            Value::Null,
            Value::Null,
            10.012,
            Some(1002.0),
            Some(78),
        ),
    ];
    let result = run(&samples);
    assert_eq!(result.delta.drives.len(), 1);
    let drive = &result.delta.drives[0];
    assert_eq!((drive.id, drive.car_id), (1, 1));
    assert_eq!(drive.start_date_ms, T0 + MIN);
    assert_eq!(drive.end_date_ms, T0 + 3 * MIN);
    // The snapshot of obs 1 is within T_gap, so the distance counts the
    // lead: 1002.0 - 1000.0 miles.
    assert!(close_to(drive.distance_km.unwrap(), 2.0 * K_MILE_KM));
    assert_eq!(drive.start_latitude, Some(10.0));
    assert_eq!(drive.start_longitude, Some(20.0));
    assert_eq!((drive.start_soc, drive.end_soc), (Some(80), Some(78)));
    assert_eq!(drive.duration_min, Some(2));
    assert_eq!(drive.speed_max, Some(97));
    assert_eq!(drive.end_latitude, Some(10.012));
    assert_eq!(result.delta.open_drive_positions.len(), 3);
    assert!(
        result
            .delta
            .open_drive_positions
            .iter()
            .all(|position| position.drive_id == Some(1))
    );
    assert_eq!(
        dates(&result.delta.open_drive_positions),
        vec![T0 + MIN, T0 + 2 * MIN, T0 + 3 * MIN]
    );
    // Obs 1 is a standalone position; the parked spot starts the drive.
    assert_eq!(dates(&result.delta.positions), vec![T0]);
    assert_eq!(result.delta.positions[0].drive_id, None);
    assert!(result.delta.discarded_drive_ids.is_empty());
    assert!(result.state.open_drive.is_none());
    assert_eq!(result.state.phase, VehiclePhase::Online);
}

/// Frames each minute: gear D at 20 mph from t0 to +5m, then an empty gear and
/// speed 0 from +6m to +17m (ST24, ST76).
pub(crate) fn st24_samples() -> Vec<LifecycleSample> {
    let mut samples = Vec::new();
    for k in 0..=5 {
        samples.push(frame(
            k + 1,
            T0 + k * MIN,
            Some("D"),
            Some(20),
            10.0 + 0.001 * k as f64,
            None,
        ));
    }
    for k in 6..=17 {
        samples.push(frame(k + 1, T0 + k * MIN, None, Some(0), 10.005, None));
    }
    samples
}

#[test]
fn st24_stream_only_motion_ends_at_the_still_checkpoint() {
    let samples = st24_samples();
    let result = run(&samples);
    assert_eq!(result.delta.drives.len(), 1);
    let drive = &result.delta.drives[0];
    // The drive closes when the +16m frame arrives (+16m minus the last
    // motion at +5m is 11 minutes, more than T_gap): the end is the still
    // checkpoint, the first frame that showed the car not moving.
    assert_eq!(drive.start_date_ms, T0);
    assert_eq!(drive.end_date_ms, T0 + 6 * MIN);
    assert_eq!(drive.duration_min, Some(6));
    assert_eq!(drive.distance_km, None, "no odometer, no distance");
    assert_eq!(result.delta.open_drive_positions.len(), 7);
    assert_eq!(
        dates(&result.delta.open_drive_positions),
        (0..=6).map(|k| T0 + k * MIN).collect::<Vec<_>>()
    );
    // R48a: the nine positions of the stationary tail are standalone, and the
    // +16m and +17m frames add none (within 15 minutes of the last stored
    // position at +15m).
    assert_eq!(
        dates(&result.delta.positions),
        (7..=15).map(|k| T0 + k * MIN).collect::<Vec<_>>()
    );
    assert!(result.delta.positions.iter().all(|p| p.drive_id.is_none()));
    assert!(result.state.open_drive.is_none());
}

#[test]
fn st25_frames_without_gear_or_power_keep_the_drive_open() {
    let mut samples = vec![frame(1, T0, Some("D"), Some(20), 10.0, None)];
    for k in 1..=3 {
        samples.push(frame(
            k + 1,
            T0 + k * MIN,
            None,
            Some(25),
            10.0 + 0.001 * k as f64,
            None,
        ));
    }
    let result = run(&samples);
    let open = result.state.open_drive.as_ref().expect("still open");
    assert_eq!(open.position_count, 4);
    assert_eq!(open.power_max, None);
    assert!(
        result
            .delta
            .open_drive_positions
            .iter()
            .all(|position| position.power.is_none())
    );
    assert!(result.delta.drives.is_empty());
}

#[test]
fn st26_an_explicit_park_frame_closes_with_its_position() {
    let samples = [
        frame(1, T0, Some("D"), Some(20), 10.0, None),
        frame(2, T0 + MIN, Some("D"), Some(20), 10.001, None),
        frame(3, T0 + 2 * MIN, Some("P"), Some(0), 10.002, None),
    ];
    let result = run(&samples);
    assert_eq!(result.delta.drives.len(), 1);
    assert_eq!(result.delta.drives[0].end_date_ms, T0 + 2 * MIN);
    assert_eq!(result.delta.drives[0].end_latitude, Some(10.002));
    assert_eq!(result.delta.open_drive_positions.len(), 3);
    assert_eq!(
        result.delta.open_drive_positions[2].date_ms,
        T0 + 2 * MIN,
        "the final position is included"
    );
}

/// D, P, R, P within 40 s, odometer +0.01 mile, displacement 5 m (ST27, ST76).
pub(crate) fn st27_samples() -> Vec<LifecycleSample> {
    vec![
        full(1, T0, json!("D"), json!(3), 10.0, Some(1000.0), None),
        full(
            2,
            T0 + 10 * SEC,
            json!("P"),
            json!(0),
            10.00002,
            Some(1000.0),
            None,
        ),
        full(
            3,
            T0 + 20 * SEC,
            json!("R"),
            json!(3),
            10.00003,
            Some(1000.005),
            None,
        ),
        full(
            4,
            T0 + 30 * SEC,
            json!("P"),
            json!(0),
            10.00004,
            Some(1000.01),
            None,
        ),
    ]
}

#[test]
fn st27_gear_shuffling_discards_both_drives_and_keeps_their_positions() {
    let samples = st27_samples();
    let result = run(&samples);
    assert!(result.delta.drives.is_empty());
    assert_eq!(result.delta.discarded_drive_ids, vec![1, 2]);
    assert!(result.delta.open_drive_positions.is_empty());
    assert_eq!(result.delta.positions.len(), 4);
    assert!(result.delta.positions.iter().all(|p| p.drive_id.is_none()));
    assert_eq!(
        dates(&result.delta.positions),
        vec![T0, T0 + 10 * SEC, T0 + 20 * SEC, T0 + 30 * SEC]
    );
    assert!(result.state.open_drive.is_none());
    // Ids of discarded drives are never reused.
    assert_eq!(result.state.next_drive_id, 3);
}

#[test]
fn st28_a_fine_odometer_decides_the_keep_rule_alone() {
    // 0.0621 mile is 0.0999403 km: discarded although the displacement is
    // 100.08 m. 0.0622 mile is 0.1001012 km: kept.
    for (delta_mi, kept) in [(0.0621, false), (0.0622, true)] {
        let samples = [
            full(1, T0, json!("D"), json!(10), 10.0, Some(1000.0), None),
            full(
                2,
                T0 + MIN,
                json!("P"),
                json!(0),
                10.0009,
                Some(1000.0 + delta_mi),
                None,
            ),
        ];
        let result = run(&samples);
        assert_eq!(result.delta.drives.len(), usize::from(kept), "{delta_mi}");
        assert_eq!(
            result.delta.discarded_drive_ids.len(),
            usize::from(!kept),
            "{delta_mi}"
        );
    }
}

#[test]
fn st28b_a_coarse_odometer_falls_back_to_displacement() {
    // Stream frames with a constant odometer: 0.0008 degrees (88.96 m) is
    // discarded, 0.0009 degrees (100.08 m) is kept.
    for (end_latitude, kept) in [(10.0008, false), (10.0009, true)] {
        let samples = [
            frame(1, T0, Some("D"), Some(10), 10.0, Some(1000.0)),
            frame(2, T0 + MIN, Some("P"), Some(0), end_latitude, Some(1000.0)),
        ];
        let result = run(&samples);
        assert_eq!(
            result.delta.drives.len(),
            usize::from(kept),
            "{end_latitude}"
        );
    }
}

#[test]
fn st29_a_coarse_odometer_that_did_not_move_still_keeps_a_displaced_drive() {
    let samples = [
        frame(1, T0, Some("D"), Some(10), 10.0, Some(1000.0)),
        frame(2, T0 + MIN, Some("P"), Some(0), 10.00135, Some(1000.0)),
    ];
    let result = run(&samples);
    assert_eq!(result.delta.drives.len(), 1);
    assert_eq!(result.delta.drives[0].distance_km, Some(0.0));
}

/// Full source, odometer constant, coordinates jump 150 m (ST29b, ST76).
pub(crate) fn st29b_samples() -> Vec<LifecycleSample> {
    vec![
        full(1, T0, json!("D"), json!(5), 10.0, Some(1000.0), None),
        full(
            2,
            T0 + MIN,
            json!("P"),
            json!(0),
            10.00135,
            Some(1000.0),
            None,
        ),
    ]
}

#[test]
fn st29b_a_gnss_jump_with_a_fine_odometer_is_discarded() {
    let samples = st29b_samples();
    let result = run(&samples);
    assert!(result.delta.drives.is_empty());
    assert_eq!(result.delta.discarded_drive_ids, vec![1]);
    assert_eq!(result.delta.positions.len(), 2);
}

#[test]
fn st30_a_drive_without_an_odometer_is_kept_by_displacement() {
    let samples = [
        full(1, T0, json!("D"), json!(30), 10.0, None, None),
        full(2, T0 + MIN, json!("D"), json!(30), 10.02, None, None),
        full(3, T0 + 2 * MIN, json!("P"), json!(0), 10.045, None, None),
    ];
    let result = run(&samples);
    assert_eq!(result.delta.drives.len(), 1);
    assert_eq!(result.delta.drives[0].distance_km, None);
}

#[test]
fn st31_a_round_trip_without_an_odometer_is_kept() {
    let samples = [
        full(1, T0, json!("D"), json!(30), 10.0, None, None),
        full(2, T0 + MIN, json!("D"), json!(30), 10.018, None, None),
        full(3, T0 + 2 * MIN, json!("D"), json!(30), 10.0, None, None),
        full(4, T0 + 3 * MIN, json!("P"), json!(0), 10.0, None, None),
    ];
    let result = run(&samples);
    assert_eq!(result.delta.drives.len(), 1);
    assert_eq!(result.delta.drives[0].distance_km, None);
}

#[test]
fn st32_a_return_within_the_gap_showing_park_closes_with_the_unobserved_part() {
    let mut samples = moving(1, T0, 2);
    samples.push(disc(3, T0 + MIN + 2 * MIN, "offline"));
    samples.push(parked(4, T0 + MIN + 8 * MIN, 10.01, Some(1000.5 + 3.0)));
    let before_offline = run(&samples[..2]);
    let offline = step_one(before_offline.state, &samples[2]);
    assert_eq!(offline.state.phase, VehiclePhase::Offline);
    assert!(offline.state.open_drive.as_ref().unwrap().saw_offline);
    assert!(offline.delta.drives.is_empty());
    let result = run(&samples);
    assert_eq!(result.delta.drives.len(), 1);
    let drive = &result.delta.drives[0];
    assert_eq!(drive.end_date_ms, T0 + MIN + 8 * MIN);
    // 1003.5 - 1000.0 miles: the three unobserved miles are included.
    assert!(close_to(drive.distance_km.unwrap(), 3.5 * K_MILE_KM));
}

#[test]
fn st33_a_return_after_the_gap_showing_park_ends_the_drive_at_its_last_fold() {
    let mut samples = moving(1, T0 - 2 * MIN, 3);
    samples.push(parked(4, T0 + 40 * MIN, 10.01, Some(1000.0 + 1.0 + 5.0)));
    let result = run(&samples);
    assert_eq!(result.delta.drives.len(), 1);
    let drive = &result.delta.drives[0];
    assert_eq!(drive.end_date_ms, T0);
    assert!(close_to(drive.distance_km.unwrap(), 1.0 * K_MILE_KM));
    // The return observation is a standalone position; the five miles driven
    // in the gap belong to no drive.
    assert_eq!(dates(&result.delta.positions), vec![T0 + 40 * MIN]);
    assert_eq!(result.delta.positions[0].drive_id, None);
}

#[test]
fn st34_a_return_after_the_gap_showing_motion_opens_a_new_drive_without_a_lead() {
    let mut samples = moving(1, T0 - 2 * MIN, 3);
    samples.push(driving(4, T0 + 40 * MIN, 10.5, Some(1010.0)));
    let result = run(&samples);
    assert_eq!(result.delta.drives.len(), 1);
    assert_eq!(result.delta.drives[0].end_date_ms, T0);
    let second = result.state.open_drive.as_ref().expect("second drive");
    assert_eq!(second.id, 2);
    assert_eq!(second.start_date_ms, T0 + 40 * MIN);
    // No lead: the first odometer is the one of the return observation.
    assert!(close_to(second.first_odometer.unwrap(), 1010.0 * K_MILE_KM));
    assert_eq!(second.start_latitude, Some(10.5));
}

#[test]
fn st35_a_drive_ending_while_unreachable_closes_on_the_no_data_event() {
    let mut samples = moving(1, T0 - MIN, 2);
    samples.push(disc(3, T0 + 5 * MIN, "online"));
    samples.push(event(4, T0 + 30 * MIN, "no_data"));
    let result = run(&samples);
    assert_eq!(result.delta.drives.len(), 1);
    assert_eq!(result.delta.drives[0].end_date_ms, T0);
    assert_eq!(result.state.phase, VehiclePhase::Online);
    // The state interval is unchanged by the event.
    assert_eq!(
        result
            .state
            .open_state
            .as_ref()
            .map(|open| open.state.as_str()),
        Some("online")
    );
    assert!(
        result
            .delta
            .states
            .iter()
            .all(|row| row.end_date_ms.is_none())
    );
}

#[test]
fn st36_asleep_closes_the_drive_at_its_last_fold() {
    let mut samples = moving(1, T0 - MIN, 2);
    samples.push(disc(3, T0 + 20 * MIN, "asleep"));
    let result = run(&samples);
    assert_eq!(result.delta.drives.len(), 1);
    assert_eq!(result.delta.drives[0].end_date_ms, T0);
    assert_eq!(result.state.phase, VehiclePhase::Asleep);
}

#[test]
fn st37_a_plug_in_parks_and_closes_the_drive_with_that_observation() {
    let mut samples = moving(1, T0 - 4 * MIN, 5);
    samples.push(vd(
        6,
        T0 + MIN,
        json!({
            "drive_state": {"timestamp": T0 + MIN, "shift_state": null, "speed": null,
                            "latitude": 10.005, "longitude": 20.0},
            "charge_state": {"timestamp": T0 + MIN, "charging_state": "Charging"},
        }),
    ));
    let result = run(&samples);
    assert_eq!(result.delta.drives.len(), 1);
    let drive = &result.delta.drives[0];
    assert_eq!(drive.end_date_ms, T0 + MIN);
    assert_eq!(drive.end_latitude, Some(10.005));
    assert!(result.state.open_drive.is_none());
    // The observation was folded into the drive that it closed: no second
    // position for it.
    assert!(result.delta.positions.is_empty());
}

#[test]
fn a_plugged_observation_without_fresh_drive_facts_closes_at_the_last_fold() {
    let mut samples = moving(1, T0 - MIN, 2);
    samples.push(vd(
        3,
        T0 + 2 * MIN,
        json!({"charge_state": {"timestamp": T0 + 2 * MIN, "charging_state": "Stopped"}}),
    ));
    let result = run(&samples);
    assert_eq!(result.delta.drives.len(), 1);
    assert_eq!(result.delta.drives[0].end_date_ms, T0);
}

#[test]
fn st38_aggregates_use_the_stored_values_and_only_fresh_readings() {
    let climate = |t: i64, outside: Option<f64>, inside: Option<f64>| {
        let mut fields = Map::new();
        fields.insert("timestamp".to_owned(), json!(t));
        if let Some(outside) = outside {
            fields.insert("outside_temp".to_owned(), json!(outside));
        }
        if let Some(inside) = inside {
            fields.insert("inside_temp".to_owned(), json!(inside));
        }
        Value::Object(fields)
    };
    let obs = |id: i64, k: i64, power: f64, climate: Value, extra: Value| {
        let t = T0 + k * MIN;
        let mut data = json!({
            "drive_state": {"timestamp": t, "shift_state": "D", "speed": 30, "power": power,
                            "latitude": 10.0123456 + 0.001 * k as f64, "longitude": 20.0},
            "climate_state": climate,
        });
        for (key, value) in extra.as_object().unwrap() {
            data[key] = value.clone();
        }
        vd(id, t, data)
    };
    let samples = [
        obs(
            1,
            0,
            -30.0,
            climate(T0, Some(10.0), Some(20.0)),
            json!({"charge_state": {"timestamp": T0, "ideal_battery_range": 200.123,
                                    "battery_range": 190.987, "battery_level": 80}}),
        ),
        obs(
            2,
            1,
            45.4,
            climate(T0 + MIN, Some(10.5), Some(20.4)),
            json!({}),
        ),
        obs(
            3,
            2,
            10.0,
            climate(T0 + 2 * MIN, Some(10.6), None),
            json!({}),
        ),
        // A climate group that is not newer than the previous one: stale.
        obs(
            4,
            3,
            120.0,
            climate(T0 + 2 * MIN, Some(30.0), Some(30.0)),
            json!({}),
        ),
        vd(
            5,
            T0 + 4 * MIN,
            json!({
                "drive_state": {"timestamp": T0 + 4 * MIN, "shift_state": "P", "speed": 0},
                "charge_state": {"timestamp": T0 + 4 * MIN, "ideal_battery_range": 190.5,
                                 "battery_range": 180.25, "battery_level": 78}
            }),
        ),
    ];
    let result = run(&samples);
    let drive = &result.delta.drives[0];
    // R49: the mean of the fresh readings (10.0, 10.5, 10.6 is 10.3667) is
    // rounded to one decimal.
    assert_eq!(drive.outside_temp_avg, Some(10.4));
    assert_eq!(drive.inside_temp_avg, Some(20.2));
    assert_eq!(drive.power_max, Some(120.0));
    assert_eq!(drive.power_min, Some(-30.0));
    let stored: Vec<_> = result
        .delta
        .open_drive_positions
        .iter()
        .map(|position| position.power)
        .collect();
    assert_eq!(
        stored,
        vec![Some(-30.0), Some(45.0), Some(10.0), Some(120.0)]
    );
    assert_eq!(drive.start_ideal_range_km, Some(322.07));
    assert_eq!(drive.start_rated_range_km, Some(307.36));
    assert_eq!(drive.end_ideal_range_km, Some(306.58));
    assert_eq!(drive.end_rated_range_km, Some(290.08));
    assert_eq!((drive.start_soc, drive.end_soc), (Some(80), Some(78)));
    assert_eq!(drive.start_latitude, Some(10.012346));
    assert_eq!(result.delta.open_drive_positions[0].latitude, 10.012346);
}

#[test]
fn st39_duration_rounds_to_the_nearest_minute_and_saturates() {
    let drive_of = |length_ms: i64| {
        let mut open = OpenDrive::empty(1, 1, T0);
        open.last_folded_at_ms = Some(T0 + length_ms);
        finalize_drive(open, true, None)
            .expect("finalize")
            .expect("kept")
            .drive
    };
    assert_eq!(drive_of(89_999).duration_min, Some(1));
    assert_eq!(drive_of(90_000).duration_min, Some(2));
    assert_eq!(drive_of(0).duration_min, Some(0));
    let forty_days = 40 * 24 * 60 * MIN;
    assert_eq!(drive_of(forty_days).duration_min, Some(K_SMALLINT_MAX));
}

#[test]
fn st40_the_ascent_descent_walk_ignores_quantisation_and_faults() {
    let walk = [100, 101, 100, 101, 100, 103, 102, 106, 104, 101];
    let some = |values: &[i64]| values.iter().map(|value| Some(*value)).collect::<Vec<_>>();
    assert_eq!(elevation_totals(some(&walk)), (Some(6), Some(5)));
    // A missing reading changes nothing.
    let mut with_gap = some(&walk);
    with_gap.insert(4, None);
    assert_eq!(elevation_totals(with_gap), (Some(6), Some(5)));
    // One valid value only: nothing can be measured.
    assert_eq!(elevation_totals([Some(100)]), (None, None));
    assert_eq!(
        elevation_totals([Some(100), None, Some(20_000)]),
        (None, None)
    );
    // Values outside -1000..=9000 are ignored.
    let mut faulty = some(&walk);
    faulty.insert(3, Some(20_000));
    faulty.insert(7, Some(-2_000));
    assert_eq!(elevation_totals(faulty), (Some(6), Some(5)));
    // The invariant ascent - descent == last reference - first reference.
    let mut walker = ElevationWalk::default();
    for value in walk {
        walker.step(Some(value));
    }
    assert_eq!(
        walker.ascent_m - walker.descent_m,
        walker.reference_m.unwrap() - 100
    );
    assert_eq!(walker.count, 10);
    // The band edge: exactly K_ELEV_BAND counts, one metre less does not.
    assert_eq!(
        elevation_totals([Some(0), Some(K_ELEV_BAND)]),
        (Some(2), Some(0))
    );
    assert_eq!(
        elevation_totals([Some(0), Some(K_ELEV_BAND - 1)]),
        (Some(0), Some(0))
    );
    assert_eq!(
        elevation_totals([Some(0), Some(-K_ELEV_BAND)]),
        (Some(0), Some(2))
    );
}

#[test]
fn st41_totals_saturate_at_the_smallint_limit() {
    let values = [0, 9_000, 0, 9_000, 0, 9_000, 0, 9_000];
    let (ascent, descent) = elevation_totals(values.iter().map(|value| Some(*value)));
    assert_eq!(ascent, Some(32_767), "36,000 saturated");
    assert_eq!(descent, Some(27_000));
    // The sums themselves use saturating arithmetic.
    let mut walker = ElevationWalk {
        reference_m: Some(0),
        count: 2,
        ascent_m: i64::MAX - 1,
        descent_m: 0,
    };
    walker.step(Some(1_000));
    assert_eq!(walker.ascent_m, i64::MAX);
}

#[test]
fn st42_the_live_walk_equals_the_one_function_over_the_stored_positions() {
    let with_elevation = |id: i64, k: i64, latitude: Option<f64>, elevation: i64| {
        let t = T0 + k * MIN;
        let mut drive =
            json!({"timestamp": t, "shift_state": "D", "speed": 30, "elevation": elevation});
        if let Some(latitude) = latitude {
            drive["latitude"] = json!(latitude);
            drive["longitude"] = json!(20.0);
        }
        vd(id, t, json!({"drive_state": drive}))
    };
    let samples = [
        with_elevation(1, 0, Some(10.0), 100),
        with_elevation(2, 1, Some(10.001), 110),
        // No coordinates: the fold carries an elevation but stores no
        // position, so the walk does not step.
        with_elevation(3, 2, None, 200),
        with_elevation(4, 3, Some(10.002), 100),
        with_elevation(5, 4, Some(10.003), 120),
        full(6, T0 + 5 * MIN, json!("P"), json!(0), 10.003, None, None),
    ];
    let result = run(&samples);
    let drive = &result.delta.drives[0];
    let stored: Vec<_> = result
        .delta
        .open_drive_positions
        .iter()
        .map(|position| position.elevation)
        .collect();
    assert_eq!(
        stored,
        vec![Some(100), Some(110), Some(100), Some(120), None]
    );
    let totals = elevation_totals(stored);
    assert_eq!(totals, (Some(30), Some(10)));
    assert_eq!((drive.ascent, drive.descent), totals);
}

#[test]
fn st43_parked_positions_are_stored_at_the_standalone_rate() {
    let samples: Vec<_> = (0..60)
        .map(|k| {
            let latitude = if k >= 50 { 10.0018 } else { 10.0 };
            parked(k + 1, T0 + k * MIN, latitude, None)
        })
        .collect();
    let result = run(&samples);
    // At t, +15m, +30m and +45m by time; +50m by movement (0.2 km tow).
    assert_eq!(
        dates(&result.delta.positions),
        vec![
            T0,
            T0 + 15 * MIN,
            T0 + 30 * MIN,
            T0 + 45 * MIN,
            T0 + 50 * MIN
        ]
    );
    assert!(result.delta.positions.iter().all(|p| p.drive_id.is_none()));
    assert_eq!(
        result.state.last_stationary_position_at_ms,
        Some(T0 + 50 * MIN)
    );
}

#[test]
fn st43_after_a_drive_closes_no_standalone_position_is_stored_for_15_minutes() {
    let mut samples = moving(1, T0, 3);
    let end = T0 + 3 * MIN;
    samples.push(parked(4, end, 10.003, Some(1002.0)));
    for k in 1..=15 {
        samples.push(parked(4 + k, end + k * MIN, 10.003, Some(1002.0)));
    }
    let result = run(&samples);
    assert_eq!(result.delta.drives.len(), 1);
    assert_eq!(result.delta.drives[0].end_date_ms, end);
    // The closing position stays with the drive; the next standalone one is
    // 15 minutes later at the same spot.
    assert_eq!(dates(&result.delta.positions), vec![end + 15 * MIN]);
}

#[test]
fn positions_need_coordinates_and_are_not_stored_twice() {
    let mut without_coordinates = full(1, T0, json!("P"), json!(0), 10.0, None, None);
    without_coordinates.payload["provider_raw_json"]["response"]["drive_state"]
        .as_object_mut()
        .unwrap()
        .remove("latitude");
    let first = step_one(OpenSessionState::new(), &without_coordinates);
    assert!(first.delta.positions.is_empty());
    assert!(first.state.last_stationary_position_at_ms.is_none());

    let located = full(2, T0 + SEC, json!("P"), json!(0), 10.0, None, None);
    let second = step_one(first.state, &located);
    assert_eq!(second.delta.positions.len(), 1);
    // Replaying the observation after a failed commit is a no-op (R2).
    let replay = step_one(second.state.clone(), &located);
    assert!(replay.delta.positions.is_empty());
    // A later observation with an older group time stores nothing either.
    let mut stale = full(3, T0 + 2 * SEC, json!("P"), json!(0), 10.5, None, None);
    stale.payload["provider_raw_json"]["response"]["drive_state"]["timestamp"] = json!(T0);
    let third = step_one(second.state, &stale);
    assert!(third.delta.positions.is_empty());
}

#[test]
fn standalone_positions_while_driving_are_drive_children() {
    let result = run(&moving(1, T0, 3));
    assert!(result.delta.positions.is_empty());
    assert_eq!(result.delta.open_drive_positions.len(), 3);
    assert!(
        result
            .delta
            .open_drive_positions
            .iter()
            .all(|position| position.drive_id == Some(1))
    );
}

// ----------------------------------------------------------- imported drive

fn imported_drive_session(start: i64, end_km: f64) -> TeslaMateOpenSession {
    let drive: TeslaMateDrive = serde_json::from_value(json!({
        "id": 70, "car_id": 1, "start_date_ms": start, "end_date_ms": null,
        "start_position_id": 700, "end_position_id": 701,
        "start_address_id": null, "end_address_id": null,
        "start_geofence_id": null, "end_geofence_id": null,
        "outside_temp_avg": 18.0, "inside_temp_avg": 20.0, "speed_max": 50,
        "power_max": 12.0, "power_min": -5.0,
        "start_ideal_range_km": 300.0, "end_ideal_range_km": 298.0,
        "start_rated_range_km": 280.0, "end_rated_range_km": 278.0,
        "start_km": 100.0, "end_km": end_km, "distance_km": null,
        "duration_min": null, "ascent": 7, "descent": 2
    }))
    .expect("imported drive");
    let mut session = TeslaMateOpenSession {
        car_id: 1,
        drive: Some(drive),
        drive_positions: vec![
            imported_position_fixture(700, 70, start, 47.5, 100.0, 80, 17.0, 19.0),
            imported_position_fixture(701, 70, start + MIN, 47.5, end_km, 78, 19.0, 21.0),
        ],
        ..Default::default()
    };
    session.watermarks.positions.max_timestamp_ms = Some(start + MIN);
    session
}

fn imported_live(
    id: i64,
    t: i64,
    gear: &str,
    speed: i64,
    latitude: f64,
    odometer_km: f64,
) -> LifecycleSample {
    vd(
        id,
        t,
        json!({
            "drive_state": {"timestamp": t, "shift_state": gear, "speed": speed,
                            "latitude": latitude, "longitude": 19.0},
            "vehicle_state": {"timestamp": t, "odometer": odometer_km / K_MILE_KM},
            "climate_state": {"timestamp": t, "inside_temp": 22.0, "outside_temp": 20.0},
        }),
    )
}

#[test]
fn st44_an_imported_open_drive_is_continued_and_closed_with_its_seeded_id() {
    let start = 1_800_100_000_000_i64;
    let session = imported_drive_session(start, 100.8);
    let seeded = seed_imported_open_session_state(uuid::Uuid::nil(), &session, None)
        .expect("seed imported drive");
    let open = seeded.open_drive.as_ref().expect("seeded drive");
    assert_eq!((open.id, open.position_count), (70, 2));
    assert_eq!(open.last_position_date_ms, Some(start + MIN));
    assert_eq!(open.start_latitude, Some(47.5));

    // A live observation below the watermark is stale: nothing changes.
    let stale = step_one(
        seeded.clone(),
        &imported_live(1, start - MIN, "D", 30, 47.6, 100.5),
    );
    assert_eq!(stale.state.open_drive, seeded.open_drive);
    assert!(stale.delta.open_drive_positions.is_empty());

    // Restart: the state decodes to the same drive.
    let restored = OpenSessionState::decode(&seeded.encode().expect("encode")).expect("decode");
    assert_eq!(restored.open_drive, seeded.open_drive);

    let continued = step_one(
        restored,
        &imported_live(2, start + 3 * MIN, "D", 30, 47.51, 100.9),
    );
    assert_eq!(continued.state.open_drive.as_ref().unwrap().id, 70);
    assert_eq!(continued.delta.open_drive_positions.len(), 1);
    assert_eq!(continued.delta.open_drive_positions[0].drive_id, Some(70));
    let closed = step_one(
        continued.state,
        &imported_live(3, start + 6 * MIN, "P", 0, 47.52, 101.0),
    );
    assert_eq!(closed.delta.drives.len(), 1);
    let drive = &closed.delta.drives[0];
    assert_eq!(drive.id, 70);
    assert_eq!(drive.start_date_ms, start);
    assert_eq!(drive.end_date_ms, start + 6 * MIN);
    assert!(close_to(drive.distance_km.unwrap(), 1.0));
    assert_eq!(drive.start_latitude, Some(47.5));
    assert_eq!(drive.end_latitude, Some(47.52));
    // R42, R49: one mean over every reading, imported and live. Outside:
    // (17 + 19 + 20 + 20) / 4; inside: (19 + 21 + 22 + 22) / 4.
    assert_eq!(drive.outside_temp_avg, Some(19.0));
    assert_eq!(drive.inside_temp_avg, Some(21.0));
    assert_eq!((drive.ascent, drive.descent), (Some(7), Some(2)));
    // The imported children are not rewritten: only the live position joins.
    assert_eq!(
        closed
            .delta
            .open_drive_positions
            .iter()
            .map(|position| position.id)
            .collect::<Vec<_>>(),
        vec![703]
    );
    assert!(closed.state.open_drive.is_none());
}

#[test]
fn st44_an_imported_drive_that_does_not_move_live_is_kept_not_discarded() {
    let start = 1_800_100_000_000_i64;
    let session = imported_drive_session(start, 100.0);
    let seeded = seed_imported_open_session_state(uuid::Uuid::nil(), &session, None)
        .expect("seed imported drive");
    let closed = step_one(
        seeded,
        &imported_live(1, start + 2 * MIN, "P", 0, 47.5, 100.0),
    );
    assert_eq!(closed.delta.drives.len(), 1);
    assert_eq!(closed.delta.drives[0].id, 70);
    assert!(close_to(closed.delta.drives[0].distance_km.unwrap(), 0.0));
    assert!(closed.delta.discarded_drive_ids.is_empty());
}

#[test]
fn an_imported_drive_refresh_keeps_hub_continuation_state() {
    let start = 1_800_100_000_000_i64;
    let mut session = imported_drive_session(start, 100.8);
    let seeded = seed_imported_open_session_state(uuid::Uuid::nil(), &session, None).unwrap();
    let mut continued = seeded.clone();
    continued.open_drive.as_mut().unwrap().saw_offline = true;
    session.watermarks.updates.max_id = Some(44);
    let watermark_only =
        seed_imported_open_session_state(uuid::Uuid::nil(), &session, Some(&continued)).unwrap();
    assert!(watermark_only.open_drive.as_ref().unwrap().saw_offline);

    session.drive_positions[1].speed = Some(65);
    let same_count =
        seed_imported_open_session_state(uuid::Uuid::nil(), &session, Some(&watermark_only))
            .unwrap();
    assert_eq!(same_count.open_drive.as_ref().unwrap().speed_max, Some(65));

    session.drive_positions.push(imported_position_fixture(
        702,
        70,
        start + 90_000,
        47.515,
        100.9,
        77,
        20.0,
        22.0,
    ));
    session.watermarks.positions.max_id = Some(702);
    session.watermarks.positions.max_timestamp_ms = Some(start + 90_000);
    let refreshed =
        seed_imported_open_session_state(uuid::Uuid::nil(), &session, Some(&same_count)).unwrap();
    let drive = refreshed.open_drive.as_ref().unwrap();
    assert_eq!(drive.position_count, 3);
    assert_eq!(drive.last_position_date_ms, Some(start + 90_000));
}

// ------------------------------------------------------- continuity and merge

#[test]
fn a_restart_mid_drive_gives_the_rows_of_an_uninterrupted_projection() {
    let samples = [
        full(1, T0, json!("D"), json!(10), 10.0, Some(1000.0), Some(60)),
        full(
            2,
            T0 + 30 * SEC,
            json!("D"),
            json!(20),
            10.01,
            Some(1000.5),
            Some(59),
        ),
        full(
            3,
            T0 + 60 * SEC,
            json!("P"),
            json!(0),
            10.02,
            Some(1001.0),
            Some(58),
        ),
    ];
    let continuous = run(&samples);

    let after_first = step_one(OpenSessionState::new(), &samples[0]);
    let restored =
        OpenSessionState::decode(&after_first.state.encode().expect("encode")).expect("decode");
    assert!(restored.open_drive.is_some());
    let after_second = step_one(restored, &samples[1]);
    // Replay of observation 1 is a no-op after the restart.
    let replay = step_one(after_second.state.clone(), &samples[0]);
    assert!(replay.delta.drives.is_empty() && replay.delta.open_drive_positions.is_empty());
    let after_third = step_one(after_second.state, &samples[2]);

    assert_eq!(after_third.delta.drives, continuous.delta.drives);
    let mut interrupted = after_first.delta.open_drive_positions;
    interrupted.extend(after_second.delta.open_drive_positions);
    interrupted.extend(after_third.delta.open_drive_positions);
    assert_eq!(interrupted, continuous.delta.open_drive_positions);
    assert_eq!(after_third.state, continuous.state);
}

#[test]
fn a_stale_park_with_a_regressed_time_neither_closes_nor_moves_the_drive() {
    let open = step_one(
        OpenSessionState::new(),
        &full(1, T0, json!("D"), json!(5), 10.0, None, None),
    );
    let stale = step_one(
        open.state.clone(),
        &full(2, T0 - 1, json!("P"), json!(0), 10.0, None, None),
    );
    assert!(stale.state.open_drive.is_some());
    assert_eq!(stale.state.last_observation_id, 2);
    assert_eq!(stale.state.last_observed_at_ms, Some(T0));
    assert!(stale.delta.drives.is_empty());
    assert!(stale.delta.positions.is_empty() && stale.delta.open_drive_positions.is_empty());
}

#[test]
fn a_service_refusal_closes_the_drive_at_its_last_position_and_persists_the_flag() {
    let mut state = OpenSessionState::new();
    for sample in &moving(1, T0, 2) {
        state = step_one(state, sample).state;
    }
    let step = force_close_for_service(state, 1, T0 + 5 * SEC).expect("service close");
    assert_eq!(step.state.service_mode, Some(true));
    assert!(step.state.open_drive.is_none());
    assert_eq!(step.delta.drives.len(), 1);
    assert_eq!(step.delta.drives[0].end_date_ms, T0 + MIN);
    // The exit: a later full-source answer that states false clears the flag.
    let exited = step_one(
        step.state,
        &vd(
            9,
            T0 + 10 * MIN,
            json!({"vehicle_state": {"timestamp": T0 + 10 * MIN, "service_mode": false}}),
        ),
    );
    assert_eq!(exited.state.service_mode, Some(false));
}

#[test]
fn thermal_flags_survive_sparse_stream_frames_until_the_drive_closes() {
    let t0 = 1_800_001_000_000_i64;
    let owner = vd(
        1,
        t0,
        json!({
            "drive_state": {"shift_state": "D", "speed": 20, "power": 12.5, "latitude": 51.0,
                            "longitude": -0.1, "timestamp": t0},
            "charge_state": {"battery_level": 70, "battery_heater_on": true,
                             "est_battery_range": 200.0},
            "climate_state": {"battery_heater": true, "battery_heater_no_power": false,
                              "fan_status": 2, "driver_temp_setting": 21.5,
                              "passenger_temp_setting": 22.0, "is_rear_defroster_on": false,
                              "is_front_defroster_on": true},
            "vehicle_state": {"odometer": 1000.0, "tpms_pressure_fl": 2.4,
                              "tpms_pressure_fr": 2.5, "tpms_pressure_rl": 2.6,
                              "tpms_pressure_rr": 2.7}
        }),
    );
    let sparse_stream = LifecycleSample {
        observation_id: 2,
        observed_at_ms: t0 + SEC,
        vehicle_state: "online".to_owned(),
        payload: json!({
            "record_type": "tesla_stream_update_v1",
            "fields": {"drive_state": {"timestamp": t0 + SEC, "shift_state": "D", "speed": 21,
                                       "latitude": 51.001, "longitude": -0.101},
                       "vehicle_state": {"odometer": 1001.0}}
        }),
    };
    let parked = vd(
        3,
        t0 + 2 * SEC,
        json!({"drive_state": {"shift_state": "P", "speed": 0, "timestamp": t0 + 2 * SEC}}),
    );
    let result = run(&[owner, sparse_stream, parked]);
    assert_eq!(result.delta.drives.len(), 1);
    let positions = result.delta.open_drive_positions;
    assert_eq!(positions.len(), 2);
    for (index, position) in positions.into_iter().enumerate() {
        assert_eq!(position.speed, Some(if index == 0 { 32 } else { 34 }));
        // Power is stored as a whole kilowatt (R35): 12.5 rounds to 13.
        assert_eq!(position.power, (index == 0).then_some(13.0));
        assert_eq!(position.est_battery_range_km, Some(321.87));
        assert_eq!(position.fan_status, Some(2));
        assert_eq!(position.driver_temp_setting, Some(21.5));
        assert_eq!(position.passenger_temp_setting, Some(22.0));
        assert_eq!(position.is_rear_defroster_on, Some(false));
        assert_eq!(position.is_front_defroster_on, Some(true));
        assert_eq!(position.battery_heater, Some(true));
        assert_eq!(position.battery_heater_on, Some(true));
        assert_eq!(position.battery_heater_no_power, Some(false));
        assert_eq!(position.tpms_pressure_fl, Some(2.4));
        assert_eq!(position.tpms_pressure_fr, Some(2.5));
        assert_eq!(position.tpms_pressure_rl, Some(2.6));
        assert_eq!(position.tpms_pressure_rr, Some(2.7));
    }
}

#[test]
fn st77_a_mixed_source_odometer_never_gives_a_negative_distance() {
    // V 1000.456789, then a stream frame at 1000.4 (a coarser reading of the
    // same car), then V 1001.123456.
    let samples = [
        full(1, T0, json!("D"), json!(30), 10.0, Some(1000.456789), None),
        frame(2, T0 + 10 * SEC, Some("D"), Some(30), 10.001, Some(1000.4)),
        full(
            3,
            T0 + 20 * SEC,
            json!("P"),
            json!(0),
            10.002,
            Some(1001.123456),
            None,
        ),
    ];
    let result = run(&samples);
    let drive = &result.delta.drives[0];
    let expected = (1001.123456 - 1000.456789) * K_MILE_KM;
    assert!((drive.distance_km.unwrap() - expected).abs() < 1e-9);
    assert!(drive.distance_km.unwrap() >= 0.0);
    // The same readings in the opposite order: the lower coarse reading
    // comes last and is not taken as the last odometer.
    let samples = [
        full(1, T0, json!("D"), json!(30), 10.0, Some(1000.456789), None),
        frame(2, T0 + 10 * SEC, Some("D"), Some(30), 10.001, Some(1000.4)),
        frame(3, T0 + 20 * SEC, Some("P"), Some(0), 10.002, Some(1000.4)),
    ];
    let result = run(&samples);
    let drive = &result.delta.drives[0];
    assert_eq!(
        drive.distance_km,
        Some(0.0),
        "never below the first reading"
    );
}

#[test]
fn st88_the_lead_is_taken_only_within_t_gap() {
    let samples = [
        parked(1, T0, 10.0, Some(1000.0)),
        driving(2, T0 + 11 * MIN, 10.5, Some(1000.5)),
    ];
    let result = run(&samples);
    let open = result.state.open_drive.as_ref().expect("drive");
    assert!(close_to(open.first_odometer.unwrap(), 1000.5 * K_MILE_KM));
    assert_eq!(open.start_latitude, Some(10.5));
    // The same sequence inside T_gap takes the lead.
    let samples = [
        parked(1, T0, 10.0, Some(1000.0)),
        driving(2, T0 + 10 * MIN, 10.5, Some(1000.5)),
    ];
    let result = run(&samples);
    let open = result.state.open_drive.as_ref().expect("drive");
    assert!(close_to(open.first_odometer.unwrap(), 1000.0 * K_MILE_KM));
    assert_eq!(open.start_latitude, Some(10.0));
    assert!(
        result.state.parked_snapshot.is_none(),
        "cleared by the open"
    );
}

#[test]
fn the_lead_takes_the_snapshot_values_and_the_opening_fold_fills_the_rest() {
    let samples = [
        vd(
            1,
            T0,
            json!({
                "drive_state": {"timestamp": T0, "shift_state": null, "speed": null,
                                "latitude": 10.0, "longitude": 20.0},
                "charge_state": {"timestamp": T0, "battery_level": 64, "ideal_battery_range": 150.0},
                "vehicle_state": {"timestamp": T0, "odometer": 1000.0},
            }),
        ),
        vd(
            2,
            T0 + MIN,
            json!({
                "drive_state": {"timestamp": T0 + MIN, "shift_state": "D", "speed": 30,
                                "latitude": 10.001, "longitude": 20.0},
                "charge_state": {"timestamp": T0 + MIN, "battery_level": 63, "battery_range": 140.0},
            }),
        ),
    ];
    let result = run(&samples);
    let open = result.state.open_drive.as_ref().expect("drive");
    assert_eq!(open.start_soc, Some(64), "from the snapshot");
    assert_eq!(open.last_soc, Some(63));
    assert_eq!(open.start_ideal_range_km, Some(241.4), "from the snapshot");
    // The snapshot had no rated range, so the opening fold supplies it.
    assert_eq!(open.start_rated_range_km, Some(225.31));
    assert_eq!(open.start_date_ms, T0 + MIN, "the start time is not moved");
}

#[test]
fn a_drive_never_starts_before_a_drive_closed_in_the_same_observation() {
    // A drive closed for a gap and a new one opened by the same observation:
    // the second starts no earlier than the first ended.
    let mut samples = moving(1, T0 - 2 * MIN, 3);
    samples.push(driving(4, T0 + 40 * MIN, 10.5, Some(1010.0)));
    let result = run(&samples);
    assert!(
        result.state.open_drive.as_ref().unwrap().start_date_ms
            >= result.delta.drives[0].end_date_ms
    );
}

#[test]
fn a_drive_that_does_not_fit_the_id_space_fails_with_identifier_exhausted() {
    let mut state = OpenSessionState::new();
    state.next_drive_id = i64::MAX;
    assert_eq!(
        apply_sample(state, 1, &driving(1, T0, 10.0, None)),
        Err(LifecycleError::IdentifierExhausted)
    );
    let mut state = OpenSessionState::new();
    state.next_position_id = i64::MAX;
    let result = apply_sample(state, 1, &driving(1, T0, 10.0, None));
    assert_eq!(result, Err(LifecycleError::IdentifierExhausted));
}

#[test]
fn old_blobs_without_the_fold_fields_read_their_fallbacks() {
    let mut open = OpenDrive::empty(5, 1, T0);
    open.last_position_date_ms = Some(T0 + 2 * MIN);
    open.last_elevation = Some(120);
    assert_eq!(open.effective_last_folded_ms(), T0 + 2 * MIN);
    assert_eq!(open.effective_last_motion_ms(), T0 + 2 * MIN);
    assert_eq!(open.effective_elevation_count(), 1);
    let bare = OpenDrive::empty(6, 1, T0);
    assert_eq!(bare.effective_last_folded_ms(), T0);
    assert_eq!(bare.effective_elevation_count(), 0);
}

#[test]
fn merge_moves_children_of_discarded_and_ended_drives_to_standalone_positions() {
    let position = |id: i64, drive: Option<i64>, date_ms: i64| ProjectionPosition {
        id,
        drive_id: drive,
        car_id: 1,
        date_ms,
        latitude: 10.0,
        longitude: 20.0,
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
    };
    let mut total = LifecycleDelta {
        open_drive_positions: vec![
            position(1, Some(1), 100),
            position(2, Some(1), 200),
            position(3, Some(2), 300),
            position(4, Some(3), 400),
        ],
        ..LifecycleDelta::default()
    };
    let drive = |id: i64, end: i64| {
        let mut open = OpenDrive::empty(id, 1, 0);
        open.last_folded_at_ms = Some(end);
        finalize_drive(open, true, None).unwrap().unwrap().drive
    };
    // Drive 1 closes at 150 (the child at 200 is after its end), drive 2 is
    // discarded, drive 3 stays open.
    let step = LifecycleDelta {
        drives: vec![drive(1, 150)],
        discarded_drive_ids: vec![2],
        ..LifecycleDelta::default()
    };
    merge_step_delta(&mut total, step);
    assert_eq!(
        total
            .open_drive_positions
            .iter()
            .map(|p| (p.id, p.drive_id))
            .collect::<Vec<_>>(),
        vec![(1, Some(1)), (4, Some(3))]
    );
    assert_eq!(
        total
            .positions
            .iter()
            .map(|p| (p.id, p.drive_id))
            .collect::<Vec<_>>(),
        vec![(2, None), (3, None)]
    );
    assert_eq!(total.discarded_drive_ids, vec![2]);
    assert_eq!(total.drives.len(), 1);
}

// ---------------------------------------------------------------- geofences

fn fence(id: i64, name: &str, latitude: f64, longitude: f64, radius_m: f64) -> GeofenceFence {
    GeofenceFence {
        source_geofence_id: id,
        name: name.to_owned(),
        latitude,
        longitude,
        radius_m,
        billing_type: None,
        cost_per_unit: None,
        session_fee: None,
    }
}

fn point(latitude: f64, longitude: f64) -> crate::location::Wgs84Point {
    crate::location::Wgs84Point::new(latitude, longitude).expect("valid point")
}

fn name_at(latitude: f64, longitude: f64, fences: &[GeofenceFence]) -> Option<String> {
    match_geofence_name(latitude, longitude, fences)
}

#[test]
fn st45_a_point_on_the_boundary_is_inside() {
    let centre = point(10.0, 20.0);
    let p = point(10.0005, 20.0003);
    let exact = crate::location::great_circle_distance_m(centre, p);
    let on_boundary = [fence(1, "Home", 10.0, 20.0, exact)];
    assert_eq!(
        name_at(p.latitude, p.longitude, &on_boundary).as_deref(),
        Some("Home")
    );
    let just_inside = [fence(1, "Home", 10.0, 20.0, exact.next_up())];
    assert!(match_geofence(p.latitude, p.longitude, &just_inside).is_some());
    let just_outside = [fence(1, "Home", 10.0, 20.0, exact.next_down())];
    assert!(match_geofence(p.latitude, p.longitude, &just_outside).is_none());
}

#[test]
fn st46_the_smallest_radius_wins_then_the_nearest_centre_then_the_lowest_id() {
    let (latitude, longitude) = (10.0, 20.0);
    // A centre at the point (radius 500, id 1) against a fence 50 m away
    // (radius 100, id 2): the smaller radius wins.
    let fences = [
        fence(1, "Area", latitude, longitude, 500.0),
        fence(2, "Garage", 10.00045, longitude, 100.0),
    ];
    assert_eq!(
        name_at(latitude, longitude, &fences).as_deref(),
        Some("Garage")
    );
    // The same centre and radius, ids 7 and 3, in both slice orders: id 3.
    let twin = |id: i64, name: &str| fence(id, name, 10.00027, longitude, 100.0);
    for fences in [
        [twin(7, "Seven"), twin(3, "Three")],
        [twin(3, "Three"), twin(7, "Seven")],
    ] {
        assert_eq!(
            name_at(latitude, longitude, &fences).as_deref(),
            Some("Three")
        );
    }
    // Equal radius, centres 10 m (id 7) and 40 m (id 3) away: the distance
    // comes before the id.
    let fences = [
        fence(3, "Far", 10.00036, longitude, 100.0),
        fence(7, "Near", 10.00009, longitude, 100.0),
    ];
    assert_eq!(
        name_at(latitude, longitude, &fences).as_deref(),
        Some("Near")
    );
}

#[test]
fn st47_the_antimeridian_and_the_poles_need_no_special_case() {
    let across = [fence(1, "Line", 0.0, 179.9999, 50.0)];
    assert_eq!(name_at(0.0, -179.9999, &across).as_deref(), Some("Line"));
    let distance =
        crate::location::great_circle_distance_m(point(0.0, 179.9999), point(0.0, -179.9999));
    assert!((distance - 22.24).abs() < 0.01, "{distance}");
    let pole = [fence(1, "Pole", 90.0, 0.0, 20.0)];
    assert_eq!(name_at(89.9999, 135.0, &pole).as_deref(), Some("Pole"));
    assert_eq!(name_at(90.0, -120.0, &pole).as_deref(), Some("Pole"));
    assert_eq!(name_at(89.99, 135.0, &pole), None);
}

#[test]
fn st48_tiny_radii_and_invalid_input_match_nothing() {
    let tiny = [fence(1, "Tiny", 10.0, 20.0, 0.05)];
    assert_eq!(name_at(10.0, 20.0, &tiny).as_deref(), Some("Tiny"));
    assert_eq!(name_at(10.000001, 20.0, &tiny), None, "0.111 m away");
    assert_eq!(name_at(10.0, 20.000001, &tiny), None);
    let fences = [fence(1, "Home", 10.0, 20.0, 500.0)];
    assert_eq!(name_at(f64::NAN, 0.0, &fences), None);
    assert_eq!(name_at(91.0, 0.0, &fences), None);
    assert_eq!(name_at(10.0, f64::INFINITY, &fences), None);
    let unusable = [
        fence(1, "Zero", 10.0, 20.0, 0.0),
        fence(2, "Negative", 10.0, 20.0, -5.0),
        fence(3, "NaN radius", 10.0, 20.0, f64::NAN),
        fence(4, "NaN centre", f64::NAN, 20.0, 500.0),
        fence(5, "Off the globe", 95.0, 20.0, 500.0),
        fence(6, "Infinite radius", 10.0, 20.0, f64::INFINITY),
    ];
    assert_eq!(name_at(10.0, 20.0, &unusable), None);
    // An unusable fence does not hide a usable one.
    let mixed = [unusable[0].clone(), fence(9, "Real", 10.0, 20.0, 10.0)];
    assert_eq!(name_at(10.0, 20.0, &mixed).as_deref(), Some("Real"));
}

fn labelled_drive(start: Option<(f64, f64)>, end: Option<(f64, f64)>) -> ProjectionDrive {
    ProjectionDrive {
        id: 1,
        car_id: 1,
        optimized_at_ms: None,
        start_date_ms: 1,
        end_date_ms: 2,
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
        start_geofence: Some("Stale".into()),
        end_geofence: Some("Stale".into()),
        start_latitude: start.map(|value| value.0),
        start_longitude: start.map(|value| value.1),
        end_latitude: end.map(|value| value.0),
        end_longitude: end.map(|value| value.1),
        start_soc: None,
        end_soc: None,
        start_rated_range_km: None,
        end_rated_range_km: None,
        ascent: None,
        descent: None,
    }
}

#[test]
fn st49_a_deleted_fence_relabels_to_the_next_match_or_none_identically() {
    let home = fence(1, "Home", 10.0, 20.0, 150.0);
    let area = fence(2, "Area", 10.0, 20.0, 800.0);
    let mut delta = LifecycleDelta {
        drives: vec![labelled_drive(Some((10.0, 20.0)), Some((10.002, 20.0)))],
        ..LifecycleDelta::default()
    };
    apply_geofence_labels(&mut delta, &[home.clone(), area.clone()]);
    assert_eq!(delta.drives[0].start_geofence.as_deref(), Some("Home"));
    assert_eq!(delta.drives[0].end_geofence.as_deref(), Some("Area"));
    // Delete "Home": the label becomes the next matching fence, and a second
    // relabel gives the same result.
    for _ in 0..2 {
        apply_geofence_labels(&mut delta, std::slice::from_ref(&area));
        assert_eq!(delta.drives[0].start_geofence.as_deref(), Some("Area"));
        assert_eq!(delta.drives[0].end_geofence.as_deref(), Some("Area"));
    }
    // Delete every fence: no label, again identically.
    for _ in 0..2 {
        apply_geofence_labels(&mut delta, &[]);
        assert_eq!(delta.drives[0].start_geofence, None);
        assert_eq!(delta.drives[0].end_geofence, None);
    }
}

#[test]
fn st50_drive_labels_come_from_the_stored_coordinates() {
    let fences = [
        fence(1, "Home", 51.0, -0.1, 150.0),
        fence(2, "Work", 51.001, -0.101, 150.0),
    ];
    let mut delta = LifecycleDelta {
        drives: vec![
            labelled_drive(Some((51.0, -0.1)), Some((51.001, -0.101))),
            labelled_drive(None, None),
            labelled_drive(Some((52.0, 0.0)), Some((51.0, -0.1))),
        ],
        ..LifecycleDelta::default()
    };
    apply_geofence_labels(&mut delta, &fences);
    assert_eq!(delta.drives[0].start_geofence.as_deref(), Some("Home"));
    assert_eq!(delta.drives[0].end_geofence.as_deref(), Some("Work"));
    assert_eq!(delta.drives[1].start_geofence, None);
    assert_eq!(delta.drives[1].end_geofence, None);
    assert_eq!(delta.drives[2].start_geofence, None);
    assert_eq!(delta.drives[2].end_geofence.as_deref(), Some("Home"));
}

#[test]
fn an_explicit_park_ends_the_drive_at_its_own_time() {
    // R43 item 6, R44, R48a: the Park fact is dated by its group time. A later
    // position (a position-only drive group delivered first, which carries no
    // motion fact and so leaves the motion watermark where it was) is folded
    // but does not stretch the end; the drive ends at the Park time and that
    // later position is dated after the end.
    let samples = [
        driving(1, T0, 10.0, Some(1000.0)),
        vd(
            2,
            T0 + 2 * MIN,
            json!({"drive_state": {"timestamp": T0 + 2 * MIN,
                                   "latitude": 10.002, "longitude": 20.0}}),
        ),
        vd(
            3,
            T0 + 3 * MIN,
            json!({"drive_state": {"timestamp": T0 + MIN, "shift_state": "P", "speed": 0,
                                   "latitude": 10.001, "longitude": 20.0},
                   "vehicle_state": {"timestamp": T0 + MIN, "odometer": 1000.2}}),
        ),
    ];
    let result = run(&samples);
    assert_eq!(result.delta.drives.len(), 1, "{:?}", result.delta);
    assert_eq!(result.delta.drives[0].end_date_ms, T0 + MIN);
    assert_eq!(result.delta.drives[0].duration_min, Some(1));
    // R48a: the later position is standalone, and the drive keeps the rest.
    assert_eq!(
        result
            .delta
            .positions
            .iter()
            .map(|position| (position.date_ms, position.drive_id))
            .collect::<Vec<_>>(),
        vec![(T0 + 2 * MIN, None)]
    );
    assert!(
        result
            .delta
            .open_drive_positions
            .iter()
            .all(|position| position.date_ms <= T0 + MIN)
    );

    // The ordinary order is unchanged: the Park fact is the latest fold.
    let ordinary = run(&[
        driving(1, T0, 10.0, Some(1000.0)),
        full(
            2,
            T0 + MIN,
            json!("P"),
            json!(0),
            10.002,
            Some(1000.2),
            None,
        ),
    ]);
    assert_eq!(ordinary.delta.drives[0].end_date_ms, T0 + MIN);
}

// ---------------------------------------------------- exact boundaries (R43, R47)

#[test]
fn the_keep_rule_keeps_a_distance_of_exactly_the_threshold() {
    // R47: `distance_km >= K_DMIN_KM` keeps the drive. In f64, 0.0621371192237334
    // mile times 1.609344 is exactly 0.1; the next value down is below it.
    let exact = 0.062_137_119_223_733_4_f64;
    let below = 0.062_137_119_223_733_39_f64;
    assert_eq!(miles_to_km(exact), K_DMIN_KM);
    assert!(miles_to_km(below) < K_DMIN_KM);
    for (miles, kept) in [(exact, true), (below, false)] {
        let result = run(&[
            full(1, T0, json!("D"), json!(10), 10.0, Some(0.0), None),
            full(2, T0 + MIN, json!("P"), json!(0), 10.0, Some(miles), None),
        ]);
        assert_eq!(result.delta.drives.len(), usize::from(kept), "{miles}");
        assert_eq!(
            result.delta.discarded_drive_ids.len(),
            usize::from(!kept),
            "{miles}"
        );
        if kept {
            assert_eq!(result.delta.drives[0].distance_km, Some(K_DMIN_KM));
        }
    }
}

#[test]
fn a_gap_of_exactly_t_gap_keeps_the_drive_and_one_millisecond_more_closes_it() {
    // R43 item 3: a fresh motion fold more than `T_gap` after the last fold is
    // a loss-of-evidence close. `T_gap` is 600 s.
    let t_gap = i64::try_from(DEFAULT_OFFLINE_DRIVE_TIMEOUT.as_millis()).expect("ms");
    assert_eq!(t_gap, 600_000);
    let base = moving(1, T0 - MIN, 2);
    let state = run(&base).state;
    assert!(state.open_drive.is_some());

    let at_gap = step_one(state.clone(), &driving(3, T0 + t_gap, 10.002, Some(1001.0)));
    assert!(
        at_gap.delta.drives.is_empty(),
        "exactly T_gap continues the drive"
    );
    let open = at_gap.state.open_drive.as_ref().expect("still open");
    assert_eq!((open.id, open.position_count), (1, 3));

    let past_gap = step_one(state, &driving(3, T0 + t_gap + 1, 10.002, Some(1002.0)));
    assert_eq!(past_gap.delta.drives.len(), 1);
    assert_eq!(past_gap.delta.drives[0].id, 1);
    assert_eq!(past_gap.delta.drives[0].end_date_ms, T0);
    // The same observation opens a second drive, without a lead (R41a: the
    // parked spot is older than `T_gap`).
    let second = past_gap.state.open_drive.as_ref().expect("a second drive");
    assert_eq!(second.id, 2);
    assert_eq!(second.start_date_ms, T0 + t_gap + 1);
    assert_eq!(second.first_odometer, Some(1002.0 * K_MILE_KM));
}

#[test]
fn an_ordinary_observation_after_t_gap_closes_the_drive_at_its_last_fold() {
    // R43 item 4: with no fresh motion or position family, `t_obs - last fold >
    // T_gap` is a loss-of-evidence close, for a discovery and for a
    // `vehicle_data` that carries no drive fact.
    let t_gap = 600_000;
    let state = run(&moving(1, T0 - MIN, 2)).state;
    type Maker = fn(i64, i64) -> LifecycleSample;
    let makers: [(&str, Maker); 2] = [
        ("discovery", |id, t| disc(id, t, "online")),
        ("charge-only vehicle_data", |id, t| {
            vd(
                id,
                t,
                json!({"charge_state": {"timestamp": t, "battery_level": 70}}),
            )
        }),
    ];
    for (label, make) in makers {
        let before = step_one(state.clone(), &make(3, T0 + t_gap));
        assert!(
            before.delta.drives.is_empty(),
            "{label}: T_gap exactly stays open"
        );
        assert!(before.state.open_drive.is_some(), "{label}");

        let after = step_one(state.clone(), &make(3, T0 + t_gap + 1));
        assert_eq!(after.delta.drives.len(), 1, "{label}");
        assert_eq!(after.delta.drives[0].end_date_ms, T0, "{label}");
        assert!(after.state.open_drive.is_none(), "{label}");
        assert_eq!(after.state.phase, VehiclePhase::Online, "{label}");
        // The state interval is unchanged by the close.
        assert_eq!(
            after
                .state
                .open_state
                .as_ref()
                .map(|open| open.state.as_str()),
            Some("online"),
            "{label}"
        );
        assert!(
            after
                .delta
                .states
                .iter()
                .all(|row| row.end_date_ms.is_none()),
            "{label}"
        );
    }
}

// ----------------------------------------------- conflicting evidence (R22, ST72)

#[test]
fn st72_equal_times_give_motion_the_win_and_nothing_closes_or_opens() {
    // (e) consequences: the drive stays open, the observation is folded, and no
    // charge opens, for every plugged state.
    for plugged in ["Charging", "Stopped", "Complete"] {
        let state = run(&moving(1, T0 - MIN, 2)).state;
        let result = step_one(
            state,
            &vd(
                3,
                T0 + MIN,
                json!({
                    "drive_state": {"timestamp": T0 + MIN, "shift_state": "D", "speed": 30,
                                    "latitude": 10.002, "longitude": 20.0},
                    "charge_state": {"timestamp": T0 + MIN, "charging_state": plugged},
                }),
            ),
        );
        assert!(result.delta.drives.is_empty(), "{plugged}");
        let open = result.state.open_drive.as_ref().expect("still open");
        assert_eq!(open.position_count, 3, "{plugged}: folded");
        assert!(result.state.open_charge.is_none(), "{plugged}");
        assert!(result.delta.charges.is_empty(), "{plugged}");
    }
}

#[test]
fn st72_a_motion_fold_that_wins_by_time_keeps_the_drive_open() {
    // (a) Drive open; the drive group at +1m, a Complete charge group at +55s:
    // Motion wins, no close, one position at +1m.
    let state = run(&moving(1, T0 - MIN, 2)).state;
    let result = step_one(
        state,
        &vd(
            3,
            T0 + MIN,
            json!({
                "drive_state": {"timestamp": T0 + MIN, "shift_state": "D", "speed": 20,
                                "latitude": 10.002, "longitude": 20.0},
                "charge_state": {"timestamp": T0 + 55 * SEC, "charging_state": "Complete"},
            }),
        ),
    );
    assert!(result.delta.drives.is_empty());
    assert!(result.state.open_drive.is_some());
    assert_eq!(dates(&result.delta.open_drive_positions), vec![T0 + MIN]);
    assert!(result.state.open_charge.is_none());
}

#[test]
fn st72_plugged_evidence_that_wins_by_time_folds_the_drive_group_and_closes_it() {
    // (b) Drive open; the drive group at +55s, a Charging group at +1m: plugged
    // wins, the drive folds its own group (a position and its end coordinates)
    // and ends at +55s, and the charge opens at +1m with its first sample then.
    let state = run(&moving(1, T0 - MIN, 2)).state;
    let result = step_one(
        state,
        &vd(
            3,
            T0 + MIN,
            json!({
                "drive_state": {"timestamp": T0 + 55 * SEC, "shift_state": "D", "speed": 20,
                                "latitude": 10.002, "longitude": 20.0},
                "charge_state": {"timestamp": T0 + MIN, "charging_state": "Charging",
                                 "charge_energy_added": 0.5, "battery_level": 60},
            }),
        ),
    );
    assert_eq!(result.delta.drives.len(), 1);
    let drive = &result.delta.drives[0];
    assert_eq!(drive.end_date_ms, T0 + 55 * SEC);
    assert_eq!(drive.end_latitude, Some(10.002));
    assert_eq!(
        dates(&result.delta.open_drive_positions).last(),
        Some(&(T0 + 55 * SEC))
    );
    let charge = result.state.open_charge.as_ref().expect("the charge opens");
    assert_eq!(charge.start_date_ms, T0 + MIN, "R9: after the drive's end");
    assert_eq!(
        result
            .delta
            .open_charge_samples
            .iter()
            .map(|sample| sample.timestamp_ms)
            .collect::<Vec<_>>(),
        vec![T0 + MIN]
    );
}
