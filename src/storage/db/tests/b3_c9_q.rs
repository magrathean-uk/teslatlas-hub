// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use serde_json::json;

const C9_T0: i64 = 1_760_000_000_000;

fn c9_store() -> (tempfile::TempDir, HubStore, Uuid) {
    let (directory, store) = b3_c8_sa::c8_store();
    let source = store
        .register_source(&SourceDescriptor::new("owner_api_compat", "c9"), 1_000)
        .unwrap();
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "one"), 1_001)
        .unwrap()
        .vehicle_id;
    (directory, store, vehicle)
}

fn c9_position(id: i64, drive: Option<i64>, elevation: Option<i64>) -> ProjectionPosition {
    let mut position = position_row(id, drive, C9_T0 + id * 1_000);
    position.elevation = elevation;
    position
}

fn c9_commit(
    store: &HubStore,
    vehicle: Uuid,
    drives: Vec<ProjectionDrive>,
    positions: Vec<ProjectionPosition>,
) {
    let state = crate::lifecycle::OpenSessionState::new().encode().unwrap();
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle,
            car_id: 1,
            open_session_json: &state,
            last_observation_id: 0,
            quarantined: false,
            updated_at_ms: C9_T0 + 20_000_000,
            delta: &crate::lifecycle::LifecycleDelta {
                drives,
                positions,
                ..Default::default()
            },
        })
        .unwrap();
}

fn c9_drive(id: i64) -> ProjectionDrive {
    let mut drive = drive_row(id, C9_T0 + 20_000_000);
    drive.start_date_ms = C9_T0 - 10_000;
    drive
}

fn c9_vehicle(store: &HubStore, vehicle: Uuid) -> TerrainVehicle {
    store
        .terrain_vehicles()
        .unwrap()
        .into_iter()
        .find(|v| v.vehicle_id == vehicle)
        .unwrap()
}

fn c9_ids(store: &HubStore, vehicle: &TerrainVehicle) -> (Vec<i64>, i64) {
    let result = store
        .terrain_forward_window(vehicle, K_SCAN_ROWS, K_PASS_CANDIDATES)
        .unwrap();
    (
        result.candidates.iter().map(|v| v.position.id).collect(),
        result.examined_through,
    )
}

fn c9_provenance(connection: &Connection, vehicle: Uuid, id: i64, status: &str, retry: i64) {
    connection.execute("INSERT INTO terrain_elevation_provenance(
        vehicle_id, position_id, drive_id, latitude, longitude, elevation_m, tile_name,
        tile_hash, dataset_source, dataset_version, status, error_code, attempts, attempted_at_ms, retry_after_ms)
        VALUES (?1, ?2, 1, 10.0, 20.0, 50, 'synthetic', 'digest', 'test', '1', ?3, NULL, 2, ?4, ?5)",
        params![vehicle.to_string(), id, status, C9_T0 - 10, retry]).unwrap();
}

fn c9_watermark(
    connection: &Connection,
    source: Uuid,
    vehicle: Uuid,
    domain: &str,
    id: i64,
    date: i64,
) {
    connection.execute("INSERT INTO lifecycle_source_watermarks VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT(source_id, vehicle_id, domain) DO UPDATE SET
        max_source_row_id = excluded.max_source_row_id, max_timestamp_ms = excluded.max_timestamp_ms",
        params![source.to_string(), vehicle.to_string(), domain, id, date]).unwrap();
}

