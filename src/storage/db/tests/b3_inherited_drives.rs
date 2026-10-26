// SPDX-License-Identifier: AGPL-3.0-only

//! C1-B storage-level spec tests (C1 section 8a, C1 R93: C1 ST111 (b), (c), (g),
//! (h), (i), (j) and (k)), through `HubStore`: a drive that was open across the
//! upgrade is recomputed once from its stored positions in the commit that
//! closes it, after every position mutation and before the charge close, and
//! `recompute_terrain_drive` reads the drive one row at a time through the drive
//! index with no `ORDER BY`. The unit half (decode, flag, listing, merge) is in
//! `src/runtime/lifecycle/inherited_drive_tests.rs`. Synthetic data only.

use super::*;
use crate::lifecycle::b3_c1_b::{
    SEEDED_START, inherited_state, old_blob, seeded_session, seeded_session_with_totals,
    st111_discard_obs1, st111_discard_obs2, st111_obs1, st111_obs2,
};
use crate::lifecycle::drive_tests::{MIN, T0, vd};
use serde_json::json;

/// Stored elevations of positions 11 to 13 in ST111 (b): with 110 and 108 from
/// obs 1 and 2 the shared walk gives (10, 2).
const STORED_B: [i64; 3] = [100, 101, 104];
/// ST111 (c)'s stored elevations: the same walk gives (43, 7), the closing row.
const STORED_C: [i64; 3] = [72, 109, 104];

/// A store holding ST111's old blob (the vehicle's lifecycle state, with
/// `next_position_id` 14) and the drive's three open positions 11 to 13, dated
/// t0-3m, t0-2m and t0-1m, as a store left by the earlier build holds them.
struct InheritedStore {
    _directory: tempfile::TempDir,
    store: HubStore,
    source: SourceRecord,
    vehicle: VehicleRecord,
}

impl InheritedStore {
    /// `reencode` writes the blob through a decode and an encode first, as C6's
    /// identity tidy does at the first read-write open (ST111 (g)).
    fn new(elevations: [i64; 3], reencode: bool) -> Self {
        Self::with_last_odometer(elevations, reencode, 1000.4)
    }

    /// As [`InheritedStore::new`] with the blob's last odometer (miles) given:
    /// ST111 (d) uses 1000.0, so that the drive never moves and is discarded.
    fn with_last_odometer(elevations: [i64; 3], reencode: bool, last_odometer_mi: f64) -> Self {
        let directory = crate::private_tempdir().expect("tempdir");
        let store = HubStore::initialize(directory.path()).expect("store");
        let (source, vehicle) = test_registered_vehicle(&store);
        let mut blob = old_blob(&inherited_state(last_odometer_mi));
        if reencode {
            let state = crate::lifecycle::OpenSessionState::decode(&blob).expect("decode");
            blob = state.encode().expect("encode with no step");
        }
        let children = [(11_i64, -3_i64), (12, -2), (13, -1)]
            .into_iter()
            .zip(elevations)
            .map(|((id, minutes), elevation)| {
                let mut position = position_row(id, Some(1), T0 + minutes * MIN);
                position.elevation = Some(elevation);
                position
            })
            .collect::<Vec<_>>();
        store
            .commit_lifecycle_delta(&LifecycleCommit {
                vehicle_id: vehicle.vehicle_id,
                car_id: 1,
                open_session_json: &blob,
                last_observation_id: 0,
                quarantined: false,
                updated_at_ms: T0,
                delta: &crate::lifecycle::LifecycleDelta {
                    open_drive_positions: children,
                    ..Default::default()
                },
            })
            .expect("store the old state and its open positions");
        Self {
            _directory: directory,
            store,
            source,
            vehicle,
        }
    }

    fn feed(&self, samples: &[crate::lifecycle::LifecycleSample]) {
        feed_owner_samples(&self.store, &self.source, &self.vehicle, samples);
    }

    fn history(&self) -> MaterialisedHistory {
        self.store
            .materialised_history(self.vehicle.vehicle_id)
            .expect("history")
    }
}

