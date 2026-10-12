// SPDX-License-Identifier: AGPL-3.0-only

//! Part P storage-level spec tests (split from `phase_tests.rs` to keep each
//! source file under the repository line limit).

use super::*;
use crate::db::{
    HubStore, LifecycleCommit, ObservationInput, SourceDescriptor, StreamFaultPoint,
    VehicleDescriptor,
};

const CAR: i64 = 10;

fn store_with_vehicle() -> (tempfile::TempDir, HubStore, uuid::Uuid, uuid::Uuid) {
    let root = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(root.path()).expect("store");
    let source = store
        .register_source(
            &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
            T0,
        )
        .expect("source");
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "9"), T0)
        .expect("vehicle");
    (root, store, source.source_id, vehicle.vehicle_id)
}

fn input(source: uuid::Uuid, vehicle: uuid::Uuid, sample: &LifecycleSample) -> ObservationInput {
    ObservationInput {
        source_id: source,
        vehicle_id: vehicle,
        observed_at_ms: sample.observed_at_ms,
        payload: sample.payload.clone(),
    }
}

/// The lifecycle cursor of the vehicle: it advances exactly when an
/// observation is delivered to the lifecycle. Processed raw rows are
/// pruned after commit, so `raw_observations` cannot be counted.
fn cursor(store: &HubStore, vehicle: uuid::Uuid) -> i64 {
    store
        .load_lifecycle_state(vehicle)
        .expect("state")
        .map_or(0, |record| record.last_observation_id)
}

#[test]
fn st68_replay_after_a_failed_commit_gives_one_set_of_rows() {
    for point in [StreamFaultPoint::LifecycleWrite, StreamFaultPoint::Commit] {
        let (_root, store, source, vehicle) = store_with_vehicle();
        let first = d(0, T0, "online");
        let second = d(0, T0 + 20 * MIN, "asleep");
        store
            .accept_owner_observation_and_lifecycle(&input(source, vehicle, &first), T0, CAR)
            .expect("first observation");
        let before = cursor(&store, vehicle);
        store.inject_stream_fault(point);
        assert!(
            store
                .accept_owner_observation_and_lifecycle(
                    &input(source, vehicle, &second),
                    T0 + 20 * MIN,
                    CAR
                )
                .is_err(),
            "fault {point:?} fails the commit"
        );
        assert_eq!(
            cursor(&store, vehicle),
            before,
            "the failed commit moved nothing"
        );
        let retry = store
            .accept_owner_observation_and_lifecycle(
                &input(source, vehicle, &second),
                T0 + 20 * MIN,
                CAR,
            )
            .expect("retry");
        assert!(retry.append.inserted);
        let after_retry = cursor(&store, vehicle);
        assert!(after_retry > before);
        // A second retry of the same observation is a duplicate: no new rows.
        store
            .accept_owner_observation_and_lifecycle(
                &input(source, vehicle, &second),
                T0 + 20 * MIN,
                CAR,
            )
            .expect("duplicate");
        assert_eq!(cursor(&store, vehicle), after_retry);
        let history = store.materialised_history(vehicle).expect("history");
        assert_eq!(
            history
                .states
                .iter()
                .map(|row| (
                    row.id,
                    row.state.clone(),
                    row.start_date_ms,
                    row.end_date_ms
                ))
                .collect::<Vec<_>>(),
            vec![
                (1, "online".to_owned(), T0, Some(T0 + 20 * MIN)),
                (2, "asleep".to_owned(), T0 + 20 * MIN, None),
            ],
            "fault {point:?}"
        );
        let record = store.load_lifecycle_state(vehicle).unwrap().unwrap();
        let state = OpenSessionState::decode(&record.open_session_json).unwrap();
        assert_eq!(state.last_observation_id, after_retry);
        assert_eq!(state.last_reach.as_deref(), Some("asleep"));
    }
}

