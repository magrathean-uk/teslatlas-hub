// SPDX-License-Identifier: AGPL-3.0-only

// Drive lifecycle through the store (C1 part D): restart mid-drive (ST66),
// `apply_samples` against storage (ST76), child placement of discarded and
// ended drives (R48, R48a), the terrain helper (ST42, ST94, R85) and geofence
// relabelling (ST49). Synthetic data only; the sequences come from the
// lifecycle drive tests.

fn drive_store_input(
    source: &SourceRecord,
    vehicle: &VehicleRecord,
    sample: &crate::lifecycle::LifecycleSample,
) -> ObservationInput {
    ObservationInput {
        source_id: source.source_id,
        vehicle_id: vehicle.vehicle_id,
        observed_at_ms: sample.observed_at_ms,
        payload: sample.payload.clone(),
    }
}

fn feed_owner_samples(
    store: &HubStore,
    source: &SourceRecord,
    vehicle: &VehicleRecord,
    samples: &[crate::lifecycle::LifecycleSample],
) {
    for sample in samples {
        store
            .accept_owner_observation_and_lifecycle(
                &drive_store_input(source, vehicle, sample),
                sample.observed_at_ms,
                1,
            )
            .expect("owner observation");
    }
}

fn feed_stream_samples(
    store: &HubStore,
    source: &SourceRecord,
    vehicle: &VehicleRecord,
    samples: &[crate::lifecycle::LifecycleSample],
) {
    for sample in samples {
        let result = store
            .accept_stream_observation_and_lifecycle(
                &drive_store_input(source, vehicle, sample),
                sample.observed_at_ms,
                1,
            )
            .expect("stream observation");
        assert!(matches!(
            result,
            StreamObservationResult::Committed { .. }
        ));
    }
}

fn placed_positions(history: &MaterialisedHistory) -> Vec<(i64, Option<i64>, i64)> {
    let mut rows = history
        .positions
        .iter()
        .map(|position| (position.id, position.drive_id, position.date_ms))
        .collect::<Vec<_>>();
    rows.sort_unstable();
    rows
}

