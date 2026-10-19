// SPDX-License-Identifier: AGPL-3.0-only

//! C1 B2 amendment A (R87): an explicit end earlier than the last fold. Spec
//! tests ST95 to ST99 at unit level. Synthetic data only. Coordinates:
//! A = (10.0, 20.0), B = (10.0012, 20.0), B' = (10.0005, 20.0) and
//! C = (10.0030, 20.0); A to B is 133.434 m, A to B' 55.598 m and A to C
//! 333.585 m. Odometer values are miles and speeds mph.

use serde_json::{Map, json};

use super::drive_tests::{MIN, SEC, T0, vd};
use super::*;

pub(crate) const A: (f64, f64) = (10.0, 20.0);
pub(crate) const B: (f64, f64) = (10.0012, 20.0);
pub(crate) const B_PRIME: (f64, f64) = (10.0005, 20.0);
pub(crate) const C: (f64, f64) = (10.0030, 20.0);

/// `V`: a full owner observation whose three groups all carry the time `group`.
/// `gear` and `speed` are JSON values (`null` means the key is present with
/// JSON null); `position` `None` omits the coordinates.
#[allow(clippy::too_many_arguments)]
pub(crate) fn v_obs(
    id: i64,
    observed: i64,
    group: i64,
    gear: Value,
    speed: Value,
    position: Option<(f64, f64)>,
    elevation: Option<i64>,
    odometer: Option<f64>,
    soc: Option<i64>,
) -> LifecycleSample {
    let mut drive = Map::new();
    drive.insert("timestamp".to_owned(), json!(group));
    drive.insert("shift_state".to_owned(), gear);
    drive.insert("speed".to_owned(), speed);
    if let Some((latitude, longitude)) = position {
        drive.insert("latitude".to_owned(), json!(latitude));
        drive.insert("longitude".to_owned(), json!(longitude));
    }
    if let Some(elevation) = elevation {
        drive.insert("elevation".to_owned(), json!(elevation));
    }
    let mut data = Map::new();
    data.insert("drive_state".to_owned(), Value::Object(drive));
    if let Some(odometer) = odometer {
        data.insert(
            "vehicle_state".to_owned(),
            json!({"timestamp": group, "odometer": odometer}),
        );
    }
    if let Some(soc) = soc {
        data.insert(
            "charge_state".to_owned(),
            json!({"timestamp": group, "battery_level": soc}),
        );
    }
    vd(id, observed, Value::Object(data))
}

/// `P`: a position-only observation stamped with `t`: no gear and no speed, so
/// no motion family. The odometer and battery groups are carried only when
/// given.
pub(crate) fn p_obs(
    id: i64,
    t: i64,
    position: (f64, f64),
    elevation: Option<i64>,
    odometer: Option<f64>,
    soc: Option<i64>,
) -> LifecycleSample {
    let mut drive = Map::new();
    drive.insert("timestamp".to_owned(), json!(t));
    drive.insert("latitude".to_owned(), json!(position.0));
    drive.insert("longitude".to_owned(), json!(position.1));
    if let Some(elevation) = elevation {
        drive.insert("elevation".to_owned(), json!(elevation));
    }
    let mut data = Map::new();
    data.insert("drive_state".to_owned(), Value::Object(drive));
    if let Some(odometer) = odometer {
        data.insert(
            "vehicle_state".to_owned(),
            json!({"timestamp": t, "odometer": odometer}),
        );
    }
    if let Some(soc) = soc {
        data.insert(
            "charge_state".to_owned(),
            json!({"timestamp": t, "battery_level": soc}),
        );
    }
    vd(id, t, Value::Object(data))
}

fn close_to(left: f64, right: f64) -> bool {
    (left - right).abs() < 1e-9
}

fn run_all(samples: &[LifecycleSample]) -> LifecycleStep {
    apply_samples(OpenSessionState::new(), 1, samples).expect("apply samples")
}

fn cuts(state: &OpenSessionState) -> Vec<i64> {
    state
        .open_drive
        .as_ref()
        .map(|drive| {
            drive
                .lead_checkpoints
                .iter()
                .map(|checkpoint| checkpoint.cut_ms)
                .collect()
        })
        .unwrap_or_default()
}