#[test]
fn c9_st11_st12_qualification() {
    let (_directory, store, vehicle) = c9_store();
    c9_commit(
        &store,
        vehicle,
        vec![c9_drive(1), c9_drive(2), c9_drive(3)],
        vec![
            c9_position(1, Some(1), None),
            c9_position(2, Some(1), None),
            c9_position(3, Some(1), None),
            c9_position(4, Some(2), None),
            c9_position(5, Some(2), Some(120)),
            c9_position(6, Some(2), None),
            c9_position(7, None, None),
            c9_position(8, Some(3), Some(0)),
        ],
    );
    assert_eq!(
        c9_ids(&store, &c9_vehicle(&store, vehicle)),
        (vec![1, 2, 3], 8)
    );
    let source = store
        .register_source(&SourceDescriptor::new(IMPORT_SOURCE_KIND, "import"), 1_000)
        .unwrap();
    let wrong = store
        .register_source(&SourceDescriptor::new("owner_api_compat", "wrong"), 1_000)
        .unwrap();
    let connection = store.open().unwrap();
    c9_watermark(
        &connection,
        source.source_id,
        vehicle,
        "drives",
        2,
        C9_T0 + 2_000,
    );
    c9_watermark(
        &connection,
        wrong.source_id,
        vehicle,
        "positions",
        2,
        C9_T0 + 2_000,
    );
    assert_eq!(
        c9_ids(&store, &c9_vehicle(&store, vehicle)),
        (vec![1, 2, 3], 8)
    );
    c9_watermark(
        &connection,
        source.source_id,
        vehicle,
        "positions",
        2,
        C9_T0 + 2_000,
    );
    assert_eq!(c9_ids(&store, &c9_vehicle(&store, vehicle)), (vec![3], 8));
    c9_watermark(
        &connection,
        source.source_id,
        vehicle,
        "positions",
        2,
        C9_T0 + 1_000,
    );
    assert_eq!(
        c9_ids(&store, &c9_vehicle(&store, vehicle)),
        (vec![2, 3], 8)
    );
    c9_watermark(
        &connection,
        source.source_id,
        vehicle,
        "positions",
        1,
        C9_T0 + 20_000,
    );
    assert_eq!(
        c9_ids(&store, &c9_vehicle(&store, vehicle)),
        (vec![2, 3], 8)
    );
    connection
        .execute("DELETE FROM lifecycle_source_watermarks", [])
        .unwrap();
    c9_provenance(&connection, vehicle, 2, "void", 0);
    connection.execute("UPDATE materialised_positions SET position_json = json_set(position_json, '$.latitude', 91)
        WHERE vehicle_id = ?1 AND position_id = 3", [vehicle.to_string()]).unwrap();
    assert_eq!(c9_ids(&store, &c9_vehicle(&store, vehicle)), (vec![1], 8));
    connection
        .execute(
            "DELETE FROM materialised_drives WHERE vehicle_id = ?1 AND drive_id = 1",
            [vehicle.to_string()],
        )
        .unwrap();
    assert_eq!(c9_ids(&store, &c9_vehicle(&store, vehicle)), (vec![], 8));
}

#[test]
fn c9_st51_st52_boundaries() {
    let (directory, store, _) = c9_store();
    let source = store
        .register_source(
            &SourceDescriptor::new(IMPORT_SOURCE_KIND, "handover"),
            1_000,
        )
        .unwrap();
    let live = store
        .register_source(&SourceDescriptor::new("owner_api_compat", "live"), 1_000)
        .unwrap();
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "handover"), 1_001)
        .unwrap()
        .vehicle_id;
    let mut session = st93_session(C9_T0 - 10_000);
    session.charge = None;
    session.charge_samples.clear();
    session.drive = Some(
        serde_json::from_value(json!({
            "id": 40, "car_id": 1, "start_date_ms": C9_T0 - 10_000, "end_date_ms": null,
            "start_position_id": 101, "end_position_id": 102, "start_km": 1.0, "end_km": 2.0
        }))
        .unwrap(),
    );
    session.drive_positions = [101, 102]
        .into_iter()
        .map(|id| {
            serde_json::from_value(json!({
                "id": id, "car_id": 1, "drive_id": 40, "date_ms": C9_T0 - (103 - id) * 1_000,
                "latitude": 10.0, "longitude": 20.0
            }))
            .unwrap()
        })
        .collect();
    session.watermarks.positions.max_id = Some(500);
    session.watermarks.positions.max_timestamp_ms = Some(C9_T0 - 1_000);
    store
        .seed_imported_open_session(source.source_id, vehicle, 1, &session, C9_T0)
        .unwrap();
    // C9 ST51: the live source is linked to the same vehicle before close.
    store
        .open()
        .unwrap()
        .execute(
            "INSERT INTO vehicle_identity_aliases(alias_kind, alias_value, source_id, source_vehicle_key, vehicle_id)
        VALUES ('source_key', 'c9-live', ?1, 'live', ?2)",
            params![live.source_id.to_string(), vehicle.to_string()],
        )
        .unwrap();
    let mut p201 = c9_position(201, Some(40), None);
    p201.date_ms = C9_T0 + 1_000;
    let mut p202 = c9_position(202, Some(40), None);
    p202.date_ms = C9_T0 + 2_000;
    c9_commit(
        &store,
        vehicle,
        vec![c9_drive(40), c9_drive(41)],
        vec![
            p201,
            p202,
            c9_position(501, Some(41), None),
            c9_position(502, Some(41), None),
        ],
    );
    let boundary = ImportBoundary {
        max_position_id: 500,
        max_date_ms: C9_T0 - 1_000,
    };
    // C9 ST51: the imported open children must exist before classification.
    let connection = store.open().unwrap();
    let children = connection
        .prepare(
            "SELECT position_id, drive_id, json_extract(position_json, '$.elevation')
            FROM materialised_positions WHERE vehicle_id = ?1 AND position_id IN (101, 102)
            ORDER BY position_id",
        )
        .unwrap()
        .query_map([vehicle.to_string()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(children, vec![(101, 40, None), (102, 40, None)]);
    assert_eq!(c9_vehicle(&store, vehicle).watermark_boundary, boundary);
    // C9 R58: the seed's changed Positions already raised the stored boundary.
    assert_eq!(c9_vehicle(&store, vehicle).stored_boundary, boundary);
    assert_eq!(
        c9_ids(&store, &c9_vehicle(&store, vehicle)),
        (vec![201, 202, 501, 502], 502)
    );
    assert_eq!(
        store
            .record_terrain_import_boundary(vehicle, boundary, C9_T0 + 3_000)
            .unwrap(),
        TerrainBoundaryRaise::Unchanged
    );
    store
        .open()
        .unwrap()
        .execute(
            "UPDATE terrain_enrichment_state SET cursor_position_id = 502 WHERE vehicle_id = ?1",
            [vehicle.to_string()],
        )
        .unwrap();
    store
        .open()
        .unwrap()
        .execute(
            "DELETE FROM lifecycle_source_watermarks WHERE vehicle_id = ?1",
            [vehicle.to_string()],
        )
        .unwrap();
    let mut p103 = c9_position(103, Some(42), None);
    p103.date_ms = C9_T0 - 1_500;
    let mut p503 = c9_position(503, Some(42), None);
    p503.date_ms = C9_T0 + 5_000;
    c9_commit(&store, vehicle, vec![c9_drive(42)], vec![p103, p503]);
    // C9 ST52: the late row is materialised below the retained boundary.
    assert_eq!(connection.query_row(
        "SELECT drive_id FROM materialised_positions WHERE vehicle_id = ?1 AND position_id = 103",
        [vehicle.to_string()], |row| row.get::<_, i64>(0)).unwrap(), 42);
    let mut v = c9_vehicle(&store, vehicle);
    assert_eq!(v.stored_boundary, boundary);
    assert_eq!(v.watermark_boundary, ImportBoundary::default());
    assert_eq!(v.cursor_position_id, 502);
    assert_eq!(c9_ids(&store, &v), (vec![503], 503));
    v.cursor_position_id = 0;
    assert_eq!(c9_ids(&store, &v), (vec![201, 202, 501, 502, 503], 503));
    assert_eq!(
        store
            .record_terrain_import_boundary(
                vehicle,
                ImportBoundary {
                    max_position_id: 10,
                    max_date_ms: C9_T0 - 10_000
                },
                C9_T0 + 4_000
            )
            .unwrap(),
        TerrainBoundaryRaise::Unchanged
    );
    drop(store);
    let store = HubStore::initialize(directory.path()).unwrap();
    assert_eq!(c9_vehicle(&store, vehicle).stored_boundary, boundary);
    let connection = store.open().unwrap();
    connection
        .execute(
            "UPDATE terrain_tidy_state SET completed_at_ms = ?1, examined = 9, cleared = 2",
            [C9_T0],
        )
        .unwrap();
    c9_provenance(&connection, vehicle, 503, "success", 0);
    assert_eq!(
        store
            .record_terrain_import_boundary(
                vehicle,
                ImportBoundary {
                    max_position_id: 503,
                    max_date_ms: C9_T0 + 503_000
                },
                C9_T0 + 600_000
            )
            .unwrap(),
        TerrainBoundaryRaise::RaisedAndRearmed
    );
    let progress = store.terrain_tidy_progress().unwrap();
    assert_eq!(progress.run, 2);
    assert_eq!(progress.armed_at_ms, Some(C9_T0 + 600_000));
    assert_eq!(
        (
            progress.examined,
            progress.cleared,
            progress.requeued,
            progress.removed,
            progress.kept,
            progress.drives_changed
        ),
        (0, 0, 0, 0, 0, 0)
    );
    assert!(!store.terrain_tidy_complete().unwrap());
}

#[test]
fn c9_st14_window() {
    // C9 section 5: pin the bounds independently of the callers using them.
    assert_eq!(K_SCAN_ROWS, 5_000);
    assert_eq!(K_PASS_CANDIDATES, 1_000);
    let (_directory, store, vehicle) = c9_store();
    let mut connection = store.open().unwrap();
    let transaction = connection.transaction().unwrap();
    for drive in 1..=12_i64 {
        transaction.execute("INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json) VALUES (?1, ?2, 1, '{}')", params![vehicle.to_string(), drive]).unwrap();
        for id in (drive - 1) * 1_000 + 1..=drive * 1_000 {
            let position = c9_position(id, Some(drive), Some(5));
            transaction.execute("INSERT INTO materialised_positions(vehicle_id, position_id, drive_id, car_id, position_json) VALUES (?1, ?2, ?3, 1, ?4)", params![vehicle.to_string(), id, drive, serde_json::to_string(&position).unwrap()]).unwrap();
        }
    }
    transaction.commit().unwrap();
    let mut v = c9_vehicle(&store, vehicle);
    for (lower, upper) in [
        (0, 5_000),
        (5_000, 10_000),
        (10_000, 12_000),
        (12_000, 12_000),
    ] {
        v.cursor_position_id = lower;
        assert_eq!(c9_ids(&store, &v), (vec![], upper));
    }
    // C9 R8: zero qualifying rows fit even a zero candidate budget.
    v.cursor_position_id = 0;
    let result = store.terrain_forward_window(&v, K_SCAN_ROWS, 0).unwrap();
    assert!(result.candidates.is_empty());
    assert_eq!(result.examined_through, 5_000);
    connection.execute("UPDATE materialised_positions SET position_json = json_set(position_json, '$.elevation', NULL)", []).unwrap();
    v.cursor_position_id = 0;
    let result = store.terrain_forward_window(&v, 5, 2).unwrap();
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|p| p.position.id)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(result.examined_through, 2);
    assert_eq!(
        store
            .terrain_forward_window(&v, 2, 2)
            .unwrap()
            .examined_through,
        2
    );
    assert_eq!(
        store
            .terrain_forward_window(&v, 0, 2)
            .unwrap()
            .examined_through,
        0
    );
    assert_eq!(
        store
            .terrain_forward_window(&v, 5, 0)
            .unwrap()
            .examined_through,
        0
    );
    // C9 R8: exactly the budget may qualify before trailing skipped rows.
    connection
        .execute(
            "UPDATE materialised_positions SET drive_id = NULL
        WHERE vehicle_id = ?1 AND position_id BETWEEN 3 AND 5",
            [vehicle.to_string()],
        )
        .unwrap();
    let result = store.terrain_forward_window(&v, 5, 2).unwrap();
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|p| p.position.id)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(result.examined_through, 5);

    // C9 R8: scan_rows counts rows across gaps in the position ids.
    let (_gapped_directory, gapped, vehicle) = c9_store();
    c9_commit(
        &gapped,
        vehicle,
        vec![c9_drive(1)],
        [10, 20, 30, 40]
            .into_iter()
            .map(|id| c9_position(id, Some(1), None))
            .collect(),
    );
    let mut v = c9_vehicle(&gapped, vehicle);
    for (lower, upper, expected) in [(0, 20, vec![10, 20]), (20, 40, vec![30, 40])] {
        v.cursor_position_id = lower;
        let result = gapped.terrain_forward_window(&v, 2, 2).unwrap();
        assert_eq!(result.examined_through, upper);
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|row| row.position.id)
                .collect::<Vec<_>>(),
            expected
        );
    }
}

