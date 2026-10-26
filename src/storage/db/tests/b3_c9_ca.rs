// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use serde_json::json;

const C9_T0: i64 = 1_760_000_000_000;
const C9_COMMIT: i64 = C9_T0 + 60_000;

fn fixture() -> (tempfile::TempDir, HubStore, Uuid) {
    let (directory, store) = b3_c8_sa::c8_store();
    let vehicle = b3_c8_sa::c8_vehicle(&store, "terrain-commit");
    (directory, store, vehicle)
}

fn position(id: i64, drive: i64) -> ProjectionPosition {
    let mut value = position_row(id, Some(drive), C9_T0 + (id - 1) * 1_000);
    value.latitude = 10.5;
    value.longitude = 20.0 + (id - 1) as f64 * 6.0 / 1200.0;
    value
}

fn seed(store: &HubStore, vehicle: Uuid, positions: &[ProjectionPosition]) {
    let connection = store.open().unwrap();
    let mut drives = HashSet::new();
    for position in positions {
        let drive = position.drive_id.unwrap();
        if drives.insert(drive) {
            let mut row = drive_row(drive, C9_COMMIT);
            row.start_date_ms = C9_T0 - 1;
            connection
                .execute(
                    "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
                 VALUES (?1, ?2, 1, ?3)",
                    params![
                        vehicle.to_string(),
                        drive,
                        serde_json::to_string(&row).unwrap()
                    ],
                )
                .unwrap();
        }
        let mut value = serde_json::to_value(position).unwrap();
        value["future_member"] = json!({"keep": [1, "two", null]});
        connection.execute(
            "INSERT INTO materialised_positions(vehicle_id, position_id, drive_id, car_id, position_json, speed)
             VALUES (?1, ?2, ?3, 1, ?4, 123)",
            params![vehicle.to_string(), position.id, drive, value.to_string()],
        ).unwrap();
    }
}

fn tile() -> TileProvenance {
    TileProvenance {
        tile_name: "N10E020".into(),
        tile_hash: "synthetic-sha256".into(),
        dataset_source: "aws".into(),
        dataset_version: "synthetic-v1".into(),
    }
}

fn sampled_outcomes(vehicle: Uuid, positions: &[ProjectionPosition]) -> Vec<TerrainOutcome> {
    use crate::collector::{SampleOutcome, sample_elevation};
    const SIDE: usize = 1201;
    let mut bytes = vec![0; SIDE * SIDE * 2];
    for (column, elevation) in [0_usize, 6, 12, 18]
        .into_iter()
        .zip([100_i16, 103, 101, 106])
    {
        for row in [599, 600, 601] {
            for col in column.saturating_sub(1)..=column + 1 {
                let offset = (row * SIDE + col) * 2;
                bytes[offset..offset + 2].copy_from_slice(&elevation.to_be_bytes());
            }
        }
    }
    let sampled = crate::terrain::HgtTile::from_bytes("N10E020", bytes).unwrap();
    let identity = TileProvenance {
        tile_hash: sampled.sha256_hex().unwrap(),
        ..tile()
    };
    positions
        .iter()
        .cloned()
        .map(|position| {
            let SampleOutcome::Elevation(elevation_m) =
                sample_elevation(&sampled, position.latitude, position.longitude).unwrap()
            else {
                panic!("synthetic node must have an elevation");
            };
            TerrainOutcome {
                candidate: TerrainCandidate {
                    vehicle_id: vehicle,
                    position,
                },
                forward: true,
                kind: TerrainOutcomeKind::Filled {
                    elevation_m,
                    tile: identity.clone(),
                },
            }
        })
        .collect()
}

fn outcome(vehicle: Uuid, position: ProjectionPosition, elevation: i16) -> TerrainOutcome {
    TerrainOutcome {
        candidate: TerrainCandidate {
            vehicle_id: vehicle,
            position,
        },
        forward: true,
        kind: TerrainOutcomeKind::Filled {
            elevation_m: elevation,
            tile: tile(),
        },
    }
}

fn apply(
    store: &HubStore,
    vehicle: Uuid,
    outcomes: &[TerrainOutcome],
    retire: &[i64],
    cursor: Option<i64>,
) -> TerrainBatchApplied {
    store
        .apply_terrain_batch(&TerrainBatch {
            vehicle_id: vehicle,
            outcomes,
            retire,
            cursor_to: cursor,
            committed_at_ms: C9_COMMIT,
        })
        .unwrap()
}

fn current_vehicle(store: &HubStore, vehicle: Uuid) -> TerrainVehicle {
    store
        .terrain_vehicles()
        .unwrap()
        .into_iter()
        .find(|row| row.vehicle_id == vehicle)
        .unwrap()
}

fn selected(store: &HubStore, vehicle: Uuid) -> Vec<TerrainCandidate> {
    store
        .terrain_forward_window(
            &current_vehicle(store, vehicle),
            K_SCAN_ROWS,
            K_PASS_CANDIDATES,
        )
        .unwrap()
        .candidates
}

fn elevation(store: &HubStore, vehicle: Uuid, id: i64) -> Option<i64> {
    store
        .open()
        .unwrap()
        .query_row(
            "SELECT json_extract(position_json, '$.elevation') FROM materialised_positions
         WHERE vehicle_id = ?1 AND position_id = ?2",
            params![vehicle.to_string(), id],
            |row| row.get(0),
        )
        .unwrap()
}

fn pair(store: &HubStore, vehicle: Uuid, drive: i64) -> (Option<i64>, Option<i64>) {
    store.open().unwrap().query_row(
        "SELECT ascent, descent FROM materialised_drives WHERE vehicle_id = ?1 AND drive_id = ?2",
        params![vehicle.to_string(), drive], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap()
}

fn observe_recomputes(store: &HubStore) {
    store
        .open()
        .unwrap()
        .execute_batch(
            "CREATE TABLE c9_recomputed(drive_id INTEGER) STRICT;
         CREATE TRIGGER c9_count_recompute AFTER UPDATE OF ascent, descent ON materialised_drives
         BEGIN INSERT INTO c9_recomputed VALUES (NEW.drive_id); END;",
        )
        .unwrap();
}

fn recomputes(store: &HubStore, drive: i64) -> i64 {
    store
        .open()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM c9_recomputed WHERE drive_id = ?1",
            [drive],
            |row| row.get(0),
        )
        .unwrap()
}

