// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use serde_json::json;

const T0: i64 = 1_760_000_000_000;
const T1: i64 = T0 + 60_000_000;
const T2: i64 = T1 + 10_000;
const T3: i64 = T2 + 10_000;

fn fixture() -> (tempfile::TempDir, HubStore, Uuid, Uuid) {
    let (dir, store) = b3_c8_sa::c8_store();
    let vehicle = b3_c8_sa::c8_vehicle(&store, "terrain-tidy");
    let source = store
        .register_source(
            &SourceDescriptor::new(IMPORT_SOURCE_KIND, "terrain-import"),
            T0,
        )
        .unwrap()
        .source_id;
    (dir, store, vehicle, source)
}

fn position(id: i64, drive: Option<i64>, elevation: Option<i64>) -> ProjectionPosition {
    let mut p = position_row(id, drive, T0 + id * 1_000);
    p.latitude = 10.5;
    p.longitude = 20.0 + id as f64 / 1200.0;
    p.elevation = elevation;
    p
}

fn commit_rows(store: &HubStore, vehicle: Uuid, rows: &[(i64, i64, Option<i64>)]) {
    // C9 ST67: use the retained lifecycle fixture path, as Ca and Cb do.
    let mut ids = Vec::new();
    for &(_, d, _) in rows {
        if !ids.contains(&d) {
            ids.push(d);
        }
    }
    let delta = crate::lifecycle::LifecycleDelta {
        drives: ids
            .into_iter()
            .map(|id| {
                let mut d = drive_row(id, T1);
                d.start_date_ms = T0 - 1;
                d
            })
            .collect(),
        positions: rows
            .iter()
            .map(|&(p, d, e)| position(p, Some(d), e))
            .collect(),
        ..Default::default()
    };
    let state = crate::lifecycle::OpenSessionState::new().encode().unwrap();
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle,
            car_id: 1,
            open_session_json: &state,
            last_observation_id: 0,
            quarantined: false,
            updated_at_ms: T1,
            delta: &delta,
        })
        .unwrap();
    clear_log(store);
}

fn clear_log(store: &HubStore) {
    store
        .open()
        .unwrap()
        .execute_batch("DELETE FROM sync_mutations; DELETE FROM export_outbox;")
        .unwrap();
}

fn watermark(tx: &Transaction<'_>, source: Uuid, vehicle: Uuid, id: i64, date: i64) {
    assert_eq!(
        raise_source_watermarks_in_transaction(
            tx,
            source,
            vehicle,
            &[(
                WatermarkDomain::Positions,
                WatermarkPair {
                    max_source_row_id: Some(id),
                    max_timestamp_ms: Some(date),
                }
            ),]
        )
        .unwrap(),
        [WatermarkDomain::Positions]
    );
}

fn raw_watermark(store: &HubStore, source: Uuid, vehicle: Uuid, id: i64, date: i64) {
    let mut connection = store.open().unwrap();
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    watermark(&tx, source, vehicle, id, date);
    tx.commit().unwrap();
}

fn import(
    store: &HubStore,
    source: Uuid,
    vehicle: Uuid,
    id: i64,
    date: i64,
    now: i64,
    commit: bool,
) -> TerrainBoundaryRaise {
    let mut connection = store.open().unwrap();
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    watermark(&tx, source, vehicle, id, date);
    let mut state = crate::lifecycle::OpenSessionState::new();
    raise_id_cursors_above_watermarks(&tx, vehicle, &mut state).unwrap();
    let result = terrain_after_import_boundary_raise_in_transaction(&tx, vehicle, now).unwrap();
    if commit {
        tx.commit().unwrap();
    } else {
        tx.rollback().unwrap();
    }
    result
}

fn plant(
    store: &HubStore,
    vehicle: Uuid,
    id: i64,
    drive: i64,
    value: Option<i64>,
    status: &str,
    coordinate_id: i64,
) {
    let p = position(coordinate_id, Some(drive), None);
    store.open().unwrap().execute(
        "INSERT INTO terrain_elevation_provenance(vehicle_id, position_id, drive_id,
         latitude, longitude, elevation_m, tile_name, tile_hash, dataset_source, dataset_version,
         status, attempts, attempted_at_ms, retry_after_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'N10E020', 'synthetic-digest', 'aws', 'synthetic-v1', ?7, 1, ?8, 0)",
        params![vehicle.to_string(),id,drive,p.latitude,p.longitude,value,status,T0],
    ).unwrap();
}