#[test]
fn st81_a_discovery_that_differs_from_the_last_reach_is_delivered() {
    let (_root, store, source, vehicle) = store_with_vehicle();
    let timeout = DEFAULT_OFFLINE_DRIVE_TIMEOUT;
    let discovery = |t: i64, state: &str| input(source, vehicle, &d(0, t, state));
    store
        .accept_owner_observation_and_lifecycle(&discovery(T0, "asleep"), T0, CAR)
        .expect("first discovery");
    assert_eq!(
        store.lifecycle_last_reach(vehicle).unwrap().as_deref(),
        Some("asleep")
    );
    let c1 = cursor(&store, vehicle);
    assert!(c1 > 0);
    // The same state again is freshness only: it never reaches the lifecycle.
    assert!(
        !store
            .record_current_discovery(&discovery(T0 + MIN, "asleep"), T0 + MIN, timeout)
            .unwrap()
    );
    assert_eq!(cursor(&store, vehicle), c1);
    // The car wakes and is seen through the stream.
    let wake = frame(
        0,
        T0 + 2 * MIN,
        "0,1000.0,80,25,180,10.0,20.0,,,200,210,181",
    );
    store
        .accept_owner_observation_and_lifecycle(&input(source, vehicle, &wake), T0 + 2 * MIN, CAR)
        .expect("stream wake");
    assert_eq!(
        store.lifecycle_last_reach(vehicle).unwrap().as_deref(),
        Some("online")
    );
    let c2 = cursor(&store, vehicle);
    assert!(c2 > c1);
    // The same asleep discovery now differs from last_reach: delivered.
    assert!(
        store
            .record_current_discovery(&discovery(T0 + 5 * MIN, "asleep"), T0 + 5 * MIN, timeout)
            .unwrap()
    );
    let c3 = cursor(&store, vehicle);
    assert!(c3 > c2);
    assert_eq!(
        store.lifecycle_last_reach(vehicle).unwrap().as_deref(),
        Some("asleep")
    );
    // A repeat afterwards is not stored again.
    assert!(
        !store
            .record_current_discovery(&discovery(T0 + 6 * MIN, "asleep"), T0 + 6 * MIN, timeout)
            .unwrap()
    );
    assert_eq!(cursor(&store, vehicle), c3);
    // A state text outside Tesla's three gives no evidence and is not delivered.
    assert!(
        !store
            .record_current_discovery(&discovery(T0 + 7 * MIN, "suspended"), T0 + 7 * MIN, timeout)
            .unwrap()
    );
    assert_eq!(cursor(&store, vehicle), c3);
    let history = store.materialised_history(vehicle).unwrap();
    assert_eq!(
        history
            .states
            .iter()
            .map(|row| (row.state.clone(), row.start_date_ms, row.end_date_ms))
            .collect::<Vec<_>>(),
        vec![
            ("asleep".to_owned(), T0, Some(T0 + 2 * MIN)),
            ("online".to_owned(), T0 + 2 * MIN, Some(T0 + 5 * MIN)),
            ("asleep".to_owned(), T0 + 5 * MIN, None),
        ]
    );
}