#[test]
fn c9_st23_retry_order() {
    assert_eq!(K_RETRY_SLICE, 250);
    let (_directory, store, vehicle) = c9_store();
    let other = b3_c8_sa::c8_vehicle(&store, "two");
    c9_commit(
        &store,
        vehicle,
        vec![c9_drive(1)],
        (10..=12).map(|id| c9_position(id, Some(1), None)).collect(),
    );
    c9_commit(
        &store,
        other,
        vec![c9_drive(1)],
        vec![c9_position(5, Some(1), None)],
    );
    let connection = store.open().unwrap();
    for (v, id, retry) in [
        (vehicle, 10, C9_T0 - 3),
        (vehicle, 11, C9_T0 - 3),
        (vehicle, 12, C9_T0 + 1),
        (other, 5, C9_T0 - 5),
    ] {
        c9_provenance(&connection, v, id, "failed", retry);
    }
    let retries = store.terrain_due_retries(C9_T0, K_RETRY_SLICE).unwrap();
    assert_eq!(
        retries
            .iter()
            .map(|r| (r.vehicle_id, r.position_id, r.retry_after_ms, r.attempts))
            .collect::<Vec<_>>(),
        vec![
            (other, 5, C9_T0 - 5, 2),
            (vehicle, 10, C9_T0 - 3, 2),
            (vehicle, 11, C9_T0 - 3, 2)
        ]
    );
    assert!(retries.iter().all(|r| r.candidate.is_some()));
    assert_eq!(store.terrain_due_retries(C9_T0, 2).unwrap(), retries[..2]);
    assert_eq!(store.terrain_next_retry_at().unwrap(), Some(C9_T0 - 5));
    connection
        .execute(
            "DELETE FROM materialised_positions WHERE vehicle_id = ?1 AND position_id = 5",
            [other.to_string()],
        )
        .unwrap();
    assert!(
        store.terrain_due_retries(C9_T0, 1).unwrap()[0]
            .candidate
            .is_none()
    );
    connection.execute("UPDATE materialised_positions SET position_json = json_set(position_json, '$.elevation', 0) WHERE vehicle_id = ?1 AND position_id = 10", [vehicle.to_string()]).unwrap();
    assert!(
        store
            .terrain_due_retries(C9_T0, 250)
            .unwrap()
            .iter()
            .all(|r| r.candidate.is_none())
    );
    connection
        .execute("DELETE FROM terrain_elevation_provenance", [])
        .unwrap();
    assert_eq!(store.terrain_next_retry_at().unwrap(), None);
}

