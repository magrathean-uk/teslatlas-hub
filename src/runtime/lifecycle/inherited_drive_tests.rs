// SPDX-License-Identifier: AGPL-3.0-only

//! C1-B unit-level spec tests (C1 section 8a, C1 R93: C1 ST111 (a), (d), (e),
//! (f), (g) and (i), unit half): a drive that was open across the upgrade is
//! identified when the stored state decodes (`walk_inherited`), listed by the
//! step that closes it (`recompute_drive_ids`) and merged in step order. The
//! storage half of ST111 (b), (c), (g), (h), (i), (j) and (k) is in
//! `src/storage/db/tests/b3_inherited_drives.rs`. Synthetic data only. `T0` is
//! 1_760_000_000_000 ms; odometer values are miles as Tesla reports them, so the
//! blob's kilometre readings are written as `miles * K_MILE_KM`.
//!
//! The fixtures (`inherited_state`, `old_blob`, `st111_obs1`, `st111_obs2`,
//! `seeded_session`) are `pub(crate)` because the storage tests build the same
//! blob, the same two observations and the same imported open drive.

use super::drive_tests::{MIN, SEC, T0, moving, parked, vd};
use super::*;

/// The R12 drive members an old blob lacks (C1 R12): the fold times, the
/// displacement, the elevation count, the coarse flag and the still checkpoint.
const R12_DRIVE_KEYS: [&str; 6] = [
    "last_folded_at_ms",
    "last_motion_at_ms",
    "max_displacement_m",
    "elevation_count",
    "odometer_coarse",
    "still_checkpoint",
];

/// ST111 (a)'s open drive 1: it lacks every R12 field and has `start_date_ms`
/// t0-3m, `last_position_date_ms` t0-1m, `last_elevation` 104, ascent 37,
/// descent 5, first odometer 1000.0 mi, last odometer `last_odometer_mi` and
/// start coordinates (10.0, 20.0).
pub(crate) fn inherited_drive(last_odometer_mi: f64) -> OpenDrive {
    let mut drive: OpenDrive = serde_json::from_value(json!({
        "id": 1, "car_id": 1, "start_date_ms": T0 - 3 * MIN,
        "outside_temp_sum": 0.0, "outside_temp_count": 0
    }))
    .expect("old open drive fixture");
    drive.start_latitude = Some(10.0);
    drive.start_longitude = Some(20.0);
    drive.position_count = 3;
    drive.last_position_date_ms = Some(T0 - MIN);
    drive.last_latitude = Some(10.0);
    drive.last_longitude = Some(20.0);
    drive.first_odometer = Some(1000.0 * K_MILE_KM);
    drive.last_odometer = Some(last_odometer_mi * K_MILE_KM);
    drive.last_elevation = Some(104);
    drive.elevation_ascent = 37;
    drive.elevation_descent = 5;
    drive
}

/// The state around [`inherited_drive`]: driving, drive ids from 2 and position
/// ids from 14 (ST111 (b)'s `next_position_id`).
pub(crate) fn inherited_state(last_odometer_mi: f64) -> OpenSessionState {
    let mut state = OpenSessionState::new();
    state.phase = VehiclePhase::Driving;
    state.next_drive_id = 2;
    state.next_position_id = 14;
    state.open_drive = Some(inherited_drive(last_odometer_mi));
    state
}

/// The state as a build before R12 wrote it: the R12 drive members are removed
/// from the JSON of the open drive.
pub(crate) fn old_blob(state: &OpenSessionState) -> Vec<u8> {
    let mut value = serde_json::to_value(state).expect("state value");
    let drive = value
        .get_mut("open_drive")
        .and_then(Value::as_object_mut)
        .expect("an open drive");
    for key in R12_DRIVE_KEYS {
        drive.remove(key);
    }
    serde_json::to_vec(&value).expect("old blob bytes")
}

/// The state read back from an old blob, as the first read after the upgrade
/// does.
fn decoded(state: &OpenSessionState) -> OpenSessionState {
    OpenSessionState::decode(&old_blob(state)).expect("the old blob decodes")
}