fn mutations(store: &HubStore, vehicle: Uuid) -> Vec<(String, i64, Value)> {
    let connection = store.open().unwrap();
    let mut statement = connection.prepare(
        "SELECT entity, entity_id, payload_json FROM sync_mutations WHERE vehicle_id = ?1 ORDER BY revision",
    ).unwrap();
    statement
        .query_map([vehicle.to_string()], |row| {
            let payload: String = row.get(2)?;
            Ok((
                row.get(0)?,
                row.get(1)?,
                serde_json::from_str(&payload).unwrap(),
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn provenance(store: &HubStore, vehicle: Uuid, id: i64) -> Option<Value> {
    store
        .open()
        .unwrap()
        .query_row(
            "SELECT json_object('drive', drive_id, 'latitude', latitude, 'longitude', longitude,
            'status', status, 'elevation', elevation_m, 'tile', tile_name, 'hash', tile_hash,
            'source', dataset_source, 'version', dataset_version, 'error', error_code,
            'attempts', attempts, 'at', attempted_at_ms, 'retry', retry_after_ms)
         FROM terrain_elevation_provenance WHERE vehicle_id = ?1 AND position_id = ?2",
            params![vehicle.to_string(), id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .unwrap()
        .map(|text| serde_json::from_str(&text).unwrap())
}

fn failed(store: &HubStore, vehicle: Uuid, id: i64, drive: i64, retry: i64) {
    store.open().unwrap().execute(
        "INSERT INTO terrain_elevation_provenance(vehicle_id, position_id, drive_id, latitude, longitude,
            status, error_code, attempts, attempted_at_ms, retry_after_ms)
         VALUES (?1, ?2, ?3, 10.5, 20.0, 'failed', 'io', 2, ?4, ?5)",
        params![vehicle.to_string(), id, drive, C9_T0, retry],
    ).unwrap();
}

fn snapshot(store: &HubStore, vehicle: Uuid) -> Vec<Vec<String>> {
    let connection = store.open().unwrap();
    [
        "SELECT json_array(position_id, drive_id, car_id, position_json, speed) FROM materialised_positions WHERE vehicle_id = ?1 ORDER BY position_id",
        "SELECT json_array(drive_id, drive_json, ascent, descent) FROM materialised_drives WHERE vehicle_id = ?1 ORDER BY drive_id",
        "SELECT json_array(position_id, status, attempts, attempted_at_ms, retry_after_ms, tile_hash, elevation_m, error_code) FROM terrain_elevation_provenance WHERE vehicle_id = ?1 ORDER BY position_id",
        "SELECT json_array(cursor_position_id, updated_at_ms, import_position_id_max, import_position_date_max_ms) FROM terrain_enrichment_state WHERE vehicle_id = ?1",
        "SELECT json_array(revision, entity, entity_id, payload_json) FROM sync_mutations WHERE vehicle_id = ?1 ORDER BY revision",
        "SELECT json_array(next_revision) FROM sync_mutation_sequences WHERE vehicle_id = ?1",
        "SELECT json_array(dirty_revision, attempts, next_attempt_ms, claimed_until_ms) FROM export_outbox WHERE vehicle_id = ?1",
    ].iter().map(|sql| {
        connection.prepare(sql).unwrap().query_map([vehicle.to_string()], |row| row.get(0)).unwrap().map(Result::unwrap).collect()
    }).collect()
}

fn import_boundary(store: &HubStore, vehicle: Uuid, id: i64, date: i64) {
    let source = store
        .register_source(
            &SourceDescriptor::new(IMPORT_SOURCE_KIND, "terrain-import"),
            C9_T0,
        )
        .unwrap();
    store.open().unwrap().execute(
        "INSERT INTO lifecycle_source_watermarks(source_id, vehicle_id, domain, max_source_row_id, max_timestamp_ms)
         VALUES (?1, ?2, 'positions', ?3, ?4)",
        params![source.source_id.to_string(), vehicle.to_string(), id, date],
    ).unwrap();
}

#[test]
fn c9_st24_to_st27_commit() {
    // C9 ST24: missing and newly elevated retries retire; a valid retry does not.
    let (_dir, store, vehicle) = fixture();
    let mut late = position(21, 2);
    late.elevation = Some(300);
    seed(&store, vehicle, &[position(1, 1), late]);
    failed(&store, vehicle, 20, 1, C9_T0);
    failed(&store, vehicle, 21, 2, C9_T0);
    failed(&store, vehicle, 1, 1, C9_T0);
    let retries = store.terrain_due_retries(C9_COMMIT, 250).unwrap();
    for id in [20, 21] {
        assert!(
            retries
                .iter()
                .find(|row| row.position_id == id)
                .unwrap()
                .candidate
                .is_none()
        );
    }
    let retired = apply(&store, vehicle, &[], &[20, 21, 1], None);
    assert_eq!(
        retired,
        TerrainBatchApplied {
            retired: 2,
            ..Default::default()
        }
    );
    assert!(provenance(&store, vehicle, 20).is_none());
    assert!(provenance(&store, vehicle, 21).is_none());
    assert_eq!(elevation(&store, vehicle, 21), Some(300));
    // C9 ST25: a due retry increments its durable attempts and clears its error.
    let mut retry = outcome(vehicle, position(1, 1), 100);
    retry.forward = false;
    let result = apply(&store, vehicle, &[retry], &[], None);
    assert_eq!(result.filled, 1);
    assert_eq!(result.cursor_written, None);
    let row = provenance(&store, vehicle, 1).unwrap();
    assert_eq!(row["status"], "success");
    assert_eq!(row["attempts"], 3);
    assert_eq!(row["at"], C9_COMMIT);
    assert_eq!(row["retry"], 0);
    assert_eq!(row["error"], Value::Null);
    assert_eq!(elevation(&store, vehicle, 1), Some(100));

    // C9 ST26: independent drives isolate the late car value from other fills.
    for held in [false, true] {
        let (_dir, store, vehicle) = fixture();
        seed(
            &store,
            vehicle,
            &(1..=3).map(|id| position(id, id)).collect::<Vec<_>>(),
        );
        let outcomes: Vec<_> = selected(&store, vehicle)
            .into_iter()
            .map(|c| outcome(vehicle, c.position, 100))
            .collect();
        store.open().unwrap().execute(
            if held { "UPDATE materialised_positions SET position_json = json_set(position_json, '$.latitude', 10.6) WHERE position_id = 2" }
            else { "UPDATE materialised_positions SET position_json = json_set(position_json, '$.elevation', 77) WHERE position_id = 2" }, [],
        ).unwrap();
        observe_recomputes(&store);
        let result = apply(&store, vehicle, &outcomes, &[], Some(3));
        assert_eq!(result.filled, 2);
        assert_eq!(result.drives_changed, 0);
        assert_eq!(recomputes(&store, 2), 0, "C9 R39: stale-only drive");
        assert_eq!(result.stale_dropped, u32::from(!held));
        assert_eq!(result.stale_held, if held { vec![2] } else { vec![] });
        assert_eq!(result.cursor_written, Some(if held { 1 } else { 3 }));
        assert!(provenance(&store, vehicle, 2).is_none());
        assert_eq!(
            elevation(&store, vehicle, 2),
            if held { None } else { Some(77) }
        );
        if held {
            let next = selected(&store, vehicle);
            assert_eq!(next.len(), 1);
            assert_eq!(next[0].position.latitude, 10.6);
            let result = apply(
                &store,
                vehicle,
                &[outcome(vehicle, next[0].position.clone(), 105)],
                &[],
                Some(3),
            );
            assert_eq!(result.filled, 1);
            assert_eq!(result.cursor_written, Some(3));
        }
        let before = snapshot(&store, vehicle);
        assert_eq!(
            apply(&store, vehicle, &outcomes, &[], Some(1)).cursor_written,
            None
        );
        assert_eq!(
            snapshot(&store, vehicle),
            before,
            "C9 R46: replay writes nothing"
        );
    }

    // C9 ST27: all four fills precede one recompute and the single export mark.
    let (_dir, store, vehicle) = fixture();
    let positions: Vec<_> = (1..=4).map(|id| position(id, 1)).collect();
    seed(&store, vehicle, &positions);
    store
        .open()
        .unwrap()
        .execute_batch(
            "CREATE TABLE c9_recomputed(drive_id INTEGER) STRICT;
         CREATE TRIGGER c9_count_recompute AFTER UPDATE OF ascent, descent ON materialised_drives
         BEGIN INSERT INTO c9_recomputed VALUES (NEW.drive_id); END;",
        )
        .unwrap();
    let outcomes = sampled_outcomes(vehicle, &positions);
    let result = apply(&store, vehicle, &outcomes, &[], Some(4));
    assert_eq!(
        result,
        TerrainBatchApplied {
            filled: 4,
            drives_changed: 1,
            cursor_written: Some(4),
            ..Default::default()
        }
    );
    assert_eq!(pair(&store, vehicle, 1), (Some(8), Some(2)));
    assert_eq!(
        store
            .open()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM c9_recomputed", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let log = mutations(&store, vehicle);
    assert_eq!(
        log.iter()
            .map(|(entity, id, _)| (entity.as_str(), *id))
            .collect::<Vec<_>>(),
        vec![
            ("position", 1),
            ("position", 2),
            ("position", 3),
            ("position", 4),
            ("drive", 1)
        ]
    );
    for (index, expected) in [100, 103, 101, 106].into_iter().enumerate() {
        let id = index as i64 + 1;
        assert_eq!(elevation(&store, vehicle, id), Some(expected));
        assert_eq!(log[index].2["elevation"], expected);
        assert_eq!(
            log[index].2["future_member"],
            json!({"keep": [1, "two", null]})
        );
        assert_eq!(
            store
                .open()
                .unwrap()
                .query_row(
                    "SELECT speed FROM materialised_positions WHERE position_id = ?1",
                    [id],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            123
        );
        let row = provenance(&store, vehicle, id).unwrap();
        assert_eq!(row["status"], "success");
        assert_eq!(row["tile"], "N10E020");
        let TerrainOutcomeKind::Filled { tile, .. } = &outcomes[index].kind else {
            unreachable!()
        };
        assert_eq!(row["hash"], tile.tile_hash);
        assert_eq!(row["source"], "aws");
    }
    assert_eq!(log[4].2["ascent"], 8);
    assert_eq!(log[4].2["descent"], 2);
    assert_eq!(b3_c8_sb::export_revision(&store, vehicle), 1);
    assert_eq!(
        store
            .open()
            .unwrap()
            .query_row(
                "SELECT updated_at_ms FROM terrain_enrichment_state WHERE vehicle_id = ?1",
                [vehicle.to_string()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        C9_COMMIT
    );

    // C9 R15: a decoder refusal during recompute rolls back fills and retirements.
    let (_dir, store, vehicle) = fixture();
    seed(&store, vehicle, &[position(1, 1), position(2, 1)]);
    failed(&store, vehicle, 20, 1, C9_T0);
    let candidate = selected(&store, vehicle)[0].clone();
    store.open().unwrap().execute("UPDATE materialised_positions SET position_json = '{\"elevation\":null}' WHERE position_id = 2", []).unwrap();
    let before = snapshot(&store, vehicle);
    assert!(matches!(
        store.apply_terrain_batch(&TerrainBatch {
            vehicle_id: vehicle,
            outcomes: &[outcome(vehicle, candidate.position, 100)],
            retire: &[20],
            cursor_to: Some(2),
            committed_at_ms: C9_COMMIT,
        }),
        Err(StoreError::DeserializeLifecycleRow(_))
    ));
    assert_eq!(snapshot(&store, vehicle), before);

    // C9 R15: a refusal at the final cursor write rolls back every earlier effect.
    let (_dir, store, vehicle) = fixture();
    seed(&store, vehicle, &[position(1, 1), position(2, 1)]);
    failed(&store, vehicle, 20, 1, C9_T0);
    store
        .open()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER c9_refuse_cursor BEFORE INSERT ON terrain_enrichment_state
         BEGIN SELECT RAISE(ABORT, 'synthetic cursor refusal'); END;",
        )
        .unwrap();
    let before = snapshot(&store, vehicle);
    assert!(
        store
            .apply_terrain_batch(&TerrainBatch {
                vehicle_id: vehicle,
                outcomes: &[
                    outcome(vehicle, position(1, 1), 100),
                    outcome(vehicle, position(2, 1), 110)
                ],
                retire: &[20],
                cursor_to: Some(2),
                committed_at_ms: C9_COMMIT,
            })
            .is_err()
    );
    assert_eq!(snapshot(&store, vehicle), before);

    // C9 R16: deletion, drive changes and a new car value invalidate prepared fills.
    for change in [
        "DELETE FROM materialised_positions WHERE position_id = 1",
        "DELETE FROM materialised_drives WHERE drive_id = 1",
        "UPDATE materialised_positions SET drive_id = 2, position_json = json_set(position_json, '$.drive_id', 2) WHERE position_id = 1",
        "UPDATE materialised_positions SET position_json = json_set(position_json, '$.elevation', 0) WHERE position_id = 2",
        "UPDATE materialised_positions SET position_json = json_set(position_json, '$.latitude', 91) WHERE position_id = 1",
    ] {
        let (_dir, store, vehicle) = fixture();
        seed(
            &store,
            vehicle,
            &[position(1, 1), position(2, 1), position(3, 2)],
        );
        let prepared = outcome(vehicle, position(1, 1), 100);
        store.open().unwrap().execute(change, []).unwrap();
        observe_recomputes(&store);
        let result = apply(&store, vehicle, &[prepared], &[], Some(1));
        assert_eq!(
            result,
            TerrainBatchApplied {
                stale_dropped: 1,
                cursor_written: Some(1),
                ..Default::default()
            }
        );
        assert_eq!(result.drives_changed, 0);
        assert_eq!(recomputes(&store, 1), 0, "C9 R39: dropped-only drive");
        assert!(provenance(&store, vehicle, 1).is_none());
        assert!(mutations(&store, vehicle).is_empty());
        assert_eq!(b3_c8_sb::export_revision(&store, vehicle), 0);
    }

    // C9 section 6.1: a stale retry stays due, and never caps a forward frontier.
    let (_dir, store, vehicle) = fixture();
    seed(&store, vehicle, &[position(1, 1)]);
    failed(&store, vehicle, 1, 1, C9_T0);
    let mut prepared = outcome(vehicle, position(1, 1), 100);
    prepared.forward = false;
    store.open().unwrap().execute("UPDATE materialised_positions SET position_json = json_set(position_json, '$.longitude', 20.1)", []).unwrap();
    observe_recomputes(&store);
    let before = provenance(&store, vehicle, 1);
    let result = apply(&store, vehicle, &[prepared], &[], Some(3));
    assert_eq!(result.stale_held, [1]);
    assert_eq!(result.drives_changed, 0);
    assert_eq!(recomputes(&store, 1), 0, "C9 R39: stale retry drive");
    assert_eq!(result.cursor_written, Some(3));
    assert_eq!(provenance(&store, vehicle, 1), before);
    assert_eq!(store.terrain_due_retries(C9_COMMIT, 250).unwrap().len(), 1);

    // C9 R9: a window without candidates may advance once, preserving its boundary.
    let (_dir, store, vehicle) = fixture();
    store
        .record_terrain_import_boundary(
            vehicle,
            ImportBoundary {
                max_position_id: 7,
                max_date_ms: C9_T0,
            },
            C9_T0,
        )
        .unwrap();
    assert_eq!(
        apply(&store, vehicle, &[], &[], Some(7)).cursor_written,
        Some(7)
    );
    let before = snapshot(&store, vehicle);
    assert_eq!(
        apply(&store, vehicle, &[], &[], Some(7)),
        TerrainBatchApplied::default()
    );
    assert_eq!(snapshot(&store, vehicle), before);
    assert_eq!(
        current_vehicle(&store, vehicle).stored_boundary,
        ImportBoundary {
            max_position_id: 7,
            max_date_ms: C9_T0
        }
    );
}

#[test]
fn c9_r9_zero_frontier_writes_nothing_without_state() {
    let (_dir, store, vehicle) = fixture();
    seed(&store, vehicle, &[position(1, 1)]);
    let prepared = outcome(vehicle, selected(&store, vehicle).remove(0).position, 100);
    let observer = store.open().unwrap();
    observer.execute(
        "UPDATE materialised_positions SET position_json = json_set(position_json, '$.latitude', 10.6)
         WHERE vehicle_id = ?1 AND position_id = 1", [vehicle.to_string()],
    ).unwrap();
    let state_rows = || {
        observer
            .query_row(
                "SELECT COUNT(*) FROM terrain_enrichment_state WHERE vehicle_id = ?1",
                [vehicle.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
    };
    assert_eq!(state_rows(), 0);
    let before = snapshot(&store, vehicle);
    let version: i64 = observer
        .pragma_query_value(None, "data_version", |row| row.get(0))
        .unwrap();
    assert_eq!(
        apply(&store, vehicle, &[prepared], &[], Some(1)),
        TerrainBatchApplied {
            stale_held: vec![1],
            ..Default::default()
        },
    );
    assert_eq!(state_rows(), 0);
    assert_eq!(snapshot(&store, vehicle), before);
    assert_eq!(
        observer
            .pragma_query_value(None, "data_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        version
    );
    assert_eq!(
        apply(&store, vehicle, &[], &[], Some(0)),
        TerrainBatchApplied::default()
    );
    assert_eq!(state_rows(), 0);
    assert_eq!(snapshot(&store, vehicle), before);
    assert_eq!(
        observer
            .pragma_query_value(None, "data_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        version
    );
}

#[test]
fn c9_r16_catalogue_admission_rechecked_in_transaction() {
    let (_dir, store, vehicle) = fixture();
    seed(&store, vehicle, &[position(1, 1)]);
    let prepared = outcome(vehicle, position(1, 1), 100);
    let before = snapshot(&store, vehicle);
    let observer = std::rc::Rc::new(store.open().unwrap());
    let writer = store.open().unwrap();
    let version = std::rc::Rc::new(std::cell::Cell::new(0_i64));
    let hook_observer = observer.clone();
    let hook_version = version.clone();
    TERRAIN_COMMIT_BEFORE_BEGIN.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            writer
                .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                .unwrap();
            hook_version.set(
                hook_observer
                    .pragma_query_value(None, "data_version", |row| row.get(0))
                    .unwrap(),
            );
        }))
    });
    let result = store.apply_terrain_batch(&TerrainBatch {
        vehicle_id: vehicle,
        outcomes: &[prepared],
        retire: &[],
        cursor_to: Some(1),
        committed_at_ms: C9_COMMIT,
    });
    let after_version: i64 = observer
        .pragma_query_value(None, "data_version", |row| row.get(0))
        .unwrap();
    observer
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .unwrap();
    assert!(matches!(result, Err(StoreError::UnsupportedSchema(v)) if v == SCHEMA_VERSION + 1));
    assert_eq!(
        after_version,
        version.get(),
        "C9 R16: refused catalogue writes nothing"
    );
    assert_eq!(snapshot(&store, vehicle), before);
}

#[test]
fn c9_r45_attempts_saturate_for_due_retries() {
    let (_dir, store, vehicle) = fixture();
    seed(&store, vehicle, &[position(1, 1)]);
    failed(&store, vehicle, 1, 1, C9_T0);
    store.open().unwrap().execute(
        "UPDATE terrain_elevation_provenance SET attempts = ?1 WHERE vehicle_id = ?2 AND position_id = 1",
        params![i64::MAX, vehicle.to_string()],
    ).unwrap();
    let mut prepared = outcome(vehicle, position(1, 1), 100);
    prepared.forward = false;
    prepared.kind = TerrainOutcomeKind::Failed {
        error_code: "io",
        schedule: RetrySchedule {
            base_ms: 100,
            cap_ms: 250,
        },
        tile: None,
    };
    assert_eq!(
        apply(&store, vehicle, &[prepared], &[], None),
        TerrainBatchApplied {
            failed: 1,
            ..Default::default()
        }
    );
    let row = provenance(&store, vehicle, 1).unwrap();
    assert_eq!(row["attempts"], i64::MAX);
    assert_eq!(row["at"], C9_COMMIT);
    assert_eq!(row["retry"], C9_COMMIT + 250);
    let mut prepared = outcome(vehicle, position(1, 1), 100);
    prepared.forward = false;
    let result = store
        .apply_terrain_batch(&TerrainBatch {
            vehicle_id: vehicle,
            outcomes: &[prepared],
            retire: &[],
            cursor_to: None,
            committed_at_ms: C9_COMMIT + 250,
        })
        .unwrap();
    assert_eq!(
        result,
        TerrainBatchApplied {
            filled: 1,
            ..Default::default()
        }
    );
    let row = provenance(&store, vehicle, 1).unwrap();
    assert_eq!(row["attempts"], i64::MAX);
    assert_eq!(row["status"], "success");
    assert_eq!(row["at"], C9_COMMIT + 250);
    assert_eq!(row["retry"], 0);
}

#[test]
fn c9_r38_r39_fill_lifecycle_written_rows() {
    let (_dir, store, vehicle) = fixture();
    let positions: Vec<_> = (1..=2)
        .map(|id| {
            let mut row = position(id, 1);
            row.date_ms = C9_T0 + id * 1_000;
            row
        })
        .collect();
    let mut drive = drive_row(1, C9_COMMIT);
    drive.start_date_ms = C9_T0 - 1;
    let state = crate::lifecycle::OpenSessionState::new().encode().unwrap();
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle,
            car_id: 1,
            open_session_json: &state,
            last_observation_id: 0,
            quarantined: false,
            updated_at_ms: C9_T0 + 3_000,
            delta: &crate::lifecycle::LifecycleDelta {
                drives: vec![drive],
                positions,
                ..Default::default()
            },
        })
        .unwrap();
    let before_mutations = mutations(&store, vehicle).len();
    let before_export = b3_c8_sb::export_revision(&store, vehicle);
    observe_recomputes(&store);
    let outcomes: Vec<_> = selected(&store, vehicle)
        .into_iter()
        .zip([100, 110])
        .map(|(candidate, elevation)| outcome(vehicle, candidate.position, elevation))
        .collect();
    assert_eq!(outcomes.len(), 2);
    assert_eq!(
        apply(&store, vehicle, &outcomes, &[], Some(2)),
        TerrainBatchApplied {
            filled: 2,
            drives_changed: 1,
            cursor_written: Some(2),
            ..Default::default()
        }
    );
    assert_eq!(pair(&store, vehicle, 1), (Some(10), Some(0)));
    assert_eq!(recomputes(&store, 1), 1);
    let log = mutations(&store, vehicle);
    let added = &log[before_mutations..];
    assert_eq!(
        added
            .iter()
            .map(|(entity, id, _)| (entity.as_str(), *id))
            .collect::<Vec<_>>(),
        [("position", 1), ("position", 2), ("drive", 1)]
    );
    for ((_, id, payload), elevation) in added[..2].iter().zip([100, 110]) {
        let stored: String = store.open().unwrap().query_row(
            "SELECT position_json FROM materialised_positions WHERE vehicle_id = ?1 AND position_id = ?2",
            params![vehicle.to_string(), id], |row| row.get(0),
        ).unwrap();
        assert_eq!(*payload, serde_json::from_str::<Value>(&stored).unwrap());
        assert_eq!(payload["date_ms"], C9_T0 + id * 1_000);
        assert_eq!(payload["elevation"], elevation);
        assert_eq!(
            provenance(&store, vehicle, *id).unwrap()["status"],
            "success"
        );
    }
    assert_eq!(added[2].2["ascent"], 10);
    assert_eq!(added[2].2["descent"], 0);
    assert_eq!(
        b3_c8_sb::export_revision(&store, vehicle),
        before_export + 1
    );
}

#[test]
fn c9_r40_fill_preserves_imported_closed_totals() {
    // C9 R40: a materialised position does not transfer copied-drive ownership.
    for inventory in ["legacy", "durable"] {
        let (_dir, store, _) = fixture();
        let (vehicle, _, lineage) = imported_v2_base(&store);
        let vehicle = vehicle.vehicle_id;
        let row = position(1, 9);
        seed(&store, vehicle, std::slice::from_ref(&row));
        let source = store
            .register_source(
                &SourceDescriptor::new(IMPORT_SOURCE_KIND, "batch-inventory"),
                C9_T0,
            )
            .unwrap();
        let connection = store.open().unwrap();
        connection
            .execute(
                "UPDATE materialised_drives SET ascent = 55, descent = 40,
             drive_json = json_set(drive_json, '$.ascent', 55, '$.descent', 40)
             WHERE vehicle_id = ?1 AND drive_id = 9",
                [vehicle.to_string()],
            )
            .unwrap();
        if inventory == "legacy" {
            connection
                .execute(
                    concat!(
                        "INSERT INTO ",
                        crate::db::import_table_names::import_projection_heads_table!(),
                        "(
                    vehicle_id, source_id, base_snapshot_id, selected_car_id, head_sequence)
                 VALUES (?1, ?2, ?3, 1, 0)"
                    ),
                    params![
                        vehicle.to_string(),
                        source.source_id.to_string(),
                        lineage.base.snapshot_id.to_string()
                    ],
                )
                .unwrap();
            connection
                .execute(
                    concat!(
                        "INSERT INTO ",
                        crate::db::import_table_names::import_projection_rows_table!(),
                        " VALUES (?1, 'drive', 9)"
                    ),
                    [vehicle.to_string()],
                )
                .unwrap();
        } else {
            connection
                .execute(
                    concat!(
                        "INSERT INTO ",
                        crate::db::import_table_names::import_projection_state_heads_table!(),
                        "(
                    vehicle_id, source_id, base_snapshot_id, selected_car_id, head_sequence)
                 VALUES (?1, ?2, ?3, 1, 0)"
                    ),
                    params![
                        vehicle.to_string(),
                        source.source_id.to_string(),
                        lineage.base.snapshot_id.to_string()
                    ],
                )
                .unwrap();
            connection
                .execute(
                    concat!(
                        "INSERT INTO ",
                        crate::db::import_table_names::import_projection_state_rows_table!(),
                        "
                 VALUES (?1, 'drive', 1, 9, 1, zeroblob(32))"
                    ),
                    [vehicle.to_string()],
                )
                .unwrap();
        }
        let copied: String = connection
            .query_row(
                "SELECT drive_json FROM materialised_drives WHERE vehicle_id = ?1 AND drive_id = 9",
                [vehicle.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        let before = mutations(&store, vehicle).len();
        observe_recomputes(&store);
        assert_eq!(
            apply(&store, vehicle, &[outcome(vehicle, row, 100)], &[], None),
            TerrainBatchApplied {
                filled: 1,
                drives_changed: 0,
                ..Default::default()
            },
            "{inventory}"
        );
        assert_eq!(elevation(&store, vehicle, 1), Some(100));
        assert_eq!(recomputes(&store, 9), 0, "C9 R40: no recompute trigger row");
        assert_eq!(pair(&store, vehicle, 9), (Some(55), Some(40)));
        let after: String = connection
            .query_row(
                "SELECT drive_json FROM materialised_drives WHERE vehicle_id = ?1 AND drive_id = 9",
                [vehicle.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(after, copied);
        let log = mutations(&store, vehicle);
        assert_eq!(log.len(), before + 1);
        assert_eq!((log[before].0.as_str(), log[before].1), ("position", 1));
    }
}

#[test]
fn c9_st29_imported() {
    let (_dir, store, vehicle) = fixture();
    let positions: Vec<_> = (1..=4).map(|id| position(id, 1)).collect();
    seed(&store, vehicle, &positions);
    let mut imported = drive_row(9, C9_T0);
    imported.ascent = Some(55);
    imported.descent = Some(40);
    let connection = store.open().unwrap();
    connection.execute("INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json, ascent, descent) VALUES (?1, 9, 1, ?2, 55, 40)", params![vehicle.to_string(), serde_json::to_string(&imported).unwrap()]).unwrap();
    let source_positions = snapshot(&store, vehicle)[0][..2].to_vec();
    let source_drive: String = connection
        .query_row(
            "SELECT drive_json FROM materialised_drives WHERE drive_id = 9",
            [],
            |row| row.get(0),
        )
        .unwrap();
    import_boundary(&store, vehicle, 2, C9_T0 + 2_000);
    let candidates = selected(&store, vehicle);
    assert_eq!(
        candidates.iter().map(|c| c.position.id).collect::<Vec<_>>(),
        [3, 4]
    );
    let outcomes: Vec<_> = candidates
        .into_iter()
        .zip([100, 110])
        .map(|(c, e)| outcome(vehicle, c.position, e))
        .collect();
    assert_eq!(apply(&store, vehicle, &outcomes, &[], Some(4)).filled, 2);
    assert_eq!(
        &snapshot(&store, vehicle)[0][..2],
        source_positions.as_slice()
    );
    for id in [1, 2] {
        assert_eq!(elevation(&store, vehicle, id), None);
        assert!(provenance(&store, vehicle, id).is_none());
    }
    assert_eq!(
        connection
            .query_row(
                "SELECT drive_json FROM materialised_drives WHERE drive_id = 9",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        source_drive
    );
    assert_eq!(pair(&store, vehicle, 9), (Some(55), Some(40)));
    assert_eq!(pair(&store, vehicle, 1), (Some(10), Some(0)));
    assert!(
        !mutations(&store, vehicle)
            .iter()
            .any(|(entity, id, _)| entity == "drive" && *id == 9)
    );
}

#[test]
fn c9_st45_provenance() {
    let (_dir, store, vehicle) = fixture();
    let positions: Vec<_> = (1..=7).map(|id| position(id, 1)).collect();
    seed(&store, vehicle, &positions);
    let schedule = RetrySchedule {
        base_ms: 100,
        cap_ms: 250,
    };
    let kinds = [
        TerrainOutcomeKind::Filled {
            elevation_m: 100,
            tile: tile(),
        },
        TerrainOutcomeKind::Void {
            tile: tile(),
            error_code: None,
        },
        TerrainOutcomeKind::Void {
            tile: tile(),
            error_code: Some("out_of_range"),
        },
        TerrainOutcomeKind::Failed {
            error_code: "io",
            schedule,
            tile: Some(tile()),
        },
        TerrainOutcomeKind::Failed {
            error_code: "unproven_tile",
            schedule,
            tile: Some(tile()),
        },
        TerrainOutcomeKind::Failed {
            error_code: "timeout",
            schedule,
            tile: None,
        },
        TerrainOutcomeKind::Failed {
            error_code: "unproven_tile",
            schedule,
            tile: None,
        },
    ];
    let mut outcomes = Vec::new();
    for (index, (position, kind)) in positions.iter().cloned().zip(kinds).enumerate() {
        if index % 2 == 0 {
            failed(&store, vehicle, position.id, 1, C9_T0);
        }
        outcomes.push(TerrainOutcome {
            candidate: TerrainCandidate {
                vehicle_id: vehicle,
                position,
            },
            forward: index % 2 != 0,
            kind,
        });
    }
    let result = apply(&store, vehicle, &outcomes, &[], Some(7));
    assert_eq!((result.filled, result.void, result.failed), (1, 2, 4));
    for (index, outcome) in outcomes.iter().enumerate() {
        let attempts = if index % 2 == 0 { 3 } else { 1 };
        let (status, value, error, has_tile, retry) = match &outcome.kind {
            TerrainOutcomeKind::Filled { elevation_m, .. } => {
                ("success", Some(*elevation_m), None, true, 0)
            }
            TerrainOutcomeKind::Void { error_code, .. } => ("void", None, *error_code, true, 0),
            TerrainOutcomeKind::Failed {
                error_code, tile, ..
            } => (
                "failed",
                None,
                Some(*error_code),
                *error_code == "unproven_tile" && tile.is_some(),
                C9_COMMIT + if attempts == 3 { 250 } else { 100 },
            ),
        };
        let p = &outcome.candidate.position;
        assert_eq!(
            provenance(&store, vehicle, p.id).unwrap(),
            json!({
                "drive": 1, "latitude": p.latitude, "longitude": p.longitude,
                "status": status, "elevation": value, "tile": has_tile.then_some(tile().tile_name),
                "hash": has_tile.then_some(tile().tile_hash), "source": has_tile.then_some(tile().dataset_source),
                "version": has_tile.then_some(tile().dataset_version), "error": error,
                "attempts": attempts, "at": C9_COMMIT, "retry": retry,
            })
        );
    }
    assert_eq!(schedule.delay_ms(i64::MAX), 250);
    assert_eq!(schedule.delay_ms(2), 200);
    assert_eq!(schedule.delay_ms(3), 250);
    assert_eq!(schedule.delay_ms(0), 100);
    assert_eq!(
        RetrySchedule {
            base_ms: i64::MAX,
            cap_ms: i64::MAX
        }
        .delay_ms(33),
        i64::MAX
    );
    // C9 section 6.1: terminal, waiting and forward-existing rows stay unchanged.
    let before = snapshot(&store, vehicle);
    let mut stale = outcomes.clone();
    for result in &mut stale {
        result.forward = false;
    }
    assert_eq!(
        apply(&store, vehicle, &stale, &[], Some(7)).stale_dropped,
        7
    );
    assert_eq!(snapshot(&store, vehicle), before);
    let mut forward = outcomes[3].clone();
    forward.forward = true;
    assert_eq!(
        apply(&store, vehicle, &[forward], &[1, 2, 4], None).retired,
        0
    );
    assert_eq!(snapshot(&store, vehicle), before);
    // C9 R28: retry deadlines saturate at the epoch representation's upper bound.
    let (_dir, store, vehicle) = fixture();
    seed(&store, vehicle, &[position(1, 1)]);
    let mut result = outcome(vehicle, position(1, 1), 1);
    result.kind = TerrainOutcomeKind::Failed {
        error_code: "io",
        schedule,
        tile: None,
    };
    store
        .apply_terrain_batch(&TerrainBatch {
            vehicle_id: vehicle,
            outcomes: &[result],
            retire: &[],
            cursor_to: None,
            committed_at_ms: i64::MAX - 1,
        })
        .unwrap();
    assert_eq!(provenance(&store, vehicle, 1).unwrap()["retry"], i64::MAX);
}

#[test]
fn c9_st50_readers() {
    let (_dir, store, vehicle) = fixture();
    let positions: Vec<_> = (1..=7).map(|id| position(id, 1)).collect();
    seed(&store, vehicle, &positions);
    let fills: Vec<_> = positions[..4]
        .iter()
        .cloned()
        .zip([100, 103, 101, 106])
        .map(|(p, e)| outcome(vehicle, p, e))
        .collect();
    apply(&store, vehicle, &fills, &[], Some(4));
    let mut gaps: Vec<_> = positions[4..]
        .iter()
        .cloned()
        .map(|p| outcome(vehicle, p, 0))
        .collect();
    gaps[0].kind = TerrainOutcomeKind::Void {
        tile: tile(),
        error_code: None,
    };
    gaps[1].kind = TerrainOutcomeKind::Void {
        tile: tile(),
        error_code: Some("out_of_range"),
    };
    gaps[2].kind = TerrainOutcomeKind::Failed {
        error_code: "io",
        schedule: RetrySchedule {
            base_ms: 100,
            cap_ms: 200,
        },
        tile: None,
    };
    apply(&store, vehicle, &gaps, &[], Some(7));
    let rows = store
        .drive_positions_window_page(vehicle, 1, 0, i64::MAX, None, 20)
        .unwrap();
    assert_eq!(rows.len(), 7);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(
            row.terrain,
            (index < 4).then(|| PositionTerrainLookup {
                dataset: Some("aws".into()),
                dataset_version: Some("synthetic-v1".into()),
                tile: Some("N10E020".into())
            })
        );
    }
    assert_eq!(pair(&store, vehicle, 1), (Some(8), Some(2)));
    assert_eq!(b3_c8_sb::export_revision(&store, vehicle), 1);
    assert_eq!(mutations(&store, vehicle).len(), 5);
}

#[test]
fn c9_st57_unchanged_pair() {
    let (_dir, store, vehicle) = fixture();
    seed(
        &store,
        vehicle,
        &(1..=3).map(|id| position(id, 1)).collect::<Vec<_>>(),
    );
    apply(
        &store,
        vehicle,
        &[
            outcome(vehicle, position(1, 1), 100),
            outcome(vehicle, position(2, 1), 103),
        ],
        &[],
        Some(2),
    );
    assert_eq!(pair(&store, vehicle, 1), (Some(3), Some(0)));
    let before = mutations(&store, vehicle).len();
    let result = apply(
        &store,
        vehicle,
        &[outcome(vehicle, position(3, 1), 104)],
        &[],
        Some(3),
    );
    assert_eq!(result.filled, 1);
    assert_eq!(result.drives_changed, 0);
    assert_eq!(pair(&store, vehicle, 1), (Some(3), Some(0)));
    assert_eq!(
        mutations(&store, vehicle)[before..]
            .iter()
            .map(|(e, id, _)| (e.as_str(), *id))
            .collect::<Vec<_>>(),
        [("position", 3)]
    );
    assert_eq!(b3_c8_sb::export_revision(&store, vehicle), 2);
}

#[test]
fn c9_st63_vehicle_deleted() {
    let (_dir, store, vehicle) = fixture();
    seed(&store, vehicle, &[position(1, 1), position(2, 1)]);
    let outcomes: Vec<_> = selected(&store, vehicle)
        .into_iter()
        .map(|c| outcome(vehicle, c.position, 100))
        .collect();
    failed(&store, vehicle, 20, 1, C9_T0);
    store
        .record_terrain_import_boundary(
            vehicle,
            ImportBoundary {
                max_position_id: 1,
                max_date_ms: 0,
            },
            C9_T0,
        )
        .unwrap();
    let mut connection = store.open().unwrap();
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    for table in [
        "materialised_positions",
        "terrain_elevation_provenance",
        "terrain_enrichment_state",
        "materialised_drives",
        "vehicles",
    ] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE vehicle_id = ?1"),
            [vehicle.to_string()],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let before = snapshot(&store, vehicle);
    assert_eq!(
        apply(&store, vehicle, &outcomes, &[20], Some(2)),
        TerrainBatchApplied {
            stale_dropped: 3,
            ..Default::default()
        }
    );
    assert_eq!(snapshot(&store, vehicle), before);
    assert!(before.iter().all(Vec::is_empty));
    assert!(store.terrain_vehicles().unwrap().is_empty());
}

#[test]
fn c9_st65_boundary_race() {
    for split_maxima in [false, true] {
        let (_dir, store, vehicle) = fixture();
        seed(
            &store,
            vehicle,
            &(1..=3).map(|id| position(id, 1)).collect::<Vec<_>>(),
        );
        let candidates = selected(&store, vehicle);
        assert_eq!(
            candidates.iter().map(|c| c.position.id).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        let before = snapshot(&store, vehicle)[0][..2].to_vec();
        // C9 R6: each maximum can come from a different durable boundary.
        if split_maxima {
            store
                .record_terrain_import_boundary(
                    vehicle,
                    ImportBoundary {
                        max_position_id: 2,
                        max_date_ms: 0,
                    },
                    C9_T0,
                )
                .unwrap();
        }
        import_boundary(
            &store,
            vehicle,
            if split_maxima { 0 } else { 2 },
            C9_T0 + 2_000,
        );
        let outcomes: Vec<_> = candidates
            .into_iter()
            .map(|c| outcome(vehicle, c.position, 100))
            .collect();
        let result = apply(&store, vehicle, &outcomes, &[], Some(3));
        assert_eq!(
            result,
            TerrainBatchApplied {
                filled: 1,
                stale_dropped: 2,
                cursor_written: Some(3),
                ..Default::default()
            }
        );
        assert_eq!(&snapshot(&store, vehicle)[0][..2], before.as_slice());
        for id in [1, 2] {
            assert_eq!(elevation(&store, vehicle, id), None);
            assert!(provenance(&store, vehicle, id).is_none());
        }
        assert_eq!(elevation(&store, vehicle, 3), Some(100));
        assert_eq!(pair(&store, vehicle, 1), (None, None));
        assert_eq!(
            mutations(&store, vehicle)
                .iter()
                .map(|(e, id, _)| (e.as_str(), *id))
                .collect::<Vec<_>>(),
            [("position", 3)]
        );
        assert_eq!(b3_c8_sb::export_revision(&store, vehicle), 1);
        assert_eq!(current_vehicle(&store, vehicle).cursor_position_id, 3);
    }
}

#[test]
fn c9_r10_retirement_rechecks_current_qualification() {
    let (_dir, store, vehicle) = fixture();
    let mut positions: Vec<_> = (1..=13).map(|id| position(id, id)).collect();
    positions[8].elevation = Some(42);
    positions.push(position(14, 6));
    seed(&store, vehicle, &positions);
    for id in 1..=13 {
        failed(&store, vehicle, id, id, C9_T0);
    }
    let retries = store.terrain_due_retries(C9_COMMIT, 250).unwrap();
    let prepared: Vec<_> = retries
        .iter()
        .filter_map(|r| r.candidate.as_ref())
        .map(|candidate| {
            let mut result = outcome(vehicle, candidate.position.clone(), 100);
            result.forward = false;
            result
        })
        .collect();
    assert!(
        retries
            .iter()
            .find(|r| r.position_id == 9)
            .unwrap()
            .candidate
            .is_none()
    );
    let connection = store.open().unwrap();
    // C9 R10: each invalidation happens after the retry selection.
    connection.execute_batch(
        "DELETE FROM materialised_positions WHERE position_id IN (1, 10, 11, 12, 13);
         UPDATE materialised_positions SET drive_id = NULL,
             position_json = json_set(position_json, '$.drive_id', NULL) WHERE position_id = 2;
         DELETE FROM materialised_drives WHERE drive_id = 3;
         UPDATE materialised_positions SET position_json = json_set(position_json, '$.elevation', 0)
             WHERE position_id IN (4, 14);
         UPDATE materialised_positions SET position_json = json_set(position_json, '$.latitude', 91)
             WHERE position_id = 7;
         UPDATE materialised_positions SET position_json = json_set(position_json, '$.longitude', 181)
             WHERE position_id = 8;
         UPDATE materialised_positions SET position_json = json_set(position_json, '$.elevation', NULL)
             WHERE position_id = 9;
         UPDATE terrain_elevation_provenance SET status = 'void' WHERE position_id = 12;
         UPDATE terrain_elevation_provenance SET status = 'success' WHERE position_id = 13;"
    ).unwrap();
    connection
        .execute(
            "UPDATE terrain_elevation_provenance SET retry_after_ms = ?1 WHERE position_id = 10",
            [C9_COMMIT + 1],
        )
        .unwrap();
    // C9 R6: isolate one imported row using its current date and id together.
    connection.execute(
        "UPDATE materialised_positions SET position_json = json_set(position_json, '$.date_ms', ?1)
         WHERE position_id = 5", [C9_T0 - 1],
    ).unwrap();
    import_boundary(&store, vehicle, 5, C9_T0 - 1);
    let before = snapshot(&store, vehicle);
    // C9 section 6.1: stale prepared retries stay untouched until explicit retirement.
    assert_eq!(
        apply(&store, vehicle, &prepared, &[], None),
        TerrainBatchApplied {
            stale_dropped: 12,
            ..Default::default()
        }
    );
    assert_eq!(snapshot(&store, vehicle), before);
    let retained: Vec<_> = [9, 10, 12, 13]
        .into_iter()
        .map(|id| (id, provenance(&store, vehicle, id)))
        .collect();
    let result = apply(&store, vehicle, &[], &(1..=13).collect::<Vec<_>>(), None);
    assert_eq!(
        result,
        TerrainBatchApplied {
            retired: 9,
            ..Default::default()
        }
    );
    for id in [1, 2, 3, 4, 5, 6, 7, 8, 11] {
        assert!(
            provenance(&store, vehicle, id).is_none(),
            "C9 R10: row {id}"
        );
    }
    for (id, expected) in retained {
        assert_eq!(
            provenance(&store, vehicle, id),
            expected,
            "C9 R10: row {id}"
        );
    }
    let after = snapshot(&store, vehicle);
    for index in [0, 1, 3, 4, 5, 6] {
        assert_eq!(after[index], before[index]);
    }
    assert_eq!(elevation(&store, vehicle, 4), Some(0));
    assert_eq!(b3_c8_sb::export_revision(&store, vehicle), 0);
}

#[test]
fn c9_r16_queue_matrix_and_stored_attempts() {
    // C9 section 6.1: null elevations isolate the queue guard from the value guard.
    for forward in [false, true] {
        for status in ["missing", "due", "waiting", "success", "void"] {
            for kind in ["filled", "void", "failed"] {
                let (_dir, store, vehicle) = fixture();
                seed(&store, vehicle, &[position(1, 1)]);
                if status != "missing" {
                    failed(&store, vehicle, 1, 1, C9_T0);
                }
                let mut prepared = outcome(vehicle, position(1, 1), 100);
                prepared.forward = forward;
                prepared.kind = match kind {
                    "filled" => prepared.kind,
                    "void" => TerrainOutcomeKind::Void {
                        tile: tile(),
                        error_code: None,
                    },
                    _ => TerrainOutcomeKind::Failed {
                        error_code: "io",
                        schedule: RetrySchedule {
                            base_ms: 10,
                            cap_ms: 1_000,
                        },
                        tile: Some(tile()),
                    },
                };
                // C9 R45: another committed attempt supersedes the selected count of two.
                let connection = store.open().unwrap();
                connection
                    .execute(
                        "UPDATE terrain_elevation_provenance SET attempts = 7, status = ?1,
                     retry_after_ms = ?2 WHERE position_id = 1",
                        params![
                            if matches!(status, "success" | "void") {
                                status
                            } else {
                                "failed"
                            },
                            if status == "waiting" {
                                C9_COMMIT + 1
                            } else {
                                C9_COMMIT
                            }
                        ],
                    )
                    .unwrap();
                let before = snapshot(&store, vehicle);
                let accepted = (forward && status == "missing") || (!forward && status == "due");
                let result = apply(&store, vehicle, &[prepared], &[], None);
                let context = format!("C9 section 6.1: {forward}/{status}/{kind}");
                if !accepted {
                    assert_eq!(
                        result,
                        TerrainBatchApplied {
                            stale_dropped: 1,
                            ..Default::default()
                        },
                        "{context}"
                    );
                    assert_eq!(snapshot(&store, vehicle), before, "{context}");
                    continue;
                }
                assert_eq!(
                    result,
                    TerrainBatchApplied {
                        filled: u32::from(kind == "filled"),
                        void: u32::from(kind == "void"),
                        failed: u32::from(kind == "failed"),
                        ..Default::default()
                    },
                    "{context}"
                );
                let row = provenance(&store, vehicle, 1).unwrap();
                assert_eq!(row["attempts"], if forward { 1 } else { 8 }, "{context}");
                assert_eq!(row["at"], C9_COMMIT, "{context}");
                assert_eq!(
                    row["retry"],
                    if kind == "failed" {
                        C9_COMMIT + if forward { 10 } else { 1_000 }
                    } else {
                        0
                    },
                    "{context}"
                );
                assert_eq!(
                    row["status"],
                    match kind {
                        "filled" => "success",
                        "void" => "void",
                        _ => "failed",
                    },
                    "{context}"
                );
                assert_eq!(
                    elevation(&store, vehicle, 1),
                    (kind == "filled").then_some(100),
                    "{context}"
                );
                assert_eq!(
                    mutations(&store, vehicle).len(),
                    usize::from(kind == "filled"),
                    "{context}"
                );
                assert_eq!(pair(&store, vehicle, 1), (None, None), "{context}");
                assert_eq!(
                    b3_c8_sb::export_revision(&store, vehicle),
                    i64::from(kind == "filled"),
                    "{context}"
                );
            }
        }
    }
}

#[test]
fn c9_r9_multiple_holds_keep_durable_frontier() {
    let (_dir, store, vehicle) = fixture();
    seed(&store, vehicle, &[1, 2, 3, 5, 7].map(|id| position(id, 1)));
    failed(&store, vehicle, 2, 1, C9_T0);
    let outcomes: Vec<_> = [7, 3, 2, 5, 1]
        .into_iter()
        .map(|id| {
            let mut result = outcome(vehicle, position(id, 1), if id == 7 { 130 } else { 100 });
            result.forward = id != 2;
            result
        })
        .collect();
    store.open().unwrap().execute(
        "UPDATE materialised_positions SET position_json = json_set(position_json, '$.longitude', 20.2)
         WHERE position_id IN (2, 3, 5)", [],
    ).unwrap();
    let retry_before = provenance(&store, vehicle, 2);
    assert_eq!(
        apply(&store, vehicle, &outcomes, &[], Some(7)),
        TerrainBatchApplied {
            filled: 2,
            stale_held: vec![3, 2, 5],
            drives_changed: 1,
            cursor_written: Some(2),
            ..Default::default()
        }
    );
    assert_eq!(pair(&store, vehicle, 1), (Some(30), Some(0)));
    assert_eq!(provenance(&store, vehicle, 2), retry_before);
    assert_eq!(
        selected(&store, vehicle)
            .iter()
            .map(|c| c.position.id)
            .collect::<Vec<_>>(),
        [3, 5]
    );
    // C9 R9: the same held frontier cannot rewrite or rewind an existing cursor.
    let before = snapshot(&store, vehicle);
    let observer = store.open().unwrap();
    let version = || {
        observer
            .query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    let before_version = version();
    assert_eq!(
        apply(&store, vehicle, &outcomes, &[], Some(7)),
        TerrainBatchApplied {
            stale_dropped: 2,
            stale_held: vec![3, 2, 5],
            ..Default::default()
        }
    );
    assert_eq!(snapshot(&store, vehicle), before);
    assert_eq!(version(), before_version);
    for (id, elevation, frontier) in [(3, 110, 4), (5, 120, 7)] {
        let current = selected(&store, vehicle)
            .into_iter()
            .find(|c| c.position.id == id)
            .unwrap();
        let result = apply(
            &store,
            vehicle,
            &[outcome(vehicle, current.position, elevation)],
            &[],
            Some(frontier),
        );
        assert_eq!(
            result,
            TerrainBatchApplied {
                filled: 1,
                cursor_written: Some(frontier),
                ..Default::default()
            }
        );
        assert_eq!(pair(&store, vehicle, 1), (Some(30), Some(0)));
    }
    assert_eq!(provenance(&store, vehicle, 2), retry_before);
    assert!(selected(&store, vehicle).is_empty());
}

#[test]
fn c9_r39_interleaved_drives_recompute_once_after_positions() {
    let (_dir, store, vehicle) = fixture();
    seed(
        &store,
        vehicle,
        &(1..=6)
            .map(|id| {
                position(
                    id,
                    match id {
                        1 | 2 => 1,
                        3 | 4 => 2,
                        _ => id,
                    },
                )
            })
            .collect::<Vec<_>>(),
    );
    let connection = store.open().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE c9_recompute_order(drive_id INTEGER, position_mutations INTEGER) STRICT;
         CREATE TRIGGER c9_capture_recompute AFTER UPDATE OF ascent, descent ON materialised_drives
         BEGIN INSERT INTO c9_recompute_order SELECT NEW.drive_id,
             COUNT(*) FROM sync_mutations WHERE entity = 'position'; END;",
        )
        .unwrap();
    let mut outcomes = vec![
        outcome(vehicle, position(3, 2), 100),
        outcome(vehicle, position(1, 1), 200),
        outcome(vehicle, position(4, 2), 110),
        outcome(vehicle, position(2, 1), 196),
        outcome(vehicle, position(5, 5), 0),
        outcome(vehicle, position(6, 6), 0),
    ];
    outcomes[4].kind = TerrainOutcomeKind::Void {
        tile: tile(),
        error_code: None,
    };
    outcomes[5].kind = TerrainOutcomeKind::Failed {
        error_code: "io",
        schedule: RetrySchedule {
            base_ms: 10,
            cap_ms: 100,
        },
        tile: None,
    };
    assert_eq!(
        apply(&store, vehicle, &outcomes, &[], Some(6)),
        TerrainBatchApplied {
            filled: 4,
            void: 1,
            failed: 1,
            drives_changed: 2,
            cursor_written: Some(6),
            ..Default::default()
        }
    );
    let recomputes: Vec<(i64, i64)> = connection
        .prepare("SELECT drive_id, position_mutations FROM c9_recompute_order ORDER BY rowid")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(recomputes, [(2, 4), (1, 4)]);
    assert_eq!(pair(&store, vehicle, 1), (Some(0), Some(4)));
    assert_eq!(pair(&store, vehicle, 2), (Some(10), Some(0)));
    for id in [5, 6] {
        assert_eq!(pair(&store, vehicle, id), (None, None));
    }
    let log = mutations(&store, vehicle);
    assert_eq!(
        log.iter()
            .map(|(e, id, _)| (e.as_str(), *id))
            .collect::<Vec<_>>(),
        [
            ("position", 3),
            ("position", 1),
            ("position", 4),
            ("position", 2),
            ("drive", 2),
            ("drive", 1)
        ]
    );
    assert_eq!(
        (log[4].2["ascent"].as_i64(), log[4].2["descent"].as_i64()),
        (Some(10), Some(0))
    );
    assert_eq!(
        (log[5].2["ascent"].as_i64(), log[5].2["descent"].as_i64()),
        (Some(0), Some(4))
    );
    assert_eq!(b3_c8_sb::export_revision(&store, vehicle), 1);
}