#[test]
fn c9_st62_backlog_caps() {
    assert_eq!(K_STATUS_COUNT_CAP, 10_000);
    let (_directory, store, vehicle) = c9_store();
    let other = b3_c8_sa::c8_vehicle(&store, "two");
    let mut connection = store.open().unwrap();
    let transaction = connection.transaction().unwrap();
    for id in 1..=12_000_i64 {
        transaction.execute("INSERT INTO materialised_positions(vehicle_id, position_id, car_id, position_json) VALUES (?1, ?2, 1, '{}')", params![vehicle.to_string(), id]).unwrap();
    }
    for id in 1..=10_500 {
        c9_provenance(&transaction, other, id, "failed", C9_T0);
    }
    for id in 1..=5 {
        c9_provenance(
            &transaction,
            vehicle,
            id,
            "failed",
            if id <= 3 { C9_T0 } else { C9_T0 + 1 },
        );
    }
    transaction.execute("INSERT INTO materialised_positions(vehicle_id, position_id, car_id, position_json) VALUES (?1, 10500, 1, '{}')", [other.to_string()]).unwrap();
    transaction.execute("INSERT INTO terrain_enrichment_state(vehicle_id, cursor_position_id, updated_at_ms) VALUES (?1, 10500, ?2)", params![other.to_string(), C9_T0]).unwrap();
    transaction.commit().unwrap();
    let opens_before = store.instrumentation().snapshot().read_only_opens;
    assert_eq!(
        store.terrain_backlog(C9_T0, K_STATUS_COUNT_CAP).unwrap(),
        TerrainBacklog {
            rows_unexamined: 10_000,
            retries_due: 10_003,
            retries_waiting: 2
        }
    );
    // C9 R24: the backlog reads vehicle cursors on its own connection.
    assert_eq!(
        store.instrumentation().snapshot().read_only_opens - opens_before,
        1
    );
    // C9 R24: the waiting count has its own cap, independent of due retries.
    let transaction = connection.transaction().unwrap();
    for id in 20_000..30_500 {
        c9_provenance(&transaction, other, id, "failed", C9_T0 + 1);
    }
    transaction.commit().unwrap();
    assert_eq!(
        store.terrain_backlog(C9_T0, K_STATUS_COUNT_CAP).unwrap(),
        TerrainBacklog {
            rows_unexamined: 10_000,
            retries_due: 10_003,
            retries_waiting: 10_002
        }
    );
    assert_eq!(
        store.terrain_backlog(C9_T0, 2).unwrap(),
        TerrainBacklog {
            rows_unexamined: 2,
            retries_due: 4,
            retries_waiting: 4
        }
    );
    assert_eq!(
        store.terrain_backlog(C9_T0, 0).unwrap(),
        TerrainBacklog::default()
    );
}

fn c9_plan(connection: &Connection, sql: &str, parameters: &[&dyn rusqlite::ToSql]) -> String {
    connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map(parameters, |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("\n")
}

#[test]
fn c9_st42_query_plans() {
    let (_directory, store, vehicle) = c9_store();
    let connection = store.open().unwrap();
    let v = vehicle.to_string();
    // C9 R24: only failed rows need the per-vehicle retry range.
    let index_sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name = 'terrain_provenance_vehicle_retry'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let index_sql = index_sql.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        index_sql.ends_with("(vehicle_id, retry_after_ms) WHERE status = 'failed'"),
        "{index_sql}"
    );
    let boundary_plan = c9_plan(
        &connection,
        TERRAIN_BOUNDARY_SUCCESS_SQL,
        &[&v, &500_i64, &C9_T0],
    );
    assert!(boundary_plan.contains("SEARCH t USING INDEX sqlite_autoindex_terrain_elevation_provenance_1 (vehicle_id=? AND position_id<?)"), "{boundary_plan}");
    assert!(!boundary_plan.contains("TEMP B-TREE"), "{boundary_plan}");
    for sql in [TERRAIN_BACKLOG_DUE_SQL, TERRAIN_BACKLOG_WAITING_SQL] {
        let plan = c9_plan(&connection, sql, &[&v, &C9_T0, &K_STATUS_COUNT_CAP]);
        assert!(
            plan.contains(
                "SEARCH terrain_elevation_provenance USING COVERING INDEX terrain_provenance_vehicle_retry"
            ),
            "{plan}"
        );
        assert!(plan.contains("vehicle_id=? AND retry_after_ms"), "{plan}");
    }
    for (sql, parameters) in [
        (
            TERRAIN_WINDOW_IDS_SQL,
            vec![&v as &dyn rusqlite::ToSql, &0_i64, &K_SCAN_ROWS],
        ),
        (TERRAIN_WINDOW_ROWS_SQL, vec![&v, &0_i64, &5_000_i64]),
        (TERRAIN_DRIVE_ELIGIBLE_SQL, vec![&v, &1_i64]),
        (TERRAIN_DUE_RETRIES_SQL, vec![&C9_T0, &K_RETRY_SLICE]),
        (
            TERRAIN_WATERMARK_BOUNDARY_SQL,
            vec![&v, &IMPORT_SOURCE_KIND],
        ),
    ] {
        let plan = c9_plan(&connection, sql, &parameters);
        assert!(!plan.contains("SCAN materialised_positions"), "{plan}");
        assert!(
            !plan.contains("SCAN terrain_elevation_provenance"),
            "{plan}"
        );
        assert!(!plan.contains("SCAN p"), "{plan}");
        assert!(!plan.contains("SCAN t"), "{plan}");
        if sql == TERRAIN_DRIVE_ELIGIBLE_SQL {
            assert!(plan.contains("materialised_positions_drive"), "{plan}");
        }
        if sql == TERRAIN_DUE_RETRIES_SQL {
            assert!(plan.contains("terrain_provenance_retry"), "{plan}");
        }
        if sql == TERRAIN_WINDOW_IDS_SQL || sql == TERRAIN_WINDOW_ROWS_SQL {
            assert!(plan.contains("SEARCH"), "{plan}");
        }
    }
}