/// The state written and read back with no step between, so the checkpoint
/// copies of a drive stay as built.
fn reread(state: &OpenSessionState) -> OpenSessionState {
    OpenSessionState::decode(&state.encode().expect("encode")).expect("decode")
}

/// An owner `vehicle_data` with a drive group (gear and speed as given, `null`
/// meaning the key is present with JSON null: Park), the elevation and the
/// odometer in miles.
fn obs(
    id: i64,
    minutes: i64,
    gear: Value,
    speed: Value,
    latitude: f64,
    elevation: i64,
    odometer_mi: f64,
) -> LifecycleSample {
    let t = T0 + minutes * MIN;
    vd(
        id,
        t,
        json!({
            "drive_state": {"timestamp": t, "shift_state": gear, "speed": speed,
                            "latitude": latitude, "longitude": 20.0,
                            "elevation": elevation},
            "vehicle_state": {"timestamp": t, "odometer": odometer_mi},
        }),
    )
}

/// ST111 (a)'s V(1, t0, D, 30 mph, (10.001, 20.0), elevation 110, odo 1000.5).
pub(crate) fn st111_obs1() -> LifecycleSample {
    obs(1, 0, json!("D"), json!(30), 10.001, 110, 1000.5)
}

/// ST111 (a)'s V(2, t0+1m, P, speed null, (10.002, 20.0), elevation 108, odo
/// 1000.6).
pub(crate) fn st111_obs2() -> LifecycleSample {
    obs(2, 1, Value::Null, Value::Null, 10.002, 108, 1000.6)
}

/// ST111 (d)'s V(1, t0, D, 30 mph, (10.001, 20.0), elevation 110, odo 1000.0):
/// with the blob's last odometer 1000.0 mi the drive never moves.
pub(crate) fn st111_discard_obs1() -> LifecycleSample {
    obs(1, 0, json!("D"), json!(30), 10.001, 110, 1000.0)
}

/// ST111 (d)'s V(2, t0+1m, P, speed null, (10.002, 20.0), elevation 108, odo
/// 1000.0).
pub(crate) fn st111_discard_obs2() -> LifecycleSample {
    obs(2, 1, Value::Null, Value::Null, 10.002, 108, 1000.0)
}

fn drive_ids(delta: &LifecycleDelta) -> Vec<i64> {
    delta.drives.iter().map(|drive| drive.id).collect()
}

fn position_ids(positions: &[ProjectionPosition]) -> Vec<i64> {
    positions.iter().map(|position| position.id).collect()
}

