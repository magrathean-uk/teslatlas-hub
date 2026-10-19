// SPDX-License-Identifier: AGPL-3.0-only

//! Part C spec tests (C1 specification section 8, with the revision 3
//! amendments): charges ST51-ST65, ST69, ST77 (charge half), ST78, ST79, ST86
//! and ST90, plus the rules of R61 to R71 that no numbered test pins. The
//! storage-level tests ST64 (stored label) and ST67 are in
//! `src/storage/db/tests/charge_lifecycle.rs`. Synthetic data only. `T0` is
//! 1_760_000_000_000 ms and `MIN` is one minute. Coordinates are in the
//! latitude 10.0, longitude 20.0 region. Odometer values are miles and speeds
//! mph, as Tesla reports them. Energies are C2's `E = k + (m - b)` (R13), which
//! is `last - first` while no preset is lowered; ST90 pins the lowered preset
//! (C2 R11, written by C2-P2).
//!
//! Observation shorthands: `cv` is an owner `vehicle_data` whose charge group
//! carries a charging state, a counter and a battery level (and a vehicle
//! group when an odometer is given), `at` adds coordinates, `disc` is a
//! discovery, `event` a Hub event, `sframe` a legacy stream frame and
//! `sparse` a Fleet `vehicle_data` with a `provider_feed` marker.

use serde_json::{Map, json};

use super::*;
use crate::tesla_stream::StreamUpdate;

pub(crate) const T0: i64 = 1_760_000_000_000;
pub(crate) const MIN: i64 = 60_000;
pub(crate) const HOUR: i64 = 60 * MIN;
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

/// An owner `vehicle_data` with a charge group (`state` empty means no
/// charging state), and a vehicle group when `odometer_mi` is given. All group
/// times are `t`.
pub(crate) fn cv(
    id: i64,
    t: i64,
    state: &str,
    energy: Option<f64>,
    soc: Option<i64>,
    odometer_mi: Option<f64>,
) -> LifecycleSample {
    let mut charge = Map::new();
    charge.insert("timestamp".to_owned(), json!(t));
    if !state.is_empty() {
        charge.insert("charging_state".to_owned(), json!(state));
    }
    if let Some(energy) = energy {
        charge.insert("charge_energy_added".to_owned(), json!(energy));
    }
    if let Some(soc) = soc {
        charge.insert("battery_level".to_owned(), json!(soc));
    }
    let mut data = Map::new();
    data.insert("charge_state".to_owned(), Value::Object(charge));
    if let Some(odometer) = odometer_mi {
        data.insert(
            "vehicle_state".to_owned(),
            json!({"timestamp": t, "odometer": odometer}),
        );
    }
    vd(id, t, Value::Object(data))
}

/// The same observation with a drive group that carries only coordinates (no
/// gear key and no speed, so it is no drive evidence).
pub(crate) fn at(mut sample: LifecycleSample, latitude: f64, longitude: f64) -> LifecycleSample {
    let t = sample.observed_at_ms;
    sample.payload["provider_raw_json"]["response"]["drive_state"] =
        json!({"timestamp": t, "latitude": latitude, "longitude": longitude});
    sample
}

/// The same observation with `vehicle_state.service_mode` set.
fn in_service(mut sample: LifecycleSample, service: bool) -> LifecycleSample {
    let t = sample.observed_at_ms;
    let vehicle = &mut sample.payload["provider_raw_json"]["response"]["vehicle_state"];
    if vehicle.is_null() {
        *vehicle = json!({"timestamp": t});
    }
    vehicle["service_mode"] = json!(service);
    sample
}

/// The same observation with the rated range (miles) in the charge group.
fn rated(mut sample: LifecycleSample, miles: f64) -> LifecycleSample {
    sample.payload["provider_raw_json"]["response"]["charge_state"]["battery_range"] = json!(miles);
    sample
}