#[test]
fn c9_st42_retry_order_has_no_temp_sort() {
    let (_directory, store, vehicle) = c9_store();
    let connection = store.open().unwrap();
    let plan = c9_plan(&connection, TERRAIN_DUE_RETRIES_SQL, &[&C9_T0, &2_u32]);
    assert!(plan.contains("terrain_provenance_retry"), "{plan}");
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    // C9 ST42: malformed JSON beyond the SQL limit must never be processed.
    for id in 1..=3 {
        c9_provenance(&connection, vehicle, id, "failed", C9_T0);
    }
    connection.execute("INSERT INTO materialised_positions(vehicle_id, position_id, drive_id, car_id, position_json) VALUES (?1, 3, 1, 1, '{}')", [vehicle.to_string()]).unwrap();
    assert_eq!(store.terrain_due_retries(C9_T0, 2).unwrap().len(), 2);
    assert!(store.terrain_due_retries(C9_T0, 3).is_err());
    let mut statement = connection
        .prepare("PRAGMA index_info(terrain_provenance_retry)")
        .unwrap();
    let columns = statement
        .query_map([], |r| r.get::<_, String>(2))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        columns,
        vec!["status", "retry_after_ms", "vehicle_id", "position_id"]
    );
}

fn c9_predecessor() -> (
    tempfile::TempDir,
    HubStore,
    Uuid,
    Vec<rusqlite::types::Value>,
) {
    let (directory, store, vehicle) = c9_store();
    c9_commit(
        &store,
        vehicle,
        vec![c9_drive(1)],
        (1..=12)
            .map(|id| c9_position(id, Some(1), (id == 4).then_some(50)))
            .collect(),
    );
    let connection = store.open().unwrap();
    c9_provenance(&connection, vehicle, 4, "success", 0);
    let before = connection
        .query_row("SELECT * FROM terrain_elevation_provenance", [], |row| {
            (0..15).map(|i| row.get(i)).collect::<Result<Vec<_>, _>>()
        })
        .unwrap();
    let predecessor = TERRAIN_SELECTION_SCHEMA_VERSION - 1;
    connection
        .execute_batch(&format!(
            "BEGIN IMMEDIATE; {REWIND_V77_SQL} PRAGMA user_version = {predecessor}; COMMIT;"
        ))
        .unwrap();
    connection.execute("INSERT INTO terrain_enrichment_state(vehicle_id, cursor_position_id, updated_at_ms) VALUES (?1, 10, ?2)", params![vehicle.to_string(), C9_T0]).unwrap();
    assert_eq!(schema_version(&connection).unwrap(), predecessor);
    assert!(!b3_c8_sa::table_exists(&connection, "terrain_tidy_state"));
    let columns = connection
        .prepare("PRAGMA index_info(terrain_provenance_retry)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(2))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(columns, vec!["status", "retry_after_ms"]);
    (directory, store, vehicle, before)
}

fn c9_assert_upgrade(store: &HubStore, vehicle: Uuid, before: &[rusqlite::types::Value]) {
    let connection = store.open().unwrap();
    assert_eq!(schema_version(&connection).unwrap(), SCHEMA_VERSION);
    const { assert!(SCHEMA_VERSION >= TERRAIN_SELECTION_SCHEMA_VERSION) };
    let v = c9_vehicle(store, vehicle);
    assert_eq!(v.cursor_position_id, 0);
    assert_eq!(v.stored_boundary, ImportBoundary::default());
    assert_eq!(
        c9_ids(store, &v),
        (vec![1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12], 12)
    );
    let after: Vec<rusqlite::types::Value> = connection
        .query_row("SELECT * FROM terrain_elevation_provenance", [], |row| {
            (0..15).map(|i| row.get(i)).collect::<Result<Vec<_>, _>>()
        })
        .unwrap();
    assert_eq!(after, before);
    assert_eq!(
        store.terrain_tidy_progress().unwrap(),
        TerrainTidyProgress {
            run: 1,
            examined: 0,
            cleared: 0,
            requeued: 0,
            removed: 0,
            kept: 0,
            drives_changed: 0,
            armed_at_ms: None,
            completed_at_ms: None
        }
    );
    assert!(!store.terrain_tidy_complete().unwrap());
    connection
        .execute(
            "UPDATE terrain_enrichment_state SET cursor_position_id = 7",
            [],
        )
        .unwrap();
}

#[test]
fn c9_st55_upgrade() {
    let (directory, store, vehicle, before) = c9_predecessor();
    drop(store);
    let store = HubStore::initialize(directory.path()).unwrap();
    c9_assert_upgrade(&store, vehicle, &before);
    drop(store);
    let store = HubStore::initialize(directory.path()).unwrap();
    assert_eq!(c9_vehicle(&store, vehicle).cursor_position_id, 7);
    store
        .open()
        .unwrap()
        .execute("DELETE FROM terrain_tidy_state", [])
        .unwrap();
    assert!(store.terrain_tidy_complete().is_err());
    assert!(store.terrain_tidy_progress().is_err());
}

#[test]
fn c9_st55b_standalone_upgrade() {
    assert!(
        REWIND_V76_SQL.starts_with(REWIND_V77_SQL),
        "assertion failed: c9_st55b_standalone_upgrade: older rewinds begin with the complete terrain-selection undo"
    );
    let (directory, store, vehicle, before) = c9_predecessor();
    let connection = store.open().unwrap();
    let data_version = connection
        .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
        .unwrap();
    drop(store);
    let preflight = HubStore::preflight_standalone_schema_read_only(directory.path());
    assert!(
        preflight.is_ok(),
        "assertion failed: c9_st55b_standalone_upgrade: standalone preflight accepts the terrain predecessor: {preflight:?}"
    );
    assert_eq!(
        schema_version(&connection).unwrap(),
        TERRAIN_SELECTION_SCHEMA_VERSION - 1
    );
    assert_eq!(
        connection
            .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        data_version
    );
    assert!(!b3_c8_sa::table_exists(&connection, "terrain_tidy_state"));
    assert!(HubStore::open_read_only(directory.path()).is_err());
    drop(connection);
    let store = HubStore::initialize(directory.path()).unwrap();
    c9_assert_upgrade(&store, vehicle, &before);
    HubStore::preflight_standalone_schema_read_only(directory.path()).unwrap();
}

#[test]
fn c9_st11_zero_success_and_missing_elevation() {
    let (_directory, store, vehicle) = c9_store();
    c9_commit(
        &store,
        vehicle,
        vec![c9_drive(1), c9_drive(2)],
        vec![
            c9_position(1, Some(1), Some(0)),
            c9_position(2, Some(1), None),
            c9_position(3, Some(2), Some(50)),
            c9_position(4, Some(2), None),
        ],
    );
    let connection = store.open().unwrap();
    c9_provenance(&connection, vehicle, 3, "success", 0);
    connection.execute("UPDATE materialised_positions SET position_json = json_remove(position_json, '$.elevation')
        WHERE vehicle_id = ?1 AND position_id = 4", [vehicle.to_string()]).unwrap();
    // C9 R5 and C9 R7: zero excludes its null sibling; Hub success does not.
    assert_eq!(c9_ids(&store, &c9_vehicle(&store, vehicle)), (vec![4], 4));
    connection
        .execute(
            "UPDATE terrain_elevation_provenance SET status = 'void'
        WHERE vehicle_id = ?1 AND position_id = 3",
            [vehicle.to_string()],
        )
        .unwrap();
    assert_eq!(c9_ids(&store, &c9_vehicle(&store, vehicle)), (vec![], 4));
    connection
        .execute(
            "UPDATE terrain_elevation_provenance SET status = 'success' WHERE position_id = 3",
            [],
        )
        .unwrap();
    for (latitude, longitude, expected) in [
        (90.0, 180.0, vec![4]),
        (90.0, 180.1, vec![]),
        (-90.0, -180.0, vec![4]),
        (-90.1, -180.0, vec![]),
    ] {
        connection.execute("UPDATE materialised_positions SET position_json = json_set(position_json, '$.latitude', ?1, '$.longitude', ?2)
            WHERE vehicle_id = ?3 AND position_id = 4", params![latitude, longitude, vehicle.to_string()]).unwrap();
        assert_eq!(c9_ids(&store, &c9_vehicle(&store, vehicle)), (expected, 4));
    }
    connection.execute("UPDATE materialised_positions SET position_json = json_set(position_json, '$.latitude', 10, '$.longitude', 20) WHERE position_id = 4", []).unwrap();
    c9_provenance(&connection, vehicle, 4, "failed", C9_T0);
    for status in ["failed", "void", "success"] {
        connection
            .execute(
                "UPDATE terrain_elevation_provenance SET status = ?1 WHERE position_id = 4",
                [status],
            )
            .unwrap();
        assert_eq!(c9_ids(&store, &c9_vehicle(&store, vehicle)), (vec![], 4));
    }
}

#[test]
fn c9_st12_independent_watermark_maxima() {
    let (_directory, store, vehicle) = c9_store();
    c9_commit(
        &store,
        vehicle,
        vec![c9_drive(1)],
        (1..=3).map(|id| c9_position(id, Some(1), None)).collect(),
    );
    let first = store
        .register_source(&SourceDescriptor::new(IMPORT_SOURCE_KIND, "first"), 1_000)
        .unwrap();
    let second = store
        .register_source(&SourceDescriptor::new(IMPORT_SOURCE_KIND, "second"), 1_000)
        .unwrap();
    let connection = store.open().unwrap();
    c9_watermark(
        &connection,
        first.source_id,
        vehicle,
        "positions",
        2,
        C9_T0 + 1_000,
    );
    c9_watermark(
        &connection,
        second.source_id,
        vehicle,
        "positions",
        1,
        C9_T0 + 2_000,
    );
    let v = c9_vehicle(&store, vehicle);
    assert_eq!(v.stored_boundary, ImportBoundary::default());
    assert_eq!(
        v.watermark_boundary,
        ImportBoundary {
            max_position_id: 2,
            max_date_ms: C9_T0 + 2_000
        }
    );
    assert_eq!(c9_ids(&store, &v), (vec![3], 3));
    // C9 R6: neither stored field is allowed to shrink when the other rises.
    for boundary in [
        ImportBoundary {
            max_position_id: 3,
            max_date_ms: C9_T0 + 1_000,
        },
        ImportBoundary {
            max_position_id: 1,
            max_date_ms: C9_T0 + 3_000,
        },
    ] {
        assert_eq!(
            store
                .record_terrain_import_boundary(vehicle, boundary, C9_T0)
                .unwrap(),
            TerrainBoundaryRaise::Raised
        );
    }
    let v = c9_vehicle(&store, vehicle);
    assert_eq!(
        v.stored_boundary,
        ImportBoundary {
            max_position_id: 3,
            max_date_ms: C9_T0 + 3_000
        }
    );
    assert_eq!(c9_ids(&store, &v), (vec![], 3));
}

#[test]
fn c9_st23_vehicle_ties_and_retirements() {
    let (_directory, store, vehicle) = c9_store();
    let other = b3_c8_sa::c8_vehicle(&store, "retry-tie");
    let mut vehicles = [vehicle, other];
    vehicles.sort_by_key(Uuid::to_string);
    for v in vehicles {
        c9_commit(
            &store,
            v,
            vec![c9_drive(1)],
            vec![c9_position(1, Some(1), None), c9_position(2, Some(1), None)],
        );
    }
    let connection = store.open().unwrap();
    for v in vehicles.into_iter().rev() {
        for id in [2, 1] {
            c9_provenance(&connection, v, id, "failed", C9_T0);
        }
    }
    let retries = store.terrain_due_retries(C9_T0, 3).unwrap();
    assert_eq!(
        retries
            .iter()
            .map(|r| (r.vehicle_id, r.position_id))
            .collect::<Vec<_>>(),
        vec![(vehicles[0], 1), (vehicles[0], 2), (vehicles[1], 1)]
    );
    assert!(store.terrain_due_retries(C9_T0 - 1, 3).unwrap().is_empty());
    assert!(store.terrain_due_retries(C9_T0, 0).unwrap().is_empty());
    let listed = store.terrain_vehicles().unwrap();
    assert!(
        listed
            .windows(2)
            .all(|pair| pair[0].vehicle_id.to_string() < pair[1].vehicle_id.to_string())
    );
    let before = connection
        .query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))
        .unwrap();
    // C9 R10: both the stored and watermark boundaries retire imported retries.
    store
        .record_terrain_import_boundary(
            vehicles[0],
            ImportBoundary {
                max_position_id: 1,
                max_date_ms: C9_T0 + 1_000,
            },
            C9_T0,
        )
        .unwrap();
    let source = store
        .register_source(
            &SourceDescriptor::new(IMPORT_SOURCE_KIND, "retry-import"),
            1_000,
        )
        .unwrap();
    c9_watermark(
        &connection,
        source.source_id,
        vehicles[1],
        "positions",
        1,
        C9_T0 + 1_000,
    );
    let retries = store.terrain_due_retries(C9_T0, 4).unwrap();
    assert_eq!(
        retries
            .iter()
            .map(|r| r.candidate.is_some())
            .collect::<Vec<_>>(),
        vec![false, true, false, true]
    );
    // C9 R10: selection itself does not delete retirement rows or write state.
    let selected = connection
        .query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))
        .unwrap();
    assert!(selected > before);
    assert_eq!(store.terrain_due_retries(C9_T0, 4).unwrap(), retries);
    assert_eq!(
        connection
            .query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        selected
    );
    assert_eq!(
        b3_c8_sa::count(&connection, "terrain_elevation_provenance"),
        4
    );
    for (id, status) in [(3, "success"), (4, "void")] {
        c9_provenance(&connection, vehicle, id, status, 0);
    }
    assert_eq!(store.terrain_next_retry_at().unwrap(), Some(C9_T0));
    assert_eq!(store.terrain_due_retries(C9_T0, 10).unwrap().len(), 4);
}