#[test]
fn c1_st111_decode_close_merge() {
    // (a) The old blob decodes flagged; the flag survives the step and an
    // encode and decode; the closing step lists the drive with the inherited
    // walk continued (37 + 6 and 5 + 2).
    let state = decoded(&inherited_state(1000.4));
    let open = state.open_drive.as_ref().expect("the open drive");
    assert!(
        open.walk_inherited,
        "decode flags a drive with no fold time"
    );
    assert_eq!(open.last_folded_at_ms, None);

    let first = apply_sample(state, 1, &st111_obs1()).expect("obs 1");
    let open = first.state.open_drive.as_ref().expect("open after obs 1");
    assert!(open.walk_inherited, "the fold keeps the flag");
    assert_eq!(
        open.last_folded_at_ms,
        Some(T0),
        "the fold fills the fold time"
    );
    assert!(first.delta.drives.is_empty() && first.delta.recompute_drive_ids.is_empty());
    assert_eq!(position_ids(&first.delta.open_drive_positions), vec![14]);
    let restored =
        OpenSessionState::decode(&first.state.encode().expect("encode")).expect("decode");
    assert!(
        restored
            .open_drive
            .as_ref()
            .expect("restored drive")
            .walk_inherited,
        "the flag survives an encode and decode"
    );
    let second = apply_sample(restored, 1, &st111_obs2()).expect("obs 2");
    assert_eq!(drive_ids(&second.delta), vec![1]);
    assert_eq!(
        (
            second.delta.drives[0].ascent,
            second.delta.drives[0].descent
        ),
        (Some(43), Some(7))
    );
    assert_eq!(second.delta.recompute_drive_ids, vec![1]);
    assert!(second.delta.discarded_drive_ids.is_empty());
    assert_eq!(position_ids(&second.delta.open_drive_positions), vec![15]);
    assert!(second.state.open_drive.is_none());

    // A drive this build opens (ST23's kind), closed with no recompute id, even
    // when the state is encoded and decoded between every step. This is the
    // check for a detection in the fold or at open instead of at decode.
    let mut state = OpenSessionState::new();
    let mut samples = moving(1, T0, 4);
    samples.push(parked(5, T0 + 4 * MIN, 10.004, Some(1002.0)));
    let (mut closed, mut listed) = (0, Vec::new());
    for sample in &samples {
        let step = apply_sample(state, 1, sample).expect("new drive step");
        closed += step.delta.drives.len();
        listed.extend(step.delta.recompute_drive_ids.clone());
        state = OpenSessionState::decode(&step.state.encode().expect("encode")).expect("decode");
        if let Some(open) = state.open_drive.as_ref() {
            assert!(
                !open.walk_inherited,
                "a drive this build opened is not flagged"
            );
        }
    }
    assert_eq!(closed, 1, "the new drive closes");
    assert!(listed.is_empty(), "and lists nothing: {listed:?}");

    // (d) Discarded. The blob's last odometer is 1000.0 mi and both
    // observations carry 1000.0: the drive is flagged after obs 1, obs 2
    // discards it (0.0 km, below K_DMIN_KM), and it is not listed.
    let state = decoded(&inherited_state(1000.0));
    let first = apply_sample(state, 1, &st111_discard_obs1()).expect("obs 1");
    assert!(
        first
            .state
            .open_drive
            .as_ref()
            .expect("open")
            .walk_inherited
    );
    let second = apply_sample(first.state, 1, &st111_discard_obs2()).expect("obs 2");
    assert!(second.delta.drives.is_empty());
    assert_eq!(second.delta.discarded_drive_ids, vec![1]);
    assert!(second.delta.recompute_drive_ids.is_empty());

    // (e) Through `apply_samples`: obs 3 closes nothing, so the merged list is
    // the closing step's entry; a merge that keeps only the last step's list
    // gives [] and fails. The merged drives and discarded ids equal the
    // per-step deltas in step order.
    let samples = [
        st111_obs1(),
        st111_obs2(),
        obs(3, 5, Value::Null, Value::Null, 10.002, 108, 1000.6),
    ];
    let state = decoded(&inherited_state(1000.4));
    let merged = apply_samples(state.clone(), 1, &samples).expect("one page");
    let mut stepped = state;
    let mut per_step = Vec::new();
    for sample in &samples {
        let step = apply_sample(stepped, 1, sample).expect("one step");
        stepped = step.state;
        per_step.push(step.delta);
    }
    let listed: Vec<i64> = per_step
        .iter()
        .flat_map(|delta| delta.recompute_drive_ids.clone())
        .collect();
    assert_eq!(listed, vec![1], "only the closing step lists the drive");
    assert_eq!(merged.delta.recompute_drive_ids, listed);
    let drives: Vec<_> = per_step
        .iter()
        .flat_map(|delta| delta.drives.clone())
        .collect();
    assert_eq!(merged.delta.drives, drives);
    let discarded: Vec<i64> = per_step
        .iter()
        .flat_map(|delta| delta.discarded_drive_ids.clone())
        .collect();
    assert_eq!(merged.delta.discarded_drive_ids, discarded);
    // The merge appends in step order, as it does the discarded ids: ids from
    // three steps, one of them empty, come out as they went in.
    let mut total = LifecycleDelta {
        recompute_drive_ids: vec![7],
        ..LifecycleDelta::default()
    };
    for ids in [vec![2], Vec::new(), vec![4]] {
        let step = LifecycleDelta {
            recompute_drive_ids: ids,
            ..LifecycleDelta::default()
        };
        merge_step_delta(&mut total, step);
    }
    assert_eq!(total.recompute_drive_ids, vec![7, 2, 4]);

    // (f) Lack-of-evidence close. Obs 2 states service mode: drive 1 is closed
    // at its previous fold t0 and kept (0.5 mi), with the walk continued by obs
    // 1 only (37 + 6, 5), and is listed.
    let state = decoded(&inherited_state(1000.4));
    let first = apply_sample(state, 1, &st111_obs1()).expect("obs 1");
    let in_service = vd(
        2,
        T0 + MIN,
        json!({
            "drive_state": {"timestamp": T0 + MIN, "shift_state": "D", "speed": 30,
                            "latitude": 10.002, "longitude": 20.0, "elevation": 108},
            "vehicle_state": {"timestamp": T0 + MIN, "odometer": 1000.6,
                              "service_mode": true},
        }),
    );
    let second = apply_sample(first.state, 1, &in_service).expect("obs 2");
    assert_eq!(drive_ids(&second.delta), vec![1]);
    let drive = &second.delta.drives[0];
    assert_eq!(drive.end_date_ms, T0);
    assert_eq!((drive.ascent, drive.descent), (Some(43), Some(5)));
    assert_eq!(second.delta.recompute_drive_ids, vec![1]);

    // The flag of the copy the row comes from decides, not the open drive's:
    // a still checkpoint with no fold time is flagged by decode, the close for
    // lack of evidence takes the row from it (end t0-1m, not the open drive's
    // later position), and lists the drive although the open drive's own flag
    // is cleared by hand.
    let mut drive = inherited_drive(1000.4);
    let checkpoint = drive.clone();
    drive.last_position_date_ms = Some(T0 - 30 * SEC);
    drive.still_checkpoint = Some(Box::new(checkpoint));
    let mut state = inherited_state(1000.4);
    state.open_drive = Some(drive);
    let mut state = reread(&state);
    let open = state.open_drive.as_mut().expect("open drive");
    assert!(open.walk_inherited);
    assert!(
        open.still_checkpoint
            .as_ref()
            .expect("still checkpoint")
            .walk_inherited,
        "decode flags the still checkpoint copy"
    );
    open.walk_inherited = false;
    let closed = force_close_for_service(state, 1, T0).expect("service close");
    assert_eq!(drive_ids(&closed.delta), vec![1]);
    assert_eq!(closed.delta.drives[0].end_date_ms, T0 - MIN);
    assert_eq!(closed.delta.recompute_drive_ids, vec![1]);

    // Decode flags each copy by its own fold time: the lead checkpoint copy
    // with none is flagged, one with a fold time and the still checkpoint with
    // a fold time are not.
    let mut folded = inherited_drive(1000.4);
    folded.last_folded_at_ms = Some(T0 - MIN);
    let mut drive = inherited_drive(1000.4);
    drive.still_checkpoint = Some(Box::new(folded.clone()));
    drive.lead_checkpoints = vec![
        LeadCheckpoint {
            cut_ms: T0 - 2 * MIN,
            drive: inherited_drive(1000.4),
        },
        LeadCheckpoint {
            cut_ms: T0 - MIN,
            drive: folded,
        },
    ];
    let mut state = inherited_state(1000.4);
    state.open_drive = Some(drive);
    let state = reread(&state);
    let open = state.open_drive.as_ref().expect("open drive");
    assert!(open.walk_inherited);
    assert!(
        !open
            .still_checkpoint
            .as_ref()
            .expect("still")
            .walk_inherited
    );
    assert!(open.lead_checkpoints[0].drive.walk_inherited);
    assert!(!open.lead_checkpoints[1].drive.walk_inherited);
}