#[test]
fn st87_the_imported_version_seeds_the_first_update_row() {
    for (live, rows) in [("2024.20.1 bb", 1), ("2024.14.3 aa", 0)] {
        let (_root, store, source, vehicle) = store_with_vehicle();
        // An imported update ending two days before the live observation.
        let mut seed_state = OpenSessionState::new();
        seed_state.last_observation_id = 0;
        let encoded = seed_state.encode().unwrap();
        let delta = LifecycleDelta {
            updates: vec![
                ProjectionUpdate {
                    id: 1,
                    car_id: CAR,
                    start_date_ms: T0 - 3 * 24 * HOUR,
                    end_date_ms: T0 - 2 * 24 * HOUR - HOUR,
                    version: "2024.8.9".to_owned(),
                },
                ProjectionUpdate {
                    id: 2,
                    car_id: CAR,
                    start_date_ms: T0 - 2 * 24 * HOUR - 30 * MIN,
                    end_date_ms: T0 - 2 * 24 * HOUR,
                    version: "2024.14.3 aa".to_owned(),
                },
                ProjectionUpdate {
                    id: 3,
                    car_id: CAR,
                    start_date_ms: T0 - 2 * 24 * HOUR - 20 * MIN,
                    end_date_ms: T0 - 2 * 24 * HOUR - 10 * MIN,
                    version: "develop".to_owned(),
                },
            ],
            ..LifecycleDelta::default()
        };
        store
            .commit_lifecycle_delta(&LifecycleCommit {
                vehicle_id: vehicle,
                car_id: CAR,
                open_session_json: &encoded,
                last_observation_id: 0,
                quarantined: false,
                updated_at_ms: T0,
                delta: &delta,
            })
            .expect("imported updates");
        let mut session = TeslaMateOpenSession {
            car_id: CAR,
            ..Default::default()
        };
        // The imported update ids are 1 to 3, so live ids start after them.
        session.watermarks.updates.max_id = Some(3);
        store
            .seed_imported_open_session(source, vehicle, CAR, &session, T0)
            .expect("seed");
        let record = store.load_lifecycle_state(vehicle).unwrap().unwrap();
        let seeded = OpenSessionState::decode(&record.open_session_json).unwrap();
        assert_eq!(seeded.last_car_version.as_deref(), Some("2024.14.3 aa"));
        assert_eq!(seeded.last_car_version_at_ms, Some(T0 - 2 * 24 * HOUR));

        let live_sample = v(
            0,
            T0,
            json!({"vehicle_state": {"timestamp": T0, "car_version": live}}),
        );
        store
            .accept_owner_observation_and_lifecycle(&input(source, vehicle, &live_sample), T0, CAR)
            .expect("live observation");
        let history = store.materialised_history(vehicle).unwrap();
        let new_rows = history
            .updates
            .iter()
            .filter(|row| row.id > 3)
            .collect::<Vec<_>>();
        assert_eq!(new_rows.len(), rows, "live {live}");
        if rows == 1 {
            assert_eq!(new_rows[0].start_date_ms, T0 - 2 * 24 * HOUR);
            assert_eq!(new_rows[0].end_date_ms, T0);
            assert_eq!(new_rows[0].version, "2024.20.1 bb");
        }
    }
}

#[test]
fn imported_version_seed_ignores_unusable_versions_and_keeps_a_known_one() {
    let updates = vec![
        ProjectionUpdate {
            id: 1,
            car_id: 1,
            start_date_ms: 1,
            end_date_ms: 10,
            version: "2024.8.9".into(),
        },
        ProjectionUpdate {
            id: 2,
            car_id: 1,
            start_date_ms: 1,
            end_date_ms: 20,
            version: "develop".into(),
        },
        ProjectionUpdate {
            id: 3,
            car_id: 1,
            start_date_ms: 1,
            end_date_ms: 15,
            version: "2024.14.3 aa".into(),
        },
        ProjectionUpdate {
            id: 4,
            car_id: 1,
            start_date_ms: 1,
            end_date_ms: 15,
            version: "2024.14.4 aa".into(),
        },
    ];
    let mut state = OpenSessionState::new();
    seed_version_from_updates(&mut state, &updates);
    assert_eq!(state.last_car_version.as_deref(), Some("2024.14.4 aa"));
    assert_eq!(state.last_car_version_at_ms, Some(15));
    let mut known = OpenSessionState::new();
    known.last_car_version = Some("2025.2.6".to_owned());
    seed_version_from_updates(&mut known, &updates);
    assert_eq!(known.last_car_version.as_deref(), Some("2025.2.6"));
    let mut none = OpenSessionState::new();
    seed_version_from_updates(&mut none, &[]);
    assert_eq!(none.last_car_version, None);
}

// ------------------------------------------------------------ Hub events