fn c9_schema(connection: &Connection) -> Vec<(String, String, String)> {
    connection
        .prepare(
            "SELECT type, name, sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type, name",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn c9_st55_failed_step_is_atomic() {
    let (_directory, store, vehicle, before) = c9_predecessor();
    let connection = store.open().unwrap();
    // C9 R61: fail the final DDL after both columns, the reset and index rebuild.
    connection.execute_batch("CREATE INDEX terrain_provenance_vehicle_retry ON terrain_elevation_provenance(attempts);").unwrap();
    let schema_before = c9_schema(&connection);
    assert!(matches!(migrate(&connection), Err(StoreError::Migrate(_))));
    assert!(connection.is_autocommit());
    assert_eq!(
        schema_version(&connection).unwrap(),
        TERRAIN_SELECTION_SCHEMA_VERSION - 1
    );
    assert!(
        c9_schema(&connection) == schema_before,
        "C9 R61: failed terrain step changed the schema"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT cursor_position_id FROM terrain_enrichment_state WHERE vehicle_id = ?1",
                [vehicle.to_string()],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        10
    );
    let provenance: Vec<rusqlite::types::Value> = connection
        .query_row("SELECT * FROM terrain_elevation_provenance", [], |r| {
            (0..15).map(|i| r.get(i)).collect::<Result<Vec<_>, _>>()
        })
        .unwrap();
    assert_eq!(provenance, before);
    connection
        .execute_batch("DROP INDEX terrain_provenance_vehicle_retry;")
        .unwrap();
    migrate(&connection).unwrap();
    c9_assert_upgrade(&store, vehicle, &before);
}

#[test]
fn c9_st51_boundary_raise_is_atomic_and_quiet() {
    let (_directory, store, vehicle) = c9_store();
    c9_commit(
        &store,
        vehicle,
        vec![c9_drive(1)],
        vec![c9_position(1, Some(1), Some(50))],
    );
    let connection = store.open().unwrap();
    c9_provenance(&connection, vehicle, 1, "success", 0);
    let boundary = ImportBoundary {
        max_position_id: 1,
        max_date_ms: C9_T0 + 1_000,
    };
    connection
        .execute("DELETE FROM terrain_tidy_state", [])
        .unwrap();
    // C9 R58: a failed re-arm cannot leave the boundary raised without a tidy.
    assert!(
        store
            .record_terrain_import_boundary(vehicle, boundary, C9_T0)
            .is_err()
    );
    assert_eq!(
        c9_vehicle(&store, vehicle).stored_boundary,
        ImportBoundary::default()
    );
    assert_eq!(b3_c8_sa::count(&connection, "terrain_enrichment_state"), 0);
    connection
        .execute(
            "INSERT INTO terrain_tidy_state(step, resume_vehicle_id, resume_position_id,
        examined, cleared, requeued, removed, kept, drives_changed, completed_at_ms)
        VALUES ('earlier_build_values_v1', ?1, 1, 9, 1, 2, 3, 4, 5, ?2)",
            params![vehicle.to_string(), C9_T0],
        )
        .unwrap();
    assert!(store.terrain_tidy_complete().unwrap());
    let progress = store.terrain_tidy_progress().unwrap();
    assert_eq!(
        (
            progress.examined,
            progress.cleared,
            progress.requeued,
            progress.removed,
            progress.kept,
            progress.drives_changed
        ),
        (9, 1, 2, 3, 4, 5)
    );
    assert_eq!(
        store
            .record_terrain_import_boundary(vehicle, boundary, C9_T0)
            .unwrap(),
        TerrainBoundaryRaise::RaisedAndRearmed
    );
    let progress = store.terrain_tidy_progress().unwrap();
    assert_eq!(
        progress,
        TerrainTidyProgress {
            run: 2,
            examined: 0,
            cleared: 0,
            requeued: 0,
            removed: 0,
            kept: 0,
            drives_changed: 0,
            armed_at_ms: Some(C9_T0),
            completed_at_ms: None
        }
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT resume_vehicle_id, resume_position_id FROM terrain_tidy_state",
                [],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            )
            .unwrap(),
        (String::new(), 0)
    );
    let before = connection
        .query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))
        .unwrap();
    assert_eq!(
        store
            .record_terrain_import_boundary(vehicle, boundary, C9_T0 + 1)
            .unwrap(),
        TerrainBoundaryRaise::Unchanged
    );
    assert_eq!(store.terrain_tidy_progress().unwrap(), progress);
    assert_eq!(
        connection
            .query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        before
    );
}