#[test]
fn c1_st111_reencoded_and_seeded() {
    // (g) Decode and re-encode with no step between, as C6's identity tidy
    // does at the first read-write open: the encoded blob carries the flag, and
    // a decode of it flags the drive again, however often it is repeated.
    let mut state = decoded(&inherited_state(1000.4));
    for _ in 0..3 {
        let bytes = state.encode().expect("encode with no step");
        let text = String::from_utf8(bytes.clone()).expect("utf8 blob");
        assert!(
            text.contains("\"walk_inherited\":true"),
            "the re-encoded blob carries the flag"
        );
        state = OpenSessionState::decode(&bytes).expect("decode");
        let open = state.open_drive.as_ref().expect("open drive");
        assert!(open.walk_inherited);
        assert_eq!(open.last_folded_at_ms, None, "no step ran");
    }
    // Obs 1 and 2 as in (a): the closing row (43, 7) and the recompute id.
    let result = apply_samples(state, 1, &[st111_obs1(), st111_obs2()]).expect("page");
    assert_eq!(drive_ids(&result.delta), vec![1]);
    assert_eq!(
        (
            result.delta.drives[0].ascent,
            result.delta.drives[0].descent
        ),
        (Some(43), Some(7))
    );
    assert_eq!(result.delta.recompute_drive_ids, vec![1]);

    // A state that was never an inherited one writes no member at all.
    let plain = OpenSessionState::new().encode().expect("encode");
    assert!(
        !String::from_utf8(plain)
            .expect("utf8 blob")
            .contains("walk_inherited")
    );

    // (i) A seeded imported open drive with no imported positions keeps
    // `last_folded_at_ms` None, so the next decode flags it and its close lists
    // it once. One that folded an imported position has a fold time and is not
    // flagged.
    for (with_position, listed) in [(false, vec![70]), (true, Vec::new())] {
        let session = seeded_session(with_position);
        let seeded =
            seed_imported_open_session_state(uuid::Uuid::nil(), &session, None).expect("seed");
        let open = seeded.open_drive.as_ref().expect("seeded drive");
        assert_eq!(open.id, 70);
        assert_eq!(open.last_folded_at_ms.is_some(), with_position);
        assert!(!open.walk_inherited, "a seed in memory is not decoded yet");
        let restored = OpenSessionState::decode(&seeded.encode().expect("encode")).expect("decode");
        let open = restored.open_drive.as_ref().expect("restored drive");
        assert_eq!(open.walk_inherited, !with_position);
        let closed = force_close_for_service(restored, 1, SEEDED_START + 10 * MIN).expect("close");
        assert_eq!(
            drive_ids(&closed.delta),
            vec![70],
            "an imported drive is kept"
        );
        assert_eq!(closed.delta.recompute_drive_ids, listed);
    }
}