fn json_position(store: &HubStore, vehicle: Uuid, id: i64) -> String {
    store.open().unwrap().query_row(
        "SELECT position_json FROM materialised_positions WHERE vehicle_id = ?1 AND position_id = ?2",
        params![vehicle.to_string(),id], |r| r.get(0),
    ).unwrap()
}
fn elevation(store: &HubStore, vehicle: Uuid, id: i64) -> Option<i64> {
    serde_json::from_str::<ProjectionPosition>(&json_position(store, vehicle, id))
        .unwrap()
        .elevation
}
fn pair(store: &HubStore, vehicle: Uuid, id: i64) -> (Option<i64>, Option<i64>) {
    store.open().unwrap().query_row(
        "SELECT ascent, descent FROM materialised_drives WHERE vehicle_id = ?1 AND drive_id = ?2",
        params![vehicle.to_string(),id], |r| Ok((r.get(0)?,r.get(1)?)),
    ).unwrap()
}
fn provenance(store: &HubStore, vehicle: Uuid, id: i64) -> Option<Value> {
    store
        .open()
        .unwrap()
        .query_row(
            "SELECT json_object('drive',drive_id,'latitude',latitude,'longitude',longitude,
         'elevation',elevation_m,'status',status,'attempts',attempts,'at',attempted_at_ms,
         'retry',retry_after_ms,'error',error_code,'tile',tile_name,'hash',tile_hash,
         'source',dataset_source,'version',dataset_version)
         FROM terrain_elevation_provenance WHERE vehicle_id = ?1 AND position_id = ?2",
            params![vehicle.to_string(), id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .unwrap()
        .map(|j| serde_json::from_str(&j).unwrap())
}
fn data(store: &HubStore, tables: &[&str]) -> Vec<Vec<Vec<String>>> {
    // C9 ST68: compare every column of every row, including opaque future JSON members.
    let connection = store.open().unwrap();
    tables
        .iter()
        .map(|table| {
            let mut st = connection
                .prepare(&format!("SELECT * FROM {table} ORDER BY 1, 2"))
                .unwrap();
            let n = st.column_count();
            st.query_map([], |r| {
                (0..n).map(|i| Ok(format!("{:?}", r.get_ref(i)?))).collect()
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
        })
        .collect()
}

fn unrelated_rows(
    store: &HubStore,
    table: &str,
    json_column: &str,
    changed: &[&str],
) -> Vec<Vec<String>> {
    // C9 R38, C9 R55: compare every column, masking only the authorised fields.
    let connection = store.open().unwrap();
    let mut statement = connection
        .prepare(&format!("SELECT * FROM {table} ORDER BY 1, 2"))
        .unwrap();
    let columns: Vec<_> = statement
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect();
    statement
        .query_map([], |row| {
            columns
                .iter()
                .enumerate()
                .map(|(i, column)| {
                    if column == json_column {
                        let mut value: Value =
                            serde_json::from_str(&row.get::<_, String>(i)?).unwrap();
                        for key in changed {
                            value.as_object_mut().unwrap().remove(*key);
                        }
                        Ok(value.to_string())
                    } else if changed.contains(&column.as_str()) {
                        Ok(String::new())
                    } else {
                        Ok(format!("{:?}", row.get_ref(i)?))
                    }
                })
                .collect()
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}
const DATA: &[&str] = &[
    "materialised_positions",
    "materialised_drives",
    "terrain_elevation_provenance",
    "sync_mutations",
    "sync_mutation_sequences",
    "export_outbox",
];
fn row_state(store: &HubStore) -> Vec<Vec<Vec<String>>> {
    let mut state = data(
        store,
        &[
            "materialised_positions",
            "materialised_drives",
            "terrain_elevation_provenance",
        ],
    );
    for table in &mut state {
        for row in table {
            row[0] = "vehicle".into();
        }
    }
    state
}

fn durable(store: &HubStore) -> Vec<Vec<Vec<String>>> {
    data(
        store,
        &[
            "materialised_positions",
            "materialised_drives",
            "terrain_elevation_provenance",
            "sync_mutations",
            "sync_mutation_sequences",
            "export_outbox",
            "terrain_tidy_state",
            "terrain_enrichment_state",
            "lifecycle_source_watermarks",
        ],
    )
}
fn version(connection: &Connection) -> i64 {
    connection
        .query_row("PRAGMA data_version", [], |r| r.get(0))
        .unwrap()
}
fn marker(store: &HubStore) -> (String, i64) {
    store
        .open()
        .unwrap()
        .query_row(
            "SELECT resume_vehicle_id,resume_position_id FROM terrain_tidy_state
        WHERE step = 'earlier_build_values_v1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}
fn counts(p: &TerrainTidyProgress) -> [i64; 6] {
    [
        p.examined,
        p.cleared,
        p.requeued,
        p.removed,
        p.kept,
        p.drives_changed,
    ]
}
fn finish(store: &HubStore, rows: u32, now: i64) {
    for _ in 0..100 {
        if store.apply_terrain_tidy_chunk(rows, now).unwrap().complete {
            return;
        }
    }
    panic!("C9 R56: bounded synthetic tidy did not complete");
}
fn reset(store: &HubStore, run: i64, armed: Option<i64>) {
    store
        .open()
        .unwrap()
        .execute(
            "UPDATE terrain_tidy_state SET completed_at_ms = NULL,
        resume_vehicle_id = '', resume_position_id = 0, examined = 0, cleared = 0,
        requeued = 0, removed = 0, kept = 0, drives_changed = 0, run = ?1, armed_at_ms = ?2",
            params![run, armed],
        )
        .unwrap();
}
fn mutation_ids(store: &HubStore, entity: &str) -> Vec<i64> {
    store
        .open()
        .unwrap()
        .prepare("SELECT entity_id FROM sync_mutations WHERE entity = ?1 ORDER BY revision")
        .unwrap()
        .query_map([entity], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}
fn st67() -> (tempfile::TempDir, HubStore, Uuid) {
    let (dir, store, v, source) = fixture();
    commit_rows(
        &store,
        v,
        &[
            (1, 1, Some(100)),
            (2, 1, None),
            (5, 1, None),
            (11, 1, Some(110)),
            (12, 1, Some(120)),
            (3, 2, Some(50)),
            (13, 2, Some(60)),
            (14, 2, Some(70)),
            (15, 3, Some(200)),
            (16, 3, Some(210)),
            (17, 3, None),
            (18, 4, Some(300)),
            (19, 4, Some(310)),
            (20, 4, None),
            (21, 5, Some(401)),
            (22, 5, Some(410)),
        ],
    );
    // C9 ST67: preserve unknown JSON and unrelated physical columns through tidy writes.
    store
        .open()
        .unwrap()
        .execute_batch(
            "UPDATE materialised_positions SET speed = 123,
        position_json = json_set(position_json, '$.future', json('{\"keep\":[1,null]}'));
        UPDATE materialised_drives SET ascent = 77, descent = 77,
        drive_json = json_set(drive_json, '$.ascent', 77, '$.descent', 77,
            '$.future', json('{\"keep\":[1,null]}'));",
        )
        .unwrap();
    raw_watermark(&store, source, v, 10, T0 + 10_000);
    for (p, d, e) in [
        (1, 1, 100),
        (11, 1, 110),
        (12, 1, 120),
        (3, 2, 55),
        (13, 2, 60),
        (14, 2, 70),
        (16, 3, 210),
        (17, 3, 215),
        (18, 4, 300),
        (19, 4, 310),
        (20, 4, 320),
        (21, 5, 400),
        (22, 5, 410),
        (23, 5, 430),
    ] {
        plant(
            &store,
            v,
            p,
            d,
            Some(e),
            "success",
            if p == 19 { 40 } else { p },
        );
    }
    plant(&store, v, 5, 1, None, "void", 5);
    // C9 R58: establish the boundary before the case-counting run.
    assert_eq!(
        store
            .record_terrain_import_boundary(
                v,
                ImportBoundary {
                    max_position_id: 10,
                    max_date_ms: T0 + 10_000
                },
                T0
            )
            .unwrap(),
        TerrainBoundaryRaise::RaisedAndRearmed
    );
    (dir, store, v)
}
fn assert_st67(store: &HubStore, v: Uuid) {
    for p in [1, 13, 14, 16, 17, 19, 20, 22] {
        assert_eq!(elevation(store, v, p), None, "position {p}");
    }
    for (p, e) in [
        (3, 50),
        (11, 110),
        (12, 120),
        (15, 200),
        (18, 300),
        (21, 401),
    ] {
        assert_eq!(elevation(store, v, p), Some(e));
    }
    for p in [1, 3, 13, 14, 16, 17, 21, 22, 23] {
        assert!(provenance(store, v, p).is_none());
    }
    for p in [19, 20] {
        let position = position(p, Some(4), None);
        assert_eq!(
            provenance(store, v, p),
            Some(json!({"drive":4,"latitude":position.latitude,
            "longitude":position.longitude,"elevation":null,"status":"failed","attempts":1,
            "at":T0,"retry":T1,"error":"position_rewritten","tile":null,"hash":null,
            "source":null,"version":null}))
        );
    }
    assert_eq!(pair(store, v, 1), (Some(10), Some(0)));
    for d in 2..=5 {
        assert_eq!(pair(store, v, d), (None, None));
    }
    assert_eq!(mutation_ids(store, "position"), [1, 13, 14, 16, 19, 22]);
    let connection = store.open().unwrap();
    let speed: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM materialised_positions WHERE speed != 123",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(speed, 0);
    for p in [1, 13, 14, 16, 19, 22] {
        let current: Value = serde_json::from_str(&json_position(store, v, p)).unwrap();
        assert_eq!(current["future"], json!({"keep":[1,null]}));
        let payload: String = connection
            .query_row(
                "SELECT payload_json FROM sync_mutations WHERE entity='position' AND entity_id=?1",
                [p],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&payload).unwrap(), current);
    }
    assert!(b3_c8_sb::export_revision(store, v) > 0);
}
fn filled_outcome(store: &HubStore, v: Uuid, p: i64, elevation_m: i16) -> TerrainOutcome {
    TerrainOutcome {
        candidate: TerrainCandidate {
            vehicle_id: v,
            position: serde_json::from_str(&json_position(store, v, p)).unwrap(),
        },
        forward: provenance(store, v, p).is_none(),
        kind: TerrainOutcomeKind::Filled {
            elevation_m,
            tile: TileProvenance {
                tile_name: "N10E020".into(),
                tile_hash: "synthetic-digest".into(),
                dataset_source: "aws".into(),
                dataset_version: "synthetic-v1".into(),
            },
        },
    }
}
fn fill(store: &HubStore, v: Uuid, values: &[(i64, i16)], now: i64) -> TerrainBatchApplied {
    let outcomes: Vec<_> = values
        .iter()
        .map(|&(p, e)| filled_outcome(store, v, p, e))
        .collect();
    store
        .apply_terrain_batch(&TerrainBatch {
            vehicle_id: v,
            outcomes: &outcomes,
            retire: &[],
            cursor_to: None,
            committed_at_ms: now,
        })
        .unwrap()
}

#[test]
fn c9_st67_cases() {
    let (_dir, store, v) = st67();
    let positions = unrelated_rows(
        &store,
        "materialised_positions",
        "position_json",
        &["elevation"],
    );
    let drives = unrelated_rows(
        &store,
        "materialised_drives",
        "drive_json",
        &["ascent", "descent"],
    );
    let before: Vec<_> = [2, 3, 5, 15, 21]
        .map(|p| (p, json_position(&store, v, p)))
        .into();
    let void = provenance(&store, v, 5);
    C9_RECOMPUTE_CALLS.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
    assert_eq!(
        store.apply_terrain_tidy_chunk(250, T1).unwrap(),
        TerrainTidyApplied {
            examined: 15,
            cleared: 5,
            requeued: 2,
            removed: 4,
            kept: 3,
            drives_changed: 5,
            complete: true,
            ..Default::default()
        }
    );
    let calls = C9_RECOMPUTE_CALLS.with(|slot| slot.borrow_mut().take().unwrap());
    assert_eq!(calls, [(1, 6), (2, 6), (3, 6), (4, 6), (5, 6)]);
    assert_st67(&store, v);
    assert_eq!(
        unrelated_rows(
            &store,
            "materialised_positions",
            "position_json",
            &["elevation"]
        ),
        positions
    );
    assert_eq!(
        unrelated_rows(
            &store,
            "materialised_drives",
            "drive_json",
            &["ascent", "descent"]
        ),
        drives
    );
    assert_eq!(
        counts(&store.terrain_tidy_progress().unwrap()),
        [15, 5, 2, 4, 3, 5]
    );
    assert_eq!(mutation_ids(&store, "drive"), [1, 2, 3, 4, 5]);
    assert_eq!(provenance(&store, v, 5), void);
    for (p, bytes) in before {
        assert_eq!(json_position(&store, v, p), bytes);
    }
    let due = store.terrain_due_retries(T1, 250).unwrap();
    assert_eq!(
        due.iter().map(|r| r.position_id).collect::<Vec<_>>(),
        [19, 20]
    );
    assert!(due.iter().all(|r| r.candidate.is_some()));
    // C9 ST67: storage refill composes the retained sampler with synthetic tile nodes.
    let mut bytes = vec![0; 1201 * 1201 * 2];
    for (col, e) in [(19, 315_i16), (20, 305_i16)] {
        let at = (600 * 1201 + col) * 2;
        bytes[at..at + 2].copy_from_slice(&e.to_be_bytes());
    }
    let tile = crate::terrain::HgtTile::from_bytes("N10E020", bytes).unwrap();
    for (p, e) in [(19, 315), (20, 305)] {
        let row = position(p, Some(4), None);
        assert_eq!(
            crate::collector::sample_elevation(&tile, row.latitude, row.longitude).unwrap(),
            crate::collector::SampleOutcome::Elevation(e)
        );
    }
    assert_eq!(fill(&store, v, &[(19, 315), (20, 305)], T1).filled, 2);
    assert_eq!(pair(&store, v, 4), (Some(15), Some(10)));
    for p in [19, 20] {
        assert_eq!(provenance(&store, v, p).unwrap()["attempts"], 2);
    }
    for p in [1, 13, 14, 16, 17, 22] {
        assert_eq!(elevation(&store, v, p), None);
        assert!(provenance(&store, v, p).is_none());
    }
    let after = data(&store, DATA);
    assert_eq!(fill(&store, v, &[(19, 315), (20, 305)], T2).filled, 0);
    assert_eq!(data(&store, DATA), after);

    // C9 R55 T1: an orphan's drive has no other acted row and keeps its planted pair.
    let (_dir, store, v, _source) = fixture();
    commit_rows(&store, v, &[(1, 9, None)]);
    store
        .open()
        .unwrap()
        .execute(
            "UPDATE materialised_drives SET ascent=55, descent=40,
         drive_json=json_set(drive_json,'$.ascent',55,'$.descent',40)
         WHERE vehicle_id=?1 AND drive_id=9",
            [v.to_string()],
        )
        .unwrap();
    plant(&store, v, 2, 9, Some(100), "success", 2);
    let before = data(&store, &["materialised_drives", "materialised_positions"]);
    C9_RECOMPUTE_CALLS.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
    let applied = store.apply_terrain_tidy_chunk(250, T1).unwrap();
    let calls = C9_RECOMPUTE_CALLS.with(|slot| slot.borrow_mut().take().unwrap());
    assert!(calls.is_empty(), "C9 R55 T1: no drive recompute: {calls:?}");
    assert_eq!(pair(&store, v, 9), (Some(55), Some(40)));
    assert_eq!(
        applied,
        TerrainTidyApplied {
            examined: 1,
            removed: 1,
            complete: true,
            ..Default::default()
        }
    );
    assert!(provenance(&store, v, 2).is_none());
    assert_eq!(
        data(&store, &["materialised_drives", "materialised_positions"]),
        before
    );
    assert!(mutation_ids(&store, "position").is_empty());
    assert!(mutation_ids(&store, "drive").is_empty());
}

#[test]
fn c9_st68_restart_fixed_point() {
    let (dir, store, v) = st67();
    let (_reference_dir, reference, rv) = st67();
    finish(&reference, 250, T1);
    for _ in 0..2 {
        assert_eq!(store.apply_terrain_tidy_chunk(2, T1).unwrap().examined, 2);
    }
    let before = durable(&store);
    TERRAIN_TIDY_REJECT_COMMIT.with(|flag| flag.set(true));
    assert!(store.apply_terrain_tidy_chunk(2, T1).is_err());
    assert_eq!(
        durable(&store),
        before,
        "C9 ST68: failed commit rolls back rows, counts and marker"
    );
    drop(store);
    let store = HubStore::initialize(dir.path()).unwrap();
    finish(&store, 2, T1);
    assert_st67(&store, v);
    assert_eq!(row_state(&store), row_state(&reference));
    for (id, e) in [
        (1, None),
        (3, Some(50)),
        (11, Some(110)),
        (19, None),
        (20, None),
        (22, None),
    ] {
        assert_eq!(elevation(&store, v, id), e);
        assert_eq!(elevation(&store, v, id), elevation(&reference, rv, id));
        assert_eq!(provenance(&store, v, id), provenance(&reference, rv, id));
    }
    for id in 1..=5 {
        assert_eq!(pair(&store, v, id), pair(&reference, rv, id));
    }
    assert_eq!(
        counts(&store.terrain_tidy_progress().unwrap())[..5],
        [15, 5, 2, 4, 3]
    );
    let observer = store.open().unwrap();
    let prior = version(&observer);
    assert!(store.terrain_tidy_complete().unwrap());
    assert_eq!(
        store.apply_terrain_tidy_chunk(250, T2).unwrap(),
        TerrainTidyApplied {
            complete: true,
            ..Default::default()
        }
    );
    assert_eq!(version(&observer), prior);
    let before = data(&store, DATA);
    reset(&store, 3, Some(T2));
    finish(&store, 2, T2);
    assert_eq!(data(&store, DATA), before);
    assert_eq!(
        counts(&store.terrain_tidy_progress().unwrap()),
        [6, 0, 0, 0, 3, 0]
    );
    let (_new_dir, new, _v, _source) = fixture();
    assert_eq!(
        new.apply_terrain_tidy_chunk(250, T1).unwrap(),
        TerrainTidyApplied {
            complete: true,
            ..Default::default()
        }
    );
    assert_eq!(
        new.terrain_tidy_progress().unwrap().completed_at_ms,
        Some(T1)
    );
}

fn l71() -> (tempfile::TempDir, HubStore, Uuid, Uuid) {
    let (dir, store, v, source) = fixture();
    commit_rows(
        &store,
        v,
        &[
            (11, 8, None),
            (12, 8, None),
            (13, 8, None),
            (14, 8, None),
            (15, 8, None),
        ],
    );
    plant(&store, v, 15, 8, None, "void", 15);
    raw_watermark(&store, source, v, 10, T0 + 10_000);
    assert_eq!(store.apply_terrain_tidy_chunk(250, T1).unwrap().examined, 1);
    assert_eq!(store.terrain_tidy_progress().unwrap().run, 1);
    assert_eq!(
        fill(&store, v, &[(11, 100), (12, 110), (13, 120), (14, 130)], T1).filled,
        4
    );
    clear_log(&store);
    (dir, store, v, source)
}
fn assert_reset(store: &HubStore, run: i64, now: i64) {
    let p = store.terrain_tidy_progress().unwrap();
    assert_eq!(p.run, run);
    assert_eq!(p.armed_at_ms, Some(now));
    assert_eq!(p.completed_at_ms, None);
    assert_eq!(counts(&p), [0; 6]);
    assert_eq!(marker(store), (String::new(), 0));
}
fn assert_l71(store: &HubStore, v: Uuid) {
    for p in [11, 12] {
        assert_eq!(elevation(store, v, p), None);
        assert!(provenance(store, v, p).is_none());
    }
    for (p, e) in [(13, 120), (14, 130)] {
        assert_eq!(elevation(store, v, p), Some(e));
        assert_eq!(provenance(store, v, p).unwrap()["status"], "success");
    }
    assert_eq!(provenance(store, v, 15).unwrap()["status"], "void");
    assert_eq!(pair(store, v, 8), (Some(10), Some(0)));
    assert_eq!(mutation_ids(store, "position"), [11, 12]);
    assert_eq!(mutation_ids(store, "drive"), [8]);
    let p = store.terrain_tidy_progress().unwrap();
    assert!(p.completed_at_ms.is_some());
    assert_eq!(p.run, 2);
    assert_eq!(counts(&p), [5, 2, 0, 0, 2, 1]);
}

#[test]
fn c9_st72_import_raise() {
    let (_dir, store, v, source) = l71();
    let original = store.terrain_tidy_progress().unwrap();
    assert_eq!(
        import(&store, source, v, 20, T0 + 10_000, T2, true),
        TerrainBoundaryRaise::Raised
    );
    assert_eq!(
        terrain_stored_boundary(&store.open().unwrap(), v).unwrap(),
        ImportBoundary {
            max_position_id: 20,
            max_date_ms: T0 + 10_000,
        }
    );
    assert_eq!(store.terrain_tidy_progress().unwrap(), original);
    let observer = store.open().unwrap();
    let before = version(&observer);
    let snapshot = durable(&store);
    let mut conn = store.open().unwrap();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let changes = tx.total_changes();
    assert_eq!(
        terrain_after_import_boundary_raise_in_transaction(&tx, v, T2 + 1_000).unwrap(),
        TerrainBoundaryRaise::Unchanged
    );
    assert_eq!(tx.total_changes(), changes);
    tx.commit().unwrap();
    assert_eq!(version(&observer), before);
    assert_eq!(durable(&store), snapshot);
    assert_eq!(
        import(&store, source, v, 20, T0 + 12_000, T2 + 2_000, true),
        TerrainBoundaryRaise::RaisedAndRearmed
    );
    assert_eq!(
        terrain_stored_boundary(&store.open().unwrap(), v).unwrap(),
        ImportBoundary {
            max_position_id: 20,
            max_date_ms: T0 + 12_000,
        }
    );
    assert_reset(&store, 2, T2 + 2_000);
    let v2 = b3_c8_sa::c8_vehicle(&store, "terrain-second");
    let pending = store.terrain_tidy_progress().unwrap();
    assert_eq!(
        import(&store, source, v2, 30, T0 + 30_000, T2 + 3_000, true),
        TerrainBoundaryRaise::Raised
    );
    assert_eq!(store.terrain_tidy_progress().unwrap(), pending);
    let snapshot = durable(&store);
    assert_eq!(
        import(&store, source, v, 40, T0 + 40_000, T2 + 4_000, false),
        TerrainBoundaryRaise::RaisedAndRearmed
    );
    assert_eq!(durable(&store), snapshot);
    // C9 ST72: growing the id field alone can also newly cover a success row.
    let (_dir, store, v, source) = l71();
    assert_eq!(
        import(&store, source, v, 10, T0 + 20_000, T2, true),
        TerrainBoundaryRaise::Raised
    );
    assert_eq!(
        import(&store, source, v, 12, T0 + 20_000, T3, true),
        TerrainBoundaryRaise::RaisedAndRearmed
    );
    assert_reset(&store, 2, T3);
    let kept: Vec<_> = [13, 14]
        .map(|p| (p, json_position(&store, v, p), provenance(&store, v, p)))
        .into();
    finish(&store, 250, T3);
    assert_l71(&store, v);
    for (p, json, row) in kept {
        assert_eq!(json_position(&store, v, p), json);
        assert_eq!(provenance(&store, v, p), row);
    }

    // C9 R6, C9 ST72 (a): ids inside the maximum, dates beyond it remain live.
    let (_dir, store, v, source) = l71();
    let original = store.terrain_tidy_progress().unwrap();
    assert_eq!(
        import(&store, source, v, 20, T0 + 10_000, T2, true),
        TerrainBoundaryRaise::Raised
    );
    assert_eq!(store.terrain_tidy_progress().unwrap(), original);
    let before = data(&store, DATA);
    reset(&store, 2, Some(T2));
    finish(&store, 250, T3);
    for (p, e) in [(11, 100), (12, 110), (13, 120), (14, 130)] {
        assert_eq!(elevation(&store, v, p), Some(e));
        assert_eq!(provenance(&store, v, p).unwrap()["status"], "success");
    }
    assert_eq!(
        counts(&store.terrain_tidy_progress().unwrap()),
        [5, 0, 0, 0, 4, 0]
    );
    assert!(store.terrain_tidy_complete().unwrap());
    assert!(mutation_ids(&store, "position").is_empty());
    assert_eq!(data(&store, DATA), before);
    assert_seed_and_promotion_hooks();
}

fn assert_seed_and_promotion_hooks() {
    // C9 R58: both retained entry points call the hook only for changed Positions.
    for promote in [false, true] {
        let (_dir, store, v, source) = l71();
        store
            .open()
            .unwrap()
            .execute(
                "INSERT INTO vehicle_identity_aliases(
            alias_kind, alias_value, source_id, source_vehicle_key, vehicle_id)
            VALUES ('source_key', 'terrain-seed', ?1, 'terrain-seed', ?2)",
                params![source.to_string(), v.to_string()],
            )
            .unwrap();
        let mut session = t69_s((None, None));
        session.watermarks.positions.max_id = Some(12);
        session.watermarks.positions.max_timestamp_ms = Some(T0 + 12_000);
        if promote {
            let mut conn = store.open().unwrap();
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            promote_imported_open_session_in_transaction(&tx, source, v, 1, &session, T2, None)
                .unwrap();
            assert_eq!(terrain_tidy_run_in_transaction(&tx).unwrap().run, 2);
            tx.commit().unwrap();
        } else {
            assert!(
                !store
                    .seed_imported_open_session(source, v, 1, &session, T2)
                    .unwrap()
                    .no_op
            );
        }
        assert_reset(&store, 2, T2);
        if !promote {
            // C9 R58: a no-op must not call even a hook that would repair a stale boundary.
            store
                .open()
                .unwrap()
                .execute(
                    "UPDATE terrain_enrichment_state SET import_position_id_max=10,
                 import_position_date_max_ms=?1 WHERE vehicle_id=?2",
                    params![T0 + 10_000, v.to_string()],
                )
                .unwrap();
        }
        let before = durable(&store);
        let observer = store.open().unwrap();
        let prior = version(&observer);
        if !promote {
            assert!(
                store
                    .seed_imported_open_session(source, v, 1, &session, T3)
                    .unwrap()
                    .no_op
            );
            assert_eq!(durable(&store), before);
            assert_eq!(version(&observer), prior);
        }
        // C9 R58: an unrelated changed domain must not repair a deliberately stale boundary.
        store
            .open()
            .unwrap()
            .execute(
                "UPDATE terrain_enrichment_state SET import_position_id_max=10,
            import_position_date_max_ms=?1 WHERE vehicle_id=?2",
                params![T0 + 10_000, v.to_string()],
            )
            .unwrap();
        let before = store.terrain_tidy_progress().unwrap();
        session.watermarks.updates.max_id = Some(99);
        session.watermarks.updates.max_timestamp_ms = Some(T3);
        if promote {
            let mut conn = store.open().unwrap();
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            promote_imported_open_session_in_transaction(&tx, source, v, 1, &session, T3, None)
                .unwrap();
            tx.commit().unwrap();
        } else {
            store
                .seed_imported_open_session(source, v, 1, &session, T3)
                .unwrap();
        }
        assert_eq!(store.terrain_tidy_progress().unwrap(), before);
        assert_eq!(
            terrain_stored_boundary(&store.open().unwrap(), v)
                .unwrap()
                .max_position_id,
            10
        );
    }
}

#[test]
fn c9_st74_restart() {
    let (dir, store, v, source) = l71();
    assert_eq!(
        import(&store, source, v, 12, T0 + 12_000, T2, true),
        TerrainBoundaryRaise::RaisedAndRearmed
    );
    assert_reset(&store, 2, T2);
    assert_eq!(store.apply_terrain_tidy_chunk(2, T3).unwrap().cleared, 2);
    assert_eq!(marker(&store), (v.to_string(), 12));
    let before = durable(&store);
    TERRAIN_TIDY_REJECT_COMMIT.with(|flag| flag.set(true));
    assert!(store.apply_terrain_tidy_chunk(2, T3).is_err());
    assert_eq!(durable(&store), before);
    drop(store);
    let store = HubStore::initialize(dir.path()).unwrap();
    finish(&store, 2, T3);
    assert_l71(&store, v);
    let before = data(&store, DATA);
    reset(&store, 3, Some(T3 + 1_000));
    finish(&store, 2, T3 + 1_000);
    assert_eq!(data(&store, DATA), before);
    let p = store.terrain_tidy_progress().unwrap();
    assert_eq!(p.run, 3);
    assert_eq!(p.armed_at_ms, Some(T3 + 1_000));
    assert_eq!(counts(&p), [3, 0, 0, 0, 2, 0]);
}

#[test]
fn c9_st75_chunk_rearm() {
    let (dir, store, v, source) = fixture();
    commit_rows(
        &store,
        v,
        &[
            (11, 1, Some(100)),
            (12, 1, Some(110)),
            (13, 1, Some(120)),
            (16, 2, None),
            (17, 2, None),
        ],
    );
    for (p, e) in [(11, 100), (12, 110), (13, 120)] {
        plant(&store, v, p, 1, Some(e), "success", p);
    }
    store
        .open()
        .unwrap()
        .execute_batch(
            "UPDATE materialised_drives SET ascent=20,descent=0,
        drive_json=json_set(drive_json,'$.ascent',20,'$.descent',0) WHERE drive_id=1",
        )
        .unwrap();
    raw_watermark(&store, source, v, 10, T0 + 10_000);
    assert_eq!(
        store.apply_terrain_tidy_chunk(2, T1).unwrap(),
        TerrainTidyApplied {
            examined: 2,
            kept: 2,
            ..Default::default()
        }
    );
    assert_eq!(
        terrain_stored_boundary(&store.open().unwrap(), v).unwrap(),
        ImportBoundary {
            max_position_id: 10,
            max_date_ms: T0 + 10_000,
        }
    );
    assert_eq!(marker(&store), (v.to_string(), 12));
    let run = store.terrain_tidy_progress().unwrap().identity();
    let outcomes = [
        filled_outcome(&store, v, 16, 140),
        filled_outcome(&store, v, 17, 150),
    ];
    raw_watermark(&store, source, v, 11, T0 + 11_000);
    let before = durable(&store);
    // C9 ST75: even a reset triggered by a chunk rolls back with a rejected commit.
    TERRAIN_TIDY_REJECT_COMMIT.with(|flag| flag.set(true));
    assert!(store.apply_terrain_tidy_chunk(2, T1 + 1_000).is_err());
    assert_eq!(durable(&store), before);
    let data_before = data(&store, DATA);
    assert_eq!(
        store.apply_terrain_tidy_chunk(2, T1 + 1_000).unwrap(),
        TerrainTidyApplied {
            rearmed: true,
            ..Default::default()
        }
    );
    assert_reset(&store, 2, T1 + 1_000);
    assert_eq!(data(&store, DATA), data_before);
    let before = durable(&store);
    let batch = TerrainBatch {
        vehicle_id: v,
        outcomes: &outcomes,
        retire: &[99],
        cursor_to: Some(17),
        committed_at_ms: T2,
    };
    assert_eq!(
        store.apply_terrain_batch_for_tidy_run(&batch, run).unwrap(),
        TerrainBatchApplied {
            rearmed: true,
            ..Default::default()
        }
    );
    assert_eq!(
        store.apply_terrain_tidy_chunk_for_run(2, T2, run).unwrap(),
        TerrainTidyApplied {
            rearmed: true,
            ..Default::default()
        }
    );
    assert_eq!(durable(&store), before);
    drop(store);
    let store = HubStore::initialize(dir.path()).unwrap();
    finish(&store, 2, T2);
    assert_eq!(elevation(&store, v, 11), None);
    assert!(provenance(&store, v, 11).is_none());
    for (p, e) in [(12, 110), (13, 120)] {
        assert_eq!(elevation(&store, v, p), Some(e));
    }
    assert_eq!(pair(&store, v, 1), (Some(10), Some(0)));
    assert_eq!(
        counts(&store.terrain_tidy_progress().unwrap()),
        [3, 1, 0, 0, 2, 1]
    );
    assert_eq!(mutation_ids(&store, "position"), [11]);
    assert_eq!(mutation_ids(&store, "drive"), [1]);
    for p in [16, 17] {
        assert_eq!(elevation(&store, v, p), None);
        assert!(provenance(&store, v, p).is_none());
    }
    // C9 R58: armed time alone invalidates prepared ordinary work as well.
    let mut wrong = store.terrain_tidy_progress().unwrap().identity();
    wrong.armed_at_ms = Some(T0);
    let before = durable(&store);
    assert!(
        store
            .apply_terrain_batch_for_tidy_run(&batch, wrong)
            .unwrap()
            .rearmed
    );
    assert!(
        store
            .apply_terrain_tidy_chunk_for_run(2, T2, wrong)
            .unwrap()
            .rearmed
    );
    assert_eq!(durable(&store), before);
    // C9 ST75: the convenience batch entry also notices a re-arm after its read.
    let before = data(&store, DATA);
    let concurrent = store.clone();
    TERRAIN_COMMIT_BEFORE_BEGIN.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            assert_eq!(
                concurrent
                    .record_terrain_import_boundary(
                        v,
                        ImportBoundary {
                            max_position_id: 12,
                            max_date_ms: T0 + 12_000,
                        },
                        T3
                    )
                    .unwrap(),
                TerrainBoundaryRaise::RaisedAndRearmed
            );
        }));
    });
    assert!(store.apply_terrain_batch(&batch).unwrap().rearmed);
    assert_eq!(data(&store, DATA), before);
    assert_reset(&store, 3, T3);
    finish(&store, 2, T3);
    assert_eq!(fill(&store, v, &[(16, 140), (17, 150)], T3).filled, 2);
}

#[test]
fn c9_r58_stale_identity_blocks_retirement_and_cursor() {
    let (_dir, store, v, _source) = fixture();
    // C9 R10, C9 R58: this due orphan really would be retired by ordinary work.
    plant(&store, v, 99, 1, None, "failed", 99);
    let identity = store.terrain_tidy_progress().unwrap().identity();
    let batch = TerrainBatch {
        vehicle_id: v,
        outcomes: &[],
        retire: &[99],
        cursor_to: Some(100),
        committed_at_ms: T1,
    };
    for stale in [
        TerrainTidyRun {
            run: identity.run + 1,
            ..identity
        },
        TerrainTidyRun {
            armed_at_ms: Some(T0),
            ..identity
        },
    ] {
        let before = durable(&store);
        let observer = store.open().unwrap();
        let prior = version(&observer);
        assert_eq!(
            store
                .apply_terrain_batch_for_tidy_run(&batch, stale)
                .unwrap(),
            TerrainBatchApplied {
                rearmed: true,
                ..Default::default()
            }
        );
        assert_eq!(durable(&store), before);
        assert_eq!(version(&observer), prior);
    }
    let applied = store
        .apply_terrain_batch_for_tidy_run(&batch, identity)
        .unwrap();
    assert_eq!(applied.retired, 1);
    assert_eq!(applied.cursor_written, Some(100));
    assert!(provenance(&store, v, 99).is_none());
}

#[test]
fn c9_r40_tidy_preserves_copied_drive_totals() {
    for inventory in ["legacy", "durable"] {
        let (_dir, store, _v, source) = fixture();
        let (vehicle, _, lineage) = imported_v2_base(&store);
        let v = vehicle.vehicle_id;
        commit_rows(&store, v, &[(1, 9, Some(100)), (2, 9, Some(120))]);
        plant(&store, v, 1, 9, Some(100), "success", 40);
        plant(&store, v, 2, 9, Some(120), "success", 2);
        let connection = store.open().unwrap();
        connection
            .execute(
                "UPDATE materialised_drives SET ascent=55, descent=40,
             drive_json=json_set(drive_json,'$.ascent',55,'$.descent',40,'$.copied',true)
             WHERE vehicle_id=?1 AND drive_id=9",
                [v.to_string()],
            )
            .unwrap();
        if inventory == "legacy" {
            connection
                .execute(
                    concat!(
                        "INSERT INTO ",
                        crate::db::import_table_names::import_projection_heads_table!(),
                        " VALUES (?1,?2,?3,1,0)"
                    ),
                    params![
                        v.to_string(),
                        source.to_string(),
                        lineage.base.snapshot_id.to_string()
                    ],
                )
                .unwrap();
            connection
                .execute(
                    concat!(
                        "INSERT INTO ",
                        crate::db::import_table_names::import_projection_rows_table!(),
                        " VALUES (?1,'drive',9)"
                    ),
                    [v.to_string()],
                )
                .unwrap();
        } else {
            connection
                .execute(
                    concat!(
                        "INSERT INTO ",
                        crate::db::import_table_names::import_projection_state_heads_table!(),
                        " VALUES (?1,?2,?3,1,0)"
                    ),
                    params![
                        v.to_string(),
                        source.to_string(),
                        lineage.base.snapshot_id.to_string()
                    ],
                )
                .unwrap();
            connection
                .execute(
                    concat!(
                        "INSERT INTO ",
                        crate::db::import_table_names::import_projection_state_rows_table!(),
                        " VALUES (?1,'drive',1,9,1,zeroblob(32))"
                    ),
                    [v.to_string()],
                )
                .unwrap();
        }
        let before = data(&store, &["materialised_drives"]);
        C9_RECOMPUTE_CALLS.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
        let applied = store.apply_terrain_tidy_chunk(250, T1).unwrap();
        let calls = C9_RECOMPUTE_CALLS.with(|slot| slot.borrow_mut().take().unwrap());
        assert_eq!(
            applied,
            TerrainTidyApplied {
                examined: 2,
                requeued: 1,
                kept: 1,
                complete: true,
                ..Default::default()
            },
            "{inventory}"
        );
        assert!(
            calls.is_empty(),
            "C9 R40: copied drives never reach recompute"
        );
        assert_eq!(
            data(&store, &["materialised_drives"]),
            before,
            "{inventory}"
        );
        assert_eq!(mutation_ids(&store, "position"), [1]);
        assert!(mutation_ids(&store, "drive").is_empty());
    }
}

#[test]
fn c9_r55_standalone_and_non_success_rows() {
    let (_dir, store, v, source) = fixture();
    commit_rows(
        &store,
        v,
        &[
            (1, 1, Some(100)),
            (11, 1, Some(110)),
            (12, 1, Some(120)),
            (13, 1, None),
        ],
    );
    store
        .open()
        .unwrap()
        .execute_batch(
            "UPDATE materialised_positions SET drive_id=NULL,
         position_json=json_set(position_json,'$.drive_id',NULL);",
        )
        .unwrap();
    plant(&store, v, 1, 1, Some(100), "success", 1);
    plant(&store, v, 11, 1, Some(110), "success", 40);
    plant(&store, v, 12, 1, None, "failed", 12);
    plant(&store, v, 13, 1, Some(130), "success", 13);
    raw_watermark(&store, source, v, 1, T0 + 1_000);
    store
        .record_terrain_import_boundary(
            v,
            ImportBoundary {
                max_position_id: 1,
                max_date_ms: T0 + 1_000,
            },
            T0,
        )
        .unwrap();
    let kept = json_position(&store, v, 11);
    let failed_position = json_position(&store, v, 12);
    let failed = provenance(&store, v, 12);
    let drives = data(&store, &["materialised_drives"]);
    // C9 R55: T2 precedes no-drive T6; a live standalone Hub value stays even at changed coordinates.
    assert_eq!(
        store.apply_terrain_tidy_chunk(250, T1).unwrap(),
        TerrainTidyApplied {
            examined: 4,
            cleared: 1,
            removed: 1,
            kept: 1,
            complete: true,
            ..Default::default()
        }
    );
    assert_eq!(elevation(&store, v, 1), None);
    assert!(provenance(&store, v, 1).is_none());
    assert!(provenance(&store, v, 13).is_none());
    assert_eq!(json_position(&store, v, 11), kept);
    assert_eq!(provenance(&store, v, 11).unwrap()["status"], "success");
    assert_eq!(json_position(&store, v, 12), failed_position);
    assert_eq!(provenance(&store, v, 12), failed);
    assert_eq!(data(&store, &["materialised_drives"]), drives);
    assert_eq!(mutation_ids(&store, "position"), [1]);
}

#[test]
fn c9_r56_upgrade_pending_run_drives_first_tidy() {
    let (dir, store, v, _source) = fixture();
    commit_rows(&store, v, &[(1, 1, Some(100)), (2, 1, Some(120))]);
    plant(&store, v, 1, 1, Some(100), "success", 40);
    plant(&store, v, 2, 1, Some(120), "success", 2);
    let connection = store.open().unwrap();
    connection
        .execute_batch(
            "UPDATE materialised_drives SET ascent=20,descent=0,
        drive_json=json_set(drive_json,'$.ascent',20,'$.descent',0);",
        )
        .unwrap();
    let predecessor = TERRAIN_SELECTION_SCHEMA_VERSION - 1;
    connection
        .execute_batch(&format!(
            "BEGIN IMMEDIATE; {REWIND_V77_SQL} PRAGMA user_version={predecessor}; COMMIT;"
        ))
        .unwrap();
    connection
        .execute(
            "INSERT INTO terrain_enrichment_state VALUES (?1,99,?2)",
            params![v.to_string(), T0],
        )
        .unwrap();
    crate::db::import_names_test_support::prepare_historical_import_names(&connection);
    drop(connection);
    drop(store);
    let store = HubStore::initialize(dir.path()).unwrap();
    let progress = store.terrain_tidy_progress().unwrap();
    assert_eq!(progress.run, 1);
    assert_eq!(progress.armed_at_ms, None);
    assert_eq!(progress.completed_at_ms, None);
    assert_eq!(counts(&progress), [0; 6]);
    assert_eq!(marker(&store), (String::new(), 0));
    assert_eq!(store.terrain_vehicles().unwrap()[0].cursor_position_id, 0);
    assert_eq!(
        store
            .apply_terrain_tidy_chunk_for_run(250, T1, progress.identity())
            .unwrap(),
        TerrainTidyApplied {
            examined: 2,
            requeued: 1,
            kept: 1,
            drives_changed: 1,
            complete: true,
            ..Default::default()
        }
    );
    assert_eq!(elevation(&store, v, 1), None);
    assert_eq!(
        provenance(&store, v, 1).unwrap()["error"],
        "position_rewritten"
    );
    assert_eq!(store.terrain_tidy_progress().unwrap().run, 1);
}

#[test]
fn c9_r58_multi_vehicle_rearm_precedes_all_actions() {
    let (_dir, store, v, source) = fixture();
    let other = b3_c8_sa::c8_vehicle(&store, "terrain-later");
    let mut vehicles = [v, other];
    vehicles.sort();
    let [first, last] = vehicles;
    for vehicle in vehicles {
        commit_rows(&store, vehicle, &[(1, 1, Some(100))]);
        plant(&store, vehicle, 1, 1, Some(100), "success", 40);
    }
    raw_watermark(&store, source, last, 1, T0 + 1_000);
    let before = data(&store, DATA);
    assert_eq!(
        store.apply_terrain_tidy_chunk(250, T1).unwrap(),
        TerrainTidyApplied {
            rearmed: true,
            ..Default::default()
        }
    );
    assert_eq!(data(&store, DATA), before);
    assert_reset(&store, 2, T1);
    assert_eq!(elevation(&store, first, 1), Some(100));
    finish(&store, 250, T2);
    assert_eq!(elevation(&store, first, 1), None);
    assert_eq!(provenance(&store, first, 1).unwrap()["status"], "failed");
    assert_eq!(elevation(&store, last, 1), None);
    assert!(provenance(&store, last, 1).is_none());
}