/// The vehicle's persisted lifecycle state.
fn stored_state(store: &HubStore, vehicle: uuid::Uuid) -> OpenSessionState {
    let record = store
        .load_lifecycle_state(vehicle)
        .expect("state")
        .expect("lifecycle state");
    OpenSessionState::decode(&record.open_session_json).expect("decode")
}

/// The vehicle's `current_observations` rows: record type, observation id and
/// payload digest.
fn current_rows(store: &HubStore, vehicle: uuid::Uuid) -> Vec<(String, i64, Vec<u8>)> {
    let connection = store.open().expect("connection");
    let mut statement = connection
        .prepare(
            "SELECT record_type, observation_id, payload_sha256
             FROM current_observations WHERE vehicle_id = ?1 ORDER BY record_type",
        )
        .expect("prepare");
    statement
        .query_map(rusqlite::params![vehicle.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .expect("query")
        .map(|row| row.expect("row"))
        .collect()
}

fn motion(t: i64, odometer: f64) -> LifecycleSample {
    v(
        0,
        t,
        json!({
            "drive_state": {"timestamp": t, "shift_state": "D", "speed": 30,
                            "latitude": 10.0, "longitude": 20.0},
            "vehicle_state": {"timestamp": t, "odometer": odometer},
        }),
    )
}

fn charging(t: i64, counter: f64) -> LifecycleSample {
    v(
        0,
        t,
        json!({"charge_state": {"timestamp": t, "charging_state": "Charging",
                                "charge_energy_added": counter, "battery_level": 50}}),
    )
}

/// Accept a sample through the owner intake and return the stored record.
fn accept(
    store: &HubStore,
    source: uuid::Uuid,
    vehicle: uuid::Uuid,
    sample: &LifecycleSample,
) -> crate::db::OwnerObservationResult {
    store
        .accept_owner_observation_and_lifecycle_with_offline_timeout(
            &input(source, vehicle, sample),
            sample.observed_at_ms,
            CAR,
            DEFAULT_OFFLINE_DRIVE_TIMEOUT,
        )
        .expect("intake")
}

#[test]
fn st91_hub_events_are_accepted_without_a_current_row() {
    let (_root, store, source, vehicle) = store_with_vehicle();
    accept(&store, source, vehicle, &motion(T0, 1000.0));
    accept(&store, source, vehicle, &motion(T0 + MIN, 1000.5));
    let current_before = current_rows(&store, vehicle);
    assert!(!current_before.is_empty());
    assert!(
        store
            .materialised_history(vehicle)
            .unwrap()
            .drives
            .is_empty()
    );

    // `no_data`: the intake stores the event (the processed raw row is pruned
    // in the same transaction, so the result is the evidence), writes no
    // current row, and the open drive closes at its last fold.
    let result = accept(&store, source, vehicle, &e(0, T0 + 30 * MIN, "no_data"));
    assert!(result.append.inserted);
    assert_eq!(
        result.append.observation.payload["record_type"],
        "hub_lifecycle_event_v1"
    );
    assert_eq!(current_rows(&store, vehicle), current_before);
    assert_eq!(
        cursor(&store, vehicle),
        result.append.observation.observation_id
    );
    let drives = store.materialised_history(vehicle).unwrap().drives;
    assert_eq!(drives.len(), 1);
    assert_eq!(drives[0].end_date_ms, T0 + MIN);

    // An unknown kind is accepted and stored, and only the cursor moves.
    let before = stored_state(&store, vehicle);
    let result = accept(&store, source, vehicle, &e(0, T0 + 31 * MIN, "later_kind"));
    assert!(result.append.inserted);
    assert_eq!(current_rows(&store, vehicle), current_before);
    assert_eq!(
        cursor(&store, vehicle),
        result.append.observation.observation_id
    );
    let mut after = stored_state(&store, vehicle);
    assert_eq!(
        after.last_observation_id,
        result.append.observation.observation_id
    );
    after.last_observation_id = before.last_observation_id;
    assert_eq!(after, before, "only the cursor moved");
    assert_eq!(store.materialised_history(vehicle).unwrap().drives.len(), 1);

    // `service_refused` closes the open charge at its last sample and sets
    // both service flags.
    accept(&store, source, vehicle, &charging(T0 + 115 * MIN, 1.0));
    accept(&store, source, vehicle, &charging(T0 + 2 * HOUR, 2.0));
    let current_charging = current_rows(&store, vehicle);
    accept(
        &store,
        source,
        vehicle,
        &e(0, T0 + 2 * HOUR + 10 * MIN, "service_refused"),
    );
    assert_eq!(current_rows(&store, vehicle), current_charging);
    let charges = store.materialised_history(vehicle).unwrap().charges;
    assert_eq!(charges.len(), 1, "{charges:?}");
    assert_eq!(charges[0].end_date_ms, Some(T0 + 2 * HOUR));
    let state = stored_state(&store, vehicle);
    assert_eq!(in_service(&state), (Some(true), true));
    assert!(state.open_charge.is_none());
}

#[test]
fn an_event_never_makes_the_next_observation_reuse_its_id() {
    // The raw row of a processed event is pruned and the event has no current
    // row, so an id taken from the surviving rows alone would be handed out
    // again to the next observation, which the cursor then skips.
    let (root, store, source, vehicle) = store_with_vehicle();
    let first = accept(&store, source, vehicle, &motion(T0, 1000.0));
    let event = accept(&store, source, vehicle, &e(0, T0 + MIN, "service_refused"));
    let event_id = event.append.observation.observation_id;
    assert!(event_id > first.append.observation.observation_id);
    // Restart between the event and the next observation.
    drop(store);
    let store = HubStore::initialize(root.path()).expect("reopen");
    let next = accept(&store, source, vehicle, &motion(T0 + 2 * MIN, 1000.5));
    assert!(
        next.append.observation.observation_id > event_id,
        "{} must be above {event_id}",
        next.append.observation.observation_id
    );
    let state = stored_state(&store, vehicle);
    assert_eq!(
        state.last_observation_id,
        next.append.observation.observation_id
    );
    // The observation was projected: fresh full-source data ends the refusal.
    assert_eq!(in_service(&state), (None, false));
    assert_eq!(state.last_drive_timestamp_ms, Some(T0 + 2 * MIN));
}

#[test]
fn st82_a_stored_sequence_replays_to_the_same_blob_and_rows() {
    let (_root, store, source, vehicle) = store_with_vehicle();
    let sequence = [
        motion(T0 - MIN, 1000.0),
        motion(T0, 1000.5),
        d(0, T0 + 5 * MIN, "online"),
        e(0, T0 + 30 * MIN, "no_data"),
    ];
    let mut stored = Vec::new();
    for sample in &sequence {
        let record = accept(&store, source, vehicle, sample).append.observation;
        stored.push(LifecycleSample {
            observation_id: record.observation_id,
            observed_at_ms: record.observed_at_ms,
            vehicle_state: if sample.payload["record_type"] == "hub_lifecycle_event_v1" {
                "unknown".to_owned()
            } else {
                sample.vehicle_state.clone()
            },
            payload: record.payload,
        });
    }
    let replay = apply_samples(OpenSessionState::new(), CAR, &stored).expect("replay");
    let record = store.load_lifecycle_state(vehicle).unwrap().unwrap();
    assert_eq!(
        record.open_session_json,
        replay.state.encode().unwrap(),
        "byte-identical blob"
    );
    assert_eq!(record.last_observation_id, replay.state.last_observation_id);
    let history = store.materialised_history(vehicle).unwrap();
    assert_eq!(history.drives.len(), 1);
    assert_eq!(history.drives, replay.delta.drives);
    let closed_states = |states: &[ProjectionState]| {
        states
            .iter()
            .map(|row| {
                (
                    row.id,
                    row.state.clone(),
                    row.start_date_ms,
                    row.end_date_ms,
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        closed_states(&history.states),
        closed_states(&replay.delta.states)
    );
}