#[test]
fn c9_st42_backlog_index_bounds() {
    let (_directory, store, vehicle) = c9_store();
    let connection = store.open().unwrap();
    let v = vehicle.to_string();
    let plan = c9_plan(
        &connection,
        TERRAIN_BACKLOG_ROWS_SQL,
        &[&v, &0_i64, &K_STATUS_COUNT_CAP],
    );
    assert!(plan.contains("SEARCH materialised_positions USING COVERING INDEX sqlite_autoindex_materialised_positions_1 (vehicle_id=? AND position_id>?)"), "{plan}");
    for sql in [TERRAIN_BACKLOG_DUE_SQL, TERRAIN_BACKLOG_WAITING_SQL] {
        let unforced = sql.replace(" INDEXED BY terrain_provenance_vehicle_retry", "");
        let with_index = c9_plan(&connection, &unforced, &[&v, &C9_T0, &K_STATUS_COUNT_CAP]);
        assert!(
            with_index.contains("vehicle_id=? AND retry_after_ms"),
            "{with_index}"
        );
        connection
            .execute_batch("BEGIN; DROP INDEX terrain_provenance_vehicle_retry;")
            .unwrap();
        let without_index = c9_plan(&connection, &unforced, &[&v, &C9_T0, &K_STATUS_COUNT_CAP]);
        connection.execute_batch("ROLLBACK;").unwrap();
        // C9 R24: a range over all vehicles or all statuses can visit unbounded
        // non-matches before LIMIT produces even one row for this vehicle.
        assert!(
            !without_index.contains("vehicle_id=? AND retry_after_ms"),
            "{without_index}"
        );
        println!(
            "C9 R24 with per-vehicle index:\n{with_index}\nC9 R24 without per-vehicle index:\n{without_index}"
        );
    }
}