fn open_position_rows(store: &HubStore, vehicle: &VehicleRecord) -> i64 {
    store
        .open()
        .expect("connection")
        .query_row(
            "SELECT COUNT(*) FROM lifecycle_open_rows
             WHERE vehicle_id = ?1 AND domain = 'position'",
            params![vehicle.vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("open position rows")
}

#[test]
fn st66_a_restart_mid_drive_gives_the_rows_of_an_uninterrupted_store() {
    use crate::lifecycle::drive_tests::{MIN, T0, moving, parked};
    let mut samples = moving(1, T0, 4);
    samples.push(parked(5, T0 + 4 * MIN, 10.004, Some(1002.0)));

    let uninterrupted_dir = crate::private_tempdir().expect("tempdir");
    let uninterrupted = HubStore::initialize(uninterrupted_dir.path()).expect("store");
    let (source, vehicle) = test_registered_vehicle(&uninterrupted);
    feed_owner_samples(&uninterrupted, &source, &vehicle, &samples);
    let expected = uninterrupted
        .materialised_history(vehicle.vehicle_id)
        .expect("history");
    assert_eq!(expected.drives.len(), 1);
    assert_eq!(expected.positions.len(), 5);

    let restarted_dir = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(restarted_dir.path()).expect("store");
    let (source, vehicle) = test_registered_vehicle(&store);
    feed_owner_samples(&store, &source, &vehicle, &samples[..2]);
    let record = store
        .load_lifecycle_state(vehicle.vehicle_id)
        .expect("load")
        .expect("state");
    let state = crate::lifecycle::OpenSessionState::decode(&record.open_session_json).expect("decode");
    let open = state.open_drive.expect("a drive is open");
    // The children live in the open rows, not in the state blob.
    assert!(open.positions.is_empty());
    assert_eq!(
        store
            .open_drive_positions(vehicle.vehicle_id, open.id)
            .expect("children")
            .len(),
        2
    );
    drop(store);

    let store = HubStore::initialize(restarted_dir.path()).expect("restart");
    feed_owner_samples(&store, &source, &vehicle, &samples[2..]);
    let actual = store
        .materialised_history(vehicle.vehicle_id)
        .expect("history");
    assert_eq!(actual.drives, expected.drives);
    assert_eq!(actual.positions, expected.positions);
    assert_eq!(open_position_rows(&store, &vehicle), 0);
}

#[test]
fn st76_apply_samples_places_children_exactly_like_the_store() {
    use crate::lifecycle::drive_tests::{st24_samples, st27_samples, st29b_samples};
    use crate::lifecycle::{OpenSessionState, apply_samples};
    type Feed = fn(
        &HubStore,
        &SourceRecord,
        &VehicleRecord,
        &[crate::lifecycle::LifecycleSample],
    );
    let cases: [(&str, Vec<crate::lifecycle::LifecycleSample>, Feed); 3] = [
        ("ST24", st24_samples(), feed_stream_samples),
        ("ST27", st27_samples(), feed_owner_samples),
        ("ST29b", st29b_samples(), feed_owner_samples),
    ];
    for (name, samples, feed) in cases {
        let step = apply_samples(OpenSessionState::new(), 1, &samples).expect("apply samples");
        let mut expected = step
            .delta
            .open_drive_positions
            .iter()
            .chain(step.delta.positions.iter())
            .map(|position| (position.id, position.drive_id, position.date_ms))
            .collect::<Vec<_>>();
        expected.sort_unstable();

        let temp = crate::private_tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (source, vehicle) = test_registered_vehicle(&store);
        feed(&store, &source, &vehicle, &samples);
        let history = store
            .materialised_history(vehicle.vehicle_id)
            .expect("history");
        assert_eq!(placed_positions(&history), expected, "{name}");
        assert_eq!(history.drives, step.delta.drives, "{name}");
        assert_eq!(open_position_rows(&store, &vehicle), 0, "{name}");
    }
}

#[test]
fn a_stationary_tail_is_materialised_as_standalone_positions() {
    use crate::lifecycle::drive_tests::{MIN, T0, st24_samples};
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (source, vehicle) = test_registered_vehicle(&store);
    feed_stream_samples(&store, &source, &vehicle, &st24_samples());
    let history = store
        .materialised_history(vehicle.vehicle_id)
        .expect("history");
    assert_eq!(history.drives.len(), 1);
    assert_eq!(history.drives[0].end_date_ms, T0 + 6 * MIN);
    let under_the_drive = history
        .positions
        .iter()
        .filter(|position| position.drive_id == Some(history.drives[0].id))
        .map(|position| position.date_ms)
        .collect::<Vec<_>>();
    assert_eq!(under_the_drive, (0..=6).map(|k| T0 + k * MIN).collect::<Vec<_>>());
    let standalone = history
        .positions
        .iter()
        .filter(|position| position.drive_id.is_none())
        .map(|position| position.date_ms)
        .collect::<Vec<_>>();
    assert_eq!(standalone, (7..=15).map(|k| T0 + k * MIN).collect::<Vec<_>>());
}

#[test]
fn a_discarded_drive_keeps_its_positions_as_standalone_rows() {
    use crate::lifecycle::drive_tests::{SEC, T0, full};
    use serde_json::json;
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (source, vehicle) = test_registered_vehicle(&store);
    // Observation 1 opens a drive and stores its position as an open row.
    let first = full(1, T0, json!("D"), json!(3), 10.0, Some(1000.0), None);
    feed_owner_samples(&store, &source, &vehicle, std::slice::from_ref(&first));
    assert_eq!(open_position_rows(&store, &vehicle), 1);
    // Observation 2 parks without having moved: the drive is discarded.
    let second = full(2, T0 + 10 * SEC, json!("P"), json!(0), 10.00002, Some(1000.0), None);
    feed_owner_samples(&store, &source, &vehicle, std::slice::from_ref(&second));
    let history = store
        .materialised_history(vehicle.vehicle_id)
        .expect("history");
    assert!(history.drives.is_empty());
    assert_eq!(
        placed_positions(&history),
        vec![(1, None, T0), (2, None, T0 + 10 * SEC)]
    );
    assert_eq!(open_position_rows(&store, &vehicle), 0);
}

fn drive_row(id: i64, end_date_ms: i64) -> crate::hub_pack::ProjectionDrive {
    serde_json::from_value(serde_json::json!({
        "id": id, "car_id": 1, "start_date_ms": end_date_ms - 3, "end_date_ms": end_date_ms
    }))
    .expect("drive row")
}

fn position_row(id: i64, drive_id: Option<i64>, date_ms: i64) -> crate::hub_pack::ProjectionPosition {
    serde_json::from_value(serde_json::json!({
        "id": id, "drive_id": drive_id, "car_id": 1, "date_ms": date_ms,
        "latitude": 10.0, "longitude": 20.0
    }))
    .expect("position row")
}

#[test]
fn the_commit_places_children_after_the_end_and_of_discarded_drives_outside_them() {
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);
    let vehicle_key = vehicle.vehicle_id.to_string();
    let state = crate::lifecycle::OpenSessionState::new().encode().expect("state");
    let commit = |last_observation_id: i64, delta: &crate::lifecycle::LifecycleDelta| {
        store
            .commit_lifecycle_delta(&LifecycleCommit {
                vehicle_id: vehicle.vehicle_id,
                car_id: 1,
                open_session_json: &state,
                last_observation_id,
                quarantined: false,
                updated_at_ms: 1_000,
                delta,
            })
            .expect("commit");
    };
    // Two drives are open with children from earlier commits.
    commit(
        1,
        &crate::lifecycle::LifecycleDelta {
            open_drive_positions: vec![
                position_row(1, Some(1), 100),
                position_row(2, Some(1), 200),
                position_row(3, Some(2), 300),
                position_row(4, Some(2), 400),
            ],
            ..Default::default()
        },
    );
    // Drive 1 closes at 150: its child at 200 is after the end. Drive 2 is
    // discarded: both children become standalone.
    commit(
        2,
        &crate::lifecycle::LifecycleDelta {
            drives: vec![drive_row(1, 150)],
            discarded_drive_ids: vec![2],
            ..Default::default()
        },
    );
    let history = store
        .materialised_history(vehicle.vehicle_id)
        .expect("history");
    assert_eq!(history.drives.len(), 1);
    assert_eq!(
        placed_positions(&history),
        vec![(1, Some(1), 100), (2, None, 200), (3, None, 300), (4, None, 400)]
    );
    let leftover: i64 = store
        .open()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM lifecycle_open_rows WHERE vehicle_id = ?1",
            params![vehicle_key],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(leftover, 0);
}

#[test]
fn st94_the_terrain_helper_writes_only_ascent_descent_and_the_drive_json() {
    use crate::lifecycle::drive_tests::{MIN, T0, moving, parked};
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (source, vehicle) = test_registered_vehicle(&store);
    // A parked observation first gives a standalone position, which the
    // helper must leave alone; the drive has no elevations.
    let mut samples = vec![parked(1, T0 - 30 * MIN, 10.0, Some(999.0))];
    samples.extend(moving(2, T0, 3));
    samples.push(parked(5, T0 + 3 * MIN, 10.003, Some(1002.0)));
    feed_owner_samples(&store, &source, &vehicle, &samples);
    let vehicle_key = vehicle.vehicle_id.to_string();
    let history = store
        .materialised_history(vehicle.vehicle_id)
        .expect("history");
    assert_eq!(history.drives.len(), 1);
    let drive_id = history.drives[0].id;
    assert_eq!((history.drives[0].ascent, history.drives[0].descent), (None, None));
    let drive_positions = history
        .positions
        .iter()
        .filter(|position| position.drive_id == Some(drive_id))
        .map(|position| position.id)
        .collect::<Vec<_>>();
    assert_eq!(drive_positions.len(), 4);
    let standalone = history
        .positions
        .iter()
        .find(|position| position.drive_id.is_none())
        .expect("standalone position")
        .id;

    let mut connection = store.open().expect("connection");
    let read_drive = |connection: &rusqlite::Connection| -> (String, Option<i64>, Option<i64>) {
        connection
            .query_row(
                "SELECT drive_json, ascent, descent FROM materialised_drives
                 WHERE vehicle_id = ?1 AND drive_id = ?2",
                params![vehicle_key, drive_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("drive row")
    };
    let read_position = |connection: &rusqlite::Connection, id: i64| -> String {
        connection
            .query_row(
                "SELECT position_json FROM materialised_positions
                 WHERE vehicle_id = ?1 AND position_id = ?2",
                params![vehicle_key, id],
                |row| row.get(0),
            )
            .expect("position row")
    };
    let mutations = |connection: &rusqlite::Connection| -> i64 {
        connection
            .query_row("SELECT COUNT(*) FROM sync_mutations", [], |row| row.get(0))
            .expect("mutation count")
    };

    // Terrain fills the three elevations: 100, 110, 100, 100.
    for (id, elevation) in drive_positions.iter().zip([100, 110, 100, 100]) {
        connection
            .execute(
                "UPDATE materialised_positions
                 SET position_json = json_set(position_json, '$.elevation', ?3)
                 WHERE vehicle_id = ?1 AND position_id = ?2",
                params![vehicle_key, id, elevation],
            )
            .expect("terrain elevation");
    }
    let (before_json, _, _) = read_drive(&connection);
    let standalone_before = read_position(&connection, standalone);
    let mutations_before = mutations(&connection);

    let transaction = connection.transaction().expect("transaction");
    recompute_terrain_drive(&transaction, &vehicle_key, drive_id).expect("recompute");
    transaction.commit().expect("commit");

    let (after_json, ascent, descent) = read_drive(&connection);
    assert_eq!((ascent, descent), (Some(10), Some(10)));
    let mut expected: serde_json::Value = serde_json::from_str(&before_json).unwrap();
    expected["ascent"] = serde_json::json!(10);
    expected["descent"] = serde_json::json!(10);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&after_json).unwrap(), expected);
    assert_eq!(read_position(&connection, standalone), standalone_before);
    assert_eq!(mutations(&connection), mutations_before, "the caller records the change");
    // The row equals the one function over the new elevations (ST42).
    assert_eq!(
        (ascent, descent),
        crate::lifecycle::elevation_totals([Some(100), Some(110), Some(100), Some(100)])
    );
}

#[test]
fn the_terrain_helper_walks_positions_in_date_then_id_order() {
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);
    let state = crate::lifecycle::OpenSessionState::new().encode().expect("state");
    // The ids run against the dates: by date the elevations rise 100, 110,
    // 130; by id they would fall.
    let mut positions = vec![
        position_row(1, Some(7), 300),
        position_row(2, Some(7), 200),
        position_row(3, Some(7), 100),
    ];
    for (position, elevation) in positions.iter_mut().zip([130, 110, 100]) {
        position.elevation = Some(elevation);
    }
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle.vehicle_id,
            car_id: 1,
            open_session_json: &state,
            last_observation_id: 1,
            quarantined: false,
            updated_at_ms: 1_000,
            delta: &crate::lifecycle::LifecycleDelta {
                drives: vec![drive_row(7, 400)],
                positions,
                ..Default::default()
            },
        })
        .expect("commit");
    let vehicle_key = vehicle.vehicle_id.to_string();
    let mut connection = store.open().expect("connection");
    let transaction = connection.transaction().expect("transaction");
    recompute_terrain_drive(&transaction, &vehicle_key, 7).expect("recompute");
    transaction.commit().expect("commit");
    let (ascent, descent): (Option<i64>, Option<i64>) = connection
        .query_row(
            "SELECT ascent, descent FROM materialised_drives
             WHERE vehicle_id = ?1 AND drive_id = 7",
            params![vehicle_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("drive row");
    assert_eq!((ascent, descent), (Some(30), Some(0)));
}

#[test]
fn st49_a_deleted_fence_relabels_stored_drives_identically() {
    use crate::lifecycle::drive_tests::{MIN, T0, moving, parked};
    let temp = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (source, vehicle) = test_registered_vehicle(&store);
    let fence = |name: &str, radius: f64| crate::teslamate_projection::TeslaMateGeofence {
        id: 0,
        name: name.to_owned(),
        latitude: Some(10.0),
        longitude: Some(20.0),
        radius_m: Some(radius),
        billing_type: Some(crate::hub_pack::GeofenceBillingType::PerKwh),
        cost_per_unit: None,
        session_fee: None,
    };
    let home = store
        .save_geofence(vehicle.vehicle_id, None, fence("Home", 150.0))
        .expect("home");
    let area = store
        .save_geofence(vehicle.vehicle_id, None, fence("Area", 800.0))
        .expect("area");
    let mut samples = moving(1, T0, 3);
    samples.push(parked(4, T0 + 3 * MIN, 10.003, Some(1002.0)));
    feed_owner_samples(&store, &source, &vehicle, &samples);
    let labels = |store: &HubStore| {
        let history = store.materialised_history(vehicle.vehicle_id).expect("history");
        (
            history.drives[0].start_geofence.clone(),
            history.drives[0].end_geofence.clone(),
        )
    };
    // Smallest radius first: the start is in both fences, the end 333 m away
    // is only in the larger one.
    assert_eq!(labels(&store), (Some("Home".to_owned()), Some("Area".to_owned())));
    store
        .delete_geofence(vehicle.vehicle_id, home.id)
        .expect("delete home");
    assert_eq!(labels(&store), (Some("Area".to_owned()), Some("Area".to_owned())));
    // Saving the same fence again relabels with the same result.
    store
        .save_geofence(vehicle.vehicle_id, Some(area.id), area.clone())
        .expect("resave area");
    assert_eq!(labels(&store), (Some("Area".to_owned()), Some("Area".to_owned())));
    store
        .delete_geofence(vehicle.vehicle_id, area.id)
        .expect("delete area");
    assert_eq!(labels(&store), (None, None));
}