/// An owner `vehicle_data` for the drive that leaves a parked snapshot: gear
/// and speed as given (`null` gear is the present-but-null key, Park for a full
/// source), coordinates, odometer, battery level and the charge counter.
pub(crate) fn drive_obs(
    id: i64,
    t: i64,
    gear: Value,
    speed: Value,
    latitude: f64,
    odometer_mi: f64,
    soc: i64,
    energy: f64,
) -> LifecycleSample {
    vd(
        id,
        t,
        json!({
            "drive_state": {"timestamp": t, "shift_state": gear, "speed": speed,
                            "latitude": latitude, "longitude": 20.0},
            "charge_state": {"timestamp": t, "battery_level": soc,
                             "charge_energy_added": energy},
            "vehicle_state": {"timestamp": t, "odometer": odometer_mi},
        }),
    )
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

/// A legacy stream frame built the way production builds it. `soc` and the
/// ranges are set, the gear and the speed are empty, so the frame is no drive
/// evidence.
fn sframe(id: i64, t: i64, soc: i64, odometer_mi: Option<f64>) -> LifecycleSample {
    let update = StreamUpdate {
        tag: "9".to_owned(),
        timestamp_ms: t,
        speed: None,
        odometer: odometer_mi,
        soc: Some(soc as f64),
        elevation: None,
        est_heading: None,
        est_lat: Some(10.0),
        est_lng: Some(20.0),
        power: None,
        shift_state: None,
        range: Some(200.0),
        est_range: Some(190.0),
        heading: None,
    };
    LifecycleSample {
        observation_id: id,
        observed_at_ms: t,
        vehicle_state: "online".to_owned(),
        payload: stream_observation_payload(&update),
    }
}

/// A Fleet `vehicle_data` with the sparse marker: only the keys given.
fn sparse(id: i64, t: i64, data: Value) -> LifecycleSample {
    LifecycleSample {
        observation_id: id,
        observed_at_ms: t,
        vehicle_state: "online".to_owned(),
        payload: json!({
            "record_type": "fleet_api_vehicle_data_v1",
            "source_vehicle_id": "9",
            "source_vehicle_state": "online",
            "provider_feed": "provider_stream",
            "provider_raw_json": {"response": data},
        }),
    }
}

fn run(samples: &[LifecycleSample]) -> LifecycleStep {
    apply_samples(OpenSessionState::new(), 1, samples).expect("apply samples")
}

fn step_one(state: OpenSessionState, sample: &LifecycleSample) -> LifecycleStep {
    apply_sample(state, 1, sample).expect("apply sample")
}

/// `(start, end)` of every closed charge of a delta.
fn spans(delta: &LifecycleDelta) -> Vec<(i64, Option<i64>)> {
    delta
        .charges
        .iter()
        .map(|charge| (charge.start_date_ms, charge.end_date_ms))
        .collect()
}

fn sample_times(delta: &LifecycleDelta) -> Vec<i64> {
    delta
        .open_charge_samples
        .iter()
        .map(|sample| sample.timestamp_ms)
        .collect()
}

fn energies(delta: &LifecycleDelta) -> Vec<Option<f64>> {
    delta
        .charges
        .iter()
        .map(|charge| charge.charge_energy_added)
        .collect()
}

// ------------------------------------------------------------------ ST51-ST53

#[test]
fn st51_a_basic_charge_opens_at_charging_and_closes_at_complete() {
    let samples = [
        cv(1, T0, "Disconnected", Some(12.0), Some(50), Some(1000.0)),
        cv(2, T0 + MIN, "Charging", Some(0.5), Some(50), Some(1000.0)),
        cv(
            3,
            T0 + 3 * MIN,
            "Charging",
            Some(5.0),
            Some(55),
            Some(1000.0),
        ),
        cv(
            4,
            T0 + 5 * MIN,
            "Charging",
            Some(10.0),
            Some(62),
            Some(1000.0),
        ),
        cv(
            5,
            T0 + 7 * MIN,
            "Charging",
            Some(15.0),
            Some(70),
            Some(1000.0),
        ),
        cv(
            6,
            T0 + 9 * MIN,
            "Complete",
            Some(20.0),
            Some(80),
            Some(1000.0),
        ),
    ];
    let result = run(&samples);
    // One charge from the first Charging observation to Complete.
    assert_eq!(spans(&result.delta), vec![(T0 + MIN, Some(T0 + 9 * MIN))]);
    assert!(result.state.open_charge.is_none());
    // Every charging observation is a sample, the Complete one included; the
    // Disconnected observation before the charge is not.
    assert_eq!(
        sample_times(&result.delta),
        vec![
            T0 + MIN,
            T0 + 3 * MIN,
            T0 + 5 * MIN,
            T0 + 7 * MIN,
            T0 + 9 * MIN
        ]
    );
    let charge = &result.delta.charges[0];
    // The baseline is 0.0 (the car was unplugged, and the counter is lower
    // than the snapshot's 12.0), so the energy is the whole counter.
    assert_eq!(charge.charge_energy_added, Some(20.0));
    assert_eq!(charge.start_battery_level, Some(50));
    assert_eq!(charge.end_battery_level, Some(80));
    assert_eq!(charge.id, 1);
    assert_eq!(
        result
            .delta
            .open_charge_samples
            .iter()
            .map(|sample| (sample.id, sample.charge_process_id))
            .collect::<Vec<_>>(),
        (1..=5).map(|id| (id, 1)).collect::<Vec<_>>()
    );
    assert_eq!(result.state.next_charge_id, 2);
    assert_eq!(result.state.next_charge_sample_id, 6);
    // R69: the snapshot the closed charge leaves.
    let snapshot = result.state.parked_snapshot.expect("snapshot");
    assert_eq!(snapshot.at_ms, Some(T0 + 9 * MIN));
    assert_eq!(snapshot.seen_at_ms, T0 + 9 * MIN);
    assert_eq!(snapshot.energy_added_kwh, Some(20.0));
    assert_eq!(snapshot.battery_level, Some(80));
    assert!(!snapshot.after_drive);
    assert!(!snapshot.unplugged);
}

#[test]
fn st52_starting_no_power_and_disconnected_with_an_unchanged_counter_open_nothing() {
    let samples = [
        cv(1, T0, "Disconnected", Some(5.0), Some(50), Some(1000.0)),
        cv(2, T0 + MIN, "Starting", Some(5.0), Some(50), Some(1000.0)),
        cv(
            3,
            T0 + 2 * MIN,
            "NoPower",
            Some(5.0),
            Some(50),
            Some(1000.0),
        ),
        cv(
            4,
            T0 + 3 * MIN,
            "Disconnected",
            Some(5.0),
            Some(50),
            Some(1000.0),
        ),
    ];
    let result = run(&samples);
    assert!(result.delta.charges.is_empty());
    assert!(result.delta.open_charge_samples.is_empty());
    assert!(result.state.open_charge.is_none());
    assert_eq!(result.state.next_charge_id, 1);
}

fn st53_samples() -> Vec<LifecycleSample> {
    let mut samples = vec![
        cv(1, T0, "Charging", Some(0.5), Some(50), Some(1000.0)),
        cv(
            2,
            T0 + 2 * MIN,
            "Charging",
            Some(1.0),
            Some(51),
            Some(1000.0),
        ),
    ];
    for (index, minutes) in [4, 6, 8, 10, 12].into_iter().enumerate() {
        samples.push(cv(
            3 + index as i64,
            T0 + minutes * MIN,
            "Stopped",
            Some(1.0),
            Some(51),
            Some(1000.0),
        ));
    }
    samples.push(cv(
        8,
        T0 + 14 * MIN,
        "NoPower",
        Some(1.0),
        Some(51),
        Some(1000.0),
    ));
    samples.push(cv(
        9,
        T0 + 16 * MIN,
        "Charging",
        Some(3.0),
        Some(54),
        Some(1000.0),
    ));
    samples.push(cv(
        10,
        T0 + 18 * MIN,
        "Charging",
        Some(6.0),
        Some(58),
        Some(1000.0),
    ));
    samples.push(cv(
        11,
        T0 + 20 * MIN,
        "Complete",
        Some(9.0),
        Some(62),
        Some(1000.0),
    ));
    samples
}

#[test]
fn st53_pauses_with_the_cable_connected_are_one_session() {
    let result = run(&st53_samples());
    assert_eq!(spans(&result.delta), vec![(T0, Some(T0 + 20 * MIN))]);
    assert_eq!(result.delta.open_charge_samples.len(), 11);
    assert_eq!(energies(&result.delta), vec![Some(9.0)]);
}

#[test]
fn st53_a_gap_of_exactly_t_gap_is_not_judged_and_one_millisecond_more_is() {
    let base = [
        cv(1, T0, "Charging", Some(1.0), Some(50), Some(1000.0)),
        cv(
            2,
            T0 + 4 * MIN,
            "Stopped",
            Some(1.0),
            Some(50),
            Some(1000.0),
        ),
    ];
    let t_gap = DEFAULT_OFFLINE_DRIVE_TIMEOUT.as_millis() as i64;
    assert_eq!(t_gap, 10 * MIN);
    let after = |gap_extra: i64, odometer: f64| {
        let mut samples = base.to_vec();
        samples.push(cv(
            3,
            T0 + 4 * MIN + t_gap + gap_extra,
            "Stopped",
            Some(1.0),
            Some(50),
            Some(odometer),
        ));
        run(&samples)
    };
    // Exactly `T_gap` is not greater than it: no continuity check, so even a
    // moved odometer samples without closing.
    let exact = after(0, 1003.0);
    assert!(exact.state.open_charge.is_some());
    assert_eq!(exact.delta.open_charge_samples.len(), 3);
    assert!(exact.delta.charges.is_empty());
    // One millisecond more: continuity is checked and holds (same odometer,
    // counter not lower).
    let held = after(1, 1000.0);
    assert!(held.state.open_charge.is_some());
    assert_eq!(held.delta.open_charge_samples.len(), 3);
    assert!(held.delta.charges.is_empty());
    // One millisecond more with a moved odometer: continuity fails, the
    // charge closes at its last sample, and a Hold with an unchanged counter
    // records nothing afterwards (R70 condition 2).
    let moved = after(1, 1003.0);
    assert!(moved.state.open_charge.is_none());
    assert_eq!(spans(&moved.delta), vec![(T0, Some(T0 + 4 * MIN))]);
    assert_eq!(moved.delta.open_charge_samples.len(), 2);
}

// ----------------------------------------------------------------------- ST54

#[test]
fn st54_a_top_up_after_complete_is_its_own_row_with_the_old_counter_as_baseline() {
    let mut samples = vec![
        cv(
            1,
            T0 - 10 * MIN,
            "Disconnected",
            Some(8.0),
            Some(50),
            Some(1000.0),
        ),
        cv(2, T0, "Charging", Some(0.5), Some(55), Some(1000.0)),
        cv(
            3,
            T0 + 30 * MIN,
            "Charging",
            Some(10.0),
            Some(70),
            Some(1000.0),
        ),
        cv(
            4,
            T0 + 60 * MIN,
            "Complete",
            Some(20.0),
            Some(90),
            Some(1000.0),
        ),
    ];
    // The counter continues for the top-up: 20.3 after 20.0.
    let mut continued = samples.clone();
    continued.push(cv(
        5,
        T0 + 3 * HOUR,
        "Charging",
        Some(20.3),
        Some(88),
        Some(1000.0),
    ));
    continued.push(cv(
        6,
        T0 + 3 * HOUR + 15 * MIN,
        "Charging",
        Some(20.8),
        Some(89),
        Some(1000.0),
    ));
    continued.push(cv(
        7,
        T0 + 3 * HOUR + 30 * MIN,
        "Complete",
        Some(21.0),
        Some(90),
        Some(1000.0),
    ));
    let result = run(&continued);
    assert_eq!(
        spans(&result.delta),
        vec![
            (T0, Some(T0 + 60 * MIN)),
            (T0 + 3 * HOUR, Some(T0 + 3 * HOUR + 30 * MIN)),
        ]
    );
    // Row 1: baseline 0.0 (unplugged, and lower); row 2: baseline 20.0.
    assert_eq!(energies(&result.delta), vec![Some(20.0), Some(1.0)]);

    // The counter restarts for the top-up: 0.2 after 20.0 is lower, so the
    // baseline is 0.0 and the energy is the whole counter.
    samples.push(cv(
        5,
        T0 + 3 * HOUR,
        "Charging",
        Some(0.2),
        Some(88),
        Some(1000.0),
    ));
    samples.push(cv(
        6,
        T0 + 3 * HOUR + 30 * MIN,
        "Complete",
        Some(1.0),
        Some(90),
        Some(1000.0),
    ));
    let restarted = run(&samples);
    assert_eq!(energies(&restarted.delta), vec![Some(20.0), Some(1.0)]);
    // Both variants: the total energy is 21.0 and no energy is counted twice.
    for rows in [&result.delta, &restarted.delta] {
        let total: f64 = rows
            .charges
            .iter()
            .filter_map(|c| c.charge_energy_added)
            .sum();
        assert!((total - 21.0).abs() < 1e-9, "{total}");
    }
}

// ----------------------------------------------------------------- ST55, ST56

#[test]
fn st55_sleep_ends_a_charge_at_its_last_sample() {
    let result = run(&[
        cv(1, T0, "Charging", Some(1.0), Some(50), None),
        cv(2, T0 + 10 * MIN, "Charging", Some(2.0), Some(53), None),
        cv(3, T0 + 20 * MIN, "Charging", Some(3.0), Some(56), None),
        cv(4, T0 + 30 * MIN, "Charging", Some(5.0), Some(60), None),
        disc(5, T0 + 45 * MIN, "asleep"),
    ]);
    assert_eq!(spans(&result.delta), vec![(T0, Some(T0 + 30 * MIN))]);
    assert!(result.state.open_charge.is_none());
    assert_eq!(result.state.phase, VehiclePhase::Asleep);
    // R69: the snapshot is the charge's last values at its end.
    let snapshot = result.state.parked_snapshot.expect("snapshot");
    assert_eq!(snapshot.at_ms, Some(T0 + 30 * MIN));
    assert_eq!(snapshot.battery_level, Some(60));
    assert_eq!(snapshot.energy_added_kwh, Some(5.0));
    assert!(!snapshot.unplugged);
}

#[test]
fn st56_a_drive_closes_an_open_charge_at_its_last_sample_and_opens_at_its_own_time() {
    let mut samples = vec![
        cv(1, T0, "Charging", Some(1.0), Some(50), None),
        cv(2, T0 + 2 * MIN, "Charging", Some(2.0), Some(52), None),
    ];
    samples.push(vd(
        3,
        T0 + 10 * MIN,
        json!({"drive_state": {"timestamp": T0 + 10 * MIN, "shift_state": "D", "speed": 30,
                               "latitude": 10.0, "longitude": 20.0}}),
    ));
    let result = run(&samples);
    assert_eq!(spans(&result.delta), vec![(T0, Some(T0 + 2 * MIN))]);
    assert!(result.state.open_charge.is_none());
    let drive = result.state.open_drive.expect("a drive is open");
    assert_eq!(drive.start_date_ms, T0 + 10 * MIN);
    assert_eq!(result.state.phase, VehiclePhase::Driving);
}

// ------------------------------------------------------------------ ST57-ST58c

fn garage_start() -> Vec<LifecycleSample> {
    vec![
        at(
            cv(1, T0, "Charging", Some(2.0), Some(40), Some(1000.0)),
            10.0,
            20.0,
        ),
        disc(2, T0 + MIN, "offline"),
        disc(3, T0 + 6 * HOUR, "online"),
    ]
}

#[test]
fn st57_an_offline_garage_charge_continues_across_the_gap() {
    let mut samples = garage_start();
    // The discoveries close nothing, however late.
    let midway = run(&samples);
    assert!(midway.state.open_charge.is_some());
    assert!(midway.delta.charges.is_empty());
    assert_eq!(midway.state.phase, VehiclePhase::Charging);
    samples.push(cv(
        4,
        T0 + 6 * HOUR + MIN,
        "Complete",
        Some(40.0),
        Some(85),
        Some(1000.0),
    ));
    let result = run(&samples);
    // One charge from the first sample to the Complete sample, the documented
    // upper-bound end (R65); 361 minutes (retained duration rule).
    assert_eq!(spans(&result.delta), vec![(T0, Some(T0 + 6 * HOUR + MIN))]);
    assert_eq!(result.delta.charges[0].duration_min, Some(361));
    assert_eq!(result.delta.open_charge_samples.len(), 2);
    assert_eq!(energies(&result.delta), vec![Some(40.0)]);
}

#[test]
fn st58_a_gap_with_a_moved_car_closes_at_the_last_sample_and_records_an_after_gap_charge() {
    let mut samples = garage_start();
    samples.push(at(
        cv(
            4,
            T0 + 6 * HOUR + MIN,
            "Complete",
            Some(40.0),
            Some(85),
            Some(1003.0),
        ),
        10.5,
        20.5,
    ));
    let result = run(&samples);
    assert_eq!(
        spans(&result.delta),
        vec![(T0, Some(T0)), (T0, Some(T0 + 6 * HOUR + MIN))]
    );
    // The first charge ends at its only sample with its own counter.
    assert_eq!(result.delta.charges[0].charge_energy_added, Some(2.0));
    let second = &result.delta.charges[1];
    // Moved against the snapshot: no start battery or ranges, baseline 0.0,
    // coordinates from the observation that proved the charge.
    assert_eq!(second.start_battery_level, None);
    assert_eq!(second.start_rated_range_km, None);
    assert_eq!(second.start_ideal_range_km, None);
    assert_eq!(
        (second.start_latitude, second.start_longitude),
        (Some(10.5), Some(20.5))
    );
    assert_eq!(second.charge_energy_added, Some(40.0));
    assert_eq!(result.delta.open_charge_samples.len(), 2);
    assert_eq!(
        result.delta.charge_start_coordinates,
        vec![(1, 10.0, 20.0), (2, 10.5, 20.5)]
    );
}

#[test]
fn st58b_a_gap_with_an_unknown_odometer_shows_no_continuity() {
    let samples = [
        cv(1, T0, "Charging", Some(2.0), Some(40), None),
        disc(2, T0 + MIN, "offline"),
        disc(3, T0 + 6 * HOUR, "online"),
        cv(
            4,
            T0 + 6 * HOUR + MIN,
            "Complete",
            Some(40.0),
            Some(85),
            None,
        ),
    ];
    let result = run(&samples);
    assert_eq!(
        spans(&result.delta),
        vec![(T0, Some(T0)), (T0, Some(T0 + 6 * HOUR + MIN))]
    );
    // No proof of a new session: the second baseline is the snapshot's 2.0.
    assert_eq!(energies(&result.delta), vec![Some(2.0), Some(38.0)]);
    let total: f64 = energies(&result.delta).into_iter().flatten().sum();
    assert!((total - 40.0).abs() < 1e-9);
}

#[test]
fn st58c_no_data_keeps_a_charge_open_and_a_service_refusal_closes_it() {
    let mut state = run(&[
        cv(1, T0, "Charging", Some(1.0), Some(50), None),
        cv(2, T0 + MIN, "Charging", Some(2.0), Some(51), None),
    ])
    .state;
    let kept = step_one(state, &event(3, T0 + 2 * HOUR, "no_data"));
    assert!(kept.state.open_charge.is_some());
    assert!(kept.delta.charges.is_empty());
    state = kept.state;
    let closed = step_one(state, &event(4, T0 + 2 * HOUR + MIN, "service_refused"));
    assert!(closed.state.open_charge.is_none());
    assert_eq!(spans(&closed.delta), vec![(T0, Some(T0 + MIN))]);
    assert_eq!(closed.state.service_mode, Some(true));
}

// ----------------------------------------------------------------- ST59, ST60

#[test]
fn st59_a_lower_counter_starts_a_new_session() {
    let result = run(&[
        cv(1, T0, "Charging", Some(10.0), Some(50), None),
        cv(2, T0 + 2 * MIN, "Charging", Some(10.0), Some(51), None),
        cv(3, T0 + 4 * MIN, "Charging", Some(0.3), Some(52), None),
        cv(4, T0 + 6 * MIN, "Complete", Some(0.3), Some(53), None),
    ]);
    assert_eq!(
        spans(&result.delta),
        vec![(T0, Some(T0 + 2 * MIN)), (T0 + 4 * MIN, Some(T0 + 6 * MIN))]
    );
    // Row 1: baseline 0.0 (no snapshot), energy 10.0. Row 2: baseline 0.0
    // (lower than the snapshot's 10.0), ending at 0.3.
    assert_eq!(energies(&result.delta), vec![Some(10.0), Some(0.3)]);
    // The reset observation is sampled once, in the new session.
    assert_eq!(result.delta.open_charge_samples.len(), 4);
    assert_eq!(result.delta.open_charge_samples[2].charge_process_id, 2);
}

#[test]
fn st60_service_mode_closes_a_charge_and_blocks_a_new_one() {
    let result = run(&[
        cv(1, T0, "Charging", Some(1.0), Some(50), None),
        cv(2, T0 + MIN, "Charging", Some(2.0), Some(51), None),
        in_service(
            cv(3, T0 + 2 * MIN, "Charging", Some(3.0), Some(52), None),
            true,
        ),
        in_service(
            cv(4, T0 + 3 * MIN, "Charging", Some(4.0), Some(53), None),
            true,
        ),
    ]);
    assert_eq!(spans(&result.delta), vec![(T0, Some(T0 + MIN))]);
    assert!(result.state.open_charge.is_none());
    assert_eq!(result.state.service_mode, Some(true));
    // The two in-service observations are not samples of any charge.
    assert_eq!(result.delta.open_charge_samples.len(), 2);
}

// ----------------------------------------------------------------- ST61, ST62

/// A drive that parks at `T0` leaving a snapshot with battery 40, counter 5.0,
/// odometer 1000.0 mi and the coordinates (10.01, 20.0).
pub(crate) fn parked_after_drive() -> Vec<LifecycleSample> {
    vec![
        drive_obs(1, T0 - 2 * MIN, json!("D"), json!(30), 10.0, 999.0, 41, 5.0),
        drive_obs(2, T0, Value::Null, Value::Null, 10.01, 1000.0, 40, 5.0),
    ]
}

#[test]
fn st61_a_charge_proved_by_the_counter_after_a_drive_is_recorded() {
    let drive_only = run(&parked_after_drive());
    assert_eq!(drive_only.delta.drives.len(), 1);
    let snapshot = drive_only.state.parked_snapshot.expect("snapshot");
    assert_eq!(snapshot.at_ms, Some(T0));
    assert_eq!(snapshot.battery_level, Some(40));
    assert_eq!(snapshot.energy_added_kwh, Some(5.0));
    assert!(snapshot.after_drive);

    let mut samples = parked_after_drive();
    samples.push(at(
        cv(
            3,
            T0 + 9 * HOUR,
            "Complete",
            Some(30.0),
            Some(85),
            Some(1000.0),
        ),
        10.01,
        20.0,
    ));
    let result = run(&samples);
    // One charge, from the drive's end to the first observation after the
    // charge, with one sample.
    assert_eq!(spans(&result.delta), vec![(T0, Some(T0 + 9 * HOUR))]);
    assert_eq!(result.delta.open_charge_samples.len(), 1);
    let charge = &result.delta.charges[0];
    assert_eq!(charge.start_battery_level, Some(40));
    assert_eq!(charge.end_battery_level, Some(85));
    // Baseline 0.0 because the snapshot is after a drive: the energy is the
    // whole counter.
    assert_eq!(charge.charge_energy_added, Some(30.0));
    assert_eq!(
        (charge.start_latitude, charge.start_longitude),
        (Some(10.01), Some(20.0))
    );
    let snapshot = result.state.parked_snapshot.expect("snapshot");
    assert!(!snapshot.after_drive);
    assert_eq!(snapshot.at_ms, Some(T0 + 9 * HOUR));
}

#[test]
fn st62_an_unchanged_counter_or_an_unchanged_battery_records_nothing() {
    // The counter is the one the snapshot already saw.
    let mut unchanged_counter = parked_after_drive();
    unchanged_counter.push(cv(
        3,
        T0 + 9 * HOUR,
        "Complete",
        Some(5.0),
        Some(85),
        Some(1000.0),
    ));
    let result = run(&unchanged_counter);
    assert!(result.delta.charges.is_empty());
    assert!(result.state.open_charge.is_none());

    // The counter moved but the battery did not rise.
    let mut unchanged_battery = parked_after_drive();
    unchanged_battery.push(cv(
        3,
        T0 + 9 * HOUR,
        "Complete",
        Some(30.0),
        Some(40),
        Some(1000.0),
    ));
    let result = run(&unchanged_battery);
    assert!(result.delta.charges.is_empty());
    assert!(result.state.open_charge.is_none());
    // The snapshot follows the car, so the same facts later record nothing.
    let snapshot = result.state.parked_snapshot.expect("snapshot");
    assert_eq!(snapshot.energy_added_kwh, Some(30.0));
    assert_eq!(snapshot.at_ms, Some(T0 + 9 * HOUR));
}

#[test]
fn a_rated_range_rise_proves_the_charge_when_a_level_is_unknown() {
    // The snapshot's level is unknown: the drive had no battery level.
    let mut state = OpenSessionState::new();
    state.parked_snapshot = Some(ParkedSnapshot {
        at_ms: Some(T0),
        seen_at_ms: T0,
        rated_range_km: Some(100.0),
        energy_added_kwh: Some(5.0),
        ..ParkedSnapshot::default()
    });
    // 90 miles is 144.84 km, above 100 km.
    let result = step_one(
        state.clone(),
        &rated(cv(1, T0 + HOUR, "Complete", Some(10.0), None, None), 90.0),
    );
    assert_eq!(spans(&result.delta), vec![(T0, Some(T0 + HOUR))]);
    // Not above: no row.
    let result = step_one(
        state,
        &rated(cv(1, T0 + HOUR, "Complete", Some(10.0), None, None), 50.0),
    );
    assert!(result.delta.charges.is_empty());
}

// ----------------------------------------------------------------------- ST63

#[test]
fn st63_a_charge_first_seen_in_progress_after_sleep_starts_at_its_first_sample() {
    let result = run(&[
        cv(1, T0, "Stopped", Some(5.0), Some(50), None),
        disc(2, T0 + 20 * MIN, "asleep"),
        disc(3, T0 + 6 * HOUR, "online"),
        cv(
            4,
            T0 + 6 * HOUR + MIN,
            "Charging",
            Some(0.4),
            Some(51),
            None,
        ),
    ]);
    // R70 is never used for Active: R61 opens at the first Charging sample.
    assert!(result.delta.charges.is_empty());
    let open = result.state.open_charge.expect("a charge is open");
    assert_eq!(open.start_date_ms, T0 + 6 * HOUR + MIN);
    // The counter is lower than the snapshot's, so the baseline is 0.0.
    assert_eq!(open.first_energy_added, Some(0.0));
    assert_eq!(open.start_battery_level, Some(51));
    assert_eq!(result.state.phase, VehiclePhase::Charging);
}

// ----------------------------------------------------------------------- ST64

#[test]
fn st64_a_closed_charge_with_start_coordinates_has_one_start_coordinate_entry() {
    let result = run(&[
        at(
            cv(1, T0, "Charging", Some(1.0), Some(50), None),
            10.123_456_7,
            20.0,
        ),
        cv(2, T0 + 2 * MIN, "Complete", Some(3.0), Some(55), None),
    ]);
    let charge = &result.delta.charges[0];
    // The entry holds the start coordinates at six decimals, in the closing
    // step only.
    assert_eq!(
        result.delta.charge_start_coordinates,
        vec![(charge.id, 10.123_457, 20.0)]
    );
    let early = run(&[at(
        cv(1, T0, "Charging", Some(1.0), Some(50), None),
        10.0,
        20.0,
    )]);
    assert!(early.delta.charge_start_coordinates.is_empty());
    // A charge without start coordinates gets no entry.
    let without = run(&[
        cv(1, T0, "Charging", Some(1.0), Some(50), None),
        cv(2, T0 + 2 * MIN, "Complete", Some(3.0), Some(55), None),
    ]);
    assert_eq!(without.delta.charges.len(), 1);
    assert!(without.delta.charge_start_coordinates.is_empty());
    assert_eq!(without.delta.charges[0].start_latitude, None);
}

// ----------------------------------------------------------------------- ST65

fn imported_charge_state(start: i64) -> OpenSessionState {
    let process: TeslaMateChargingProcess = serde_json::from_value(json!({
        "id": 80, "car_id": 1, "position_id": null, "address_id": null,
        "geofence_id": null, "start_date_ms": start, "end_date_ms": null,
        "charge_energy_added": 3.0, "charge_energy_used_kwh": null,
        "start_ideal_range_km": 151.0, "end_ideal_range_km": 152.0,
        "start_battery_level": 40, "end_battery_level": 50, "duration_min": null,
        "outside_temp_avg": 11.0, "start_rated_range_km": 141.0,
        "end_rated_range_km": 142.0, "cost": null
    }))
    .expect("imported charging process");
    let mut session = TeslaMateOpenSession {
        car_id: 1,
        charge: Some(process),
        charge_samples: vec![
            super::tests::imported_charge_fixture(1, 80, start, 1.0, 40, 10.0),
            super::tests::imported_charge_fixture(2, 80, start + 5 * MIN, 4.0, 50, 12.0),
        ],
        ..Default::default()
    };
    session.watermarks.charges.max_timestamp_ms = Some(start + 5 * MIN);
    seed_imported_open_session_state(uuid::Uuid::nil(), &session, None).expect("seed")
}

#[test]
fn st65_an_imported_open_charge_continues_and_closes_with_its_seeded_id() {
    let start = T0 - HOUR;
    let state = imported_charge_state(start);
    let seeded = state.open_charge.clone().expect("seeded charge");
    assert_eq!(seeded.id, 80);
    assert_eq!(seeded.sample_count, 2);
    // A live observation at or before the imported watermark is stale.
    let stale = step_one(
        state.clone(),
        &cv(1, start + 5 * MIN, "Charging", Some(5.0), Some(51), None),
    );
    assert!(stale.delta.open_charge_samples.is_empty());
    assert_eq!(stale.state.open_charge, Some(seeded));
    // Live Charging and then Complete continue the session.
    let result = run_from(
        state,
        &[
            cv(2, start + 7 * MIN, "Charging", Some(6.0), Some(60), None),
            cv(3, start + 9 * MIN, "Complete", Some(10.0), Some(80), None),
        ],
    );
    assert_eq!(spans(&result.delta), vec![(start, Some(start + 9 * MIN))]);
    let closed = &result.delta.charges[0];
    assert_eq!(closed.id, 80);
    assert_eq!(closed.start_battery_level, Some(40));
    assert_eq!(closed.end_battery_level, Some(80));
    // The imported counter 1.0 is no baseline: no earlier row holds any part
    // of it, so the whole counter is new energy (C2 R11, R14): 10.0 - 0.0.
    assert_eq!(closed.charge_energy_added, Some(10.0));
    // Only the two live samples travel in the delta; the imported ones stay.
    assert_eq!(
        sample_times(&result.delta),
        vec![start + 7 * MIN, start + 9 * MIN]
    );
    assert!(
        result
            .delta
            .open_charge_samples
            .iter()
            .all(|s| s.charge_process_id == 80)
    );
}

fn run_from(state: OpenSessionState, samples: &[LifecycleSample]) -> LifecycleStep {
    apply_samples(state, 1, samples).expect("apply samples")
}

// ----------------------------------------------------------------------- ST69

#[test]
fn st69_interleaved_sources_give_one_charge_sampled_only_by_vehicle_data() {
    let mut id = 1;
    let mut samples = vec![vd(
        id,
        T0 - 10 * MIN,
        json!({
            "charge_state": {"timestamp": T0 - 10 * MIN, "charging_state": "Disconnected",
                             "charge_energy_added": 8.0, "battery_level": 50},
            "vehicle_state": {"timestamp": T0 - 10 * MIN, "odometer": 1000.0,
                              "car_version": "2024.14.3 aa", "service_mode": false},
        }),
    )];
    for k in 0..5_i64 {
        for frame_index in 1..=6_i64 {
            id += 1;
            samples.push(sframe(
                id,
                T0 + k * MIN + frame_index * 10 * SEC,
                80,
                Some(1000.0),
            ));
        }
        id += 1;
        let frame_time = T0 + (k + 1) * MIN;
        let stamp = frame_time - 5 * SEC;
        let (state, energy) = [
            ("Charging", 0.4),
            ("Charging", 1.2),
            ("Charging", 2.0),
            ("Complete", 2.6),
            ("Complete", 2.6),
        ][k as usize];
        let mut vehicle = json!({"timestamp": stamp, "odometer": 1000.0});
        vehicle["car_version"] = json!(if k >= 2 {
            "2024.20.1 bb"
        } else {
            "2024.14.3 aa"
        });
        if k == 1 {
            vehicle["software_update"] = json!({"status": "installing"});
        }
        if k == 2 {
            vehicle["software_update"] = json!({"status": ""});
        }
        if k == 4 {
            vehicle["service_mode"] = json!(true);
        }
        samples.push(vd(
            id,
            frame_time + SEC,
            json!({
                "charge_state": {"timestamp": stamp, "charging_state": state,
                                 "charge_energy_added": energy, "battery_level": 51 + k},
                "vehicle_state": vehicle,
            }),
        ));
    }
    let result = run(&samples);
    // One charge: started at the first V's own group time, ended at Complete.
    assert_eq!(
        spans(&result.delta),
        vec![(T0 + 55 * SEC, Some(T0 + 235 * SEC))]
    );
    // Four samples, each at its own V's charge time and none from a frame.
    assert_eq!(
        sample_times(&result.delta),
        vec![
            T0 + 55 * SEC,
            T0 + 115 * SEC,
            T0 + 175 * SEC,
            T0 + 235 * SEC
        ]
    );
    // Each sample's battery comes from its own V.
    assert_eq!(
        result
            .delta
            .open_charge_samples
            .iter()
            .map(|sample| sample.battery_level)
            .collect::<Vec<_>>(),
        vec![Some(51), Some(52), Some(53), Some(54)]
    );
    // Baseline 0.0 (unplugged, and lower): the energy is the whole counter.
    assert_eq!(energies(&result.delta), vec![Some(2.6)]);
    // One update row, from the install start to the new version.
    assert_eq!(result.delta.updates.len(), 1);
    assert_eq!(result.delta.updates[0].version, "2024.20.1 bb");
    assert_eq!(result.delta.updates[0].start_date_ms, T0 + 115 * SEC);
    assert_eq!(result.delta.updates[0].end_date_ms, T0 + 175 * SEC);
    // The last observation states service mode, and the car stayed online.
    assert_eq!(result.state.service_mode, Some(true));
    assert_eq!(result.state.last_reach.as_deref(), Some("online"));
}

// ----------------------------------------------------------------------- ST77

#[test]
fn st77_a_sparse_odometer_below_the_last_reading_is_unmoved() {
    let mut state = step_one(
        OpenSessionState::new(),
        &cv(1, T0, "Charging", Some(2.0), Some(50), Some(1_000.456_789)),
    )
    .state;
    assert_eq!(
        state
            .open_charge
            .as_ref()
            .and_then(|open| open.last_odometer_km),
        Some(1_000.456_789 * K_MILE_KM)
    );
    // After a 30 minute gap a sparse source states 1000.4 miles (a smaller
    // value from different rounding) with Charging and a counter that did not
    // fall.
    let t = T0 + 30 * MIN;
    let result = step_one(
        state.clone(),
        &sparse(
            2,
            t,
            json!({
                "charge_state": {"timestamp": t, "charging_state": "Charging",
                                 "charge_energy_added": 3.0},
                "vehicle_state": {"timestamp": t, "odometer": 1000.4},
            }),
        ),
    );
    assert!(result.delta.charges.is_empty());
    let open = result
        .state
        .open_charge
        .clone()
        .expect("one session continues");
    assert_eq!(open.id, 1);
    assert_eq!(open.sample_count, 2);
    assert_eq!(open.last_odometer_km, Some(1000.4 * K_MILE_KM));
    // The session then closes as one row with both samples' energy.
    state = result.state;
    let closed = step_one(
        state,
        &cv(3, t + MIN, "Complete", Some(4.0), Some(60), None),
    );
    assert_eq!(spans(&closed.delta), vec![(T0, Some(t + MIN))]);
    assert_eq!(energies(&closed.delta), vec![Some(4.0)]);
}

// ----------------------------------------------------------------------- ST78

#[test]
fn st78_a_stream_frame_judges_nothing_and_the_complete_after_a_gap_closes_one_session() {
    let result = run(&[
        cv(1, T0, "Charging", Some(2.0), Some(40), Some(1000.0)),
        sframe(2, T0 + 3 * MIN, 41, Some(1000.0)),
        cv(
            3,
            T0 + 40 * MIN,
            "Complete",
            Some(12.0),
            Some(60),
            Some(1000.0),
        ),
    ]);
    assert_eq!(spans(&result.delta), vec![(T0, Some(T0 + 40 * MIN))]);
    // The frame is not a sample.
    assert_eq!(sample_times(&result.delta), vec![T0, T0 + 40 * MIN]);
    assert_eq!(energies(&result.delta), vec![Some(12.0)]);
    // A frame never closes a charge, however late: 40 minutes of silence
    // followed by a frame leaves the charge open.
    let open = run(&[
        cv(1, T0, "Charging", Some(2.0), Some(40), Some(1000.0)),
        sframe(2, T0 + 40 * MIN, 41, Some(1000.0)),
    ]);
    assert!(open.state.open_charge.is_some());
    assert!(open.delta.charges.is_empty());
    // The frame's odometer is the last known one for the next gap.
    assert_eq!(
        open.state
            .open_charge
            .and_then(|charge| charge.last_odometer_km),
        Some(1000.0 * K_MILE_KM)
    );
}

// ----------------------------------------------------------------------- ST79

#[test]
fn st79_a_home_charge_while_the_hub_was_down_needs_an_unmoved_car_for_disconnected() {
    let mut samples = parked_after_drive();
    samples.push(cv(
        3,
        T0 + 9 * HOUR,
        "Disconnected",
        Some(30.0),
        Some(85),
        Some(1000.0),
    ));
    let result = run(&samples);
    assert_eq!(spans(&result.delta), vec![(T0, Some(T0 + 9 * HOUR))]);
    assert_eq!(result.delta.open_charge_samples.len(), 1);
    let charge = &result.delta.charges[0];
    assert_eq!(charge.start_battery_level, Some(40));
    // The observation carries no coordinates: they come from the snapshot,
    // because the car is unmoved.
    assert_eq!(
        (charge.start_latitude, charge.start_longitude),
        (Some(10.01), Some(20.0))
    );
    assert_eq!(charge.charge_energy_added, Some(30.0));
    // The Disconnected sample ended it: the snapshot records the unplug.
    assert!(result.state.parked_snapshot.expect("snapshot").unplugged);

    // With the odometer 2 miles higher the car moved: nothing is recorded.
    let mut moved = parked_after_drive();
    moved.push(cv(
        3,
        T0 + 9 * HOUR,
        "Disconnected",
        Some(30.0),
        Some(85),
        Some(1002.0),
    ));
    let result = run(&moved);
    assert!(result.delta.charges.is_empty());
    assert!(result.state.open_charge.is_none());
}

// ----------------------------------------------------------------------- ST86

#[test]
fn st86_an_old_blob_with_a_pending_seed_records_the_charge_it_proves() {
    let mut state = OpenSessionState::new();
    state.pending_gained_range_charge = Some(GainedRangeChargeSeed {
        start_date_ms: T0,
        start_battery_level: Some(40),
        start_ideal_range_km: None,
        start_rated_range_km: None,
        start_latitude: None,
        start_longitude: None,
        first_energy_added: 5.0,
    });
    let result = step_one(
        state,
        &cv(1, T0 + 5 * HOUR, "Complete", Some(25.0), Some(80), None),
    );
    assert_eq!(spans(&result.delta), vec![(T0, Some(T0 + 5 * HOUR))]);
    // No proof of a new session: the baseline is the seed's 5.0.
    assert_eq!(energies(&result.delta), vec![Some(20.0)]);
    assert!(result.state.pending_gained_range_charge.is_none());
}

// ----------------------------------------------------------------------- ST90

/// ST90's rows. C2 R9 S6 (a) lowers a preset the first usable counter is
/// below (C2 R11, `observe_charge_sample`).
fn st90_samples() -> Vec<LifecycleSample> {
    vec![
        cv(1, T0 - 30 * MIN, "Charging", Some(0.5), Some(60), None),
        cv(2, T0 - 20 * MIN, "Complete", Some(20.0), Some(70), None),
        cv(3, T0, "Charging", None, Some(70), None),
        cv(4, T0 + 10 * MIN, "Charging", Some(0.30), Some(72), None),
        cv(5, T0 + 20 * MIN, "Complete", Some(1.38), Some(75), None),
    ]
}

#[test]
fn st90_the_preset_is_the_snapshots_counter_and_one_session_spans_the_lowered_counter() {
    let first = run(&st90_samples()[..3]);
    // Row 1 is closed; the snapshot has energy 20.0, no unplug, no drive.
    assert_eq!(
        spans(&first.delta),
        vec![(T0 - 30 * MIN, Some(T0 - 20 * MIN))]
    );
    let snapshot = first.state.parked_snapshot.as_ref().expect("snapshot");
    assert_eq!(snapshot.energy_added_kwh, Some(20.0));
    assert!(!snapshot.unplugged);
    assert!(!snapshot.after_drive);
    // Row 2 opens with the preset 20.00 on a Charging observation without a
    // counter.
    let open = first.state.open_charge.as_ref().expect("row 2 is open");
    assert_eq!(open.first_energy_added, Some(20.0));
    assert_eq!(open.start_date_ms, T0);
    assert_eq!(open.last_energy_added, None);

    let all = run(&st90_samples());
    // No R66 split at observation 4: the open session has no counter yet.
    assert_eq!(
        spans(&all.delta),
        vec![
            (T0 - 30 * MIN, Some(T0 - 20 * MIN)),
            (T0, Some(T0 + 20 * MIN))
        ]
    );
    assert_eq!(
        all.delta
            .open_charge_samples
            .iter()
            .filter(|sample| sample.charge_process_id == 2)
            .count(),
        3
    );
}

#[test]
fn st90_energies_are_never_negative() {
    let all = run(&st90_samples());
    assert_eq!(energies(&all.delta), vec![Some(20.0), Some(1.38)]);
}

// ------------------------------------------------- rules without a numbered test

#[test]
fn a_none_class_observation_with_a_counter_is_still_sampled() {
    let result = run(&[
        cv(1, T0, "Charging", Some(1.0), Some(50), None),
        cv(2, T0 + MIN, "", Some(1.5), Some(51), None),
        cv(3, T0 + 2 * MIN, "Unknown", Some(2.0), Some(52), None),
        cv(4, T0 + 3 * MIN, "Complete", Some(2.5), Some(53), None),
    ]);
    assert_eq!(result.delta.open_charge_samples.len(), 4);
    assert_eq!(spans(&result.delta), vec![(T0, Some(T0 + 3 * MIN))]);
}

#[test]
fn an_observation_without_a_state_or_a_counter_is_no_sample_and_judges_nothing() {
    // The charge group carries only the battery level.
    let result = run(&[
        cv(1, T0, "Charging", Some(1.0), Some(50), Some(1000.0)),
        cv(2, T0 + 40 * MIN, "", None, Some(60), Some(1003.0)),
    ]);
    assert_eq!(result.delta.open_charge_samples.len(), 1);
    assert!(result.state.open_charge.is_some());
    assert!(result.delta.charges.is_empty());
}

#[test]
fn a_gap_with_a_lower_counter_or_no_class_fails_continuity() {
    let start = cv(1, T0, "Charging", Some(5.0), Some(50), Some(1000.0));
    // The counter fell across the gap (R66 and R65 agree).
    let fell = run(&[
        start.clone(),
        cv(2, T0 + HOUR, "Charging", Some(0.2), Some(60), Some(1000.0)),
    ]);
    assert_eq!(spans(&fell.delta), vec![(T0, Some(T0))]);
    assert_eq!(fell.state.open_charge.as_ref().map(|open| open.id), Some(2));
    // No class (a state text Tesla does not define) after a gap fails too.
    let unknown = run(&[
        start.clone(),
        cv(2, T0 + HOUR, "Standby", Some(6.0), Some(60), Some(1000.0)),
    ]);
    assert_eq!(spans(&unknown.delta), vec![(T0, Some(T0))]);
    // A counter missing now fails: the counter must be known and not lower.
    let missing = run(&[
        start,
        cv(2, T0 + HOUR, "Charging", None, Some(60), Some(1000.0)),
    ]);
    assert_eq!(spans(&missing.delta), vec![(T0, Some(T0))]);
}

#[test]
fn a_session_closed_after_a_gap_continues_under_a_new_id_when_active() {
    let result = run(&[
        cv(1, T0, "Charging", Some(5.0), Some(50), Some(1000.0)),
        cv(2, T0 + HOUR, "Charging", Some(9.0), Some(60), Some(1003.0)),
    ]);
    // Moved: the first session ends at its last sample; the Active
    // observation opens a new one at its own time. The snapshot's counter is
    // 5.0, the car moved, so the baseline is 0.0.
    assert_eq!(spans(&result.delta), vec![(T0, Some(T0))]);
    let open = result.state.open_charge.expect("second session");
    assert_eq!(open.start_date_ms, T0 + HOUR);
    assert_eq!(open.first_energy_added, Some(0.0));
    assert_eq!(open.id, 2);
}

#[test]
fn a_charge_after_a_drive_close_in_the_same_observation_starts_no_earlier_than_the_drive_end() {
    // ST37 shape: the drive group is a few seconds ahead of the charge group.
    let t = T0 + 5 * MIN;
    let samples = [
        drive_obs(1, T0, json!("D"), json!(30), 10.0, 1000.0, 80, 1.0),
        vd(
            2,
            t + 5 * SEC,
            json!({
                "drive_state": {"timestamp": t + 5 * SEC, "shift_state": null, "speed": null,
                                "latitude": 10.01, "longitude": 20.0},
                "charge_state": {"timestamp": t, "charging_state": "Charging",
                                 "charge_energy_added": 0.2, "battery_level": 79},
                "vehicle_state": {"timestamp": t + 5 * SEC, "odometer": 1001.0},
            }),
        ),
        cv(3, t + MIN, "Complete", Some(1.0), Some(80), None),
    ];
    let first = run(&samples[..2]);
    // The drive is kept and closes at its Park time, which leads the charge
    // time; the charge starts no earlier (R9) and keeps its own first sample.
    assert_eq!(first.delta.drives.len(), 1);
    assert_eq!(first.delta.drives[0].end_date_ms, t + 5 * SEC);
    let open = first.state.open_charge.expect("charge opened");
    assert_eq!(open.start_date_ms, t + 5 * SEC);
    assert_eq!(sample_times(&first.delta), vec![t]);
    let result = run(&samples);
    assert_eq!(spans(&result.delta), vec![(t + 5 * SEC, Some(t + MIN))]);
}

#[test]
fn a_charge_never_ends_before_it_starts() {
    // A charge opened at a drive end that leads the observation's charge time
    // closes at its start, not before it.
    let mut state = OpenSessionState::new();
    state.open_charge = Some(OpenCharge {
        last_sample_timestamp_ms: Some(T0 - HOUR),
        ..OpenCharge::empty(1, 1, T0)
    });
    let result = step_one(state, &disc(1, T0 + MIN, "asleep"));
    assert_eq!(spans(&result.delta), vec![(T0, Some(T0))]);
}

#[test]
fn an_unreachable_or_stale_discovery_does_not_close_a_charge() {
    let mut state = OpenSessionState::new();
    state.open_charge = Some(OpenCharge {
        last_sample_timestamp_ms: Some(T0),
        ..OpenCharge::empty(1, 1, T0 - HOUR)
    });
    state.last_observed_at_ms = Some(T0 + 10 * MIN);
    // An asleep discovery older than the newest observation is ignored (R7).
    let stale = step_one(state.clone(), &disc(1, T0 + 5 * MIN, "asleep"));
    assert!(stale.state.open_charge.is_some());
    // Offline closes nothing by itself, nor does a late online discovery.
    let offline = step_one(state.clone(), &disc(1, T0 + 20 * MIN, "offline"));
    assert!(offline.state.open_charge.is_some());
    let late = step_one(state, &disc(1, T0 + 12 * HOUR, "online"));
    assert!(late.state.open_charge.is_some());
    assert!(late.delta.charges.is_empty());
}

#[test]
fn a_conflicting_plugged_observation_in_a_drive_is_discarded_and_the_charge_closes() {
    // ST72 (c) and (d) at charge level.
    let mut state = OpenSessionState::new();
    state = step_one(
        state,
        &cv(1, T0 - HOUR, "Charging", Some(1.0), Some(50), None),
    )
    .state;
    let drive_newer = vd(
        2,
        T0,
        json!({"drive_state": {"timestamp": T0, "shift_state": "D", "speed": 20,
                               "latitude": 10.0, "longitude": 20.0},
               "charge_state": {"timestamp": T0 - 5 * SEC, "charging_state": "Charging",
                                "charge_energy_added": 2.0}}),
    );
    let result = step_one(state.clone(), &drive_newer);
    assert_eq!(spans(&result.delta), vec![(T0 - HOUR, Some(T0 - HOUR))]);
    assert!(result.delta.open_charge_samples.is_empty());
    assert_eq!(
        result.state.open_drive.map(|drive| drive.start_date_ms),
        Some(T0)
    );
    // The charge group newer: the charge is sampled and no drive opens.
    let charge_newer = vd(
        2,
        T0,
        json!({"drive_state": {"timestamp": T0 - 5 * SEC, "shift_state": "D", "speed": 20,
                               "latitude": 10.0, "longitude": 20.0},
               "charge_state": {"timestamp": T0, "charging_state": "Charging",
                                "charge_energy_added": 2.0}}),
    );
    let result = step_one(state, &charge_newer);
    assert!(result.state.open_drive.is_none());
    assert_eq!(sample_times(&result.delta), vec![T0]);
    assert!(result.state.open_charge.is_some());
}

#[test]
fn a_sample_takes_every_field_the_observation_carries_whatever_the_freshness() {
    // A stream frame advances the battery family to T0 + 60 s; the next
    // vehicle_data carries older group times but is the car's latest statement
    // of its own fields, so its charge sample keeps the battery level.
    let frame_time = T0 + 60 * SEC;
    let stamp = frame_time - 5 * SEC;
    let result = run(&[
        sframe(1, frame_time, 80, Some(1000.0)),
        vd(
            2,
            frame_time + SEC,
            json!({
                "charge_state": {"timestamp": stamp, "charging_state": "Charging",
                                 "charge_energy_added": 0.4, "battery_level": 55,
                                 "usable_battery_level": 54, "charger_power": 11.0,
                                 "charger_voltage": 230, "charger_actual_current": 16,
                                 "charger_pilot_current": 16, "charger_phases": 3,
                                 "fast_charger_present": false, "conn_charge_cable": "IEC",
                                 "battery_range": 120.0, "ideal_battery_range": 125.0,
                                 "battery_heater_on": true,
                                 "not_enough_power_to_heat": false},
                "climate_state": {"timestamp": stamp, "outside_temp": 12.25,
                                  "battery_heater": false, "battery_heater_no_power": true},
            }),
        ),
    ]);
    assert_eq!(result.delta.open_charge_samples.len(), 1);
    let sample = &result.delta.open_charge_samples[0];
    assert_eq!(sample.timestamp_ms, stamp);
    assert_eq!(sample.battery_level, Some(55));
    assert_eq!(sample.usable_battery_level, Some(54));
    assert_eq!(sample.charge_energy_added_kwh, Some(0.4));
    assert_eq!(sample.charger_power_kw, Some(11.0));
    assert_eq!(sample.charger_voltage, Some(230.0));
    assert_eq!(sample.charger_actual_current, Some(16.0));
    assert_eq!(sample.charger_pilot_current, Some(16.0));
    assert_eq!(sample.charger_phases, Some(3));
    assert_eq!(sample.rated_range_km, Some(miles_to_km_rounded(120.0, 2)));
    assert_eq!(sample.ideal_range_km, Some(miles_to_km_rounded(125.0, 2)));
    assert_eq!(sample.outside_temp_c, Some(12.3));
    assert_eq!(sample.battery_heater_on, Some(true));
    assert_eq!(sample.battery_heater, Some(false));
    assert_eq!(sample.battery_heater_no_power, Some(true));
    assert_eq!(sample.not_enough_power_to_heat, Some(false));
    assert_eq!(sample.fast_charger_present, Some(false));
    assert_eq!(sample.charge_cable.as_deref(), Some("IEC"));
}

#[test]
fn the_baseline_follows_r61a() {
    let snapshot =
        |energy: Option<f64>, after_drive: bool, unplugged: bool, odometer: Option<f64>| {
            ParkedSnapshot {
                at_ms: Some(T0),
                seen_at_ms: T0,
                energy_added_kwh: energy,
                after_drive,
                unplugged,
                odometer_km: odometer,
                ..ParkedSnapshot::default()
            }
        };
    let km = |miles: f64| Some(miles * K_MILE_KM);
    // No snapshot, an unknown counter, a lower counter, after a drive, after
    // an unplug and after a move: the whole counter is new.
    assert_eq!(charge_energy_baseline(None, Some(3.0), None), 0.0);
    assert_eq!(
        charge_energy_baseline(Some(&snapshot(None, false, false, None)), Some(3.0), None),
        0.0
    );
    assert_eq!(
        charge_energy_baseline(
            Some(&snapshot(Some(5.0), false, false, None)),
            Some(4.9),
            None
        ),
        0.0
    );
    assert_eq!(
        charge_energy_baseline(
            Some(&snapshot(Some(5.0), true, false, None)),
            Some(6.0),
            None
        ),
        0.0
    );
    assert_eq!(
        charge_energy_baseline(
            Some(&snapshot(Some(5.0), false, true, None)),
            Some(6.0),
            None
        ),
        0.0
    );
    assert_eq!(
        charge_energy_baseline(
            Some(&snapshot(Some(5.0), false, false, km(1000.0))),
            Some(6.0),
            km(1000.1),
        ),
        0.0
    );
    // Otherwise only its growth is new: the baseline is the snapshot's counter
    // (equal counter, unknown odometer, an odometer that rose by less than the
    // tolerance, an odometer that fell, and an unknown counter now).
    for (counter, odometer) in [
        (Some(5.0), None),
        (Some(6.0), None),
        (Some(6.0), km(1000.05)),
        (Some(6.0), km(999.0)),
        (None, None),
    ] {
        assert_eq!(
            charge_energy_baseline(
                Some(&snapshot(Some(5.0), false, false, km(1000.0))),
                counter,
                odometer,
            ),
            5.0,
            "{counter:?} {odometer:?}"
        );
    }
}

#[test]
fn movement_uses_the_one_tolerance_of_moved() {
    assert_eq!(
        movement_between(Some(0.0), Some(K_DMIN_KM)),
        Movement::Moved
    );
    assert_eq!(
        movement_between(Some(10.0), Some(10.0 + K_DMIN_KM / 2.0)),
        Movement::Unmoved
    );
    assert_eq!(movement_between(Some(10.0), Some(9.0)), Movement::Unmoved);
    assert_eq!(movement_between(None, Some(9.0)), Movement::Unknown);
    assert_eq!(movement_between(Some(10.0), None), Movement::Unknown);
    assert_eq!(
        movement_between(Some(f64::NAN), Some(9.0)),
        Movement::Unknown
    );
}

#[test]
fn identifiers_come_from_the_cursors_and_exhaustion_is_an_error() {
    let mut state = OpenSessionState::new();
    state.next_charge_id = i64::MAX;
    let result = apply_sample(state, 1, &cv(1, T0, "Charging", Some(1.0), Some(50), None));
    assert_eq!(result.err(), Some(LifecycleError::IdentifierExhausted));
    let mut state = OpenSessionState::new();
    state.next_charge_sample_id = i64::MAX;
    let result = apply_sample(state, 1, &cv(1, T0, "Charging", Some(1.0), Some(50), None));
    assert_eq!(result.err(), Some(LifecycleError::IdentifierExhausted));
}

#[test]
fn a_restart_between_any_two_observations_gives_the_same_charge_rows() {
    let samples = st53_samples();
    let whole = run(&samples);
    for split in 1..samples.len() {
        let first = run(&samples[..split]);
        let blob = first.state.encode().expect("encode");
        let restored = OpenSessionState::decode(&blob).expect("decode");
        let rest = run_from(restored, &samples[split..]);
        let mut rows = first.delta;
        merge_step_delta(&mut rows, rest.delta);
        assert_eq!(rows.charges, whole.delta.charges, "split {split}");
        assert_eq!(
            rows.open_charge_samples, whole.delta.open_charge_samples,
            "split {split}"
        );
        assert_eq!(rest.state.encode().unwrap(), whole.state.encode().unwrap());
    }
}

#[test]
fn an_open_charge_does_not_suppress_standalone_positions() {
    // R74: samples carry no coordinates, so a car that moves while a charge is
    // open (a tow, a ferry) still leaves standalone positions (R53 a).
    let result = run(&[
        at(cv(1, T0, "Charging", Some(1.0), Some(50), None), 10.0, 20.0),
        at(
            cv(2, T0 + MIN, "Charging", Some(2.0), Some(51), None),
            10.002,
            20.0,
        ),
        at(
            cv(3, T0 + 2 * MIN, "Charging", Some(3.0), Some(52), None),
            10.004,
            20.0,
        ),
    ]);
    assert!(result.state.open_charge.is_some());
    assert_eq!(
        result
            .delta
            .positions
            .iter()
            .map(|position| (position.drive_id, position.date_ms))
            .collect::<Vec<_>>(),
        vec![(None, T0), (None, T0 + MIN), (None, T0 + 2 * MIN)]
    );
    assert_eq!(result.delta.open_charge_samples.len(), 3);
}

#[test]
fn replay_is_byte_identical() {
    // Replay of the same observations gives byte-identical state (R1, R18).
    let first = run(&st53_samples());
    let second = run(&st53_samples());
    assert_eq!(
        first.state.encode().unwrap(),
        second.state.encode().unwrap()
    );
    assert_eq!(first.delta, second.delta);
}

fn shuffle_obs(id: i64, t: i64, gear: &str, odometer_mi: f64) -> LifecycleSample {
    vd(
        id,
        t,
        json!({
            "drive_state": {"timestamp": t, "shift_state": gear, "speed": 2,
                            "latitude": 10.00135, "longitude": 20.0},
            "vehicle_state": {"timestamp": t, "odometer": odometer_mi},
        }),
    )
}

#[test]
fn a_fine_odometer_stays_fine_through_a_charge_close() {
    // R47 and R69: a charge that Motion evidence closes (the unplug fell
    // between two polls) leaves the snapshot with the coarseness of its last
    // odometer reading. A full source's reading is fine, so a 0.05 km shuffle
    // with a 150 m GNSS jump in a covered garage is discarded, not kept.
    let shuffle_mi = 0.05 / K_MILE_KM;
    let result = run(&[
        at(
            cv(1, T0, "Charging", Some(1.0), Some(50), Some(1000.0)),
            10.0,
            20.0,
        ),
        shuffle_obs(2, T0 + 2 * MIN, "D", 1000.0 + shuffle_mi),
        shuffle_obs(3, T0 + 4 * MIN, "P", 1000.0 + shuffle_mi),
    ]);
    assert_eq!(result.delta.charges.len(), 1, "Motion closes the charge");
    assert!(
        result.delta.drives.is_empty(),
        "a fine odometer decides alone: {:?}",
        result.delta.drives
    );
    assert_eq!(result.delta.discarded_drive_ids.len(), 1);

    // The same shuffle with a reading of the same distance is kept when the
    // odometer is coarse and the displacement is 100 m or more (R47).
    let coarse = run(&[
        at(cv(1, T0, "Charging", Some(1.0), Some(50), None), 10.0, 20.0),
        sframe(2, T0 + MIN, 50, Some(1000.0)),
        shuffle_obs(3, T0 + 2 * MIN, "D", 1000.0 + shuffle_mi),
        shuffle_obs(4, T0 + 4 * MIN, "P", 1000.0 + shuffle_mi),
    ]);
    assert_eq!(coarse.delta.charges.len(), 1);
    assert_eq!(
        coarse.delta.drives.len(),
        1,
        "a coarse reading falls back to displacement"
    );
}

#[test]
fn the_charge_snapshot_takes_the_source_of_the_last_odometer_reading() {
    let closed = |samples: &[LifecycleSample]| {
        run(samples)
            .state
            .parked_snapshot
            .expect("a charge close leaves a snapshot")
    };
    // A full source reading is fine.
    let fine = closed(&[
        cv(1, T0, "Charging", Some(1.0), Some(50), Some(1000.0)),
        cv(
            2,
            T0 + 5 * MIN,
            "Complete",
            Some(2.0),
            Some(60),
            Some(1000.0),
        ),
    ]);
    assert_eq!(fine.odometer_km, Some(1000.0 * K_MILE_KM));
    assert!(!fine.odometer_coarse);
    // The latest reading decides: a stream frame while the charge is open.
    let coarse = closed(&[
        cv(1, T0, "Charging", Some(1.0), Some(50), Some(1000.0)),
        sframe(2, T0 + 2 * MIN, 52, Some(1000.0)),
        cv(3, T0 + 5 * MIN, "Complete", Some(2.0), Some(60), None),
    ]);
    assert!(coarse.odometer_coarse);
    // A full source reading after it makes the snapshot fine again.
    let fine_again = closed(&[
        cv(1, T0, "Charging", Some(1.0), Some(50), Some(1000.0)),
        sframe(2, T0 + 2 * MIN, 52, Some(1000.0)),
        cv(
            3,
            T0 + 5 * MIN,
            "Complete",
            Some(2.0),
            Some(60),
            Some(1000.0),
        ),
    ]);
    assert!(!fine_again.odometer_coarse);
    // Without an odometer there is nothing to be coarse about.
    let none = closed(&[
        cv(1, T0, "Charging", Some(1.0), Some(50), None),
        cv(2, T0 + 5 * MIN, "Complete", Some(2.0), Some(60), None),
    ]);
    assert_eq!(none.odometer_km, None);
    assert!(!none.odometer_coarse);
}

#[test]
fn an_unknown_odometer_source_counts_as_coarse() {
    // A blob written before the source was recorded, and an imported charge,
    // carry no `last_odometer_coarse`: the keep rule falls back to displacement
    // as well, so a real trip is never lost to a resolution nobody vouches for.
    let mut open = OpenCharge::empty(1, 1, T0);
    open.last_odometer_km = Some(1609.344);
    assert_eq!(open.last_odometer_coarse, None);
    assert!(snapshot_after_charge(&open, T0 + MIN, false).odometer_coarse);
    open.last_odometer_coarse = Some(false);
    assert!(!snapshot_after_charge(&open, T0 + MIN, false).odometer_coarse);
    open.last_odometer_km = None;
    open.last_odometer_coarse = Some(true);
    assert!(!snapshot_after_charge(&open, T0 + MIN, false).odometer_coarse);
    // The field decodes from a blob without it.
    let mut value = serde_json::to_value(&open).expect("value");
    value
        .as_object_mut()
        .expect("object")
        .remove("last_odometer_coarse");
    let decoded: OpenCharge = serde_json::from_value(value).expect("old blob");
    assert_eq!(decoded.last_odometer_coarse, None);
}

#[test]
fn only_a_sample_anchoring_charge_fact_advances_the_snapshot_time() {
    // R69: `at_ms` is the latest time up to which the car's charged energy is
    // accounted for. A parked observation with only battery and odometer facts
    // moves `seen_at_ms`, not `at_ms`, so the charge it later proves starts at
    // the drive's end and not at that observation.
    let mut samples = parked_after_drive();
    samples.push(vd(
        3,
        T0 + HOUR,
        json!({"charge_state": {"timestamp": T0 + HOUR, "battery_level": 41},
               "vehicle_state": {"timestamp": T0 + HOUR, "odometer": 1000.0}}),
    ));
    let parked = run(&samples);
    let snapshot = parked.state.parked_snapshot.as_ref().expect("snapshot");
    assert_eq!(snapshot.at_ms, Some(T0));
    assert_eq!(snapshot.seen_at_ms, T0 + HOUR);
    assert_eq!(snapshot.battery_level, Some(41));
    samples.push(cv(
        4,
        T0 + 9 * HOUR,
        "Complete",
        Some(30.0),
        Some(85),
        Some(1000.0),
    ));
    let result = run(&samples);
    assert_eq!(spans(&result.delta), vec![(T0, Some(T0 + 9 * HOUR))]);

    // From a new state a stream frame creates a snapshot without `at_ms`: a
    // counter and a higher battery then prove nothing recorded, because the
    // energy accounted so far is unknown.
    let result = run(&[
        sframe(1, T0, 50, Some(1000.0)),
        cv(2, T0 + HOUR, "Complete", Some(30.0), Some(80), Some(1000.0)),
    ]);
    assert!(result.delta.charges.is_empty());
    assert!(result.state.open_charge.is_none());
    let snapshot = result.state.parked_snapshot.expect("snapshot");
    assert_eq!(snapshot.energy_added_kwh, Some(30.0));
    assert_eq!(snapshot.at_ms, Some(T0 + HOUR));
}