pub(crate) const SEEDED_START: i64 = 1_800_100_000_000;

/// An imported open drive 70, with or without one imported position.
pub(crate) fn seeded_session(with_position: bool) -> TeslaMateOpenSession {
    seeded_session_with_totals(with_position, Some(7), Some(2))
}

/// [`seeded_session`] with the imported drive's `ascent` and `descent` as
/// given: `None` is an imported drive with no totals, which the seed reads as
/// 0 and 0 (`open_drive_from_source`).
pub(crate) fn seeded_session_with_totals(
    with_position: bool,
    ascent: Option<i64>,
    descent: Option<i64>,
) -> TeslaMateOpenSession {
    let drive: TeslaMateDrive = serde_json::from_value(json!({
        "id": 70, "car_id": 1, "start_date_ms": SEEDED_START, "end_date_ms": null,
        "start_position_id": 700, "end_position_id": 701,
        "start_address_id": null, "end_address_id": null,
        "start_geofence_id": null, "end_geofence_id": null,
        "outside_temp_avg": 18.0, "inside_temp_avg": 20.0, "speed_max": 50,
        "power_max": 12.0, "power_min": -5.0,
        "start_ideal_range_km": 300.0, "end_ideal_range_km": 298.0,
        "start_rated_range_km": 280.0, "end_rated_range_km": 278.0,
        "start_km": 100.0, "end_km": 100.8, "distance_km": null,
        "duration_min": null, "ascent": ascent, "descent": descent
    }))
    .expect("imported drive");
    let mut session = TeslaMateOpenSession {
        car_id: 1,
        drive: Some(drive),
        ..Default::default()
    };
    if with_position {
        session.drive_positions = vec![super::tests::imported_position_fixture(
            700,
            70,
            SEEDED_START,
            47.5,
            100.0,
            80,
            17.0,
            19.0,
        )];
        session.watermarks.positions.max_timestamp_ms = Some(SEEDED_START);
    }
    session
}