#[test]
fn c9_st51_rearm_requires_current_imported_success() {
    let (_directory, store, vehicle) = c9_store();
    c9_commit(
        &store,
        vehicle,
        vec![c9_drive(1)],
        vec![
            c9_position(1, Some(1), None),
            c9_position(2, Some(1), Some(50)),
        ],
    );
    let connection = store.open().unwrap();
    c9_provenance(&connection, vehicle, 1, "failed", C9_T0);
    c9_provenance(&connection, vehicle, 2, "success", 0);
    c9_provenance(&connection, vehicle, 4, "success", 0);
    connection
        .execute(
            "UPDATE terrain_tidy_state SET completed_at_ms = ?1",
            [C9_T0],
        )
        .unwrap();
    let progress = store.terrain_tidy_progress().unwrap();
    // C9 R58: a failed row, an uncovered date/id and an orphan cannot re-arm.
    for id in [1, 2, 4] {
        assert_eq!(
            store
                .record_terrain_import_boundary(
                    vehicle,
                    ImportBoundary {
                        max_position_id: id,
                        max_date_ms: C9_T0 + 1_000,
                    },
                    C9_T0
                )
                .unwrap(),
            TerrainBoundaryRaise::Raised
        );
        assert_eq!(store.terrain_tidy_progress().unwrap(), progress);
    }
    assert_eq!(
        store
            .record_terrain_import_boundary(
                vehicle,
                ImportBoundary {
                    max_position_id: 4,
                    max_date_ms: C9_T0 + 2_000,
                },
                C9_T0
            )
            .unwrap(),
        TerrainBoundaryRaise::RaisedAndRearmed
    );
    assert_eq!(store.terrain_tidy_progress().unwrap().run, 2);
}

#[test]
fn c9_st55_repeated_rewinds_restore_fresh_schema() {
    let (directory, store, vehicle) = c9_store();
    let connection = store.open().unwrap();
    let fresh_schema = c9_schema(&connection);
    connection
        .execute(
            "INSERT INTO terrain_enrichment_state(vehicle_id, cursor_position_id, updated_at_ms,
        import_position_id_max, import_position_date_max_ms) VALUES (?1, 19, ?2, 7, ?2)",
            params![vehicle.to_string(), C9_T0],
        )
        .unwrap();
    for rewind in [REWIND_V77_SQL, REWIND_V77_SQL, REWIND_V76_SQL] {
        connection
            .execute_batch(&format!("BEGIN IMMEDIATE; {rewind} COMMIT;"))
            .unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT cursor_position_id, updated_at_ms
            FROM terrain_enrichment_state WHERE vehicle_id = ?1",
                    [vehicle.to_string()],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
                )
                .unwrap(),
            (19, C9_T0)
        );
    }
    let predecessor = GEOFENCE_STEP_SCHEMA_VERSION - 1;
    connection
        .execute_batch(&format!("PRAGMA user_version = {predecessor};"))
        .unwrap();
    drop(connection);
    drop(store);
    let migrated = HubStore::initialize(directory.path()).unwrap();
    let connection = migrated.open().unwrap();
    assert_eq!(schema_version(&connection).unwrap(), SCHEMA_VERSION);
    let strip = |schema: Vec<(String, String, String)>| {
        schema
            .into_iter()
            .map(|(kind, name, sql)| {
                (
                    kind,
                    name,
                    sql.split_whitespace().collect::<Vec<_>>().join(" "),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(strip(c9_schema(&connection)), strip(fresh_schema));
    assert_eq!(c9_vehicle(&migrated, vehicle).cursor_position_id, 0);
}

#[test]
fn c9_r8_forward_window_uses_one_snapshot() {
    let (_directory, store, vehicle) = c9_store();
    c9_commit(
        &store,
        vehicle,
        vec![c9_drive(1)],
        vec![
            c9_position(10, Some(1), None),
            c9_position(20, Some(1), None),
        ],
    );
    let v = c9_vehicle(&store, vehicle);
    let writer = store.open().unwrap();
    let text = vehicle.to_string();
    TERRAIN_WINDOW_AFTER_UPPER.with(|slot| *slot.borrow_mut() = Some(Box::new(move || {
        writer.execute("UPDATE materialised_positions SET position_json = json_set(position_json, '$.elevation', 120)
            WHERE vehicle_id = ?1 AND position_id = 10", [text]).unwrap();
    })));
    let result = store.terrain_forward_window(&v, 2, 2).unwrap();
    assert_eq!(result.examined_through, 20);
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|row| row.position.id)
            .collect::<Vec<_>>(),
        vec![10, 20]
    );
    let next = store.terrain_forward_window(&v, 2, 2).unwrap();
    assert_eq!(next.examined_through, 20);
    assert!(next.candidates.is_empty());
}

#[test]
fn c9_r58_boundary_commit_reports_write_error() {
    let (_directory, store, vehicle) = c9_store();
    let connection = store.open().unwrap();
    // C9 R58: defer a synthetic foreign-key failure until the boundary commit.
    connection
        .execute_batch(
            "CREATE TABLE c9_boundary_commit_fault (
        vehicle_id TEXT REFERENCES vehicles(vehicle_id) DEFERRABLE INITIALLY DEFERRED);
        CREATE TRIGGER c9_boundary_commit_fault AFTER INSERT ON terrain_enrichment_state
        BEGIN INSERT INTO c9_boundary_commit_fault VALUES ('missing-synthetic-vehicle'); END;",
        )
        .unwrap();
    assert!(matches!(
        store.record_terrain_import_boundary(
            vehicle,
            ImportBoundary {
                max_position_id: 1,
                max_date_ms: C9_T0
            },
            C9_T0
        ),
        Err(StoreError::LifecycleWrite(_))
    ));
    assert_eq!(b3_c8_sa::count(&connection, "terrain_enrichment_state"), 0);
    assert_eq!(b3_c8_sa::count(&connection, "c9_boundary_commit_fault"), 0);
}