/// ST95 and ST96 use these two sequences at unit and storage level.
pub(crate) fn st95_samples() -> Vec<LifecycleSample> {
    vec![
        v_obs(
            1,
            T0,
            T0,
            json!("D"),
            json!(30),
            Some(A),
            Some(100),
            Some(1000.0),
            Some(80),
        ),
        p_obs(2, T0 + MIN, B, Some(104), Some(1000.1), Some(79)),
        p_obs(3, T0 + 2 * MIN, C, Some(120), Some(1000.3), Some(78)),
        v_obs(
            4,
            T0 + 2 * MIN + 10 * SEC,
            T0 + MIN,
            json!("P"),
            Value::Null,
            Some(B),
            Some(104),
            Some(1000.1),
            Some(79),
        ),
    ]
}

pub(crate) fn st96_samples() -> Vec<LifecycleSample> {
    vec![
        v_obs(1, T0, T0, json!("D"), json!(30), Some(A), None, None, None),
        p_obs(2, T0 + MIN, B_PRIME, None, None, None),
        p_obs(3, T0 + 2 * MIN, C, None, None, None),
        v_obs(
            4,
            T0 + 2 * MIN + 10 * SEC,
            T0 + MIN,
            json!("P"),
            Value::Null,
            Some(B_PRIME),
            None,
            None,
            None,
        ),
    ]
}

pub(crate) fn st97a_samples() -> Vec<LifecycleSample> {
    vec![
        v_obs(
            1,
            T0,
            T0,
            json!("D"),
            json!(30),
            Some(A),
            None,
            Some(1000.0),
            None,
        ),
        p_obs(2, T0 + 2 * MIN, C, None, None, None),
        v_obs(
            3,
            T0 + 2 * MIN + 5 * SEC,
            T0 + MIN,
            json!("D"),
            json!(50),
            None,
            None,
            Some(1000.5),
            None,
        ),
        v_obs(
            4,
            T0 + 2 * MIN + 10 * SEC,
            T0 + MIN + 30 * SEC,
            json!("P"),
            Value::Null,
            None,
            None,
            Some(1000.6),
            None,
        ),
    ]
}

pub(crate) fn st98_samples() -> Vec<LifecycleSample> {
    let mut samples = vec![v_obs(
        1,
        T0,
        T0,
        json!("D"),
        json!(30),
        Some(A),
        None,
        None,
        None,
    )];
    for k in 1..=5_i64 {
        samples.push(p_obs(
            k + 1,
            T0 + k * MIN,
            (10.0009 + 0.0001 * k as f64, 20.0),
            None,
            None,
            None,
        ));
    }
    samples.push(v_obs(
        7,
        T0 + 5 * MIN + 10 * SEC,
        T0 + 30 * SEC,
        json!("P"),
        Value::Null,
        None,
        None,
        None,
        None,
    ));
    samples
}

#[test]
fn st95_an_explicit_end_before_a_later_position_ends_the_row_at_the_end() {
    let samples = st95_samples();
    let mut state = OpenSessionState::new();
    let mut steps = Vec::new();
    for sample in &samples[..3] {
        let step = apply_sample(state, 1, sample).expect("apply");
        state = step.state.clone();
        steps.push(step);
    }
    // Obs 2 and 3 are lead folds: position fresh, no motion family, dated
    // above the motion watermark (T0).
    assert_eq!(cuts(&steps[1].state), vec![T0 + MIN]);
    assert_eq!(cuts(&steps[2].state), vec![T0 + MIN, T0 + 2 * MIN]);
    assert_eq!(steps[2].delta.open_drive_positions.len(), 1);
    let last = apply_sample(state, 1, &samples[3]).expect("park");
    let delta = &last.delta;
    assert_eq!(delta.drives.len(), 1);
    let drive = &delta.drives[0];
    assert_eq!(drive.start_date_ms, T0);
    assert_eq!(drive.end_date_ms, T0 + MIN);
    assert_eq!(drive.duration_min, Some(1));
    assert_eq!(
        (drive.start_latitude, drive.start_longitude),
        (Some(10.0), Some(20.0))
    );
    assert_eq!(
        (drive.end_latitude, drive.end_longitude),
        (Some(10.0012), Some(20.0))
    );
    assert!(close_to(drive.distance_km.unwrap(), 0.1 * K_MILE_KM));
    assert_eq!((drive.start_soc, drive.end_soc), (Some(80), Some(79)));
    assert_eq!(drive.speed_max, Some(48));
    assert_eq!((drive.ascent, drive.descent), (Some(4), Some(0)));
    // Obs 4's position family is stale, so it emits no position.
    assert!(delta.open_drive_positions.is_empty());
    assert!(delta.positions.is_empty());
    // The snapshot and the references come from the open drive (point 5).
    let snapshot = last.state.parked_snapshot.as_ref().expect("snapshot");
    assert_eq!(snapshot.at_ms, Some(T0 + 2 * MIN));
    assert_eq!(snapshot.battery_level, Some(78));
    assert!(close_to(snapshot.odometer_km.unwrap(), 1000.3 * K_MILE_KM));
    assert_eq!(
        last.state.last_stationary_position_at_ms,
        Some(T0 + 2 * MIN)
    );
    assert_eq!(last.state.last_standalone_latitude, Some(10.003));
    assert_eq!(last.state.last_standalone_longitude, Some(20.0));
    assert!(last.state.open_drive.is_none());
}