/// `(revision, payload)` of the `upsert` sync mutations of one drive, oldest
/// first.
fn drive_mutations(
    store: &HubStore,
    vehicle: &VehicleRecord,
    drive_id: i64,
) -> Vec<(i64, serde_json::Value)> {
    let connection = store.open().expect("connection");
    let mut statement = connection
        .prepare(
            "SELECT revision, payload_json FROM sync_mutations
             WHERE vehicle_id = ?1 AND entity = 'drive' AND entity_id = ?2
             ORDER BY revision",
        )
        .expect("statement");
    let rows = statement
        .query_map(params![vehicle.vehicle_id.to_string(), drive_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .expect("rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect");
    rows.into_iter()
        .map(|(revision, payload)| {
            (
                revision,
                serde_json::from_str(&payload).expect("payload json"),
            )
        })
        .collect()
}

/// Every sync mutation of the vehicle in revision order, as
/// `(entity, entity_id, operation, payload)`.
fn mutation_rows(store: &HubStore, vehicle: &VehicleRecord) -> Vec<(String, i64, String, String)> {
    mutation_rows_on(
        &store.open().expect("connection"),
        &vehicle.vehicle_id.to_string(),
    )
}

/// [`mutation_rows`] over a given connection or open transaction, so that a
/// test can look at what a transaction has written before it is dropped.
fn mutation_rows_on(
    connection: &rusqlite::Connection,
    vehicle_key: &str,
) -> Vec<(String, i64, String, String)> {
    let mut statement = connection
        .prepare(
            "SELECT entity, entity_id, operation, payload_json FROM sync_mutations
             WHERE vehicle_id = ?1 ORDER BY revision",
        )
        .expect("statement");
    statement
        .query_map(params![vehicle_key], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .expect("rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect")
}

/// `(smallest revision, largest revision, count)` of the vehicle's `position`
/// sync mutations.
fn position_mutation_span(store: &HubStore, vehicle: &VehicleRecord) -> (i64, i64, i64) {
    store
        .open()
        .expect("connection")
        .query_row(
            "SELECT MIN(revision), MAX(revision), COUNT(*) FROM sync_mutations
             WHERE vehicle_id = ?1 AND entity = 'position'",
            params![vehicle.vehicle_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?.unwrap_or(0),
                    row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .expect("position mutation span")
}

/// The stored `drive_json` and the `ascent` and `descent` columns of a drive.
fn stored_drive(
    store: &HubStore,
    vehicle: &VehicleRecord,
    drive_id: i64,
) -> (serde_json::Value, Option<i64>, Option<i64>) {
    stored_drive_on(
        &store.open().expect("connection"),
        &vehicle.vehicle_id.to_string(),
        drive_id,
    )
}

/// [`stored_drive`] over a given connection or open transaction.
fn stored_drive_on(
    connection: &rusqlite::Connection,
    vehicle_key: &str,
    drive_id: i64,
) -> (serde_json::Value, Option<i64>, Option<i64>) {
    let (json, ascent, descent): (String, Option<i64>, Option<i64>) = connection
        .query_row(
            "SELECT drive_json, ascent, descent FROM materialised_drives
             WHERE vehicle_id = ?1 AND drive_id = ?2",
            params![vehicle_key, drive_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("drive row");
    (
        serde_json::from_str(&json).expect("drive json"),
        ascent,
        descent,
    )
}

fn totals_of(payload: &serde_json::Value) -> (Option<i64>, Option<i64>) {
    (payload["ascent"].as_i64(), payload["descent"].as_i64())
}

/// The rows and the mutation sequence of two stores are equal.
fn assert_same_rows(left: &InheritedStore, right: &InheritedStore, label: &str) {
    let (left_history, right_history) = (left.history(), right.history());
    assert_eq!(left_history.drives, right_history.drives, "{label}: drives");
    assert_eq!(
        placed_positions(&left_history),
        placed_positions(&right_history),
        "{label}: positions"
    );
    assert_eq!(
        mutation_rows(&left.store, &left.vehicle),
        mutation_rows(&right.store, &right.vehicle),
        "{label}: mutation sequence"
    );
}

#[test]
fn c1_st111_recompute_and_replay() {
    // (b) Stored positions 11 to 13 (100, 101, 104), next position id 14, new
    // positions 14 and 15 (110, 108). The closing row carries the inherited
    // walk (43, 7); the recompute over the five stored positions gives (10, 2).
    let run = InheritedStore::new(STORED_B, false);
    run.feed(&[st111_obs1(), st111_obs2()]);
    let history = run.history();
    assert_eq!(history.drives.len(), 1);
    assert_eq!(
        (history.drives[0].ascent, history.drives[0].descent),
        (Some(10), Some(2))
    );
    assert_eq!(
        placed_positions(&history),
        vec![
            (11, Some(1), T0 - 3 * MIN),
            (12, Some(1), T0 - 2 * MIN),
            (13, Some(1), T0 - MIN),
            (14, Some(1), T0),
            (15, Some(1), T0 + MIN),
        ]
    );
    let mutations = drive_mutations(&run.store, &run.vehicle, 1);
    assert_eq!(
        mutations.len(),
        2,
        "the closing upsert and the corrective one"
    );
    assert_eq!(totals_of(&mutations[0].1), (Some(43), Some(7)));
    assert_eq!(totals_of(&mutations[1].1), (Some(10), Some(2)));
    // The corrective upsert comes after every position mutation of the commit,
    // the closing one before them.
    let (first_position, last_position, positions) =
        position_mutation_span(&run.store, &run.vehicle);
    assert_eq!(positions, 5);
    assert!(
        mutations[0].0 < first_position,
        "the closing upsert is first"
    );
    assert!(
        last_position < mutations[1].0,
        "the corrective upsert follows the position mutations"
    );
    // The stored `drive_json` equals the closing row with only the two totals
    // replaced, and so does the corrective mutation's payload.
    let mut expected = mutations[0].1.clone();
    expected["ascent"] = json!(10);
    expected["descent"] = json!(2);
    assert_eq!(mutations[1].1, expected);
    let (stored, ascent, descent) = stored_drive(&run.store, &run.vehicle, 1);
    assert_eq!((ascent, descent), (Some(10), Some(2)));
    assert_eq!(stored, expected);

    // Replay after a failed commit (ST68's fault points): the failed commit
    // leaves nothing, and the retry gives the same rows and the same mutation
    // sequence as the uninterrupted run.
    for point in [StreamFaultPoint::LifecycleWrite, StreamFaultPoint::Commit] {
        let faulted = InheritedStore::new(STORED_B, false);
        faulted.feed(&[st111_obs1()]);
        let cursor = faulted
            .store
            .load_lifecycle_state(faulted.vehicle.vehicle_id)
            .expect("load")
            .expect("state")
            .last_observation_id;
        let before = mutation_rows(&faulted.store, &faulted.vehicle);
        faulted.store.inject_stream_fault(point);
        let closing = st111_obs2();
        assert!(
            faulted
                .store
                .accept_owner_observation_and_lifecycle(
                    &drive_store_input(&faulted.source, &faulted.vehicle, &closing),
                    closing.observed_at_ms,
                    1,
                )
                .is_err(),
            "fault {point:?} fails the commit"
        );
        assert!(faulted.history().drives.is_empty(), "fault {point:?}");
        assert_eq!(mutation_rows(&faulted.store, &faulted.vehicle), before);
        assert_eq!(
            faulted
                .store
                .load_lifecycle_state(faulted.vehicle.vehicle_id)
                .expect("load")
                .expect("state")
                .last_observation_id,
            cursor,
            "the failed commit moved nothing"
        );
        faulted.feed(&[closing]);
        assert_same_rows(&faulted, &run, &format!("replay after {point:?}"));
    }

    // (c) Stored elevations 72, 109 and 104: the shared walk over the five
    // positions gives (43, 7), the closing row's own totals, so the row keeps
    // them and the drive has exactly one sync mutation.
    let same = InheritedStore::new(STORED_C, false);
    same.feed(&[st111_obs1(), st111_obs2()]);
    let (_, ascent, descent) = stored_drive(&same.store, &same.vehicle, 1);
    assert_eq!((ascent, descent), (Some(43), Some(7)));
    let mutations = drive_mutations(&same.store, &same.vehicle, 1);
    assert_eq!(mutations.len(), 1, "an unchanged pair adds no mutation");
    assert_eq!(totals_of(&mutations[0].1), (Some(43), Some(7)));
}

#[test]
fn c1_st111_sorted_triples() {
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);
    let state = crate::lifecycle::OpenSessionState::new()
        .encode()
        .expect("state");
    // `(id, minutes after t0, elevation)`. Drive 2: ids and dates run together
    // but the rows are written in the order 24, 21, 25, 23, 22, which is the
    // order the drive index returns them in; by date the elevations are 100,
    // 104, 101, 110, 108, giving (13, 5), and in insertion order they would be
    // 110, 100, 108, 101, 104, giving (11, 17). Drive 3: in id order the
    // elevations are 100, 104, 101, 110, 108 (13, 5), but the dates are
    // t0+4m, t0, t0+3m, t0+1m, t0+2m, so by date they are 104, 110, 108, 101,
    // 100, giving (6, 9).
    let drive_2 = [
        (24, 3, 110),
        (21, 0, 100),
        (25, 4, 108),
        (23, 2, 101),
        (22, 1, 104),
    ];
    let drive_3 = [
        (31, 4, 100),
        (32, 0, 104),
        (33, 3, 101),
        (34, 1, 110),
        (35, 2, 108),
    ];
    let positions = |drive_id: i64, rows: &[(i64, i64, i64)]| {
        rows.iter()
            .map(|&(id, minutes, elevation)| {
                let mut position = position_row(id, Some(drive_id), T0 + minutes * MIN);
                position.elevation = Some(elevation);
                position
            })
            .collect::<Vec<_>>()
    };
    let mut written = positions(2, &drive_2);
    written.extend(positions(3, &drive_3));
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle.vehicle_id,
            car_id: 1,
            open_session_json: &state,
            last_observation_id: 1,
            quarantined: false,
            updated_at_ms: T0,
            delta: &crate::lifecycle::LifecycleDelta {
                drives: vec![drive_row(2, T0 + 10 * MIN), drive_row(3, T0 + 10 * MIN)],
                positions: written,
                ..Default::default()
            },
        })
        .expect("commit");
    let vehicle_key = vehicle.vehicle_id.to_string();
    let mut connection = store.open().expect("connection");
    for (drive_id, expected) in [(2, (Some(13), Some(5))), (3, (Some(6), Some(9)))] {
        let transaction = connection.transaction().expect("transaction");
        recompute_terrain_drive(&transaction, &vehicle_key, drive_id).expect("recompute");
        transaction.commit().expect("commit");
        let (_, ascent, descent) = stored_drive(&store, &vehicle, drive_id);
        assert_eq!((ascent, descent), expected, "drive {drive_id}");
    }
    // The shared function over the same series, in date order, agrees.
    assert_eq!(
        crate::lifecycle::elevation_totals([100, 104, 101, 110, 108].map(Some)),
        (Some(13), Some(5))
    );
    assert_eq!(
        crate::lifecycle::elevation_totals([104, 110, 108, 101, 100].map(Some)),
        (Some(6), Some(9))
    );
}

#[test]
fn c1_st111_decoder_refusals() {
    // Positions 11 to 15 of ST111 (b), dated t0-3m to t0+1m, elevations 100,
    // 101, 104, 110, 108: totals (10, 2). Position 14 is the one each case
    // damages.
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);
    let state = crate::lifecycle::OpenSessionState::new()
        .encode()
        .expect("state");
    let positions = [
        (11, -3, 100),
        (12, -2, 101),
        (13, -1, 104),
        (14, 0, 110),
        (15, 1, 108),
    ]
    .map(|(id, minutes, elevation): (i64, i64, i64)| {
        let mut position = position_row(id, Some(1), T0 + minutes * MIN);
        position.elevation = Some(elevation);
        position
    });
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle.vehicle_id,
            car_id: 1,
            open_session_json: &state,
            last_observation_id: 1,
            quarantined: false,
            updated_at_ms: T0,
            delta: &crate::lifecycle::LifecycleDelta {
                drives: vec![drive_row(1, T0 + 10 * MIN)],
                positions: positions.to_vec(),
                ..Default::default()
            },
        })
        .expect("commit");
    let vehicle_key = vehicle.vehicle_id.to_string();
    let mut connection = store.open().expect("connection");
    let original: String = connection
        .query_row(
            "SELECT position_json FROM materialised_positions
             WHERE vehicle_id = ?1 AND position_id = 14",
            params![vehicle_key],
            |row| row.get(0),
        )
        .expect("position 14");
    let damage =
        |connection: &rusqlite::Connection, key: &str, value: Option<serde_json::Value>| {
            let mut row: serde_json::Value = serde_json::from_str(&original).expect("row json");
            let object = row.as_object_mut().expect("row object");
            match value {
                Some(value) => {
                    object.insert(key.to_owned(), value);
                }
                None => {
                    object.remove(key);
                }
            }
            connection
                .execute(
                    "UPDATE materialised_positions SET position_json = ?2
                 WHERE vehicle_id = ?1 AND position_id = 14",
                    params![vehicle_key, serde_json::to_string(&row).expect("row text")],
                )
                .expect("damage the row");
        };
    let above_i64: serde_json::Value =
        serde_json::from_str("9223372036854775808").expect("a number above i64");
    let drive_before = stored_drive(&store, &vehicle, 1);
    let mutations_before = mutation_rows(&store, &vehicle);
    // Write observation: any insert, update or delete of a drive row or of a
    // sync mutation counts, even one that stores the same values again. A
    // refusal that wrote first and was rolled back would still be counted.
    connection
        .execute_batch(
            "CREATE TABLE refusal_writes(n INTEGER NOT NULL);
             INSERT INTO refusal_writes VALUES (0);
             CREATE TRIGGER refusal_drive_insert AFTER INSERT ON materialised_drives
               BEGIN UPDATE refusal_writes SET n = n + 1; END;
             CREATE TRIGGER refusal_drive_update AFTER UPDATE ON materialised_drives
               BEGIN UPDATE refusal_writes SET n = n + 1; END;
             CREATE TRIGGER refusal_drive_delete AFTER DELETE ON materialised_drives
               BEGIN UPDATE refusal_writes SET n = n + 1; END;
             CREATE TRIGGER refusal_mutation_insert AFTER INSERT ON sync_mutations
               BEGIN UPDATE refusal_writes SET n = n + 1; END;",
        )
        .expect("write counters");
    let refusals: [(&str, &str, Option<serde_json::Value>); 8] = [
        ("date_ms omitted", "date_ms", None),
        ("date_ms string", "date_ms", Some(json!("soon"))),
        ("date_ms fraction", "date_ms", Some(json!(1.5))),
        ("date_ms above i64", "date_ms", Some(above_i64.clone())),
        ("elevation string", "elevation", Some(json!("high"))),
        ("elevation fraction", "elevation", Some(json!(1.5))),
        ("elevation above i64", "elevation", Some(above_i64)),
        ("latitude string", "latitude", Some(json!("north"))),
    ];
    for (label, key, value) in refusals {
        damage(&connection, key, value);
        let transaction = connection.transaction().expect("transaction");
        let result = recompute_terrain_drive(&transaction, &vehicle_key, 1);
        assert!(
            matches!(result, Err(StoreError::DeserializeLifecycleRow(_))),
            "{label}: {result:?}"
        );
        // Read through the same transaction, before it is dropped: a refusal
        // that wrote and then returned the error would show here, where a
        // rollback has not yet hidden it.
        let writes: i64 = transaction
            .query_row("SELECT n FROM refusal_writes", [], |row| row.get(0))
            .expect("write count");
        assert_eq!(writes, 0, "{label}: the refusal comes before any write");
        assert_eq!(
            stored_drive_on(&transaction, &vehicle_key, 1),
            drive_before,
            "{label}: stored drive, in the transaction"
        );
        assert_eq!(
            mutation_rows_on(&transaction, &vehicle_key),
            mutations_before,
            "{label}: sync mutations, in the transaction"
        );
        drop(transaction);
        assert_eq!(stored_drive(&store, &vehicle, 1), drive_before, "{label}");
        assert_eq!(mutation_rows(&store, &vehicle), mutations_before, "{label}");
    }
    // Controls: an omitted `elevation` and a JSON null both read as None, and
    // the totals are the shared function's over the same date-ordered series
    // with that reading absent (100, 101, 104, none, 108): (8, 0).
    let expected =
        crate::lifecycle::elevation_totals([Some(100), Some(101), Some(104), None, Some(108)]);
    assert_eq!(expected, (Some(8), Some(0)));
    for (label, value) in [
        ("elevation omitted", None),
        ("elevation null", Some(serde_json::Value::Null)),
    ] {
        damage(&connection, "elevation", value);
        let transaction = connection.transaction().expect("transaction");
        recompute_terrain_drive(&transaction, &vehicle_key, 1)
            .unwrap_or_else(|error| panic!("{label}: {error:?}"));
        transaction.commit().expect("commit");
        let (_, ascent, descent) = stored_drive(&store, &vehicle, 1);
        assert_eq!((ascent, descent), expected, "{label}");
    }
}

/// Whether `EXPLAIN QUERY PLAN` of `sql` shows one line for
/// `materialised_positions`, a `SEARCH` using `materialised_positions_drive`
/// with both `vehicle_id=?` and `drive_id=?`, and no `TEMP B-TREE` line.
fn streams_by_the_drive_index(connection: &rusqlite::Connection, sql: &str) -> bool {
    let mut statement = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("query plan");
    let plan: Vec<String> = statement
        .query_map(params![Uuid::new_v4().to_string(), 1_i64], |row| {
            row.get::<_, String>(3)
        })
        .expect("plan rows")
        .collect::<Result<_, _>>()
        .expect("plan details");
    let on_table: Vec<&String> = plan
        .iter()
        .filter(|line| line.contains("materialised_positions"))
        .collect();
    on_table.len() == 1
        && on_table[0].contains("SEARCH")
        && on_table[0].contains("materialised_positions_drive")
        && on_table[0].contains("vehicle_id=?")
        && on_table[0].contains("drive_id=?")
        && !plan.iter().any(|line| line.contains("TEMP B-TREE"))
}

#[test]
fn c1_st111_query_plan() {
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let connection = store.open().expect("connection");
    // The exact production statement: it selects the primary key and the JSON
    // of the drive's rows, has no ORDER BY, and is searched by the drive index
    // on both keys, so SQLite never sorts (and so never buffers) the JSON.
    assert!(TERRAIN_DRIVE_POSITIONS_SQL.starts_with("SELECT position_id, position_json"));
    assert!(!TERRAIN_DRIVE_POSITIONS_SQL.contains("ORDER BY"));
    assert!(
        streams_by_the_drive_index(&connection, TERRAIN_DRIVE_POSITIONS_SQL),
        "the read must be one SEARCH of the drive index with no TEMP B-TREE"
    );
    // The check itself is sound: each ordering the mutants add fails it.
    for order in [
        "ORDER BY json_extract(position_json, '$.date_ms'), position_id",
        "ORDER BY position_id",
    ] {
        assert!(
            !streams_by_the_drive_index(
                &connection,
                &format!("{TERRAIN_DRIVE_POSITIONS_SQL} {order}")
            ),
            "{order} must fail the plan check"
        );
    }
}

#[test]
fn c1_st111_reencoded_and_seeded_stored() {
    // (g) at storage level: ST111 (b)'s rows with the re-encoded blob give the
    // same row 1 (10, 2) after the recompute and the same mutation sequence.
    let plain = InheritedStore::new(STORED_B, false);
    plain.feed(&[st111_obs1(), st111_obs2()]);
    let reencoded = InheritedStore::new(STORED_B, true);
    reencoded.feed(&[st111_obs1(), st111_obs2()]);
    let (_, ascent, descent) = stored_drive(&reencoded.store, &reencoded.vehicle, 1);
    assert_eq!((ascent, descent), (Some(10), Some(2)));
    let mutations = drive_mutations(&reencoded.store, &reencoded.vehicle, 1);
    assert_eq!(mutations.len(), 2);
    assert_eq!(totals_of(&mutations[0].1), (Some(43), Some(7)));
    assert_same_rows(&reencoded, &plain, "re-encoded blob");

    // (i) A seeded imported open drive with no imported positions and no
    // imported totals closes through the live observations. It is flagged
    // without need: the live walk (100, 110) from totals of 0 and 0 gives
    // (10, 0), and the recompute over its two stored positions gives the same
    // pair, so the close writes no corrective mutation.
    let session = seeded_session_with_totals(false, None, None);
    let (_directory, store, vehicle) = close_seeded(&session);
    let (_, ascent, descent) = stored_drive(&store, &vehicle, 70);
    assert_eq!((ascent, descent), (Some(10), Some(0)));
    let mutations = drive_mutations(&store, &vehicle, 70);
    assert_eq!(mutations.len(), 1, "equal totals add no mutation");
    assert_eq!(totals_of(&mutations[0].1), (Some(10), Some(0)));
}

#[test]
fn c1_st111_seeded_nonzero_totals_are_corrected() {
    // (i) The other side of the equality: the imported drive carries ascent 7
    // and descent 2, which the seed keeps, so the live walk (100, 110) closes
    // the row at (17, 2). The recompute over the two stored positions gives
    // (10, 0), the pair changes, and the commit records the closing upsert and
    // then one corrective upsert.
    let session = seeded_session(false);
    let (_directory, store, vehicle) = close_seeded(&session);
    let (_, ascent, descent) = stored_drive(&store, &vehicle, 70);
    assert_eq!((ascent, descent), (Some(10), Some(0)));
    let mutations = drive_mutations(&store, &vehicle, 70);
    assert_eq!(
        mutations.len(),
        2,
        "the closing upsert and the corrective one"
    );
    assert_eq!(totals_of(&mutations[0].1), (Some(17), Some(2)));
    assert_eq!(totals_of(&mutations[1].1), (Some(10), Some(0)));
}

/// Seeds `session` as the vehicle's imported open drive 70 and feeds a Drive
/// observation (gear D, 30 mph) at SEEDED_START + 3m, elevation 100, and a Park
/// one (gear and speed JSON null) at + 6m, elevation 110, at (47.5 + 0.001 id,
/// 19.0): the second closes the drive.
fn close_seeded(
    session: &crate::teslamate_projection::TeslaMateOpenSession,
) -> (tempfile::TempDir, HubStore, VehicleRecord) {
    let directory = crate::private_tempdir().expect("tempdir");
    let store = HubStore::initialize(directory.path()).expect("store");
    let (source, vehicle) = test_registered_vehicle(&store);
    store
        .seed_imported_open_session(
            source.source_id,
            vehicle.vehicle_id,
            1,
            session,
            SEEDED_START,
        )
        .expect("seed");
    let live = |id: i64, minutes: i64, driving: bool, elevation: i64| {
        let t = SEEDED_START + minutes * MIN;
        let (gear, speed) = if driving {
            (json!("D"), json!(30))
        } else {
            (serde_json::Value::Null, serde_json::Value::Null)
        };
        vd(
            id,
            t,
            json!({"drive_state": {"timestamp": t, "shift_state": gear, "speed": speed,
                                   "latitude": 47.5 + 0.001 * id as f64, "longitude": 19.0,
                                   "elevation": elevation}}),
        )
    };
    feed_owner_samples(
        &store,
        &source,
        &vehicle,
        &[live(1, 3, true, 100), live(2, 6, false, 110)],
    );
    let history = store
        .materialised_history(vehicle.vehicle_id)
        .expect("history");
    assert_eq!(history.drives.len(), 1);
    assert_eq!(history.drives[0].id, 70);
    (directory, store, vehicle)
}

#[test]
fn c1_st111_discarded_inherited_drive_stored() {
    // (d) at storage level: the blob's last odometer is 1000.0 mi and obs 1 and
    // 2 carry 1000.0, so the drive is discarded (R47). There is no
    // `materialised_drives` row 1, positions 11 to 15 are standalone (R48), and
    // R93 records no drive mutation: the recompute list is empty.
    let run = InheritedStore::with_last_odometer(STORED_B, false, 1000.0);
    run.feed(&[st111_discard_obs1(), st111_discard_obs2()]);
    let history = run.history();
    assert!(history.drives.is_empty(), "no drive row is materialised");
    assert_eq!(
        placed_positions(&history),
        vec![
            (11, None, T0 - 3 * MIN),
            (12, None, T0 - 2 * MIN),
            (13, None, T0 - MIN),
            (14, None, T0),
            (15, None, T0 + MIN),
        ],
        "positions 11 to 15 are standalone"
    );
    let drive_rows: i64 = run
        .store
        .open()
        .expect("connection")
        .query_row(
            "SELECT COUNT(*) FROM materialised_drives WHERE vehicle_id = ?1 AND drive_id = 1",
            params![run.vehicle.vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("drive row count");
    assert_eq!(drive_rows, 0, "no materialised_drives row 1");
    assert!(
        drive_mutations(&run.store, &run.vehicle, 1).is_empty(),
        "R93 records no drive mutation"
    );
    assert!(
        mutation_rows(&run.store, &run.vehicle)
            .iter()
            .all(|(entity, _, _, _)| entity != "drive"),
        "no drive mutation of any id"
    );
}

#[test]
fn c1_st111_recompute_precedes_charge_close() {
    // The recompute follows every position mutation of the commit and comes
    // before whatever the commit writes next: the charge close. The drive row is
    // written with no totals and its two stored positions (100, 110) give
    // (10, 0), so the pair changes and one corrective upsert is recorded.
    let harness = CostHarness::new();
    let mut delta = crate::lifecycle::LifecycleDelta::default();
    delta.drives.push(drive_row(1, 300));
    for (id, date_ms, elevation) in [(1, 100, 100), (2, 200, 110)] {
        let mut position = position_row(id, Some(1), date_ms);
        position.elevation = Some(elevation);
        delta.positions.push(position);
    }
    delta.recompute_drive_ids = vec![1];
    delta.charges.push(cost_row(
        1,
        None,
        Some(9.0),
        Some(true),
        Some("Supercharger"),
    ));
    harness.commit_delta(&delta);
    let sequence = mutation_rows(&harness.store, &harness.vehicle)
        .into_iter()
        .filter(|(entity, _, _, _)| matches!(entity.as_str(), "drive" | "position" | "charge"))
        .map(|(entity, id, _, _)| (entity, id))
        .collect::<Vec<_>>();
    assert_eq!(
        sequence,
        vec![
            ("drive".to_owned(), 1),
            ("position".to_owned(), 1),
            ("position".to_owned(), 2),
            ("drive".to_owned(), 1),
            ("charge".to_owned(), 1),
        ]
    );
    let (_, ascent, descent) = stored_drive(&harness.store, &harness.vehicle, 1);
    assert_eq!((ascent, descent), (Some(10), Some(0)));
    // A persisted but unwritten drive must keep its deliberately stale totals.
    let untouched = CostHarness::new();
    let mut seed = crate::lifecycle::LifecycleDelta::default();
    let mut stale = drive_row(99, 300);
    stale.ascent = Some(77);
    stale.descent = Some(88);
    seed.drives.push(stale);
    for (id, date_ms, elevation) in [(1, 100, 100), (2, 200, 110)] {
        let mut position = position_row(id, Some(99), date_ms);
        position.elevation = Some(elevation);
        seed.positions.push(position);
    }
    untouched.commit_delta(&seed);
    let before = stored_drive(&untouched.store, &untouched.vehicle, 99);
    assert_eq!((before.1, before.2), (Some(77), Some(88)));
    let raw_json = || -> String {
        untouched.store.open().expect("fixture connection").query_row(
            "SELECT drive_json FROM materialised_drives WHERE vehicle_id = ?1 AND drive_id = 99",
            params![untouched.vehicle.vehicle_id.to_string()],
            |row| row.get(0),
        ).expect("fixture drive JSON")
    };
    let bytes_before = raw_json();
    let mutations_before = drive_mutations(&untouched.store, &untouched.vehicle, 99);
    assert!(
        !mutations_before.is_empty(),
        "assertion failed: c1_st111_recompute_precedes_charge_close: stored fixture has a drive mutation"
    );
    let other = crate::lifecycle::LifecycleDelta {
        recompute_drive_ids: vec![99],
        ..Default::default()
    };
    harness.commit_delta(&other);
    assert_eq!(
        drive_mutations(&harness.store, &harness.vehicle, 99).len(),
        0,
        "c1_st111_recompute_precedes_charge_close: an absent drive creates no mutation"
    );
    untouched.commit_delta(&other);
    assert!(
        stored_drive(&untouched.store, &untouched.vehicle, 99) == before
            && raw_json() == bytes_before
            && drive_mutations(&untouched.store, &untouched.vehicle, 99) == mutations_before,
        "assertion failed: c1_st111_recompute_precedes_charge_close: an unwritten persisted drive is not recomputed"
    );
}
