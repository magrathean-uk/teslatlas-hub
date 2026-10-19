// SPDX-License-Identifier: AGPL-3.0-only

//! Part P spec tests (C1 specification section 8): the common contract, the
//! phases, service mode, software updates, conversions, the stream fold and
//! the storage-level checks of part P. Synthetic data only. `t0` is
//! 1_760_000_000_000 ms; `+Nm` is N minutes. Observation shorthands: `v` is an
//! owner `vehicle_data`, `fleet` a Fleet `vehicle_data`, `sparse` a Fleet
//! `vehicle_data` with a `provider_feed` marker, `d` a discovery, `frame` a
//! legacy stream frame and `e` a Hub event.
//!
//! The tests whose names end in `_part_d` or `_part_c` assert behaviour of the
//! drive and charge steps that the later parts own; they ran as ignored tests
//! until those parts landed.

use std::sync::{Arc, Mutex};

use serde_json::json;

use super::*;
use crate::tesla_stream::parse_data_update;

const T0: i64 = 1_760_000_000_000;
const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;

fn v(id: i64, t: i64, data: Value) -> LifecycleSample {
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

fn fleet(id: i64, t: i64, data: Value) -> LifecycleSample {
    LifecycleSample {
        observation_id: id,
        observed_at_ms: t,
        vehicle_state: "online".to_owned(),
        payload: json!({
            "record_type": "fleet_api_vehicle_data_v1",
            "source_vehicle_id": "9",
            "source_vehicle_state": "online",
            "provider_raw_json": {"response": data},
        }),
    }
}

fn sparse(id: i64, t: i64, data: Value) -> LifecycleSample {
    let mut sample = fleet(id, t, data);
    sample.payload["provider_feed"] = json!("provider_stream");
    sample
}

fn d(id: i64, t: i64, state: &str) -> LifecycleSample {
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

fn e(id: i64, t: i64, kind: &str) -> LifecycleSample {
    LifecycleSample {
        observation_id: id,
        observed_at_ms: t,
        vehicle_state: "unknown".to_owned(),
        payload: json!({"record_type": "hub_lifecycle_event_v1", "event": kind}),
    }
}

/// A stream frame built the way production builds it: wire frame, parser,
/// stored payload. `columns` are the 11 wire columns in `STREAM_COLUMNS` order;
/// the sample time `t` is element 0 of `value` (C4 R19).
fn frame(id: i64, t: i64, columns: &str) -> LifecycleSample {
    let wire = format!(r#"{{"msg_type":"data:update","tag":"9","value":"{t},{columns}"}}"#);
    let update = parse_data_update(&wire).expect("synthetic frame");
    LifecycleSample {
        observation_id: id,
        observed_at_ms: t,
        vehicle_state: "online".to_owned(),
        payload: stream_observation_payload(&update),
    }
}

fn step(state: OpenSessionState, sample: &LifecycleSample) -> LifecycleStep {
    apply_sample(state, 1, sample).expect("apply")
}

fn all(state: OpenSessionState, samples: &[LifecycleSample]) -> LifecycleStep {
    apply_samples(state, 1, samples).expect("apply many")
}

fn open_drive(id: i64, start: i64) -> OpenDrive {
    serde_json::from_value(json!({
        "id": id, "car_id": 1, "start_date_ms": start,
        "outside_temp_sum": 0.0, "outside_temp_count": 0
    }))
    .expect("open drive fixture")
}

fn open_charge(id: i64, start: i64, last_sample: i64) -> OpenCharge {
    let mut charge: OpenCharge = serde_json::from_value(json!({
        "id": id, "car_id": 1, "start_date_ms": start,
        "outside_temp_sum": 0.0, "outside_temp_count": 0
    }))
    .expect("open charge fixture");
    charge.last_sample_timestamp_ms = Some(last_sample);
    charge
}

fn closed_states(delta: &LifecycleDelta) -> Vec<(i64, String, i64, i64)> {
    delta
        .states
        .iter()
        .filter_map(|row| {
            row.end_date_ms
                .map(|end| (row.id, row.state.clone(), row.start_date_ms, end))
        })
        .collect()
}

fn families_of(state: &OpenSessionState, sample: &LifecycleSample) -> Families {
    fresh_families(state, &parse_sample(sample).expect("parse"))
}

/// Evidence of one observation after R22, computed the way the step skeleton
/// computes it, for tests that need the classes without a drive or a charge.
fn evidence(
    state: &OpenSessionState,
    sample: &LifecycleSample,
) -> (DriveEvidence, ChargeEvidence, UpdateStatusClass) {
    let parsed = parse_sample(sample).expect("parse");
    let fresh = fresh_families(state, &parsed);
    let mut drive = classify_drive(&parsed, fresh.motion);
    let mut charge = classify_charge(&parsed, fresh.charge_session);
    let mut status = if fresh.vehicle_status && parsed.kind.has_vehicle_data() {
        update_status_class(parsed.software_update_status.as_deref())
    } else {
        UpdateStatusClass::NoInfo
    };
    resolve_conflicts(&parsed, &mut drive, &mut charge, &mut status);
    (drive, charge, status)
}

// ---------------------------------------------------------------- common

#[test]
fn st1_replay_guard_returns_state_and_empty_delta() {
    let first = step(
        OpenSessionState::new(),
        &v(
            5,
            T0,
            json!({"drive_state": {"shift_state": "P", "speed": 0}}),
        ),
    );
    let again = step(
        first.state.clone(),
        &v(
            5,
            T0 + MIN,
            json!({"drive_state": {"shift_state": "D", "speed": 9}}),
        ),
    );
    assert_eq!(again.state, first.state);
    assert_eq!(again.delta, LifecycleDelta::default());
    assert!(!again.quarantined);
    let older = step(first.state.clone(), &v(4, T0 + 2 * MIN, json!({})));
    assert_eq!(older.state, first.state);
    assert_eq!(older.delta, LifecycleDelta::default());
}

#[test]
fn st2_unknown_record_type_only_advances_the_cursor() {
    let fresh = OpenSessionState::new();
    let unknown = LifecycleSample {
        observation_id: 7,
        observed_at_ms: T0,
        vehicle_state: "online".to_owned(),
        payload: json!({"record_type": "x"}),
    };
    let result = step(fresh.clone(), &unknown);
    assert!(!result.quarantined);
    assert_eq!(result.delta, LifecycleDelta::default());
    let mut expected = fresh;
    expected.last_observation_id = 7;
    assert_eq!(result.state, expected);

    let event = step(result.state, &e(8, T0, "unknown_kind"));
    assert!(!event.quarantined);
    assert_eq!(event.delta, LifecycleDelta::default());
    expected.last_observation_id = 8;
    assert_eq!(event.state, expected);
}

#[test]
fn st3_quarantine_advances_the_cursor_and_nothing_else() {
    let broken = v(1, T0, json!({"drive_state": "not an object"}));
    let result = step(OpenSessionState::new(), &broken);
    assert!(result.quarantined);
    assert_eq!(result.delta, LifecycleDelta::default());
    let mut expected = OpenSessionState::new();
    expected.last_observation_id = 1;
    assert_eq!(result.state, expected);

    let not_an_object = LifecycleSample {
        payload: json!([1, 2, 3]),
        ..v(2, T0, json!({}))
    };
    let result = step(result.state, &not_an_object);
    assert!(result.quarantined);

    let next = step(result.state, &d(3, T0 + MIN, "online"));
    assert!(!next.quarantined);
    assert_eq!(
        next.state
            .open_state
            .as_ref()
            .map(|open| open.state.as_str()),
        Some("online")
    );
}

#[test]
fn st4_field_level_invalid_values_become_none_without_quarantine() {
    let parsed = parse_sample(&v(
        1,
        T0,
        json!({
            "drive_state": {"speed": "fast", "latitude": 95.0, "longitude": 20.0},
            "charge_state": {"battery_level": 140}
        }),
    ))
    .expect("parse");
    assert_eq!(parsed.speed, None);
    assert_eq!(parsed.battery_level, None);
    assert_eq!((parsed.latitude, parsed.longitude), (None, None));

    let boolean_speed =
        parse_sample(&v(2, T0, json!({"drive_state": {"speed": true}}))).expect("parse");
    assert_eq!(boolean_speed.speed, None);

    let result = step(
        OpenSessionState::new(),
        &v(
            3,
            T0,
            json!({"drive_state": {"speed": "fast", "latitude": 95.0, "longitude": 20.0}}),
        ),
    );
    assert!(!result.quarantined);
    assert!(result.delta.positions.is_empty());
    assert!(result.delta.open_drive_positions.is_empty());
}

#[test]
fn st5_stale_families_are_judged_per_family_and_equal_times_are_duplicates() {
    let first = v(
        1,
        T0 + 2 * MIN,
        json!({"drive_state": {"timestamp": T0 + 2 * MIN, "shift_state": "D", "speed": 10,
                               "latitude": 10.0, "longitude": 20.0}}),
    );
    let after_first = step(OpenSessionState::new(), &first);
    assert_eq!(
        after_first.state.last_drive_timestamp_ms,
        Some(T0 + 2 * MIN)
    );
    assert_eq!(after_first.state.last_position_fact_ms, Some(T0 + 2 * MIN));

    let older = v(
        2,
        T0 + 3 * MIN,
        json!({"drive_state": {"timestamp": T0 + MIN, "shift_state": "P", "speed": 0,
                               "latitude": 10.001, "longitude": 20.0}}),
    );
    let families = families_of(&after_first.state, &older);
    assert!(!families.motion && !families.position);
    let after_second = step(after_first.state.clone(), &older);
    assert_eq!(
        after_second.state.last_drive_timestamp_ms,
        Some(T0 + 2 * MIN)
    );
    assert_eq!(after_second.state.last_position_fact_ms, Some(T0 + 2 * MIN));
    assert_eq!(after_second.state.last_observed_at_ms, Some(T0 + 3 * MIN));

    // Tesla re-serves a cached group with the same timestamp: a duplicate.
    let cached = v(
        3,
        T0 + 4 * MIN,
        json!({"drive_state": {"timestamp": T0 + 2 * MIN, "shift_state": "D", "speed": 10,
                               "latitude": 10.0, "longitude": 20.0}}),
    );
    let families = families_of(&after_second.state, &cached);
    assert!(!families.motion && !families.position);

    // A later time is fresh.
    let later = v(
        4,
        T0 + 4 * MIN,
        json!({"drive_state": {"timestamp": T0 + 2 * MIN + 1, "shift_state": "D", "speed": 10,
                               "latitude": 10.0, "longitude": 20.0}}),
    );
    let families = families_of(&after_second.state, &later);
    assert!(families.motion && families.position);
}

#[test]
fn st5_stale_drive_group_changes_no_drive_part_d() {
    // Full spec text of ST5: the drive stays open and obs 2 stores no position.
    let first = v(
        1,
        T0 + 2 * MIN,
        json!({"drive_state": {"timestamp": T0 + 2 * MIN, "shift_state": "D", "speed": 10,
                               "latitude": 10.0, "longitude": 20.0}}),
    );
    let after_first = step(OpenSessionState::new(), &first);
    assert!(after_first.state.open_drive.is_some());
    let second = step(
        after_first.state,
        &v(
            2,
            T0 + 3 * MIN,
            json!({"drive_state": {"timestamp": T0 + MIN, "shift_state": "P", "speed": 0,
                                   "latitude": 10.001, "longitude": 20.0}}),
        ),
    );
    assert!(second.state.open_drive.is_some());
    assert!(second.delta.open_drive_positions.is_empty());
    assert!(second.delta.positions.is_empty());
}

#[test]
fn st6_group_times_are_kept_per_group() {
    let sample = v(
        1,
        T0,
        json!({
            "drive_state": {"timestamp": T0, "shift_state": "P", "speed": 0,
                            "latitude": 10.0, "longitude": 20.0},
            "charge_state": {"timestamp": T0 - 90_000, "charging_state": "Charging",
                             "battery_level": 50},
            "climate_state": {"timestamp": T0 - 30_000, "inside_temp": 20.0},
            "vehicle_state": {"timestamp": T0 - 10_000, "odometer": 1000.0}
        }),
    );
    let parsed = parse_sample(&sample).expect("parse");
    assert_eq!(parsed.drive_timestamp_ms, T0);
    assert_eq!(parsed.charge_timestamp_ms, T0 - 90_000);
    assert_eq!(parsed.climate_timestamp_ms, T0 - 30_000);
    assert_eq!(parsed.vehicle_timestamp_ms, T0 - 10_000);
    let result = step(OpenSessionState::new(), &sample);
    assert_eq!(result.state.last_drive_timestamp_ms, Some(T0));
    assert_eq!(result.state.last_position_fact_ms, Some(T0));
    assert_eq!(result.state.last_charge_timestamp_ms, Some(T0 - 90_000));
    assert_eq!(result.state.last_battery_fact_ms, Some(T0 - 90_000));
    assert_eq!(result.state.last_climate_fact_ms, Some(T0 - 30_000));
    assert_eq!(result.state.last_odometer_fact_ms, Some(T0 - 10_000));
    assert_eq!(result.state.last_vehicle_timestamp_ms, None);
}

#[test]
fn st6_group_time_outside_the_plausible_window_falls_back_to_the_observation() {
    for bad in [1_i64, K_EARLIEST_MS - 1, T0 + K_SKEW_MS + 1] {
        let parsed = parse_sample(&v(
            1,
            T0,
            json!({"drive_state": {"timestamp": bad, "speed": 5}}),
        ))
        .expect("parse");
        assert_eq!(parsed.drive_timestamp_ms, T0, "timestamp {bad}");
    }
    for good in [K_EARLIEST_MS, T0 + K_SKEW_MS] {
        let parsed = parse_sample(&v(
            1,
            T0,
            json!({"drive_state": {"timestamp": good, "speed": 5}}),
        ))
        .expect("parse");
        assert_eq!(parsed.drive_timestamp_ms, good);
    }
    let text = parse_sample(&v(
        1,
        T0,
        json!({"drive_state": {"timestamp": (T0 - 5).to_string(), "speed": 5}}),
    ))
    .expect("parse");
    assert_eq!(text.drive_timestamp_ms, T0 - 5);
}

fn determinism_sequence() -> Vec<LifecycleSample> {
    let mut samples = Vec::new();
    for k in 0..40_i64 {
        let id = k + 1;
        let t = T0 + k * 30_000;
        let sample = match k {
            0 => d(id, t, "online"),
            20 => e(id, t, "no_data"),
            k if k % 7 == 3 => d(id, t, if k % 2 == 0 { "asleep" } else { "online" }),
            k if k < 20 => v(
                id,
                t,
                json!({
                    "drive_state": {"timestamp": t, "shift_state": "D", "speed": 20 + k,
                                    "latitude": 10.0 + k as f64 * 0.001, "longitude": 20.0},
                    "charge_state": {"timestamp": t, "battery_level": 80 - k / 4},
                    "vehicle_state": {"timestamp": t, "odometer": 1000.0 + k as f64 * 0.3,
                                      "car_version": "2024.14.3 aa"}
                }),
            ),
            k => v(
                id,
                t,
                json!({
                    "drive_state": {"timestamp": t, "shift_state": "P", "speed": 0,
                                    "latitude": 10.02, "longitude": 20.0},
                    "charge_state": {"timestamp": t, "charging_state": if k < 35 { "Charging" } else { "Complete" },
                                     "charge_energy_added": (k - 20) as f64 * 0.5, "battery_level": 60},
                    "vehicle_state": {"timestamp": t, "odometer": 1006.0,
                                      "car_version": if k < 30 { "2024.14.3 aa" } else { "2024.20.1 bb" }}
                }),
            ),
        };
        samples.push(sample);
    }
    samples
}

#[test]
fn st7_replay_is_deterministic_and_survives_encode_decode() {
    let samples = determinism_sequence();
    let first = all(OpenSessionState::new(), &samples);
    let second = all(OpenSessionState::new(), &samples);
    assert_eq!(
        first.state.encode().unwrap(),
        second.state.encode().unwrap()
    );
    assert_eq!(first.delta, second.delta);

    for split in [1, 13, 20, 21, 39] {
        let head = all(OpenSessionState::new(), &samples[..split]);
        let restored =
            OpenSessionState::decode(&head.state.encode().unwrap()).expect("decode between");
        let tail = all(restored, &samples[split..]);
        assert_eq!(
            tail.state.encode().unwrap(),
            first.state.encode().unwrap(),
            "split at {split}"
        );
        let mut states = head.delta.states.clone();
        states.extend(tail.delta.states.clone());
        assert_eq!(states, first.delta.states, "split at {split}");
        let mut updates = head.delta.updates.clone();
        updates.extend(tail.delta.updates.clone());
        assert_eq!(updates, first.delta.updates, "split at {split}");
    }
}

/// The R12 fields as they appear in JSON, to strip them from an old blob.
const R12_STATE_KEYS: [&str; 11] = [
    "parked_snapshot",
    "last_car_version",
    "last_car_version_at_ms",
    "last_standalone_latitude",
    "last_standalone_longitude",
    "last_reach",
    "last_position_fact_ms",
    "last_battery_fact_ms",
    "last_climate_fact_ms",
    "last_odometer_fact_ms",
    "service_mode_refused",
];
const R12_DRIVE_KEYS: [&str; 6] = [
    "last_folded_at_ms",
    "last_motion_at_ms",
    "max_displacement_m",
    "elevation_count",
    "odometer_coarse",
    "still_checkpoint",
];

fn old_blob(state: &OpenSessionState) -> Vec<u8> {
    let mut value = serde_json::to_value(state).expect("state value");
    let object = value.as_object_mut().expect("state object");
    for key in R12_STATE_KEYS {
        assert!(object.remove(key).is_some(), "state key {key} exists");
    }
    if let Some(drive) = object.get_mut("open_drive").and_then(Value::as_object_mut) {
        for key in R12_DRIVE_KEYS {
            assert!(drive.remove(key).is_some(), "drive key {key} exists");
        }
    }
    if let Some(charge) = object.get_mut("open_charge").and_then(Value::as_object_mut) {
        assert!(charge.remove("last_odometer_km").is_some());
    }
    serde_json::to_vec(&value).expect("old blob bytes")
}

#[test]
fn st8_old_blob_decodes_and_reads_with_the_r12_fallbacks() {
    let mut state = OpenSessionState::new();
    state.phase = VehiclePhase::Suspended;
    let mut drive = open_drive(3, T0);
    drive.last_elevation = Some(120);
    drive.last_position_date_ms = Some(T0 + 5 * MIN);
    state.open_drive = Some(drive);
    state.pending_gained_range_charge = Some(GainedRangeChargeSeed {
        start_date_ms: T0 - HOUR,
        start_battery_level: Some(40),
        start_ideal_range_km: Some(250.5),
        start_rated_range_km: Some(240.5),
        start_latitude: Some(10.0),
        start_longitude: Some(20.0),
        first_energy_added: 5.0,
    });
    state.last_drive_timestamp_ms = Some(T0 + 5 * MIN);
    state.last_charge_timestamp_ms = Some(T0 - HOUR);
    state.last_vehicle_timestamp_ms = Some(T0 + 4 * MIN);
    let bytes = old_blob(&state);

    let decoded = OpenSessionState::decode(&bytes).expect("old blob decodes");
    assert_eq!(decoded.phase, VehiclePhase::Suspended);
    assert_eq!(decoded.phase.effective(), VehiclePhase::Online);
    let drive = decoded.open_drive.as_ref().expect("drive");
    assert_eq!(drive.last_folded_at_ms, None);
    assert_eq!(drive.effective_last_folded_ms(), T0 + 5 * MIN);
    assert_eq!(drive.effective_last_motion_ms(), T0 + 5 * MIN);
    assert_eq!(drive.effective_elevation_count(), 1);
    assert!(drive.still_checkpoint.is_none());
    assert!(decoded.parked_snapshot.is_none());
    assert!(!decoded.service_mode_refused);

    // Family watermarks fall back to the retained group watermarks (R6).
    let data = json!({
        "drive_state": {"timestamp": T0 + 5 * MIN, "latitude": 10.0, "longitude": 20.0,
                        "shift_state": "D", "speed": 4},
        "charge_state": {"timestamp": T0 - HOUR, "battery_level": 40},
        "vehicle_state": {"timestamp": T0 + 4 * MIN, "odometer": 5.0}
    });
    let families = families_of(&decoded, &v(1, T0 + 6 * MIN, data.clone()));
    assert!(!families.motion && !families.position);
    assert!(!families.battery && !families.odometer);
    let later = json!({
        "drive_state": {"timestamp": T0 + 6 * MIN, "latitude": 10.0, "longitude": 20.0},
        "charge_state": {"timestamp": T0 - HOUR + 1, "battery_level": 40},
        "vehicle_state": {"timestamp": T0 + 4 * MIN + 1, "odometer": 5.0}
    });
    let families = families_of(&decoded, &v(1, T0 + 6 * MIN, later));
    assert!(families.position && families.battery && families.odometer);

    // The next step drops the pending seed; a drive is open, so no snapshot.
    let next = step(decoded, &v(1, T0 + 6 * MIN, json!({})));
    assert_eq!(next.state.pending_gained_range_charge, None);
    assert!(next.state.parked_snapshot.is_none());
    assert!(next.state.open_drive.is_some());
}

#[test]
fn st8_pending_seed_becomes_a_parked_snapshot_once() {
    let mut state = OpenSessionState::new();
    state.pending_gained_range_charge = Some(GainedRangeChargeSeed {
        start_date_ms: T0,
        start_battery_level: Some(40),
        start_ideal_range_km: Some(250.5),
        start_rated_range_km: Some(240.5),
        start_latitude: Some(10.0),
        start_longitude: Some(20.0),
        first_energy_added: 5.0,
    });
    convert_pending_seed(&mut state);
    assert_eq!(state.pending_gained_range_charge, None);
    assert_eq!(
        state.parked_snapshot,
        Some(ParkedSnapshot {
            at_ms: Some(T0),
            seen_at_ms: T0,
            battery_level: Some(40),
            ideal_range_km: Some(250.5),
            rated_range_km: Some(240.5),
            odometer_km: None,
            odometer_coarse: false,
            latitude: Some(10.0),
            longitude: Some(20.0),
            energy_added_kwh: Some(5.0),
            after_drive: false,
            unplugged: false,
        })
    );
    // Converted once: a later call finds nothing to convert.
    let snapshot = state.parked_snapshot.clone();
    convert_pending_seed(&mut state);
    assert_eq!(state.parked_snapshot, snapshot);

    // An existing snapshot is newer and is kept; the seed is only dropped.
    let mut with_snapshot = OpenSessionState::new();
    with_snapshot.parked_snapshot = Some(ParkedSnapshot {
        seen_at_ms: T0 + HOUR,
        ..ParkedSnapshot::default()
    });
    with_snapshot.pending_gained_range_charge = Some(GainedRangeChargeSeed {
        start_date_ms: T0,
        start_battery_level: None,
        start_ideal_range_km: None,
        start_rated_range_km: None,
        start_latitude: None,
        start_longitude: None,
        first_energy_added: 1.0,
    });
    convert_pending_seed(&mut with_snapshot);
    assert_eq!(with_snapshot.pending_gained_range_charge, None);
    assert_eq!(
        with_snapshot.parked_snapshot.map(|s| s.seen_at_ms),
        Some(T0 + HOUR)
    );
}

#[test]
fn st9_errors() {
    let sample = v(1, T0, json!({}));
    assert_eq!(
        apply_sample(OpenSessionState::new(), 0, &sample),
        Err(LifecycleError::InvalidCarId)
    );
    assert_eq!(
        apply_sample(OpenSessionState::new(), -3, &sample),
        Err(LifecycleError::InvalidCarId)
    );
    assert_eq!(
        apply_sample_with_offline_drive_timeout(
            OpenSessionState::new(),
            1,
            &sample,
            Duration::ZERO
        ),
        Err(LifecycleError::InvalidOfflineDriveTimeout)
    );
    assert_eq!(
        apply_sample_with_offline_drive_timeout(OpenSessionState::new(), 1, &sample, Duration::MAX),
        Err(LifecycleError::InvalidOfflineDriveTimeout)
    );
    // The id cursor of the state intervals would overflow.
    let mut exhausted = OpenSessionState::new();
    exhausted.next_state_id = i64::MAX;
    assert_eq!(
        apply_sample(exhausted, 1, &d(1, T0, "online")),
        Err(LifecycleError::IdentifierExhausted)
    );
    let mut exhausted = OpenSessionState::new();
    exhausted.next_update_id = i64::MAX;
    exhausted.last_car_version = Some("2024.14.3".to_owned());
    exhausted.last_car_version_at_ms = Some(T0 - HOUR);
    assert_eq!(
        apply_sample(
            exhausted,
            1,
            &v(
                1,
                T0,
                json!({"vehicle_state": {"timestamp": T0, "car_version": "2024.20.1"}})
            )
        ),
        Err(LifecycleError::IdentifierExhausted)
    );
}

#[test]
fn st9_exhausted_drive_ids_fail_part_d() {
    let mut exhausted = OpenSessionState::new();
    exhausted.next_drive_id = i64::MAX;
    assert_eq!(
        apply_sample(
            exhausted,
            1,
            &v(
                1,
                T0,
                json!({"drive_state": {"shift_state": "D", "speed": 20}})
            )
        ),
        Err(LifecycleError::IdentifierExhausted)
    );
}

// ---------------------------------------------------------------- phases

#[test]
fn st10_state_intervals_open_close_and_continue() {
    let samples = [
        d(1, T0, "online"),
        v(
            2,
            T0 + MIN,
            json!({"drive_state": {"timestamp": T0 + MIN, "shift_state": "P", "speed": 0}}),
        ),
        d(3, T0 + 20 * MIN, "asleep"),
        d(4, T0 + 5 * HOUR, "online"),
    ];
    let result = all(OpenSessionState::new(), &samples);
    assert_eq!(
        closed_states(&result.delta),
        vec![
            (1, "online".to_owned(), T0, T0 + 20 * MIN),
            (2, "asleep".to_owned(), T0 + 20 * MIN, T0 + 5 * HOUR),
        ]
    );
    assert_eq!(
        result.state.open_state,
        Some(OpenState {
            id: 3,
            car_id: 1,
            state: "online".to_owned(),
            start_date_ms: T0 + 5 * HOUR,
        })
    );
    // The open intervals are emitted once, when they open.
    assert_eq!(result.delta.states.len(), 5);
    assert_eq!(result.state.phase, VehiclePhase::Online);
    assert_eq!(result.state.last_reach.as_deref(), Some("online"));
}

#[test]
fn st11_silence_never_changes_an_interval() {
    let result = all(
        OpenSessionState::new(),
        &[
            d(1, T0, "online"),
            v(
                2,
                T0 + 4 * HOUR,
                json!({"drive_state": {"timestamp": T0 + 4 * HOUR, "shift_state": "P"}}),
            ),
        ],
    );
    assert_eq!(result.delta.states.len(), 1);
    assert_eq!(result.delta.states[0].end_date_ms, None);
    assert!(closed_states(&result.delta).is_empty());
}

#[test]
fn st12_late_discovery_is_ignored_and_a_later_one_accepted() {
    let first = step(
        OpenSessionState::new(),
        &v(
            1,
            T0 + 10 * MIN,
            json!({"vehicle_state": {"timestamp": T0 + 10 * MIN, "odometer": 1.0}}),
        ),
    );
    assert_eq!(first.state.last_reach.as_deref(), Some("online"));
    let late = step(first.state.clone(), &d(2, T0 + 5 * MIN, "asleep"));
    assert!(late.delta.states.is_empty());
    assert_eq!(late.state.open_state, first.state.open_state);
    assert_eq!(late.state.last_reach.as_deref(), Some("online"));
    assert_eq!(late.state.last_observed_at_ms, Some(T0 + 10 * MIN));
    assert_eq!(late.state.phase, VehiclePhase::Online);

    let accepted = step(late.state, &d(3, T0 + 11 * MIN, "asleep"));
    assert_eq!(
        closed_states(&accepted.delta),
        vec![(1, "online".to_owned(), T0 + 10 * MIN, T0 + 11 * MIN)]
    );
    assert_eq!(
        accepted
            .state
            .open_state
            .map(|open| (open.state, open.start_date_ms)),
        Some(("asleep".to_owned(), T0 + 11 * MIN))
    );
    assert_eq!(accepted.state.phase, VehiclePhase::Asleep);
}

#[test]
fn st13_offline_during_a_drive_keeps_it_open_and_records_the_flag() {
    let mut state = OpenSessionState::new();
    state.open_drive = Some(open_drive(1, T0));
    state.phase = VehiclePhase::Driving;
    state.open_state = Some(OpenState {
        id: 1,
        car_id: 1,
        state: "online".to_owned(),
        start_date_ms: T0 - HOUR,
    });
    state.next_state_id = 2;
    state.last_observed_at_ms = Some(T0);
    let result = step(state, &d(1, T0 + MIN, "offline"));
    assert_eq!(result.state.phase, VehiclePhase::Offline);
    assert!(
        result
            .state
            .open_drive
            .as_ref()
            .is_some_and(|drive| drive.saw_offline)
    );
    assert_eq!(
        closed_states(&result.delta),
        vec![(1, "online".to_owned(), T0 - HOUR, T0 + MIN)]
    );
    assert_eq!(
        result
            .state
            .open_state
            .map(|open| (open.state, open.start_date_ms)),
        Some(("offline".to_owned(), T0 + MIN))
    );
}

#[test]
fn phase_precedence_follows_the_r21_table() {
    let base = |reach: &str| {
        let mut state = OpenSessionState::new();
        state.last_reach = Some(reach.to_owned());
        state.last_observed_at_ms = Some(T0);
        state
    };
    // A discovery with no evidence of its own judges the persisted reach. It
    // is not a `no_data` event, which would close an open drive (R15).
    let silent = |state: OpenSessionState| step(state, &d(1, T0 + MIN, "unknown")).state.phase;

    let mut asleep = base("asleep");
    asleep.open_drive = Some(open_drive(1, T0));
    assert_eq!(silent(asleep), VehiclePhase::Asleep);
    let mut offline = base("offline");
    offline.open_charge = Some(open_charge(1, T0, T0));
    assert_eq!(silent(offline), VehiclePhase::Offline);
    let mut driving = base("online");
    driving.open_drive = Some(open_drive(1, T0));
    driving.open_update = Some(OpenUpdate {
        id: 1,
        car_id: 1,
        start_date_ms: T0,
    });
    driving.open_charge = Some(open_charge(1, T0, T0));
    assert_eq!(silent(driving), VehiclePhase::Driving);
    let mut updating = base("online");
    updating.open_update = Some(OpenUpdate {
        id: 1,
        car_id: 1,
        start_date_ms: T0,
    });
    updating.open_charge = Some(open_charge(1, T0, T0));
    assert_eq!(silent(updating), VehiclePhase::Updating);
    let mut charging = base("online");
    charging.open_charge = Some(open_charge(1, T0, T0));
    assert_eq!(silent(charging), VehiclePhase::Charging);
    assert_eq!(silent(base("online")), VehiclePhase::Online);
    // No evidence: the seeded phase stays; Suspended reads as Online.
    let mut seeded = OpenSessionState::new();
    seeded.phase = VehiclePhase::Suspended;
    assert_eq!(silent(seeded), VehiclePhase::Online);
    assert_eq!(silent(OpenSessionState::new()), VehiclePhase::Unknown);
}

#[test]
fn asleep_and_offline_reach_override_the_envelope_only_without_fresh_data() {
    let fresh_data = LifecycleSample {
        vehicle_state: "asleep".to_owned(),
        payload: json!({
            "record_type": "owner_api_vehicle_data_v1",
            "source_vehicle_state": "asleep",
            "vehicle_data": {"drive_state": {"timestamp": T0, "shift_state": "P", "speed": 0}}
        }),
        ..v(1, T0, json!({}))
    };
    // Data from a connected car proves online whatever a stale envelope says.
    let result = step(OpenSessionState::new(), &fresh_data);
    assert_eq!(result.state.last_reach.as_deref(), Some("online"));
    assert_eq!(result.state.phase, VehiclePhase::Online);

    // The same cached data again has no fresh family: the envelope is used.
    let cached = LifecycleSample {
        observation_id: 2,
        observed_at_ms: T0 + MIN,
        ..fresh_data
    };
    let result = step(result.state, &cached);
    assert_eq!(result.state.last_reach.as_deref(), Some("asleep"));
    assert_eq!(result.state.phase, VehiclePhase::Asleep);
}

#[test]
fn storage_substitute_unknown_gives_no_reachability() {
    let result = step(OpenSessionState::new(), &d(1, T0, "unknown"));
    assert_eq!(result.state.last_reach, None);
    assert!(result.state.open_state.is_none());
    assert_eq!(result.state.phase, VehiclePhase::Unknown);
    let result = step(result.state, &d(2, T0 + MIN, "Online"));
    assert_eq!(result.state.last_reach, None);
}

#[test]
fn helper_semantics_follow_r23() {
    assert_eq!(logical_state(VehiclePhase::Online), Some("online"));
    assert_eq!(logical_state(VehiclePhase::Driving), Some("online"));
    assert_eq!(logical_state(VehiclePhase::Charging), Some("online"));
    assert_eq!(logical_state(VehiclePhase::Updating), Some("online"));
    assert_eq!(logical_state(VehiclePhase::Suspended), Some("online"));
    assert_eq!(logical_state(VehiclePhase::Asleep), Some("asleep"));
    assert_eq!(logical_state(VehiclePhase::Offline), Some("offline"));
    assert_eq!(logical_state(VehiclePhase::Unknown), None);
    assert_eq!(phase_from_vehicle_state("online"), VehiclePhase::Online);
    assert_eq!(phase_from_vehicle_state("asleep"), VehiclePhase::Asleep);
    assert_eq!(phase_from_vehicle_state("offline"), VehiclePhase::Offline);
    assert_eq!(phase_from_vehicle_state("suspended"), VehiclePhase::Unknown);
    assert_eq!(phase_from_vehicle_state(""), VehiclePhase::Unknown);
    for (shift, expected) in [
        (Some("D"), true),
        (Some("r"), true),
        (Some(" N "), true),
        (Some("ShiftStateD"), true),
        (Some("P"), false),
        (Some("SNA"), false),
        (Some("INVALID"), false),
        (Some("UNKNOWN"), false),
        (Some(""), false),
        (None, false),
    ] {
        assert_eq!(is_drive_shift(shift), expected, "shift {shift:?}");
    }
}

// ----------------------------------------------------------- service mode

fn in_service(state: &OpenSessionState) -> (Option<bool>, bool) {
    (state.service_mode, state.service_mode_refused)
}

#[test]
fn st14_service_mode_flag_follows_the_vehicle_statement() {
    let mut state = OpenSessionState::new();
    let stated = |id: i64, t: i64, value: bool| {
        v(
            id,
            t,
            json!({"vehicle_state": {"timestamp": t, "service_mode": value}}),
        )
    };
    state = step(state, &stated(1, T0, true)).state;
    assert_eq!(in_service(&state), (Some(true), false));
    state = step(state, &stated(2, T0 + MIN, false)).state;
    assert_eq!(in_service(&state), (Some(false), false));
    // A stale vehicle group is no statement.
    state = step(state, &stated(3, T0 + 2 * MIN, true)).state;
    state.last_vehicle_timestamp_ms = Some(T0 + 10 * MIN);
    state = step(state, &stated(4, T0 + 3 * MIN, false)).state;
    assert_eq!(in_service(&state), (Some(true), false));
}

#[test]
fn st14_service_mode_blocks_and_closes_drives_part_d() {
    let mut state = OpenSessionState::new();
    for (id, t) in [(1, T0), (2, T0 + MIN)] {
        state = step(
            state,
            &v(
                id,
                t,
                json!({"drive_state": {"timestamp": t, "shift_state": "D", "speed": 20,
                                       "latitude": 10.0 + id as f64 * 0.001, "longitude": 20.0},
                       "vehicle_state": {"timestamp": t, "service_mode": false}}),
            ),
        )
        .state;
    }
    let in_service = step(
        state,
        &v(
            3,
            T0 + 2 * MIN,
            json!({"drive_state": {"timestamp": T0 + 2 * MIN, "shift_state": "D", "speed": 20,
                                   "latitude": 10.003, "longitude": 20.0},
                   "vehicle_state": {"timestamp": T0 + 2 * MIN, "service_mode": true}}),
        ),
    );
    assert_eq!(in_service.delta.drives.len(), 1);
    assert_eq!(in_service.delta.drives[0].end_date_ms, T0 + MIN);
    assert!(in_service.state.open_drive.is_none());
    assert_eq!(in_service.delta.positions.len(), 1);
    assert_eq!(in_service.delta.positions[0].drive_id, None);
    let exited = step(
        in_service.state,
        &v(
            4,
            T0 + 3 * MIN,
            json!({"drive_state": {"timestamp": T0 + 3 * MIN, "shift_state": "D", "speed": 20,
                                   "latitude": 10.004, "longitude": 20.0},
                   "vehicle_state": {"timestamp": T0 + 3 * MIN, "service_mode": false}}),
        ),
    );
    assert!(exited.state.open_drive.is_some());
}

#[test]
fn st14_in_service_motion_gives_a_standalone_position_per_observation() {
    // R53 (b): while in service, every observation with Motion evidence stores
    // a standalone position even when the car has hardly moved and a position
    // was stored a minute ago; an observation without Motion follows rule (a).
    let in_service_obs = |id: i64, t: i64, gear: Value, latitude: f64| {
        let speed = if gear == json!("D") { 20 } else { 0 };
        v(
            id,
            t,
            json!({"drive_state": {"timestamp": t, "shift_state": gear, "speed": speed,
                                   "latitude": latitude, "longitude": 20.0},
                   "vehicle_state": {"timestamp": t, "service_mode": true}}),
        )
    };
    let mut state = OpenSessionState::new();
    let mut stored = Vec::new();
    for (id, k, gear, latitude) in [
        (1, 0, json!("D"), 10.0),
        (2, 1, json!("D"), 10.00005),
        (3, 2, json!("D"), 10.0001),
        // Park at the same spot, 5 m away: no Motion, nothing new.
        (4, 3, json!("P"), 10.00011),
        // NoInfo at the same spot: nothing new.
        (5, 4, Value::Null, 10.00012),
    ] {
        let result = step(state, &in_service_obs(id, T0 + k * MIN, gear, latitude));
        stored.push(result.delta.positions.len());
        state = result.state;
    }
    // The first is rule (a) (no earlier standalone position), the next two are
    // rule (b); Park and the gear-less observation are neither.
    assert_eq!(stored, vec![1, 1, 1, 0, 0]);
    assert!(state.open_drive.is_none());
}

#[test]
fn a_late_observation_does_not_move_a_last_known_value_backwards() {
    // R6a: a stale climate group states its own values on its own row, but the
    // last known values move only for fresh families, so a later frame that
    // omits the field does not inherit the older reading.
    let climate = |id: i64, t: i64, group_time: i64, fan: i64, rear_defroster: bool| {
        v(
            id,
            t,
            json!({"drive_state": {"timestamp": t, "latitude": 10.0, "longitude": 20.0},
                   "climate_state": {"timestamp": group_time, "fan_status": fan,
                                     "is_rear_defroster_on": rear_defroster}}),
        )
    };
    let mut state = step(OpenSessionState::new(), &climate(1, T0, T0, 3, false)).state;
    assert_eq!(state.last_position_fan_status, Some(3));
    // Delivered later, but its climate group is older than the last one.
    let late = step(state, &climate(2, T0 + MIN, T0 - 1_000, 7, true));
    // The row it may emit keeps what it states (rule (a) emits none here: the
    // car has not moved), and the carried values did not move backwards.
    state = late.state;
    assert_eq!(state.last_position_fan_status, Some(3));
    assert_eq!(state.last_position_is_rear_defroster_on, Some(false));
    // A later frame without climate inherits the fresh values.
    let next = step(
        state,
        &frame(3, T0 + 20 * MIN, "25,10.0,20.0,210,181,1000.0,,200,,80,0"),
    );
    let position = next
        .delta
        .positions
        .first()
        .expect("a position after 15 minutes");
    assert_eq!(position.fan_status, Some(3));
    assert_eq!(position.is_rear_defroster_on, Some(false));
}

#[test]
fn a_late_vehicle_status_observation_does_not_move_the_last_tyre_pressure_backwards() {
    // R6a for a family other than climate: tyre pressures are vehicle_status
    // facts, so a late-delivered vehicle group states its own values on its own
    // row but moves no last known value, and a later frame that omits them
    // inherits the fresh reading.
    let pressure = |id: i64, t: i64, group_time: i64, front_left: f64| {
        v(
            id,
            t,
            json!({"drive_state": {"timestamp": t, "latitude": 10.0, "longitude": 20.0},
                   "vehicle_state": {"timestamp": group_time, "tpms_pressure_fl": front_left}}),
        )
    };
    let mut state = step(OpenSessionState::new(), &pressure(1, T0, T0, 2.9)).state;
    assert_eq!(state.last_position_tpms_pressure_fl, Some(2.9));
    // Delivered later, but its vehicle group is older than the last one.
    state = step(state, &pressure(2, T0 + MIN, T0 - 1_000, 3.4)).state;
    assert_eq!(state.last_position_tpms_pressure_fl, Some(2.9));
    // A later frame without pressures inherits the fresh value.
    let next = step(
        state,
        &frame(3, T0 + 20 * MIN, "25,10.0,20.0,210,181,1000.0,,200,,80,0"),
    );
    let position = next
        .delta
        .positions
        .first()
        .expect("a position after 15 minutes");
    assert_eq!(position.tpms_pressure_fl, Some(2.9));
    // The same field in order does move it.
    let moved = step(next.state, &pressure(4, T0 + 21 * MIN, T0 + 21 * MIN, 3.1)).state;
    assert_eq!(moved.last_position_tpms_pressure_fl, Some(3.1));
}

#[test]
fn a_late_battery_observation_does_not_move_the_last_estimated_range_backwards() {
    // R6a for the battery family: `est_battery_range` is a battery fact, so a
    // late-delivered charge group states its own value on its own row but
    // moves no last known value, and a later frame that omits it inherits the
    // fresh reading. The observations carry no other charge field, so only the
    // battery family is carried and judged.
    let range = |id: i64, t: i64, group_time: i64, miles: f64| {
        v(
            id,
            t,
            json!({"drive_state": {"timestamp": t, "latitude": 10.0, "longitude": 20.0},
                   "charge_state": {"timestamp": group_time, "est_battery_range": miles}}),
        )
    };
    let mut state = step(OpenSessionState::new(), &range(1, T0, T0, 200.0)).state;
    assert_eq!(state.last_position_est_battery_range_km, Some(321.87));
    // Delivered later, but its charge group is older than the last one.
    state = step(state, &range(2, T0 + MIN, T0 - 1_000, 300.0)).state;
    assert_eq!(state.last_position_est_battery_range_km, Some(321.87));
    // A later frame without the field inherits the fresh value.
    // (A stream frame would carry its own estimated range, so use a drive-only
    // observation.)
    let next = step(
        state,
        &v(
            3,
            T0 + 20 * MIN,
            json!({"drive_state": {"timestamp": T0 + 20 * MIN,
                                   "latitude": 10.0, "longitude": 20.0}}),
        ),
    );
    let position = next
        .delta
        .positions
        .first()
        .expect("a position after 15 minutes");
    assert_eq!(position.est_battery_range_km, Some(321.87));
    // The same field in order does move it.
    let moved = step(next.state, &range(4, T0 + 21 * MIN, T0 + 21 * MIN, 250.0)).state;
    assert_eq!(moved.last_position_est_battery_range_km, Some(402.34));
}

#[test]
fn a_late_charge_session_observation_does_not_move_the_last_battery_heater_on_backwards() {
    // R6a for the charge_session family: `battery_heater_on` is a charge
    // session fact, so a late-delivered charge group states its own value on
    // its own row but moves no last known value, and a later frame that omits
    // it inherits the fresh reading. The observations carry no battery field,
    // so only the charge_session family is carried and judged.
    let heater = |id: i64, t: i64, group_time: i64, on: bool| {
        v(
            id,
            t,
            json!({"drive_state": {"timestamp": t, "latitude": 10.0, "longitude": 20.0},
                   "charge_state": {"timestamp": group_time, "battery_heater_on": on}}),
        )
    };
    let mut state = step(OpenSessionState::new(), &heater(1, T0, T0, true)).state;
    assert_eq!(state.last_position_battery_heater_on, Some(true));
    // Delivered later, but its charge group is older than the last one.
    state = step(state, &heater(2, T0 + MIN, T0 - 1_000, false)).state;
    assert_eq!(state.last_position_battery_heater_on, Some(true));
    // A later frame without the field inherits the fresh value.
    let next = step(
        state,
        &frame(3, T0 + 20 * MIN, "25,10.0,20.0,210,181,1000.0,,200,,80,0"),
    );
    let position = next
        .delta
        .positions
        .first()
        .expect("a position after 15 minutes");
    assert_eq!(position.battery_heater_on, Some(true));
    // The same field in order does move it.
    let moved = step(next.state, &heater(4, T0 + 21 * MIN, T0 + 21 * MIN, false)).state;
    assert_eq!(moved.last_position_battery_heater_on, Some(false));
}

#[test]
fn a_fresh_value_still_moves_a_last_known_value() {
    // The same fields in order do move the carried values.
    let fan = |id: i64, t: i64, level: i64| {
        v(
            id,
            t,
            json!({"climate_state": {"timestamp": t, "fan_status": level}}),
        )
    };
    let mut state = OpenSessionState::new();
    for (id, k, level) in [(1, 0, 3), (2, 1, 5)] {
        state = step(state, &fan(id, T0 + k * MIN, level)).state;
        assert_eq!(state.last_position_fan_status, Some(level));
    }
    // An observation that does not carry the field leaves it alone.
    state = step(
        state,
        &frame(3, T0 + 2 * MIN, "25,10.0,20.0,210,181,1000.0,,200,,80,0"),
    )
    .state;
    assert_eq!(state.last_position_fan_status, Some(5));
}

#[test]
fn st15_service_refusal_event_sets_the_flags_whatever_its_time() {
    for event_time in [T0 + 50 * MIN, T0 - HOUR] {
        let mut state = OpenSessionState::new();
        state.open_charge = Some(open_charge(1, T0, T0 + 30 * MIN));
        state.last_observed_at_ms = Some(T0 + 30 * MIN);
        state.last_charge_timestamp_ms = Some(T0 + 30 * MIN);
        let result = apply_sample(state, 1, &e(1, event_time, "service_refused"))
            .expect("no error whatever the event time");
        assert_eq!(in_service(&result.state), (Some(true), true));
        // The event time never sets an end: the charge ends at its last sample
        // whether the event is dated after it or before the charge began.
        assert_eq!(result.delta.charges.len(), 1, "{event_time}");
        assert_eq!(result.delta.charges[0].end_date_ms, Some(T0 + 30 * MIN));
        // A Hub event advances no watermark and not the observation time.
        assert_eq!(result.state.last_observed_at_ms, Some(T0 + 30 * MIN));
        assert_eq!(result.state.last_charge_timestamp_ms, Some(T0 + 30 * MIN));
        assert_eq!(result.state.last_reach, None);
        assert_eq!(result.state.last_observation_id, 1);
    }
}

#[test]
fn st15_service_refusal_closes_an_open_charge_part_c() {
    let mut state = OpenSessionState::new();
    state.open_charge = Some(open_charge(1, T0, T0 + 30 * MIN));
    let result = step(state, &e(1, T0 + 50 * MIN, "service_refused"));
    assert!(result.state.open_charge.is_none());
    assert_eq!(result.delta.charges.len(), 1);
    assert_eq!(result.delta.charges[0].end_date_ms, Some(T0 + 30 * MIN));
}

#[test]
fn st70_refusal_exit_clears_the_flag_on_any_fresh_full_source_answer() {
    let refused = step(
        OpenSessionState::new(),
        &e(1, T0 + 2 * MIN, "service_refused"),
    )
    .state;
    assert_eq!(in_service(&refused), (Some(true), true));

    // The answer has a vehicle group without the field: the flag becomes None.
    let cleared = step(
        refused.clone(),
        &v(
            2,
            T0 + 30 * MIN,
            json!({"drive_state": {"timestamp": T0 + 30 * MIN, "shift_state": "D", "speed": 20},
                   "vehicle_state": {"timestamp": T0 + 30 * MIN, "odometer": 5.0}}),
        ),
    );
    assert_eq!(in_service(&cleared.state), (None, false));

    // Variant: no vehicle group at all, fresh drive and charge groups.
    let cleared = step(
        refused.clone(),
        &v(
            2,
            T0 + 30 * MIN,
            json!({"drive_state": {"timestamp": T0 + 30 * MIN, "shift_state": "D", "speed": 20},
                   "charge_state": {"timestamp": T0 + 30 * MIN, "battery_level": 50}}),
        ),
    );
    assert_eq!(in_service(&cleared.state), (None, false));

    // A stale answer (no fresh family) leaves the refusal in place.
    let mut stale_state = refused.clone();
    stale_state.last_drive_timestamp_ms = Some(T0 + 40 * MIN);
    let kept = step(
        stale_state,
        &v(
            2,
            T0 + 30 * MIN,
            json!({"drive_state": {"timestamp": T0 + 30 * MIN, "speed": 1}}),
        ),
    );
    assert_eq!(in_service(&kept.state), (Some(true), true));

    // A sparse source cannot clear a refusal by saying nothing.
    let sparse_answer = step(
        refused.clone(),
        &sparse(
            2,
            T0 + 30 * MIN,
            json!({"drive_state": {"timestamp": T0 + 30 * MIN, "speed": 30}}),
        ),
    );
    assert_eq!(in_service(&sparse_answer.state), (Some(true), true));

    // A stated false is a statement and keeps its value.
    let stated = step(
        refused,
        &v(
            2,
            T0 + 30 * MIN,
            json!({"vehicle_state": {"timestamp": T0 + 30 * MIN, "service_mode": false}}),
        ),
    );
    assert_eq!(in_service(&stated.state), (Some(false), false));
}

#[test]
fn st70_a_refusal_exit_lets_a_drive_open_part_d() {
    let mut state = step(
        OpenSessionState::new(),
        &e(1, T0 + 2 * MIN, "service_refused"),
    )
    .state;
    state = step(
        state,
        &v(
            2,
            T0 + 30 * MIN,
            json!({"drive_state": {"timestamp": T0 + 30 * MIN, "shift_state": "D", "speed": 20,
                                   "latitude": 10.0, "longitude": 20.0},
                   "vehicle_state": {"timestamp": T0 + 30 * MIN, "odometer": 5.0}}),
        ),
    )
    .state;
    assert_eq!(
        state.open_drive.as_ref().map(|drive| drive.start_date_ms),
        Some(T0 + 30 * MIN)
    );
}

#[test]
fn st71_observed_flag_then_a_sparse_source() {
    let mut state = step(
        OpenSessionState::new(),
        &v(
            1,
            T0,
            json!({"vehicle_state": {"timestamp": T0, "service_mode": true}}),
        ),
    )
    .state;
    assert_eq!(in_service(&state), (Some(true), false));
    // A sparse observation without the field is no statement.
    state = step(
        state,
        &sparse(
            2,
            T0 + MIN,
            json!({"drive_state": {"timestamp": T0 + MIN, "shift_state": "D", "speed": 30}}),
        ),
    )
    .state;
    assert_eq!(in_service(&state), (Some(true), false));
    // A sparse statement counts.
    state = step(
        state,
        &sparse(
            3,
            T0 + 2 * MIN,
            json!({"vehicle_state": {"timestamp": T0 + 2 * MIN, "service_mode": false}}),
        ),
    )
    .state;
    assert_eq!(in_service(&state), (Some(false), false));

    // Variant: a full source with a vehicle group but no field clears to None.
    let flagged = step(
        OpenSessionState::new(),
        &v(
            1,
            T0,
            json!({"vehicle_state": {"timestamp": T0, "service_mode": true}}),
        ),
    )
    .state;
    let none = step(
        flagged,
        &v(
            2,
            T0 + MIN,
            json!({"vehicle_state": {"timestamp": T0 + MIN, "car_version": "2024.14.3 aa"}}),
        ),
    );
    assert_eq!(in_service(&none.state), (None, false));
}

#[test]
fn st71_sparse_motion_in_service_is_a_standalone_position_part_d() {
    let mut state = step(
        OpenSessionState::new(),
        &v(
            1,
            T0,
            json!({"vehicle_state": {"timestamp": T0, "service_mode": true}}),
        ),
    )
    .state;
    let moving = step(
        state.clone(),
        &sparse(
            2,
            T0 + MIN,
            json!({"drive_state": {"timestamp": T0 + MIN, "shift_state": "D", "speed": 30,
                                   "latitude": 10.0, "longitude": 20.0}}),
        ),
    );
    assert!(moving.state.open_drive.is_none());
    assert_eq!(moving.delta.positions.len(), 1);
    assert_eq!(moving.delta.positions[0].drive_id, None);
    state = moving.state;
    state = step(
        state,
        &sparse(
            3,
            T0 + 2 * MIN,
            json!({"vehicle_state": {"timestamp": T0 + 2 * MIN, "service_mode": false}}),
        ),
    )
    .state;
    let opened = step(
        state,
        &sparse(
            4,
            T0 + 3 * MIN,
            json!({"drive_state": {"timestamp": T0 + 3 * MIN, "shift_state": "D", "speed": 30,
                                   "latitude": 10.001, "longitude": 20.0}}),
        ),
    );
    assert_eq!(
        opened.state.open_drive.map(|drive| drive.start_date_ms),
        Some(T0 + 3 * MIN)
    );
}

// ------------------------------------------------------- conflicting evidence

#[test]
fn st72_conflicting_motion_and_plugged_evidence_resolves_to_the_later_time() {
    let state = OpenSessionState::new();
    // (a) Motion at +1m, Complete at +55s: Motion wins.
    let (drive, charge, _) = evidence(
        &state,
        &v(
            1,
            T0 + MIN,
            json!({"drive_state": {"timestamp": T0 + MIN, "shift_state": "D", "speed": 20},
                   "charge_state": {"timestamp": T0 + 55_000, "charging_state": "Complete"}}),
        ),
    );
    assert_eq!(
        (drive, charge),
        (DriveEvidence::Motion, ChargeEvidence::None)
    );
    // (b) Motion at +55s, Charging at +1m: plugged wins, Motion becomes NoInfo.
    let (drive, charge, _) = evidence(
        &state,
        &v(
            1,
            T0 + MIN,
            json!({"drive_state": {"timestamp": T0 + 55_000, "shift_state": "D", "speed": 20},
                   "charge_state": {"timestamp": T0 + MIN, "charging_state": "Charging"}}),
        ),
    );
    assert_eq!(
        (drive, charge),
        (DriveEvidence::NoInfo, ChargeEvidence::Active)
    );
    // (c) Charging 5 s older than the drive group: Motion wins.
    let (drive, charge, _) = evidence(
        &state,
        &v(
            1,
            T0,
            json!({"drive_state": {"timestamp": T0, "shift_state": "D", "speed": 20},
                   "charge_state": {"timestamp": T0 - 5_000, "charging_state": "Charging"}}),
        ),
    );
    assert_eq!(
        (drive, charge),
        (DriveEvidence::Motion, ChargeEvidence::None)
    );
    // (d) The drive group 5 s older than Charging: plugged wins.
    let (drive, charge, _) = evidence(
        &state,
        &v(
            1,
            T0,
            json!({"drive_state": {"timestamp": T0 - 5_000, "shift_state": "D", "speed": 20},
                   "charge_state": {"timestamp": T0, "charging_state": "Charging"}}),
        ),
    );
    assert_eq!(
        (drive, charge),
        (DriveEvidence::NoInfo, ChargeEvidence::Active)
    );
    // (e) Equal times: Motion wins, whichever plugged state the car reports.
    for plugged in ["Charging", "Stopped", "Complete"] {
        let (drive, charge, _) = evidence(
            &state,
            &v(
                1,
                T0,
                json!({"drive_state": {"timestamp": T0, "shift_state": "D", "speed": 20},
                       "charge_state": {"timestamp": T0, "charging_state": plugged}}),
            ),
        );
        assert_eq!(
            (drive, charge),
            (DriveEvidence::Motion, ChargeEvidence::None),
            "{plugged}"
        );
    }
    // Park is not driving evidence: it coexists with plugged evidence.
    let (drive, charge, _) = evidence(
        &state,
        &v(
            1,
            T0,
            json!({"drive_state": {"timestamp": T0, "shift_state": "P", "speed": 0},
                   "charge_state": {"timestamp": T0, "charging_state": "Charging"}}),
        ),
    );
    assert_eq!(
        (drive, charge),
        (DriveEvidence::Park, ChargeEvidence::Active)
    );
    // Disconnected is not plugged evidence and does not conflict with Motion.
    let (drive, charge, _) = evidence(
        &state,
        &v(
            1,
            T0,
            json!({"drive_state": {"timestamp": T0 - 1_000, "shift_state": "D", "speed": 20},
                   "charge_state": {"timestamp": T0, "charging_state": "Disconnected"}}),
        ),
    );
    assert_eq!(
        (drive, charge),
        (DriveEvidence::Motion, ChargeEvidence::Disconnected)
    );
}

#[test]
fn st72_the_consequences_of_conflicts_part_d_and_c() {
    // (c) Charge open, the drive group newer: the charge is closed at its last
    // sample and a drive opens at the drive time.
    let mut state = OpenSessionState::new();
    state.open_charge = Some(open_charge(1, T0 - HOUR, T0 - 10 * MIN));
    let result = step(
        state,
        &v(
            1,
            T0,
            json!({"drive_state": {"timestamp": T0, "shift_state": "D", "speed": 20,
                                   "latitude": 10.0, "longitude": 20.0},
                   "charge_state": {"timestamp": T0 - 5_000, "charging_state": "Charging"}}),
        ),
    );
    assert_eq!(result.delta.charges[0].end_date_ms, Some(T0 - 10 * MIN));
    assert_eq!(
        result.state.open_drive.map(|drive| drive.start_date_ms),
        Some(T0)
    );
}

// ------------------------------------------------------------ install timing

fn status_sample(id: i64, t: i64, status: &str, version: &str, drive: Value) -> LifecycleSample {
    v(
        id,
        t,
        json!({
            "drive_state": drive,
            "vehicle_state": {"timestamp": t, "car_version": version,
                              "software_update": {"status": status}}
        }),
    )
}

#[test]
fn st16_update_finish_records_one_row_from_the_install_start() {
    let mut state = OpenSessionState::new();
    state = step(
        state,
        &v(
            1,
            T0,
            json!({"vehicle_state": {"timestamp": T0, "car_version": "2024.14.3 aa"}}),
        ),
    )
    .state;
    assert_eq!(state.last_car_version.as_deref(), Some("2024.14.3 aa"));
    state = step(
        state,
        &status_sample(2, T0 + MIN, "installing", "2024.14.3 aa", json!({})),
    )
    .state;
    assert_eq!(state.phase, VehiclePhase::Updating);
    assert_eq!(
        state.open_update,
        Some(OpenUpdate {
            id: 1,
            car_id: 1,
            start_date_ms: T0 + MIN
        })
    );
    state = step(state, &d(3, T0 + 2 * MIN, "offline")).state;
    assert_eq!(state.phase, VehiclePhase::Offline);
    assert!(
        state.open_update.is_some(),
        "an install survives unreachability"
    );
    let encoded = OpenSessionState::decode(&state.encode().unwrap()).unwrap();
    assert!(encoded.open_update.is_some(), "and a restart");
    let finished = step(
        encoded,
        &status_sample(4, T0 + 40 * MIN, "", "2024.20.1 bb", json!({})),
    );
    assert_eq!(
        finished.delta.updates,
        vec![ProjectionUpdate {
            id: 1,
            car_id: 1,
            start_date_ms: T0 + MIN,
            end_date_ms: T0 + 40 * MIN,
            version: "2024.20.1 bb".to_owned(),
        }]
    );
    assert!(finished.state.open_update.is_none());
    assert_eq!(finished.state.phase, VehiclePhase::Online);
    assert_eq!(
        finished.state.last_car_version.as_deref(),
        Some("2024.20.1 bb")
    );
    assert_eq!(finished.state.last_car_version_at_ms, Some(T0 + 40 * MIN));
    assert_eq!(finished.state.next_update_id, 2);
}

#[test]
fn st17_update_cancel_leaves_no_row() {
    let mut state = step(
        OpenSessionState::new(),
        &v(
            1,
            T0,
            json!({"vehicle_state": {"timestamp": T0, "car_version": "2024.14.3 aa"}}),
        ),
    )
    .state;
    state = step(
        state,
        &status_sample(2, T0 + MIN, "installing", "2024.14.3 aa", json!({})),
    )
    .state;
    assert!(state.open_update.is_some());
    let cancelled = step(
        state,
        &status_sample(3, T0 + 2 * MIN, "available", "2024.14.3 aa", json!({})),
    );
    assert!(cancelled.state.open_update.is_none());
    assert!(cancelled.delta.updates.is_empty());
    // An id is never reused: the next install takes id 2.
    let again = step(
        cancelled.state,
        &status_sample(4, T0 + 3 * MIN, "installing", "2024.14.3 aa", json!({})),
    );
    assert_eq!(again.state.open_update.map(|open| open.id), Some(2));
}

#[test]
fn st18_missed_update_has_a_lower_bound_start() {
    let first = step(
        OpenSessionState::new(),
        &v(
            1,
            T0,
            json!({"vehicle_state": {"timestamp": T0, "car_version": "2024.14.3"}}),
        ),
    );
    let missed = step(
        first.state,
        &v(
            2,
            T0 + 3 * 24 * HOUR,
            json!({"vehicle_state": {"timestamp": T0 + 3 * 24 * HOUR, "car_version": "2024.26.5"}}),
        ),
    );
    assert_eq!(
        missed.delta.updates,
        vec![ProjectionUpdate {
            id: 1,
            car_id: 1,
            start_date_ms: T0,
            end_date_ms: T0 + 3 * 24 * HOUR,
            version: "2024.26.5".to_owned(),
        }]
    );
}

#[test]
fn st19_version_order() {
    let key = |text: &str| version_key(text).unwrap_or_else(|| panic!("{text} parses"));
    let order = |a: &str, b: &str| compare_version_keys(&key(a), &key(b));
    use std::cmp::Ordering::{Equal, Greater, Less};
    assert_eq!(order("2024.8.9", "2024.14"), Less);
    assert_eq!(order("2024.14", "2024.14.0"), Equal);
    assert_eq!(order("2023.44.30.8", "2024.2.7"), Less);
    assert_eq!(order("2024.14.3 aa", "2024.14.3 bb"), Equal);
    assert_eq!(order("2025.2.6", "2024.44.9"), Greater);
    for unusable in [
        "develop",
        "2024",
        "2024.",
        ".14",
        "2024.x.1",
        "2024.1234567890.1",
        "",
        "  ",
    ] {
        assert!(version_key(unusable).is_none(), "{unusable:?}");
    }
    assert!(version_key(&format!("2024.{}", "1".repeat(300))).is_none());
    assert!(version_key("2024.123456789.1").is_some());

    // A lower version after a higher one gives no row and does not replace it.
    let mut state = step(
        OpenSessionState::new(),
        &v(
            1,
            T0,
            json!({"vehicle_state": {"timestamp": T0, "car_version": "2024.14.3 aa"}}),
        ),
    )
    .state;
    for (id, version) in [(2, "2024.8.9"), (3, "develop"), (4, "2024.14.3 zz")] {
        let result = step(
            state,
            &v(
                id,
                T0 + id * MIN,
                json!({"vehicle_state": {"timestamp": T0 + id * MIN, "car_version": version}}),
            ),
        );
        assert!(result.delta.updates.is_empty(), "{version}");
        state = result.state;
        assert_eq!(
            state.last_car_version.as_deref(),
            Some("2024.14.3 aa"),
            "{version}"
        );
    }
    // An equal version only refreshes the time.
    assert_eq!(state.last_car_version_at_ms, Some(T0 + 4 * MIN));
}

#[test]
fn st19_a_version_text_is_limited_to_255_characters() {
    // R30: the public `updates.version` is varchar(255), so a longer text is
    // ignored whole, even when its leading key parses.
    let at_limit = format!("2024.20.1 {}", "a".repeat(245));
    let over_limit = format!("2024.20.1 {}", "a".repeat(246));
    assert_eq!(
        (at_limit.chars().count(), over_limit.chars().count()),
        (255, 256)
    );
    assert!(version_key(&at_limit).is_some());
    assert!(version_key(&over_limit).is_none());
    let known = |text: &str| {
        step(
            OpenSessionState::new(),
            &v(
                1,
                T0,
                json!({"vehicle_state": {"timestamp": T0, "car_version": text}}),
            ),
        )
        .state
    };
    // First adoption: the long text is not adopted, the one at the limit is.
    assert_eq!(known(&over_limit).last_car_version, None);
    assert_eq!(
        known(&at_limit).last_car_version.as_deref(),
        Some(at_limit.as_str())
    );
    // A higher version: the long text gives no row, the one at the limit does,
    // with its text unchanged.
    let older = known("2024.14.3 aa");
    let observe = |text: &str| {
        step(
            older.clone(),
            &v(
                2,
                T0 + MIN,
                json!({"vehicle_state": {"timestamp": T0 + MIN, "car_version": text}}),
            ),
        )
    };
    let long = observe(&over_limit);
    assert!(long.delta.updates.is_empty());
    assert_eq!(long.state.last_car_version.as_deref(), Some("2024.14.3 aa"));
    let exact = observe(&at_limit);
    assert_eq!(exact.delta.updates.len(), 1);
    assert_eq!(exact.delta.updates[0].version, at_limit);
}

#[test]
fn st20_first_version_is_adopted_without_a_row() {
    let result = step(
        OpenSessionState::new(),
        &v(
            1,
            T0,
            json!({"vehicle_state": {"timestamp": T0, "car_version": "2024.14.3"}}),
        ),
    );
    assert!(result.delta.updates.is_empty());
    assert_eq!(result.state.last_car_version.as_deref(), Some("2024.14.3"));
    assert_eq!(result.state.last_car_version_at_ms, Some(T0));
}

#[test]
fn st73_install_timing() {
    // (a) No drive; installing with motion: the install is discarded.
    let result = step(
        OpenSessionState::new(),
        &status_sample(
            1,
            T0,
            "installing",
            "2024.14.3",
            json!({"timestamp": T0, "shift_state": "D", "speed": 30, "latitude": 10.0, "longitude": 20.0}),
        ),
    );
    assert!(result.state.open_update.is_none());
    assert!(result.delta.updates.is_empty());

    // (b) A drive is open: installing without evidence of its end opens nothing.
    let mut state = OpenSessionState::new();
    state.open_drive = Some(open_drive(1, T0));
    state.next_drive_id = 2;
    let result = step(
        state,
        &status_sample(
            1,
            T0 + MIN,
            "installing",
            "2024.14.3",
            json!({"timestamp": T0 + MIN}),
        ),
    );
    assert!(result.state.open_drive.is_some());
    assert!(result.state.open_update.is_none());

    // (d) An open update survives asleep; the version increase closes it.
    let mut state = step(
        OpenSessionState::new(),
        &v(
            1,
            T0,
            json!({"vehicle_state": {"timestamp": T0, "car_version": "2024.14.3 aa"}}),
        ),
    )
    .state;
    state = step(
        state,
        &status_sample(2, T0 + MIN, "installing", "2024.14.3 aa", json!({})),
    )
    .state;
    state = step(state, &d(3, T0 + 5 * MIN, "asleep")).state;
    assert_eq!(state.phase, VehiclePhase::Asleep);
    assert!(state.open_update.is_some());
    state = step(state, &d(4, T0 + 30 * MIN, "online")).state;
    let finished = step(
        state,
        &status_sample(5, T0 + 31 * MIN, "", "2024.20.1 bb", json!({})),
    );
    assert_eq!(finished.delta.updates.len(), 1);
    assert_eq!(finished.delta.updates[0].start_date_ms, T0 + MIN);
    assert_eq!(finished.delta.updates[0].end_date_ms, T0 + 31 * MIN);
}

#[test]
fn st73_a_park_closes_the_drive_and_the_install_opens_part_d() {
    let mut state = OpenSessionState::new();
    state = step(
        state,
        &v(
            1,
            T0,
            json!({"drive_state": {"timestamp": T0, "shift_state": "D", "speed": 30,
                                   "latitude": 10.0, "longitude": 20.0}}),
        ),
    )
    .state;
    let result = step(
        state,
        &status_sample(
            2,
            T0 + MIN,
            "installing",
            "2024.14.3",
            json!({"timestamp": T0 + MIN, "shift_state": "P", "speed": 0, "latitude": 10.001, "longitude": 20.0}),
        ),
    );
    assert!(result.state.open_drive.is_none());
    assert_eq!(
        result.state.open_update.map(|open| open.start_date_ms),
        Some(T0 + MIN)
    );
}

#[test]
fn a_motion_observation_clears_an_open_update() {
    let mut state = OpenSessionState::new();
    state.open_update = Some(OpenUpdate {
        id: 1,
        car_id: 1,
        start_date_ms: T0,
    });
    state.next_update_id = 2;
    let result = step(
        state,
        &v(
            1,
            T0 + MIN,
            json!({"drive_state": {"timestamp": T0 + MIN, "shift_state": "D", "speed": 20}}),
        ),
    );
    assert!(result.state.open_update.is_none());
    assert!(result.delta.updates.is_empty());
}

#[test]
fn update_status_classes_follow_r29() {
    use UpdateStatusClass::*;
    for (status, class) in [
        (Some("installing"), Installing),
        (Some(" INSTALLING "), Installing),
        (Some("available"), Offered),
        (Some("scheduled"), Offered),
        (Some("downloading"), Offered),
        (Some("downloading_wifi_wait"), Offered),
        (Some("wifi_wait"), Offered),
        (Some(""), Idle),
        (Some("  "), Idle),
        (Some("something_new"), NoInfo),
        (None, NoInfo),
    ] {
        assert_eq!(update_status_class(status), class, "{status:?}");
    }
    // The idle status is carried as a statement, not dropped as empty text.
    let parsed = parse_sample(&v(
        1,
        T0,
        json!({"vehicle_state": {"software_update": {"status": ""}}}),
    ))
    .expect("parse");
    assert_eq!(parsed.software_update_status.as_deref(), Some(""));
    assert!(parsed.carried.vehicle_status);
}

// ------------------------------------------------------------- conversions

#[test]
fn st21_conversions_and_column_scales() {
    for (mph, kmh) in [(30.0, 48), (60.0, 97), (25.4, 41)] {
        let parsed = parse_sample(&v(1, T0, json!({"drive_state": {"speed": mph}}))).unwrap();
        assert_eq!(parsed.speed, Some(kmh), "{mph} mph");
    }
    let parsed = parse_sample(&v(
        1,
        T0,
        json!({
            "drive_state": {"latitude": 10.1234567, "longitude": 20.0, "power": 12.6},
            "charge_state": {"battery_range": 200.00, "ideal_battery_range": 150.5,
                             "est_battery_range": 0.0},
            "climate_state": {"outside_temp": 21.25},
            "vehicle_state": {"odometer": 1000.0, "tpms_pressure_fl": 2.875}
        }),
    ))
    .unwrap();
    assert_eq!(parsed.rated_range_km, Some(321.87));
    assert_eq!(parsed.ideal_range_km, Some(242.21));
    assert_eq!(parsed.est_range_km, Some(0.0));
    assert_eq!(parsed.odometer, Some(1609.344));
    assert_eq!(parsed.latitude, Some(10.123457));
    assert_eq!(parsed.tpms_pressure_fl, Some(2.9));
    assert_eq!(parsed.outside_temp, Some(21.3));
    assert_eq!(parsed.power, Some(13.0));
    assert_eq!(K_MILE_KM, 1.609_344);
    assert_eq!(round_to_precision(0.125, 2), 0.13);
    assert_eq!(round_to_precision(-0.125, 2), -0.13);
    assert_eq!(miles_to_km(1.0), 1.609_344);
    assert_eq!(miles_to_km_rounded(1.0, 2), 1.61);
    assert_eq!(mph_to_kmh(0.3), 0);
    assert_eq!(mph_to_kmh(0.31), 0);
    assert_eq!(mph_to_kmh(0.32), 1);
}

#[test]
fn column_ranges_reject_out_of_range_values() {
    let parsed = parse_sample(&v(
        1,
        T0,
        json!({
            "drive_state": {"speed": 30000.0, "power": 40000, "elevation": 40000},
            "charge_state": {"battery_level": -1, "usable_battery_level": 101,
                             "battery_range": 7000.0, "charge_energy_added": 1000000.0,
                             "charger_power": 40000, "charger_phases": 40000},
            "climate_state": {"outside_temp": 1000.0, "fan_status": 3000000000.0},
            "vehicle_state": {"odometer": -1.0, "tpms_pressure_rr": 1000.0}
        }),
    ))
    .unwrap();
    assert_eq!(parsed.speed, None);
    assert_eq!(parsed.power, None);
    assert_eq!(parsed.elevation, None);
    assert_eq!(parsed.battery_level, None);
    assert_eq!(parsed.usable_battery_level, None);
    assert_eq!(parsed.rated_range_km, None);
    assert_eq!(parsed.charge_energy_added, None);
    assert_eq!(parsed.charger_power_kw, None);
    assert_eq!(parsed.charger_phases, None);
    assert_eq!(parsed.outside_temp, None);
    assert_eq!(parsed.fan_status, None);
    assert_eq!(parsed.odometer, None);
    assert_eq!(parsed.tpms_pressure_rr, None);
    assert_eq!(parsed.carried, Families::default());
    // The edges of the ranges are valid.
    let parsed = parse_sample(&v(
        1,
        T0,
        json!({
            "drive_state": {"speed": 0, "power": -32768, "elevation": 32767},
            "charge_state": {"battery_level": 0, "usable_battery_level": 100,
                             "charge_energy_added": 999999.99},
            "climate_state": {"outside_temp": -999.9},
            "vehicle_state": {"odometer": 0.0, "tpms_pressure_rr": 999.9}
        }),
    ))
    .unwrap();
    assert_eq!(parsed.speed, Some(0));
    assert_eq!(parsed.power, Some(-32768.0));
    assert_eq!(parsed.elevation, Some(32767));
    assert_eq!(parsed.battery_level, Some(0));
    assert_eq!(parsed.usable_battery_level, Some(100));
    assert_eq!(parsed.charge_energy_added, Some(999_999.99));
    assert_eq!(parsed.outside_temp, Some(-999.9));
    assert_eq!(parsed.odometer, Some(0.0));
    assert_eq!(parsed.tpms_pressure_rr, Some(999.9));
}

#[test]
fn a_reported_zero_phase_count_is_stored_as_reported() {
    // R36: `charger_phases` is rounded within the smallint column and nothing
    // else; a reported 0 reaches the stored sample (C2 R38 keeps it lossless).
    for (reported, stored) in [
        (json!(0), Some(0)),
        (json!(-1), Some(-1)),
        (json!(2.6), Some(3)),
        (json!(32767), Some(32767)),
        (json!(32768), None),
        (json!(-32769), None),
        (json!("1"), Some(1)),
    ] {
        let parsed = parse_sample(&v(
            1,
            T0,
            json!({"charge_state": {"charging_state": "Charging", "charger_phases": reported}}),
        ))
        .unwrap();
        assert_eq!(parsed.charger_phases, stored, "{reported}");
    }
}

#[test]
fn text_fields_are_trimmed_and_bounded() {
    let long = "x".repeat(256);
    let fine = "y".repeat(255);
    let parsed = parse_sample(&v(
        1,
        T0,
        json!({"charge_state": {"fast_charger_type": format!("  {fine} "),
                               "fast_charger_brand": long,
                               "conn_charge_cable": "  "}}),
    ))
    .unwrap();
    assert_eq!(parsed.fast_charger_type.as_deref(), Some(fine.as_str()));
    assert_eq!(parsed.fast_charger_brand, None);
    assert_eq!(parsed.charge_cable, None);
}

#[test]
fn st22_stream_fold_has_the_stored_shape() {
    let wire = r#"{"msg_type":"data:update","tag":"9","timestamp":1760000000000,"value":"1760000000000,25,10.0001,20.0001,210,181,1000.5,,200,,80,12"}"#;
    let update = parse_data_update(wire).expect("frame");
    let payload = stream_observation_payload(&update);
    assert_eq!(
        payload,
        json!({
            "record_type": "tesla_stream_update_v1",
            "source_vehicle_state": "online",
            "fields": {
                "drive_state": {
                    "timestamp": 1_760_000_000_000_i64,
                    "speed": 12.0,
                    "odometer": 1000.5,
                    "elevation": 25.0,
                    "heading": 181.0,
                    "est_heading": null,
                    "latitude": 10.0001,
                    "longitude": 20.0001,
                    "est_lat": 10.0001,
                    "est_lng": 20.0001,
                    "power": null,
                    "shift_state": null
                },
                "charge_state": {
                    "timestamp": 1_760_000_000_000_i64,
                    "battery_level": 80.0,
                    "ideal_battery_range": 200.0,
                    "est_battery_range": 210.0
                },
                "vehicle_state": {
                    "timestamp": 1_760_000_000_000_i64,
                    "odometer": 1000.5
                }
            }
        })
    );
    // A shift text is trimmed.
    let with_shift = parse_data_update(
        r#"{"msg_type":"data:update","tag":"9","timestamp":1760000000000,"value":"1760000000000,25,10.0,20.0,210,181,1.0,3,200,P,80,0"}"#,
    )
    .expect("frame");
    let payload = stream_observation_payload(&with_shift);
    assert_eq!(payload["fields"]["drive_state"]["shift_state"], json!("P"));
    assert_eq!(payload["fields"]["drive_state"]["power"], json!(3));
}

#[test]
fn stream_frames_carry_motion_position_battery_and_odometer_only() {
    let sample = frame(1, T0, "25,10.0,20.0,210,181,1000.0,,200,,80,20");
    let parsed = parse_sample(&sample).unwrap();
    assert_eq!(parsed.kind, SourceKind::Stream);
    assert_eq!(
        parsed.carried,
        Families {
            motion: true,
            position: true,
            battery: true,
            charge_session: false,
            climate: false,
            odometer: true,
            vehicle_status: false,
        }
    );
    assert_eq!(parsed.speed, Some(32));
    assert_eq!(parsed.ideal_range_km, Some(321.87));
    assert_eq!(parsed.battery_level, Some(80));
    // Every group carries the frame time.
    assert_eq!(parsed.drive_timestamp_ms, T0);
    assert_eq!(parsed.charge_timestamp_ms, T0);
    assert_eq!(parsed.vehicle_timestamp_ms, T0);
    // The empty gear is a null key, which a stream never reads as Park.
    let result = step(
        OpenSessionState::new(),
        &frame(1, T0, "25,10.0,20.0,210,181,1000.0,,200,,80,0"),
    );
    assert_eq!(result.state.last_reach.as_deref(), Some("online"));
    let (drive, _, _) = evidence(
        &OpenSessionState::new(),
        &frame(1, T0, "25,10.0,20.0,210,181,1000.0,,200,,80,0"),
    );
    assert_eq!(drive, DriveEvidence::NoInfo);
}

// ------------------------------------------------------ classification

#[test]
fn drive_evidence_classes_follow_r40() {
    let state = OpenSessionState::new();
    let class = |sample: LifecycleSample| evidence(&state, &sample).0;
    let full = |drive: Value| v(1, T0, json!({"drive_state": drive}));
    assert_eq!(
        class(full(json!({"shift_state": "D"}))),
        DriveEvidence::Motion
    );
    assert_eq!(
        class(full(json!({"shift_state": "R", "speed": 0}))),
        DriveEvidence::Motion
    );
    assert_eq!(
        class(full(json!({"shift_state": "N"}))),
        DriveEvidence::Motion
    );
    assert_eq!(
        class(full(json!({"shift_state": "P", "speed": 30}))),
        DriveEvidence::Park
    );
    assert_eq!(class(full(json!({"speed": 5}))), DriveEvidence::Motion);
    assert_eq!(
        class(full(json!({"shift_state": "SNA", "speed": 5}))),
        DriveEvidence::Motion
    );
    assert_eq!(
        class(full(json!({"shift_state": "INVALID", "speed": 0}))),
        DriveEvidence::NoInfo
    );
    assert_eq!(
        class(full(json!({"shift_state": "", "speed": 0}))),
        DriveEvidence::NoInfo
    );
    // A present-but-null gear parks a full source when there is no speed.
    assert_eq!(
        class(full(json!({"shift_state": null, "speed": null}))),
        DriveEvidence::Park
    );
    assert_eq!(
        class(full(json!({"shift_state": null}))),
        DriveEvidence::Park
    );
    assert_eq!(
        class(full(json!({"shift_state": null, "speed": 0}))),
        DriveEvidence::Park
    );
    assert_eq!(
        class(full(json!({"shift_state": null, "speed": 7}))),
        DriveEvidence::Motion
    );
    // No key at all is no information.
    assert_eq!(
        class(full(json!({"latitude": 10.0, "longitude": 20.0}))),
        DriveEvidence::NoInfo
    );
    assert_eq!(class(v(1, T0, json!({}))), DriveEvidence::NoInfo);
    // Fleet without a marker is a full source; with one it is sparse.
    let fleet_full = fleet(1, T0, json!({"drive_state": {"shift_state": null}}));
    assert_eq!(class(fleet_full), DriveEvidence::Park);
    let sparse_null = sparse(
        1,
        T0,
        json!({"drive_state": {"shift_state": null, "speed": null}}),
    );
    assert_eq!(class(sparse_null), DriveEvidence::NoInfo);
    // Enum prefixes from Fleet Telemetry are normalised.
    for text in ["ShiftStateD", "shiftstated", " d "] {
        assert_eq!(
            class(sparse(1, T0, json!({"drive_state": {"shift_state": text}}))),
            DriveEvidence::Motion,
            "{text}"
        );
    }
    assert_eq!(
        class(sparse(
            1,
            T0,
            json!({"drive_state": {"shift_state": "ShiftStateP"}})
        )),
        DriveEvidence::Park
    );
    // A stale motion family is NoInfo.
    let mut stale = OpenSessionState::new();
    stale.last_drive_timestamp_ms = Some(T0);
    assert_eq!(
        evidence(&stale, &full(json!({"shift_state": "D"}))).0,
        DriveEvidence::NoInfo
    );
}

#[test]
fn charge_evidence_classes_follow_r60() {
    let state = OpenSessionState::new();
    let class = |text: &str| {
        evidence(
            &state,
            &v(1, T0, json!({"charge_state": {"charging_state": text}})),
        )
        .1
    };
    assert_eq!(class("Charging"), ChargeEvidence::Active);
    for hold in ["Starting", "Stopped", "NoPower"] {
        assert_eq!(class(hold), ChargeEvidence::Hold);
    }
    assert_eq!(class("Complete"), ChargeEvidence::Complete);
    assert_eq!(class("Disconnected"), ChargeEvidence::Disconnected);
    for none in ["", "Unknown", "Unplugged", "Pending", "D"] {
        assert_eq!(class(none), ChargeEvidence::None, "{none:?}");
    }
    assert_eq!(class("DetailedChargeStateCharging"), ChargeEvidence::Active);
    assert_eq!(class("ChargeStateComplete"), ChargeEvidence::Complete);
    assert_eq!(class("charging"), ChargeEvidence::Active);
    assert_eq!(class("  NOPOWER "), ChargeEvidence::Hold);
    assert!(ChargeEvidence::Active.is_plugged());
    assert!(ChargeEvidence::Hold.is_plugged());
    assert!(ChargeEvidence::Complete.is_plugged());
    assert!(!ChargeEvidence::Disconnected.is_plugged());
    assert!(!ChargeEvidence::None.is_plugged());
    assert!(ChargeEvidence::Complete.is_end() && ChargeEvidence::Disconnected.is_end());
    assert!(!ChargeEvidence::Active.is_end());
    let mut stale = OpenSessionState::new();
    stale.last_charge_timestamp_ms = Some(T0);
    assert_eq!(
        evidence(
            &stale,
            &v(
                1,
                T0,
                json!({"charge_state": {"charging_state": "Charging"}})
            )
        )
        .1,
        ChargeEvidence::None
    );
}

#[test]
fn enum_text_normalisation_follows_r40a() {
    for (text, canonical) in [
        ("D", Some("D")),
        (" p ", Some("P")),
        ("ShiftStateD", Some("D")),
        ("SHIFTSTATEN", Some("N")),
        ("ShiftStateSNA", Some("SNA")),
        ("DetailedChargeStateNoPower", Some("NoPower")),
        ("ChargeStateDisconnected", Some("Disconnected")),
        ("NoPower", Some("NoPower")),
        ("ChargeState", None),
        ("ShiftStateShiftStateD", None),
        ("DetailedChargeStateChargeStateCharging", None),
        ("Dx", None),
        ("", None),
    ] {
        assert_eq!(normalise_enum_text(text), canonical, "{text:?}");
    }
}

#[test]
fn st83_real_valued_numbers_are_read_like_integers() {
    for speed in [json!(25.4), json!("25.4"), json!(" 25.4 ")] {
        let parsed = parse_sample(&v(1, T0, json!({"drive_state": {"speed": speed}}))).unwrap();
        assert_eq!(parsed.speed, Some(41));
        let (drive, _, _) = evidence(
            &OpenSessionState::new(),
            &v(1, T0, json!({"drive_state": {"speed": speed}})),
        );
        assert_eq!(drive, DriveEvidence::Motion);
    }
    let parsed = parse_sample(&v(
        1,
        T0,
        json!({"charge_state": {"battery_level": 79.6, "usable_battery_level": "79.4"}}),
    ))
    .unwrap();
    assert_eq!(parsed.battery_level, Some(80));
    assert_eq!(parsed.usable_battery_level, Some(79));
    // Wrong types and non-numbers are None.
    let parsed = parse_sample(&v(
        1,
        T0,
        json!({"drive_state": {"speed": "inf", "power": "nan", "elevation": [1]},
               "charge_state": {"battery_level": {"v": 1}, "charge_energy_added": false}}),
    ))
    .unwrap();
    assert_eq!(parsed.speed, None);
    assert_eq!(parsed.power, None);
    assert_eq!(parsed.elevation, None);
    assert_eq!(parsed.battery_level, None);
    assert_eq!(parsed.charge_energy_added, None);
    // Booleans are JSON booleans only; text is JSON strings only.
    let parsed = parse_sample(&v(
        1,
        T0,
        json!({"climate_state": {"is_climate_on": "true"},
               "drive_state": {"shift_state": 4}}),
    ))
    .unwrap();
    assert_eq!(parsed.is_climate_on, None);
    assert_eq!(parsed.shift_state, None);
}

#[test]
fn st83_a_real_valued_speed_opens_a_drive_part_d() {
    let result = step(
        OpenSessionState::new(),
        &v(
            1,
            T0,
            json!({"drive_state": {"speed": 25.4, "latitude": 10.0, "longitude": 20.0}}),
        ),
    );
    assert_eq!(
        result.state.open_drive.map(|drive| drive.start_date_ms),
        Some(T0)
    );
}

#[test]
fn st84_sparse_sources_and_enum_text() {
    // A sparse source's version is ignored (R32).
    let state = step(
        OpenSessionState::new(),
        &v(
            1,
            T0,
            json!({"vehicle_state": {"timestamp": T0, "car_version": "2024.14.3 aa"}}),
        ),
    )
    .state;
    let result = step(
        state,
        &sparse(
            2,
            T0 + MIN,
            json!({"vehicle_state": {"timestamp": T0 + MIN, "car_version": "2024.44.1"}}),
        ),
    );
    assert!(result.delta.updates.is_empty());
    assert_eq!(
        result.state.last_car_version.as_deref(),
        Some("2024.14.3 aa")
    );
    // It does not even seed a first version.
    let result = step(
        OpenSessionState::new(),
        &sparse(
            1,
            T0,
            json!({"vehicle_state": {"timestamp": T0, "car_version": "2024.44.1"}}),
        ),
    );
    assert_eq!(result.state.last_car_version, None);
    // Nor does it become the installed firmware of the car metadata.
    let parsed = parse_sample(&sparse(
        1,
        T0,
        json!({"vehicle_state": {"timestamp": T0, "car_version": "2024.44.1", "vehicle_name": "Synthetic"}}),
    ))
    .unwrap();
    assert_eq!(parsed.provider_feed.as_deref(), Some("provider_stream"));
    assert_eq!(
        parsed
            .car_metadata
            .as_ref()
            .and_then(|m| m.firmware_version.clone()),
        None
    );
    // A sparse observation still carries a statement it makes.
    assert!(parsed.carried.vehicle_status);
}

#[test]
fn st84_a_sparse_null_gear_keeps_an_open_drive_part_d() {
    let mut state = step(
        OpenSessionState::new(),
        &sparse(
            1,
            T0,
            json!({"drive_state": {"timestamp": T0, "shift_state": "D", "speed": 20,
                                   "latitude": 10.0, "longitude": 20.0}}),
        ),
    )
    .state;
    assert!(state.open_drive.is_some());
    state = step(
        state,
        &sparse(
            2,
            T0 + 30_000,
            json!({"drive_state": {"timestamp": T0 + 30_000, "shift_state": null, "speed": null}}),
        ),
    )
    .state;
    assert!(state.open_drive.is_some());
}

#[test]
fn st85_unrounded_odometer_values_survive_the_f64_encodings() {
    let first = parse_sample(&v(
        1,
        T0,
        json!({"vehicle_state": {"odometer": 1000.123456789}}),
    ))
    .unwrap();
    let second = parse_sample(&v(
        2,
        T0,
        json!({"vehicle_state": {"odometer": 1003.987654321}}),
    ))
    .unwrap();
    let start = first.odometer.expect("odometer");
    let end = second.odometer.expect("odometer");
    assert_eq!(start, 1000.123456789 * K_MILE_KM);
    assert_eq!(end, 1003.987654321 * K_MILE_KM);
    let distance = end - start;
    // The stored JSON of the row types is serde's shortest round trip. The
    // physical pack's big-endian columns are pinned, from a closed drive, by
    // `st85_unrounded_odometer_values_survive_the_physical_pack_bit_for_bit`
    // in `src/sync/hub_pack/tests/lifecycle_float_bits.rs`.
    for value in [start, end, distance] {
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(
            serde_json::from_str::<f64>(&json).unwrap().to_bits(),
            value.to_bits()
        );
    }
    let position = position_from_sample(
        1,
        None,
        1,
        &parse_sample(&v(
            3,
            T0,
            json!({"drive_state": {"latitude": 10.0, "longitude": 20.0},
               "vehicle_state": {"odometer": 1000.123456789}}),
        ))
        .unwrap(),
    )
    .unwrap()
    .expect("position");
    assert_eq!(position.odometer.map(f64::to_bits), Some(start.to_bits()));
    let stored: ProjectionPosition =
        serde_json::from_str(&serde_json::to_string(&position).unwrap()).unwrap();
    assert_eq!(stored.odometer.map(f64::to_bits), Some(start.to_bits()));
}

#[test]
fn st89_elevation_precedence() {
    let parse = |drive: Value| {
        parse_sample(&v(1, T0, json!({"drive_state": drive})))
            .unwrap()
            .elevation
    };
    assert_eq!(
        parse(json!({"elevation": 120, "native_location_elevation": 118})),
        Some(120)
    );
    assert_eq!(
        parse(json!({"elevation": null, "native_location_elevation": 118})),
        Some(118)
    );
    assert_eq!(
        parse(json!({"elevation": 40000, "native_location_elevation": 118})),
        Some(118)
    );
    assert_eq!(parse(json!({"native_location_elevation": 118})), Some(118));
    assert_eq!(parse(json!({"elevation": 40000})), None);
    assert_eq!(parse(json!({"elevation": 120.4})), Some(120));
}

#[test]
fn coordinates_need_a_valid_pair() {
    let parse = |drive: Value| parse_sample(&v(1, T0, json!({"drive_state": drive}))).unwrap();
    assert_eq!(parse(json!({"latitude": 10.0})).latitude, None);
    assert_eq!(parse(json!({"longitude": 20.0})).longitude, None);
    assert_eq!(
        parse(json!({"latitude": 10.0, "longitude": 181.0})).latitude,
        None
    );
    assert!(
        !parse(json!({"latitude": 10.0, "longitude": 181.0}))
            .carried
            .position
    );
    let parsed = parse(json!({"latitude": "10.0000004", "longitude": -20}));
    assert_eq!(
        (parsed.latitude, parsed.longitude),
        (Some(10.0), Some(-20.0))
    );
    assert!(parsed.carried.position);
    // Rounded to 6 decimals first, so a value that rounds onto the pole is valid.
    assert_eq!(
        parse(json!({"latitude": 90.0000004, "longitude": 0.0})).latitude,
        Some(90.0)
    );
}

// ------------------------------------------------- Hub events and blobs

#[path = "phase_events_tests.rs"]
mod events;

// ------------------------------------------------------------- storage

#[path = "phase_storage_tests.rs"]
mod storage;

#[path = "phase_imported_tests.rs"]
mod imported;