#[test]
fn st95_c_a_blob_without_lead_checkpoints_decodes_with_none() {
    let samples = st95_samples();
    let step = run_all(&samples[..3]);
    assert_eq!(cuts(&step.state), vec![T0 + MIN, T0 + 2 * MIN]);
    // Checkpoints survive encode and decode.
    let decoded = OpenSessionState::decode(&step.state.encode().unwrap()).unwrap();
    assert_eq!(decoded, step.state);
    // An old blob has no member; it decodes with an empty list, and a state
    // without checkpoints writes none.
    let mut value = serde_json::to_value(&step.state).unwrap();
    value["open_drive"]
        .as_object_mut()
        .unwrap()
        .remove("lead_checkpoints")
        .expect("the member was written");
    let old = OpenSessionState::decode(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(old.open_drive.as_ref().unwrap().lead_checkpoints.is_empty());
    let plain = run_all(&samples[..1]);
    let text = String::from_utf8(plain.state.encode().unwrap()).unwrap();
    assert!(!text.contains("lead_checkpoints"));
}

#[test]
fn st96_an_explicit_end_before_a_later_position_discards_what_the_end_source_discards() {
    let step = run_all(&st96_samples());
    assert!(step.delta.drives.is_empty(), "C alone would cross 100 m");
    assert_eq!(step.delta.discarded_drive_ids, vec![1]);
    assert_eq!(
        step.state.last_stationary_position_at_ms,
        Some(T0 + 2 * MIN)
    );
    assert_eq!(step.state.last_standalone_latitude, Some(10.003));
    assert_eq!(step.state.last_standalone_longitude, Some(20.0));
}

#[test]
fn st97_a_delayed_motion_fact_counts_and_removes_nothing() {
    let samples = st97a_samples();
    let mut state = OpenSessionState::new();
    for sample in &samples[..3] {
        state = apply_sample(state, 1, sample).expect("apply").state;
    }
    // Obs 3 is dated before the cut: it folds into the drive and the
    // checkpoint and removes nothing.
    assert_eq!(cuts(&state), vec![T0 + 2 * MIN]);
    let open = state.open_drive.as_ref().unwrap();
    let checkpoint = &open.lead_checkpoints[0].drive;
    assert_eq!(checkpoint.speed_max, Some(80));
    assert_eq!(checkpoint.last_folded_at_ms, Some(T0 + MIN));
    assert_eq!(open.speed_max, Some(80));
    assert!(checkpoint.positions.is_empty());
    assert!(checkpoint.still_checkpoint.is_none());
    assert!(checkpoint.lead_checkpoints.is_empty());
    let last = apply_sample(state, 1, &samples[3]).expect("park");
    let drive = &last.delta.drives[0];
    assert_eq!(drive.end_date_ms, T0 + MIN + 30 * SEC);
    assert_eq!(drive.duration_min, Some(2));
    assert_eq!(drive.speed_max, Some(80));
    assert!(close_to(drive.distance_km.unwrap(), 0.6 * K_MILE_KM));
    assert_eq!(
        (drive.start_latitude, drive.end_latitude),
        (Some(10.0), Some(10.0))
    );
    assert_eq!(
        (drive.start_longitude, drive.end_longitude),
        (Some(20.0), Some(20.0))
    );
}

#[test]
fn st97_b_a_fresh_motion_fact_at_or_after_the_cut_settles_the_checkpoint() {
    let samples = vec![
        v_obs(1, T0, T0, json!("D"), json!(30), Some(A), None, None, None),
        p_obs(2, T0 + 2 * MIN, C, None, None, None),
        v_obs(
            3,
            T0 + 3 * MIN,
            T0 + 3 * MIN,
            json!("D"),
            json!(40),
            Some((10.0040, 20.0)),
            None,
            None,
            None,
        ),
        v_obs(
            4,
            T0 + 4 * MIN,
            T0 + 4 * MIN,
            json!("P"),
            Value::Null,
            None,
            None,
            None,
            None,
        ),
    ];
    let mut state = OpenSessionState::new();
    state = apply_sample(state, 1, &samples[0]).unwrap().state;
    state = apply_sample(state, 1, &samples[1]).unwrap().state;
    assert_eq!(cuts(&state), vec![T0 + 2 * MIN]);
    state = apply_sample(state, 1, &samples[2]).unwrap().state;
    assert!(cuts(&state).is_empty());
    let last = apply_sample(state, 1, &samples[3]).unwrap();
    assert_eq!(last.delta.drives.len(), 1);
    let drive = &last.delta.drives[0];
    assert_eq!((drive.start_date_ms, drive.end_date_ms), (T0, T0 + 4 * MIN));
    assert_eq!(
        (drive.end_latitude, drive.end_longitude),
        (Some(10.004), Some(20.0))
    );
    assert_eq!(drive.speed_max, Some(64));
    let all_positions = run_all(&samples)
        .delta
        .open_drive_positions
        .iter()
        .map(|position| (position.latitude, position.drive_id))
        .collect::<Vec<_>>();
    assert_eq!(
        all_positions,
        vec![(10.0, Some(1)), (10.003, Some(1)), (10.004, Some(1))]
    );
}

#[test]
fn st98_an_overflowing_checkpoint_list_ends_the_drive_at_the_newest_dropped_fact() {
    let samples = st98_samples();
    let before_park = run_all(&samples[..6]);
    // Five lead folds, four checkpoints held: the one cut at T0 + 1 min was
    // dropped.
    assert_eq!(
        cuts(&before_park.state),
        vec![T0 + 2 * MIN, T0 + 3 * MIN, T0 + 4 * MIN, T0 + 5 * MIN]
    );
    let step = run_all(&samples);
    assert_eq!(step.delta.drives.len(), 1);
    let drive = &step.delta.drives[0];
    // E is T0 + 30 s; the end source holds L1 (T0 + 1 min), so the end moves
    // to it and the row stays consistent with its children.
    assert_eq!(drive.end_date_ms, T0 + MIN);
    assert_eq!(drive.duration_min, Some(1));
    assert_eq!(
        (drive.end_latitude, drive.end_longitude),
        (Some(10.001), Some(20.0))
    );
    assert_eq!(drive.start_latitude, Some(10.0));
}

#[test]
fn st99_a_plugged_fact_older_than_the_newest_fold_ends_the_drive_at_its_last_evidence() {
    let samples = vec![
        v_obs(
            1,
            T0,
            T0,
            json!("D"),
            json!(30),
            Some(A),
            None,
            Some(1000.0),
            None,
        ),
        v_obs(
            2,
            T0 + 2 * MIN,
            T0 + 2 * MIN,
            json!("D"),
            json!(30),
            None,
            None,
            Some(1000.5),
            None,
        ),
        {
            let mut data = Map::new();
            data.insert(
                "drive_state".to_owned(),
                json!({"timestamp": T0 + MIN, "latitude": B_PRIME.0, "longitude": B_PRIME.1}),
            );
            data.insert(
                "charge_state".to_owned(),
                json!({"timestamp": T0 + 2 * MIN + 5 * SEC, "charging_state": "Charging",
                       "charge_energy_added": 0.1}),
            );
            vd(3, T0 + 2 * MIN + 10 * SEC, Value::Object(data))
        },
    ];
    let step = run_all(&samples);
    assert_eq!(step.delta.drives.len(), 1);
    let drive = &step.delta.drives[0];
    assert_eq!((drive.start_date_ms, drive.end_date_ms), (T0, T0 + 2 * MIN));
    assert!(close_to(drive.distance_km.unwrap(), 0.5 * K_MILE_KM));
    assert_eq!(
        (drive.end_latitude, drive.end_longitude),
        (Some(10.0), Some(20.0))
    );
    // Obs 3 is not folded and gives no standalone position.
    assert!(step.delta.positions.is_empty());
    assert_eq!(step.delta.open_drive_positions.len(), 1);
    let charge = step.state.open_charge.as_ref().expect("charge opens");
    assert_eq!(charge.start_date_ms, T0 + 2 * MIN + 5 * SEC);
    assert_eq!(charge.start_latitude, Some(B_PRIME.0));
    assert_eq!(charge.start_longitude, Some(B_PRIME.1));
    assert_eq!(step.delta.open_charge_samples.len(), 1);
    assert_eq!(
        step.delta.open_charge_samples[0].timestamp_ms,
        T0 + 2 * MIN + 5 * SEC
    );
}
